//! Run events: a run's progress as it happens, with each node's stats (#322, ADR-0024).
//!
//! An [`Executor`](super::executor::Executor) with the
//! [`run_events`](ods_core::Capability::RunEvents) capability reports, through a
//! [`RunEventSink`], what it does while it runs:
//!
//! - [`run_started`](RunEventKind::RunStarted), first, exactly once;
//! - [`node_queued`](RunEventKind::NodeQueued), [`node_started`](RunEventKind::NodeStarted)
//!   and [`node_finished`](RunEventKind::NodeFinished) per node, in that order, with
//!   the node's [`NodeRunStats`] on the finish;
//! - [`check_finished`](RunEventKind::CheckFinished) per check (e.g. a data test), with
//!   the nodes it covers, so each node can count its tests;
//! - [`run_finished`](RunEventKind::RunFinished), last, exactly once, also when the
//!   execution ends in an error after it started.
//!
//! Every event carries the run id (the report's), the request's scope and when it
//! happened. Events are emitted in order and their times never go backwards. Every
//! requested node gets exactly one `node_finished`, and a node the engine didn't report
//! on finishes as `skipped` or `unknown`, never `success`.
//!
//! **Missing is never zero.** Every stat an engine doesn't report is `None`: rows
//! affected in particular, which many engines don't report for views or merges.
//!
//! **No values.** Events hold no SQL, no variable values and no secrets (AGENTS.md rule
//! 9). There is no field for SQL or the command line; an error is kept only as an
//! [`ErrorSummary`], which removes quoted values, numbers and SQL; and adapter extras
//! are single-line scalars, cut short.
//!
//! An executor without the capability still gives a host the run's events, rebuilt from
//! its final report by [`events_from_report`]: the run and each node's outcome, with no
//! times beyond the report's, no rows and no extras. They say so with
//! [`live: false`](RunEventKind::RunStarted::live).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use ods_core::SchemaVersion;
use ods_core::redact;
use ods_core::state::TimestampMs;
use serde::{Deserialize, Serialize};

use super::executor::{ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus};

/// The version of [`RunEvent`], carried by every event and every journal line.
pub const RUN_EVENTS_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The longest an [`ErrorSummary`] message is, in characters.
pub const MAX_SUMMARY_CHARS: usize = 200;

/// The longest an adapter extra's value is, in characters.
pub const MAX_EXTRA_CHARS: usize = 120;

/// The most adapter extras a node keeps; the first by key are kept.
pub const MAX_EXTRAS: usize = 16;

/// One thing that happened during a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunEvent {
    /// [`RUN_EVENTS_SCHEMA_VERSION`] when written.
    pub schema_version: SchemaVersion,
    /// The run's id: the [`ExecutionReport::run_id`] of the same execution.
    pub run_id: String,
    /// The state scope the run is for, as the request gave it, if any.
    #[serde(default)]
    pub scope: Option<String>,
    /// When it happened.
    pub at: TimestampMs,
    /// What happened.
    #[serde(flatten)]
    pub kind: RunEventKind,
}

impl RunEvent {
    /// An event, at the current schema version.
    pub fn new(
        run_id: impl Into<String>,
        scope: Option<String>,
        at: TimestampMs,
        kind: RunEventKind,
    ) -> Self {
        Self {
            schema_version: RUN_EVENTS_SCHEMA_VERSION,
            run_id: run_id.into(),
            scope,
            at,
            kind,
        }
    }

    /// The node the event is about, for node events.
    pub fn node(&self) -> Option<&str> {
        match &self.kind {
            RunEventKind::NodeQueued { node }
            | RunEventKind::NodeStarted { node, .. }
            | RunEventKind::NodeFinished { node, .. } => Some(node),
            _ => None,
        }
    }
}

/// What happened, serialized as `"kind": "<snake_case name>"` beside its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "events are made once and passed on; boxing the stats would only complicate providers"
)]
pub enum RunEventKind {
    /// The run started.
    RunStarted {
        /// The requested nodes, in request order.
        nodes: Vec<String>,
        /// Build, run or test.
        mode: ExecutionMode,
        /// Whether the executor reported events as they happened (`run_events`), or
        /// they were rebuilt from its final report afterwards.
        live: bool,
    },
    /// A node is waiting to run.
    NodeQueued {
        /// The node's id.
        node: String,
    },
    /// A node started.
    NodeStarted {
        /// The node's id.
        node: String,
        /// The engine's worker that runs it, if it says.
        #[serde(default)]
        thread: Option<String>,
    },
    /// A node finished, however it ended.
    NodeFinished {
        /// The node's id.
        node: String,
        /// How it ended, and what the engine reported about it.
        stats: NodeRunStats,
    },
    /// A check (e.g. a data test) finished.
    CheckFinished {
        /// The check's id.
        check: String,
        /// The nodes it checks, sorted. Empty when the executor can't tell.
        covers: Vec<String>,
        /// How it ended.
        status: CheckStatus,
    },
    /// The run finished. Nodes that hadn't finished by then are `unknown`.
    RunFinished {
        /// How it ended.
        outcome: RunOutcome,
    },
}

/// Where a node is in a run, or how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeRunStatus {
    /// Waiting to run.
    Queued,
    /// Running now.
    Running,
    /// Built (or, in a test run, tested).
    Success,
    /// Tried and failed.
    Error,
    /// Not tried, e.g. because something upstream failed.
    Skipped,
    /// The engine didn't say. Never read as a success.
    Unknown,
}

impl NodeRunStatus {
    /// Whether the node is done: it won't change again in this run.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

impl From<ExecutionStatus> for NodeRunStatus {
    fn from(status: ExecutionStatus) -> Self {
        match status {
            ExecutionStatus::Success => Self::Success,
            ExecutionStatus::Failed => Self::Error,
            ExecutionStatus::Skipped => Self::Skipped,
        }
    }
}

/// How a check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckStatus {
    /// Ran and passed.
    Passed,
    /// Ran and failed, or couldn't run because of an error.
    Failed,
    /// Ran and found something, at warning severity.
    Warned,
    /// Didn't run.
    Skipped,
    /// The engine didn't say.
    Unknown,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunOutcome {
    /// Every node and check succeeded.
    Succeeded,
    /// Something failed or was skipped.
    Failed,
    /// The outcome couldn't be read, e.g. the execution ended in an error.
    Unknown,
}

/// How many checks on a node ended each way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TestCounts {
    /// Passed.
    pub passed: u32,
    /// Failed, or errored.
    pub failed: u32,
    /// Warned.
    pub warned: u32,
    /// Skipped, or ended in a way the engine didn't say.
    pub skipped: u32,
}

impl TestCounts {
    /// Counts one more check that ended as `status`.
    pub fn add(&mut self, status: CheckStatus) {
        let count = match status {
            CheckStatus::Passed => &mut self.passed,
            CheckStatus::Failed => &mut self.failed,
            CheckStatus::Warned => &mut self.warned,
            CheckStatus::Skipped | CheckStatus::Unknown => &mut self.skipped,
        };
        *count = count.saturating_add(1);
    }
}

/// A failed node's error, safe to keep and show: its kind and the first line of the
/// engine's message, with quoted values, numbers and SQL removed
/// ([`ods_core::redact::summary_line`]). The full message stays in the engine's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ErrorSummary {
    /// The error's kind, when the message starts with one (`KeyError`, `Binder Error`).
    #[serde(default)]
    pub kind: Option<String>,
    /// The first line of the message, values and SQL removed.
    pub message: String,
    /// Where the full message is, for people (e.g. a log file), if the executor knows.
    #[serde(default)]
    pub details_at: Option<String>,
}

impl ErrorSummary {
    /// A summary of an engine's message; `None` if it has no text. Only this removes
    /// values, so every summary is safe to keep.
    pub fn from_message(message: &str) -> Option<Self> {
        let message = redact::summary_line(message, MAX_SUMMARY_CHARS)?;
        let kind = message.split_once(": ").and_then(|(head, _)| {
            let looks_like_a_kind = (1..=40).contains(&head.chars().count())
                && head.starts_with(|c: char| c.is_ascii_alphabetic())
                && head
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.'));
            looks_like_a_kind.then(|| head.to_owned())
        });
        Some(Self {
            kind,
            message,
            details_at: None,
        })
    }

    /// Names the error's kind, when the engine gives it apart from the message (e.g. a
    /// header line). Values are removed from it too, and it is cut to 40 characters.
    #[must_use]
    pub fn with_kind(mut self, kind: &str) -> Self {
        self.kind = redact::summary_line(kind, 40);
        self
    }

    /// Says where the full message is.
    #[must_use]
    pub fn with_details_at(mut self, at: impl Into<String>) -> Self {
        self.details_at = Some(at.into());
        self
    }
}

/// What a run reported about one node. Every stat the engine didn't report is `None`,
/// never zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeRunStats {
    /// Where the node is, or how it ended.
    pub status: NodeRunStatus,
    /// When it started.
    #[serde(default)]
    pub started_at: Option<TimestampMs>,
    /// When it finished.
    #[serde(default)]
    pub finished_at: Option<TimestampMs>,
    /// How long it took, as the engine timed it. When `None`, a reader may compute it
    /// from [`started_at`](Self::started_at) and [`finished_at`](Self::finished_at)
    /// ([`Self::took_ms`]).
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// How long compiling it took, if the engine timed that on its own.
    #[serde(default)]
    pub compile_ms: Option<u64>,
    /// How long executing it took, if the engine timed that on its own.
    #[serde(default)]
    pub execute_ms: Option<u64>,
    /// Rows the engine says it wrote, if it says.
    #[serde(default)]
    pub rows_affected: Option<u64>,
    /// Anything else the engine reported about the node's execution (e.g. bytes
    /// processed, a query id), by the engine's own key. Single-line scalars only, cut to
    /// [`MAX_EXTRA_CHARS`]; at most [`MAX_EXTRAS`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adapter: BTreeMap<String, String>,
    /// The engine's worker that ran it.
    #[serde(default)]
    pub thread: Option<String>,
    /// Why it failed.
    #[serde(default)]
    pub error: Option<ErrorSummary>,
    /// How its checks ended; `None` when none ran.
    #[serde(default)]
    pub tests: Option<TestCounts>,
    /// For a skipped node, the failed nodes upstream that stopped it, sorted, when the
    /// executor knows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

impl NodeRunStats {
    /// Stats with only a status: everything else not reported.
    pub fn new(status: NodeRunStatus) -> Self {
        Self {
            status,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            compile_ms: None,
            execute_ms: None,
            rows_affected: None,
            adapter: BTreeMap::new(),
            thread: None,
            error: None,
            tests: None,
            blocked_by: Vec::new(),
        }
    }

    /// When it started and finished.
    #[must_use]
    pub fn with_times(
        mut self,
        started: Option<TimestampMs>,
        finished: Option<TimestampMs>,
    ) -> Self {
        self.started_at = started;
        self.finished_at = finished;
        self
    }

    /// How long it took, in total and compiling and executing, as the engine timed it.
    #[must_use]
    pub fn with_durations(
        mut self,
        total_ms: Option<u64>,
        compile_ms: Option<u64>,
        execute_ms: Option<u64>,
    ) -> Self {
        self.duration_ms = total_ms;
        self.compile_ms = compile_ms;
        self.execute_ms = execute_ms;
        self
    }

    /// Rows the engine says it wrote. A negative count (some engines' "unknown") is
    /// not reported.
    #[must_use]
    pub fn with_rows_affected(mut self, rows: Option<i64>) -> Self {
        self.rows_affected = rows.and_then(|r| u64::try_from(r).ok());
        self
    }

    /// Something else the engine reported. The value is cut to its first line and
    /// [`MAX_EXTRA_CHARS`]; an empty key or value, or one past [`MAX_EXTRAS`], is
    /// dropped.
    #[must_use]
    pub fn with_extra(mut self, key: impl Into<String>, value: impl AsRef<str>) -> Self {
        let key: String = key.into().chars().filter(|c| !c.is_control()).collect();
        let value = value.as_ref().lines().next().unwrap_or_default().trim();
        let mut value: String = value.chars().filter(|c| !c.is_control()).collect();
        if value.chars().count() > MAX_EXTRA_CHARS {
            value = value.chars().take(MAX_EXTRA_CHARS - 1).collect();
            value.push('…');
        }
        if !key.is_empty()
            && !value.is_empty()
            && (self.adapter.len() < MAX_EXTRAS || self.adapter.contains_key(&key))
        {
            self.adapter.insert(key, value);
        }
        self
    }

    /// The worker that ran it.
    #[must_use]
    pub fn with_thread(mut self, thread: impl Into<String>) -> Self {
        self.thread = Some(thread.into());
        self
    }

    /// Why it failed.
    #[must_use]
    pub fn with_error(mut self, error: Option<ErrorSummary>) -> Self {
        self.error = error;
        self
    }

    /// How its checks ended.
    #[must_use]
    pub fn with_tests(mut self, tests: TestCounts) -> Self {
        self.tests = Some(tests);
        self
    }

    /// The failed nodes upstream that stopped it.
    #[must_use]
    pub fn with_blocked_by(mut self, mut nodes: Vec<String>) -> Self {
        nodes.sort();
        nodes.dedup();
        self.blocked_by = nodes;
        self
    }

    /// How long it took: the engine's own timing, or else the time from its start to
    /// its finish when both were recorded; `None` otherwise.
    pub fn took_ms(&self) -> Option<u64> {
        self.duration_ms
            .or_else(|| self.finished_at?.millis_since(self.started_at?))
    }
}

/// Receives a run's events as they happen. Implementations must be quick (e.g. append
/// a line to a file), and deal with their own failures: a run doesn't stop because its
/// events couldn't be kept.
pub trait RunEventSink: Send + Sync {
    /// Receives the next event.
    fn emit(&self, event: RunEvent);
}

/// A sink that keeps every event in memory, for tests and short runs.
#[derive(Debug, Default)]
pub struct CollectedEvents(Mutex<Vec<RunEvent>>);

impl CollectedEvents {
    /// An empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The events so far, in order.
    pub fn events(&self) -> Vec<RunEvent> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl RunEventSink for CollectedEvents {
    fn emit(&self, event: RunEvent) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}

/// The events of an execution whose executor didn't report them as they happened,
/// rebuilt from its request and report: the run, each node's outcome (with its
/// completion time and error summary) and each check's, and nothing the report doesn't
/// say. Times never go backwards. `run_started` says `live: false`.
pub fn events_from_report(request: &ExecutionRequest, report: &ExecutionReport) -> Vec<RunEvent> {
    let finished = TimestampMs::from(report.finished_at);
    let mut at = report.started_at.map_or(finished, TimestampMs::from);
    let event = |at: TimestampMs, kind| {
        RunEvent::new(report.run_id.clone(), request.scope.clone(), at, kind)
    };
    let mut events = vec![event(
        at,
        RunEventKind::RunStarted {
            nodes: request.nodes.iter().map(|n| n.id.clone()).collect(),
            mode: request.mode,
            live: false,
        },
    )];
    // Which nodes each check covers, and how it ended: failed beats skipped beats
    // passed, as a check that failed anywhere failed.
    let mut checks: BTreeMap<&str, (CheckStatus, Vec<String>)> = BTreeMap::new();
    for node in report.nodes.iter().chain(&report.sources) {
        for (list, status) in [
            (&node.checks_passed, CheckStatus::Passed),
            (&node.checks_skipped, CheckStatus::Skipped),
            (&node.checks_failed, CheckStatus::Failed),
        ] {
            for check in list {
                let entry = checks.entry(check.as_str()).or_insert((status, Vec::new()));
                if rank(status) > rank(entry.0) {
                    entry.0 = status;
                }
                entry.1.push(node.node.clone());
            }
        }
    }
    for check in &report.checks_failed {
        checks
            .entry(check.as_str())
            .or_insert((CheckStatus::Failed, Vec::new()))
            .0 = CheckStatus::Failed;
    }
    for node in &report.nodes {
        let completed = node.completed_at.map(TimestampMs::from);
        at = at.max(completed.unwrap_or(finished));
        let status = NodeRunStatus::from(node.status);
        let error = match status {
            NodeRunStatus::Error => node.message.as_deref().and_then(ErrorSummary::from_message),
            _ => None,
        };
        events.push(event(
            at,
            RunEventKind::NodeFinished {
                node: node.node.clone(),
                stats: NodeRunStats::new(status)
                    .with_times(None, completed)
                    .with_error(error),
            },
        ));
    }
    at = at.max(finished);
    for (check, (status, mut covers)) in checks {
        covers.sort();
        covers.dedup();
        events.push(event(
            at,
            RunEventKind::CheckFinished {
                check: check.to_owned(),
                covers,
                status,
            },
        ));
    }
    events.push(event(
        at,
        RunEventKind::RunFinished {
            outcome: if report.succeeded {
                RunOutcome::Succeeded
            } else {
                RunOutcome::Failed
            },
        },
    ));
    events
}

fn rank(status: CheckStatus) -> u8 {
    match status {
        CheckStatus::Passed => 0,
        CheckStatus::Warned => 1,
        CheckStatus::Unknown => 2,
        CheckStatus::Skipped => 3,
        CheckStatus::Failed => 4,
    }
}

/// One node's part in a run, as its events tell it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeSummary {
    /// The node's id.
    pub node: String,
    /// Whether the run was asked to build it (rather than the engine running it
    /// anyway).
    pub requested: bool,
    /// Its latest stats.
    pub stats: NodeRunStats,
}

/// A run's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunTotals {
    /// From the run's start to its finish, if both were recorded.
    pub duration_ms: Option<u64>,
    /// How many nodes are in each status.
    pub by_status: BTreeMap<NodeRunStatus, usize>,
    /// Rows written, summed over the nodes that reported them. A lower bound when
    /// [`rows_unreported`](Self::rows_unreported) isn't zero.
    pub rows_affected: u64,
    /// Nodes that built but didn't report rows.
    pub rows_unreported: usize,
}

impl RunTotals {
    /// Whether [`rows_affected`](Self::rows_affected) is only "at least": some nodes
    /// that built didn't report their rows.
    pub fn rows_is_lower_bound(&self) -> bool {
        self.rows_unreported > 0
    }

    /// How many nodes ended as `status`.
    pub fn count(&self, status: NodeRunStatus) -> usize {
        self.by_status.get(&status).copied().unwrap_or(0)
    }
}

/// A run, as its events tell it: what is known now, whether the run is still going or
/// finished. Built by folding events in order ([`RunSummary::from_events`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunSummary {
    /// The run's id, once an event said it.
    pub run_id: Option<String>,
    /// Its scope, if its events carry one.
    pub scope: Option<String>,
    /// Build, run or test, once it started.
    pub mode: Option<ExecutionMode>,
    /// Whether its events were reported as they happened.
    pub live: bool,
    /// When it started.
    pub started_at: Option<TimestampMs>,
    /// When it finished; `None` while it runs, or if it stopped without saying.
    pub finished_at: Option<TimestampMs>,
    /// How it ended; `None` while it runs, or if it stopped without saying.
    pub outcome: Option<RunOutcome>,
    /// Every node, requested ones first in request order, then others as first seen.
    pub nodes: Vec<NodeSummary>,
    /// Its totals, over [`nodes`](Self::nodes).
    pub totals: RunTotals,
}

impl RunSummary {
    /// Folds events, in the order they were emitted.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a RunEvent>) -> Self {
        let mut run = Self {
            run_id: None,
            scope: None,
            mode: None,
            live: false,
            started_at: None,
            finished_at: None,
            outcome: None,
            nodes: Vec::new(),
            totals: RunTotals::default(),
        };
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        for event in events {
            run.run_id.get_or_insert_with(|| event.run_id.clone());
            if run.scope.is_none() {
                run.scope.clone_from(&event.scope);
            }
            match &event.kind {
                RunEventKind::RunStarted { nodes, mode, live } => {
                    run.mode = Some(*mode);
                    run.live = *live;
                    run.started_at = Some(event.at);
                    for node in nodes {
                        run.node(&mut index, node, true);
                    }
                }
                RunEventKind::NodeQueued { node } => {
                    let entry = run.node(&mut index, node, false);
                    if !entry.stats.status.is_finished() {
                        entry.stats.status = NodeRunStatus::Queued;
                    }
                }
                RunEventKind::NodeStarted { node, thread } => {
                    let stats = &mut run.node(&mut index, node, false).stats;
                    stats.status = NodeRunStatus::Running;
                    stats.started_at = Some(event.at);
                    stats.thread.clone_from(thread);
                }
                RunEventKind::NodeFinished { node, stats } => {
                    let entry = run.node(&mut index, node, false);
                    let mut stats = stats.clone();
                    // What the start said, unless the finish says better.
                    stats.started_at = stats.started_at.or(entry.stats.started_at);
                    if stats.thread.is_none() {
                        stats.thread.clone_from(&entry.stats.thread);
                    }
                    stats.tests = merge_tests(stats.tests, entry.stats.tests);
                    entry.stats = stats;
                }
                RunEventKind::CheckFinished { covers, status, .. } => {
                    for node in covers {
                        if let Some(&i) = index.get(node) {
                            run.nodes[i]
                                .stats
                                .tests
                                .get_or_insert_with(TestCounts::default)
                                .add(*status);
                        }
                    }
                }
                RunEventKind::RunFinished { outcome } => {
                    run.outcome = Some(*outcome);
                    run.finished_at = Some(event.at);
                    // Whatever hadn't finished by now never will: unknown, not success.
                    for node in &mut run.nodes {
                        if !node.stats.status.is_finished() {
                            node.stats.status = NodeRunStatus::Unknown;
                        }
                    }
                }
            }
        }
        run.totals = RunTotals {
            duration_ms: run
                .finished_at
                .zip(run.started_at)
                .and_then(|(end, start)| end.millis_since(start)),
            ..RunTotals::default()
        };
        for node in &run.nodes {
            *run.totals.by_status.entry(node.stats.status).or_default() += 1;
            match node.stats.rows_affected {
                Some(rows) => {
                    run.totals.rows_affected = run.totals.rows_affected.saturating_add(rows);
                }
                None if node.stats.status == NodeRunStatus::Success => {
                    run.totals.rows_unreported += 1;
                }
                None => {}
            }
        }
        run
    }

    fn node(
        &mut self,
        index: &mut BTreeMap<String, usize>,
        id: &str,
        requested: bool,
    ) -> &mut NodeSummary {
        let i = *index.entry(id.to_owned()).or_insert_with(|| {
            self.nodes.push(NodeSummary {
                node: id.to_owned(),
                requested,
                stats: NodeRunStats::new(NodeRunStatus::Queued),
            });
            self.nodes.len() - 1
        });
        &mut self.nodes[i]
    }

    /// The node's summary, if it is in the run.
    pub fn get(&self, node: &str) -> Option<&NodeSummary> {
        self.nodes.iter().find(|n| n.node == node)
    }
}

/// Counts from `check_finished` events seen before the node's finish are added to
/// whatever the finish itself reports.
fn merge_tests(reported: Option<TestCounts>, seen: Option<TestCounts>) -> Option<TestCounts> {
    match (reported, seen) {
        (Some(a), Some(b)) => Some(TestCounts {
            passed: a.passed.saturating_add(b.passed),
            failed: a.failed.saturating_add(b.failed),
            warned: a.warned.saturating_add(b.warned),
            skipped: a.skipped.saturating_add(b.skipped),
        }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::executor::{NodeExecution, RequestedNode};
    use ods_core::state::Timestamp;

    fn ms(millis: i64) -> TimestampMs {
        TimestampMs::from_unix_millis(1_790_000_000_000 + millis)
    }

    fn event(at: i64, kind: RunEventKind) -> RunEvent {
        RunEvent::new("run-1", Some("shop/dev".to_owned()), ms(at), kind)
    }

    #[test]
    fn events_serialize_flat_with_their_kind_and_read_back() {
        let e = event(
            1900,
            RunEventKind::NodeFinished {
                node: "model.shop.orders".to_owned(),
                stats: NodeRunStats::new(NodeRunStatus::Success)
                    .with_times(Some(ms(0)), Some(ms(1900)))
                    .with_rows_affected(Some(99))
                    .with_extra("query_id", "q-1"),
            },
        );
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "node_finished");
        assert_eq!(json["schema_version"]["major"], 1);
        assert_eq!(json["run_id"], "run-1");
        assert_eq!(json["scope"], "shop/dev");
        assert_eq!(json["stats"]["rows_affected"], 99);
        assert_eq!(json["stats"]["adapter"]["query_id"], "q-1");
        // Not reported is null, never zero.
        assert!(json["stats"]["compile_ms"].is_null());
        assert_eq!(serde_json::from_value::<RunEvent>(json).unwrap(), e);
    }

    #[test]
    fn unreported_rows_stay_missing() {
        let stats = NodeRunStats::new(NodeRunStatus::Success).with_rows_affected(Some(-1));
        assert_eq!(stats.rows_affected, None);
        assert_eq!(
            NodeRunStats::new(NodeRunStatus::Success)
                .with_rows_affected(Some(0))
                .rows_affected,
            Some(0)
        );
    }

    #[test]
    fn extras_are_short_single_line_scalars() {
        let long = "x".repeat(500);
        let stats = NodeRunStats::new(NodeRunStatus::Success)
            .with_extra("code", "OK\nselect secret")
            .with_extra("long", &long)
            .with_extra("", "v")
            .with_extra("empty", " ");
        assert_eq!(stats.adapter["code"], "OK");
        assert_eq!(stats.adapter["long"].chars().count(), MAX_EXTRA_CHARS);
        assert_eq!(stats.adapter.len(), 2);
        let mut many = NodeRunStats::new(NodeRunStatus::Success);
        for i in 0..40 {
            many = many.with_extra(format!("k{i:02}"), "v");
        }
        assert_eq!(many.adapter.len(), MAX_EXTRAS);
    }

    #[test]
    fn error_summaries_keep_the_kind_and_drop_values() {
        let s = ErrorSummary::from_message("KeyError: 'sk_live_42'\nTraceback").unwrap();
        assert_eq!(s.kind.as_deref(), Some("KeyError"));
        assert_eq!(s.message, "KeyError: [value removed]");
        let s = ErrorSummary::from_message("Binder Error: column \"x\" in select 1").unwrap();
        assert_eq!(s.kind.as_deref(), Some("Binder Error"));
        assert!(!s.message.contains("select"));
        assert_eq!(ErrorSummary::from_message("failed").unwrap().kind, None);
        let s = ErrorSummary::from_message("column x not found")
            .unwrap()
            .with_kind("Runtime Error 'x'");
        assert_eq!(s.kind.as_deref(), Some("Runtime Error [value removed]"));
        assert_eq!(ErrorSummary::from_message(""), None);
    }

    #[test]
    fn a_run_folds_into_node_stats_and_totals() {
        let finished = |node: &str, status, rows| RunEventKind::NodeFinished {
            node: node.to_owned(),
            stats: NodeRunStats::new(status)
                .with_times(None, Some(ms(3000)))
                .with_rows_affected(rows),
        };
        let events = vec![
            event(
                0,
                RunEventKind::RunStarted {
                    nodes: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                    mode: ExecutionMode::Build,
                    live: true,
                },
            ),
            event(0, RunEventKind::NodeQueued { node: "a".into() }),
            event(
                1000,
                RunEventKind::NodeStarted {
                    node: "a".into(),
                    thread: Some("Thread-1".into()),
                },
            ),
            event(
                1000,
                RunEventKind::NodeStarted {
                    node: "b".into(),
                    thread: None,
                },
            ),
            event(3000, finished("a", NodeRunStatus::Success, Some(99))),
            event(
                3000,
                RunEventKind::CheckFinished {
                    check: "test.a".into(),
                    covers: vec!["a".into(), "elsewhere".into()],
                    status: CheckStatus::Passed,
                },
            ),
            event(3000, finished("b", NodeRunStatus::Success, None)),
            event(3000, finished("c", NodeRunStatus::Error, None)),
        ];
        let live = RunSummary::from_events(&events);
        assert_eq!(live.outcome, None, "still going");
        assert_eq!(live.get("d").unwrap().stats.status, NodeRunStatus::Queued);
        let a = &live.get("a").unwrap().stats;
        assert_eq!(a.started_at, Some(ms(1000)));
        assert_eq!(a.thread.as_deref(), Some("Thread-1"));
        assert_eq!(a.took_ms(), Some(2000));
        assert_eq!(a.tests.unwrap().passed, 1);
        assert!(live.get("elsewhere").is_none(), "checks add no nodes");

        let mut events = events;
        events.push(event(
            3500,
            RunEventKind::RunFinished {
                outcome: RunOutcome::Failed,
            },
        ));
        let done = RunSummary::from_events(&events);
        assert_eq!(done.run_id.as_deref(), Some("run-1"));
        assert_eq!(done.scope.as_deref(), Some("shop/dev"));
        assert_eq!(
            done.get("d").unwrap().stats.status,
            NodeRunStatus::Unknown,
            "never finished: unknown, not success"
        );
        assert_eq!(done.totals.duration_ms, Some(3500));
        assert_eq!(done.totals.count(NodeRunStatus::Success), 2);
        assert_eq!(done.totals.count(NodeRunStatus::Error), 1);
        assert_eq!(done.totals.rows_affected, 99);
        assert_eq!(done.totals.rows_unreported, 1);
        assert!(done.totals.rows_is_lower_bound());
        assert_eq!(
            done.nodes
                .iter()
                .map(|n| n.node.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
    }

    #[test]
    fn a_report_rebuilds_its_events_without_stats() {
        let request = ExecutionRequest::new(
            vec![RequestedNode::new("a", "a"), RequestedNode::new("b", "b")],
            ExecutionMode::Build,
        )
        .with_scope("shop/dev");
        let at = Timestamp::from_unix(1_790_000_000);
        let report = ExecutionReport::new(
            "run-9",
            Some(at),
            Timestamp::from_unix(1_790_000_005),
            vec![
                NodeExecution::new(
                    "a",
                    ExecutionStatus::Success,
                    Some(Timestamp::from_unix(1_790_000_003)),
                    None,
                )
                .with_checks_passed(vec!["test.a".into()]),
                NodeExecution::new(
                    "b",
                    ExecutionStatus::Failed,
                    None,
                    Some("Runtime Error: value 'hunter2' too long".into()),
                )
                .with_checks_skipped(vec!["test.a".into()]),
            ],
            vec![],
        );
        let events = events_from_report(&request, &report);
        assert!(events.iter().all(|e| e.run_id == "run-9"));
        assert!(
            events
                .iter()
                .all(|e| e.scope.as_deref() == Some("shop/dev"))
        );
        assert!(events.windows(2).all(|w| w[0].at <= w[1].at));
        assert!(matches!(
            events[0].kind,
            RunEventKind::RunStarted { live: false, .. }
        ));
        let run = RunSummary::from_events(&events);
        assert_eq!(run.outcome, Some(RunOutcome::Failed));
        let a = &run.get("a").unwrap().stats;
        assert_eq!(a.status, NodeRunStatus::Success);
        assert_eq!(a.rows_affected, None);
        assert_eq!(a.took_ms(), None, "no start, no duration");
        let b = &run.get("b").unwrap().stats;
        assert_eq!(b.status, NodeRunStatus::Error);
        let error = b.error.as_ref().unwrap();
        assert!(!error.message.contains("hunter2"), "{error:?}");
        // The shared check is skipped on b and passed on a: it didn't pass everywhere.
        let check = events
            .iter()
            .find_map(|e| match &e.kind {
                RunEventKind::CheckFinished { status, covers, .. } => {
                    Some((*status, covers.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            check,
            (CheckStatus::Skipped, vec!["a".to_owned(), "b".to_owned()])
        );
    }
}
