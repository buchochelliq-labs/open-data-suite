//! In-memory [`Executor`] and [`RelationInspector`].
//!
//! The executor simulates a run on a few workers (threads), so its
//! [run events](ods_sdk::contracts::run_events) show nodes running in parallel, a
//! failure, the nodes it stops downstream, and nodes that report rows and nodes that
//! don't (#322).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use ods_core::state::{Timestamp, TimestampMs};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, NodeExecution,
    PrepareReport, PrepareRequest, RequestedNode,
};
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence, RelationReport};
use ods_sdk::contracts::run_events::{
    CheckStatus, ErrorSummary, NodeRunStats, NodeRunStatus, RunEvent, RunEventKind, RunEventSink,
    RunOutcome, events_from_report,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;
use crate::clock::FakeClock;

#[derive(Debug, Default)]
struct Inner {
    runs: u64,
    built: Vec<String>,
    dropped: BTreeSet<String>,
}

/// An executor over a set of known nodes, some of which fail. It builds nothing real:
/// it records which nodes it "built", for tests to check. Every known node's relation
/// exists until [dropped](FakeExecutor::drop_relation); building it creates it again.
#[derive(Debug, Clone)]
pub struct FakeExecutor {
    clock: FakeClock,
    nodes: BTreeSet<String>,
    failing: BTreeSet<String>,
    checks: BTreeMap<String, Vec<String>>,
    sources: BTreeMap<String, FakeSource>,
    inspection_fails: bool,
    stats: BTreeMap<String, FakeStats>,
    upstream: BTreeMap<String, BTreeSet<String>>,
    threads: usize,
    run_events: bool,
    inner: Arc<Mutex<Inner>>,
}

/// What a node's simulated build reports (#322).
#[derive(Debug, Clone, Default)]
struct FakeStats {
    duration_ms: Option<u64>,
    rows_affected: Option<i64>,
    extras: BTreeMap<String, String>,
    error: Option<String>,
}

/// How long a node takes when no timing is set.
const DEFAULT_DURATION_MS: u64 = 100;

/// A source with checks (#232).
#[derive(Debug, Clone, Default)]
struct FakeSource {
    checks: Vec<String>,
    readers: BTreeSet<String>,
    failing: bool,
}

impl FakeExecutor {
    /// An executor that knows `nodes`, all of which build.
    pub fn new(clock: FakeClock, nodes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            clock,
            nodes: nodes.into_iter().map(Into::into).collect(),
            failing: BTreeSet::new(),
            checks: BTreeMap::new(),
            sources: BTreeMap::new(),
            inspection_fails: false,
            stats: BTreeMap::new(),
            upstream: BTreeMap::new(),
            threads: 2,
            run_events: true,
            inner: Arc::default(),
        }
    }

    /// Makes `node` fail with `message`, as the engine would word it (#322).
    #[must_use]
    pub fn failing_with(mut self, node: impl Into<String>, message: impl Into<String>) -> Self {
        let node = node.into();
        self.stats.entry(node.clone()).or_default().error = Some(message.into());
        self.failing(node)
    }

    /// Makes building `node` take `ms` milliseconds of simulated time (default 100).
    #[must_use]
    pub fn with_duration(mut self, node: impl Into<String>, ms: u64) -> Self {
        self.stats.entry(node.into()).or_default().duration_ms = Some(ms);
        self
    }

    /// Makes `node` report `rows` rows affected when it builds. Without this it reports
    /// none, as many engines don't for views; a negative count is the engine's
    /// "unknown".
    #[must_use]
    pub fn with_rows(mut self, node: impl Into<String>, rows: i64) -> Self {
        self.stats.entry(node.into()).or_default().rows_affected = Some(rows);
        self
    }

    /// Makes `node` report something else about its build, e.g. a query id.
    #[must_use]
    pub fn with_extra(
        mut self,
        node: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.stats
            .entry(node.into())
            .or_default()
            .extras
            .insert(key.into(), value.into());
        self
    }

    /// Makes `node` depend on `parents`: when they are requested too, it waits for
    /// them, and is skipped if one fails or is skipped.
    #[must_use]
    pub fn with_upstream(
        mut self,
        node: impl Into<String>,
        parents: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.upstream
            .entry(node.into())
            .or_default()
            .extend(parents.into_iter().map(Into::into));
        self
    }

    /// Runs up to `threads` nodes at once (default 2, at least 1).
    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// Drops the `run_events` capability: events are then rebuilt from the report, as
    /// for an executor that can't report them as they happen.
    #[must_use]
    pub fn without_run_events(mut self) -> Self {
        self.run_events = false;
        self
    }

    /// Makes `node` fail whenever it is built.
    #[must_use]
    pub fn failing(mut self, node: impl Into<String>) -> Self {
        let node = node.into();
        self.nodes.insert(node.clone());
        self.failing.insert(node);
        self
    }

    /// Gives `node` checks, which pass whenever they run (in `Build` and `Test`
    /// modes). A node without checks is never fully checked.
    #[must_use]
    pub fn with_checks(
        mut self,
        node: impl Into<String>,
        checks: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let node = node.into();
        self.nodes.insert(node.clone());
        self.checks
            .insert(node, checks.into_iter().map(Into::into).collect());
        self
    }

    /// Gives `source` checks, which pass whenever they run, and the nodes that read it
    /// (#232).
    #[must_use]
    pub fn with_source(
        mut self,
        source: impl Into<String>,
        checks: impl IntoIterator<Item = impl Into<String>>,
        readers: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.sources.insert(
            source.into(),
            FakeSource {
                checks: checks.into_iter().map(Into::into).collect(),
                readers: readers.into_iter().map(Into::into).collect(),
                failing: false,
            },
        );
        self
    }

    /// Makes every check on `source` fail. In `Build` mode, the requested nodes that
    /// read it directly are then skipped.
    #[must_use]
    pub fn failing_source(mut self, source: impl Into<String>) -> Self {
        self.sources.entry(source.into()).or_default().failing = true;
        self
    }

    /// Makes [`inspect`](RelationInspector::inspect) fail, as when the warehouse can't
    /// be reached.
    #[must_use]
    pub fn failing_inspection(mut self) -> Self {
        self.inspection_fails = true;
        self
    }

    /// Drops `node`'s relation, as if someone dropped its table (shared between clones).
    pub fn drop_relation(&self, node: &str) {
        self.inner().dropped.insert(node.to_owned());
    }

    /// The nodes built so far, in order (shared between clones).
    pub fn built(&self) -> Vec<String> {
        self.inner().built.clone()
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> Timestamp {
        let secs = self
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Timestamp::from_unix(i64::try_from(secs).unwrap_or(i64::MAX))
    }
}

impl Provider for FakeExecutor {
    fn info(&self) -> ProviderInfo {
        let mut capabilities = CapabilitySet::from([Capability::RelationExistence]);
        if self.run_events {
            capabilities.insert(Capability::RunEvents);
        }
        ProviderInfo::new(KIND, "fake", env!("CARGO_PKG_VERSION"), capabilities)
    }
}

#[async_trait]
impl Executor for FakeExecutor {
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError> {
        Ok(PrepareReport::new(request.measure_sources, Vec::new()))
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionReport, ProviderError> {
        self.simulate(request).map(|(report, _)| report)
    }

    async fn execute_with_events(
        &self,
        request: &ExecutionRequest,
        events: &dyn RunEventSink,
    ) -> Result<ExecutionReport, ProviderError> {
        let (report, simulated) = self.simulate(request)?;
        let simulated = if self.run_events {
            simulated
        } else {
            events_from_report(request, &report)
        };
        for event in simulated {
            events.emit(event);
        }
        Ok(report)
    }
}

/// How a requested node's simulated run goes, decided before it is scheduled.
#[derive(Debug, Clone)]
struct Planned {
    status: ExecutionStatus,
    message: Option<String>,
    /// Whether it runs on a worker (and takes time), or ends as soon as it is reached.
    runs: bool,
    checks: Vec<String>,
}

/// How a requested source's checks went.
struct SourceOutcome {
    id: String,
    status: ExecutionStatus,
    message: Option<&'static str>,
    checks: Vec<String>,
}

/// A node running on a worker: when it ends, the worker, the node and when it started.
type Running<'r> = (u64, usize, &'r str, u64);

/// One simulated execution: its events so far, in time order, and how each node ended.
struct Sim<'a> {
    executor: &'a FakeExecutor,
    request: &'a ExecutionRequest,
    run_id: String,
    t0: TimestampMs,
    events: Vec<RunEvent>,
    planned: BTreeMap<&'a str, Planned>,
    /// Requested nodes once each, in request order.
    order: Vec<&'a str>,
    /// How each node ended, and the failed nodes that stopped it.
    done: BTreeMap<&'a str, (ExecutionStatus, Option<String>, Vec<String>)>,
    /// Milliseconds since the start.
    now: u64,
}

impl<'a> Sim<'a> {
    fn at(&self, offset: u64) -> TimestampMs {
        TimestampMs::from_unix_millis(
            self.t0
                .unix_millis()
                .saturating_add(i64::try_from(offset).unwrap_or(i64::MAX)),
        )
    }

    fn emit(&mut self, offset: u64, kind: RunEventKind) {
        let at = self.at(offset);
        self.events.push(RunEvent::new(
            self.run_id.clone(),
            self.request.scope.clone(),
            at,
            kind,
        ));
    }

    /// Runs the requested sources' checks, first, as `dbt build` does (#232). Returns
    /// their outcomes, the checks that failed and the nodes that read a failed source.
    fn check_sources(&mut self) -> (Vec<SourceOutcome>, Vec<String>, BTreeSet<String>) {
        let mut checks_failed = Vec::new();
        let mut blocked = BTreeSet::new();
        let mut outcomes = Vec::new();
        let mode = self.request.mode;
        for s in &self.request.sources {
            let outcome = |status, message, checks| SourceOutcome {
                id: s.id.clone(),
                status,
                message,
                checks,
            };
            let Some(source) = self
                .executor
                .sources
                .get(&s.id)
                .filter(|f| !f.checks.is_empty())
            else {
                outcomes.push(outcome(
                    ExecutionStatus::Skipped,
                    Some("no checks"),
                    Vec::new(),
                ));
                continue;
            };
            if mode == ExecutionMode::Run {
                outcomes.push(outcome(
                    ExecutionStatus::Skipped,
                    Some(NO_CHECKS_IN_RUN),
                    Vec::new(),
                ));
                continue;
            }
            let check_status = if source.failing {
                checks_failed.extend(source.checks.iter().cloned());
                blocked.extend(source.readers.iter().cloned());
                outcomes.push(outcome(
                    ExecutionStatus::Failed,
                    None,
                    source.checks.clone(),
                ));
                CheckStatus::Failed
            } else {
                outcomes.push(outcome(
                    ExecutionStatus::Success,
                    None,
                    source.checks.clone(),
                ));
                CheckStatus::Passed
            };
            for check in &source.checks {
                self.emit(
                    0,
                    RunEventKind::CheckFinished {
                        check: check.clone(),
                        covers: vec![s.id.clone()],
                        status: check_status,
                        // The fake engine says only that a source check failed.
                        failures: None,
                        error: None,
                    },
                );
            }
        }
        (outcomes, checks_failed, blocked)
    }

    /// Decides how each requested node goes, before upstream failures are known.
    fn plan(&mut self, blocked: &BTreeSet<String>) {
        let fake = self.executor;
        let mode = self.request.mode;
        for n in &self.request.nodes {
            let checks = fake.checks.get(&n.id).cloned().unwrap_or_default();
            let (status, message, runs) = if !fake.nodes.contains(&n.id) {
                (
                    ExecutionStatus::Failed,
                    Some("unknown node".to_owned()),
                    false,
                )
            } else if mode == ExecutionMode::Build && blocked.contains(&n.id) {
                (
                    ExecutionStatus::Skipped,
                    Some("a source it reads failed its checks".to_owned()),
                    false,
                )
            } else if fake.failing.contains(&n.id) {
                let message = fake.stats.get(&n.id).and_then(|s| s.error.clone());
                (
                    ExecutionStatus::Failed,
                    Some(message.unwrap_or_else(|| "failed".to_owned())),
                    true,
                )
            } else if mode == ExecutionMode::Test && checks.is_empty() {
                // A test run builds nothing, and tests nothing without checks.
                (
                    ExecutionStatus::Skipped,
                    Some("no checks".to_owned()),
                    false,
                )
            } else {
                (ExecutionStatus::Success, None, true)
            };
            if !self.planned.contains_key(n.id.as_str()) {
                self.order.push(&n.id);
                self.planned.insert(
                    &n.id,
                    Planned {
                        status,
                        message,
                        runs,
                        checks,
                    },
                );
            }
        }
        for n in &self.request.nodes {
            self.emit(0, RunEventKind::NodeQueued { node: n.id.clone() });
        }
    }

    /// The requested nodes `id` waits for.
    fn parents(&self, id: &str) -> Vec<&'a str> {
        let planned = &self.planned;
        self.executor
            .upstream
            .get(id)
            .map(|parents| {
                parents
                    .iter()
                    .filter_map(|p| planned.get_key_value(p.as_str()).map(|(k, _)| *k))
                    .filter(|p| *p != id)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn waiting(&self, id: &str, running: &[Running<'_>]) -> bool {
        !self.done.contains_key(id) && !running.iter().any(|r| r.2 == id)
    }

    /// Ends, now, every waiting node that won't run: one stopped by a failure upstream,
    /// or one the engine doesn't try (unknown, blocked by a source, nothing to test).
    fn end_what_cannot_run(&mut self, running: &[Running<'_>]) {
        let mut changed = true;
        while changed {
            changed = false;
            for id in self.order.clone() {
                let parents = self.parents(id);
                if !self.waiting(id, running) || !parents.iter().all(|p| self.done.contains_key(p))
                {
                    continue;
                }
                let mut stopped_by = Vec::new();
                let mut upstream_failed = false;
                for p in &parents {
                    match self.done.get(p) {
                        Some((ExecutionStatus::Success, ..)) | None => {}
                        Some((ExecutionStatus::Failed, ..)) => {
                            upstream_failed = true;
                            stopped_by.push((*p).to_owned());
                        }
                        Some((_, _, roots)) => {
                            upstream_failed = true;
                            stopped_by.extend(roots.iter().cloned());
                        }
                    }
                }
                let Some(plan) = self.planned.get(id) else {
                    continue;
                };
                if !upstream_failed && plan.runs {
                    continue;
                }
                let (outcome, message) = if upstream_failed {
                    (
                        ExecutionStatus::Skipped,
                        Some("an upstream node failed".to_owned()),
                    )
                } else {
                    (plan.status, plan.message.clone())
                };
                let error = (outcome == ExecutionStatus::Failed)
                    .then(|| message.as_deref().and_then(ErrorSummary::from_message))
                    .flatten();
                let finished = NodeRunStats::new(outcome.into())
                    .with_error(error)
                    .with_blocked_by(stopped_by.clone());
                self.emit(
                    self.now,
                    RunEventKind::NodeFinished {
                        node: id.to_owned(),
                        stats: finished,
                    },
                );
                self.done.insert(id, (outcome, message, stopped_by));
                changed = true;
            }
        }
    }

    /// Starts what is ready, in request order, while workers are free.
    fn start_what_is_ready(&mut self, running: &mut Vec<Running<'a>>, free: &mut BTreeSet<usize>) {
        for id in self.order.clone() {
            if free.is_empty() {
                break;
            }
            if !self.waiting(id, running)
                || !self.parents(id).iter().all(|p| self.done.contains_key(p))
            {
                continue;
            }
            let Some(worker) = free.pop_first() else {
                break;
            };
            let took = self
                .executor
                .stats
                .get(id)
                .and_then(|s| s.duration_ms)
                .unwrap_or(DEFAULT_DURATION_MS);
            self.emit(
                self.now,
                RunEventKind::NodeStarted {
                    node: id.to_owned(),
                    thread: Some(format!("thread-{worker}")),
                },
            );
            running.push((self.now + took, worker, id, self.now));
        }
    }

    /// Ends a node that ran from `start` to `end` on `worker`.
    fn finish(&mut self, (end, worker, id, start): Running<'a>) {
        let Some(plan) = self.planned.get(id).cloned() else {
            return;
        };
        let fake = self.executor.stats.get(id).cloned().unwrap_or_default();
        let took = end - start;
        let compile = took / 4;
        let outcome = plan.status;
        let error = (outcome == ExecutionStatus::Failed)
            .then(|| plan.message.as_deref().and_then(ErrorSummary::from_message))
            .flatten();
        let mut finished = NodeRunStats::new(outcome.into())
            .with_times(Some(self.at(start)), Some(self.at(end)))
            .with_durations(Some(took), Some(compile), Some(took - compile))
            .with_thread(format!("thread-{worker}"))
            .with_error(error);
        let mode = self.request.mode;
        if outcome == ExecutionStatus::Success && mode != ExecutionMode::Test {
            finished = finished.with_rows_affected(fake.rows_affected);
            for (key, value) in &fake.extras {
                finished = finished.with_extra(key.clone(), value);
            }
        }
        self.emit(
            end,
            RunEventKind::NodeFinished {
                node: id.to_owned(),
                stats: finished,
            },
        );
        if outcome == ExecutionStatus::Success && mode != ExecutionMode::Run {
            for check in &plan.checks {
                self.emit(
                    end,
                    RunEventKind::CheckFinished {
                        check: check.clone(),
                        covers: vec![id.to_owned()],
                        status: CheckStatus::Passed,
                        failures: None,
                        error: None,
                    },
                );
            }
        }
        self.done.insert(id, (outcome, plan.message, Vec::new()));
    }

    /// Runs the schedule to the end. Returns when the last node ended.
    fn schedule(&mut self) -> u64 {
        let mut running: Vec<Running<'a>> = Vec::new();
        let mut free: BTreeSet<usize> = (1..=self.executor.threads).collect();
        loop {
            self.end_what_cannot_run(&running);
            self.start_what_is_ready(&mut running, &mut free);
            let Some(&(end, ..)) = running.iter().min() else {
                break;
            };
            self.now = end;
            running.sort_unstable();
            let (ending, still): (Vec<_>, Vec<_>) = running.into_iter().partition(|r| r.0 == end);
            running = still;
            for node in ending {
                free.insert(node.1);
                self.finish(node);
            }
        }
        // A cycle among the requested nodes: what never became ready isn't tried.
        for id in self.order.clone() {
            if !self.done.contains_key(id) {
                self.emit(
                    self.now,
                    RunEventKind::NodeFinished {
                        node: id.to_owned(),
                        stats: NodeRunStats::new(NodeRunStatus::Skipped),
                    },
                );
                let why = Some("it waits on itself".to_owned());
                self.done
                    .insert(id, (ExecutionStatus::Skipped, why, Vec::new()));
            }
        }
        self.now
    }
}

/// Why a source's checks didn't run in a run without tests.
const NO_CHECKS_IN_RUN: &str = "no checks run without tests";

impl FakeExecutor {
    /// Runs the request: the report, and the events of a simulated schedule on
    /// [`threads`](Self::threads) workers, in time order.
    fn simulate(
        &self,
        request: &ExecutionRequest,
    ) -> Result<(ExecutionReport, Vec<RunEvent>), ProviderError> {
        if request.is_empty() || (request.mode == ExecutionMode::Run && request.nodes.is_empty()) {
            return Err(ProviderError::Other(
                "nothing to execute: the request names no nodes".to_owned(),
            ));
        }
        let started = self.now();
        let mut inner = self.inner();
        inner.runs += 1;
        let mut sim = Sim {
            executor: self,
            request,
            run_id: format!("fake-run-{}", inner.runs),
            t0: TimestampMs::from(started),
            events: Vec::new(),
            planned: BTreeMap::new(),
            order: Vec::new(),
            done: BTreeMap::new(),
            now: 0,
        };
        sim.emit(
            0,
            RunEventKind::RunStarted {
                nodes: request.nodes.iter().map(|n| n.id.clone()).collect(),
                mode: request.mode,
                live: true,
            },
        );
        let (source_outcomes, checks_failed, blocked) = sim.check_sources();
        sim.plan(&blocked);
        let makespan = sim.schedule();

        // Each execution takes at least a second, so runs never share a timestamp.
        self.clock
            .advance(Duration::from_secs(makespan.div_ceil(1000).max(1)));
        let finished = self.now();
        let nodes = request
            .nodes
            .iter()
            .map(|n| {
                // Every requested node is planned and ends in `schedule`; one that
                // somehow didn't is reported skipped, never a success.
                let (status, message, _) = sim.done.get(n.id.as_str()).cloned().unwrap_or((
                    ExecutionStatus::Skipped,
                    Some("not scheduled".to_owned()),
                    Vec::new(),
                ));
                if status == ExecutionStatus::Success && request.mode != ExecutionMode::Test {
                    inner.built.push(n.id.clone());
                    inner.dropped.remove(&n.id);
                }
                let ran_checks =
                    status == ExecutionStatus::Success && request.mode != ExecutionMode::Run;
                let passed = if ran_checks {
                    sim.planned
                        .get(n.id.as_str())
                        .map(|p| p.checks.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                NodeExecution::new(n.id.clone(), status, Some(finished), message)
                    .with_checks_passed(passed)
            })
            .collect();
        let sources = source_outcomes
            .into_iter()
            .map(|s| {
                let completed = (s.message != Some(NO_CHECKS_IN_RUN)).then_some(finished);
                let execution =
                    NodeExecution::new(s.id, s.status, completed, s.message.map(str::to_owned));
                match s.status {
                    ExecutionStatus::Failed => execution.with_checks_failed(s.checks),
                    ExecutionStatus::Success => execution.with_checks_passed(s.checks),
                    _ => execution,
                }
            })
            .collect();
        let report = ExecutionReport::new(
            sim.run_id.clone(),
            Some(started),
            finished,
            nodes,
            checks_failed,
        )
        .with_sources(sources);
        sim.emit(
            makespan,
            RunEventKind::RunFinished {
                outcome: if report.succeeded {
                    RunOutcome::Succeeded
                } else {
                    RunOutcome::Failed
                },
            },
        );
        Ok((report, sim.events))
    }
}

#[async_trait]
impl RelationInspector for FakeExecutor {
    async fn inspect(&self, nodes: &[RequestedNode]) -> Result<RelationReport, ProviderError> {
        if self.inspection_fails {
            return Err(ProviderError::Other(
                "the fake warehouse can't be reached".to_owned(),
            ));
        }
        let inner = self.inner();
        let nodes = nodes
            .iter()
            .map(|n| {
                let presence = if !self.nodes.contains(&n.id) {
                    RelationPresence::Unknown("unknown node".to_owned())
                } else if inner.dropped.contains(&n.id) {
                    RelationPresence::Missing
                } else {
                    RelationPresence::Present {
                        kind: Some("table".to_owned()),
                    }
                };
                (n.id.clone(), presence)
            })
            .collect();
        Ok(RelationReport::new(nodes))
    }
}
