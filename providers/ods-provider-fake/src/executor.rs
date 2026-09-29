//! In-memory [`Executor`] and [`RelationInspector`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use ods_core::state::Timestamp;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, NodeExecution,
    PrepareReport, PrepareRequest, RequestedNode,
};
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence, RelationReport};
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
    inner: Arc<Mutex<Inner>>,
}

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
            inner: Arc::default(),
        }
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
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationExistence]),
        )
    }
}

#[async_trait]
impl Executor for FakeExecutor {
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError> {
        Ok(PrepareReport::new(request.measure_sources, Vec::new()))
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionReport, ProviderError> {
        if request.is_empty() || (request.mode == ExecutionMode::Run && request.nodes.is_empty()) {
            return Err(ProviderError::Other(
                "nothing to execute: the request names no nodes".to_owned(),
            ));
        }
        let started = self.now();
        // Each execution takes a second, so runs never share a timestamp.
        self.clock.advance(Duration::from_secs(1));
        let finished = self.now();
        let mut inner = self.inner();
        inner.runs += 1;
        let run_id = format!("fake-run-{}", inner.runs);
        // Source checks run first, as `dbt build` runs them (#232).
        let mut checks_failed = Vec::new();
        let mut blocked = BTreeSet::new();
        let sources = request
            .sources
            .iter()
            .map(|s| {
                let Some(source) = self.sources.get(&s.id).filter(|f| !f.checks.is_empty()) else {
                    return NodeExecution::new(
                        s.id.clone(),
                        ExecutionStatus::Skipped,
                        Some(finished),
                        Some("no checks".to_owned()),
                    );
                };
                if request.mode == ExecutionMode::Run {
                    NodeExecution::new(
                        s.id.clone(),
                        ExecutionStatus::Skipped,
                        None,
                        Some("no checks run without tests".to_owned()),
                    )
                } else if source.failing {
                    checks_failed.extend(source.checks.iter().cloned());
                    blocked.extend(source.readers.iter().cloned());
                    NodeExecution::new(s.id.clone(), ExecutionStatus::Failed, Some(finished), None)
                        .with_checks_failed(source.checks.clone())
                } else {
                    NodeExecution::new(s.id.clone(), ExecutionStatus::Success, Some(finished), None)
                        .with_checks_passed(source.checks.clone())
                }
            })
            .collect();
        let nodes = request
            .nodes
            .iter()
            .map(|n| {
                let checks = self.checks.get(&n.id).cloned().unwrap_or_default();
                let (status, message) = if !self.nodes.contains(&n.id) {
                    (ExecutionStatus::Failed, Some("unknown node".to_owned()))
                } else if request.mode == ExecutionMode::Build && blocked.contains(&n.id) {
                    (
                        ExecutionStatus::Skipped,
                        Some("a source it reads failed its checks".to_owned()),
                    )
                } else if self.failing.contains(&n.id) {
                    (ExecutionStatus::Failed, Some("failed".to_owned()))
                } else if request.mode == ExecutionMode::Test {
                    // A test run builds nothing, and tests nothing without checks.
                    if checks.is_empty() {
                        (ExecutionStatus::Skipped, Some("no checks".to_owned()))
                    } else {
                        (ExecutionStatus::Success, None)
                    }
                } else {
                    inner.built.push(n.id.clone());
                    inner.dropped.remove(&n.id);
                    (ExecutionStatus::Success, None)
                };
                let ran_checks =
                    status == ExecutionStatus::Success && request.mode != ExecutionMode::Run;
                NodeExecution::new(n.id.clone(), status, Some(finished), message)
                    .with_checks_passed(if ran_checks { checks } else { Vec::new() })
            })
            .collect();
        Ok(
            ExecutionReport::new(run_id, Some(started), finished, nodes, checks_failed)
                .with_sources(sources),
        )
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
