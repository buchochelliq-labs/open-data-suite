//! In-memory [`Executor`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::Timestamp;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, NodeExecution,
    PrepareReport, PrepareRequest,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;
use crate::clock::FakeClock;

#[derive(Debug, Default)]
struct Inner {
    runs: u64,
    built: Vec<String>,
}

/// An executor over a set of known nodes, some of which fail. It builds nothing real:
/// it records which nodes it "built", for tests to check.
#[derive(Debug, Clone)]
pub struct FakeExecutor {
    clock: FakeClock,
    nodes: BTreeSet<String>,
    failing: BTreeSet<String>,
    checks: BTreeMap<String, Vec<String>>,
    inner: Arc<Mutex<Inner>>,
}

impl FakeExecutor {
    /// An executor that knows `nodes`, all of which build.
    pub fn new(clock: FakeClock, nodes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            clock,
            nodes: nodes.into_iter().map(Into::into).collect(),
            failing: BTreeSet::new(),
            checks: BTreeMap::new(),
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
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl Executor for FakeExecutor {
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError> {
        Ok(PrepareReport::new(request.measure_sources, Vec::new()))
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionReport, ProviderError> {
        if request.nodes.is_empty() {
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
        let nodes = request
            .nodes
            .iter()
            .map(|n| {
                let checks = self.checks.get(&n.id).cloned().unwrap_or_default();
                let (status, message) = if !self.nodes.contains(&n.id) {
                    (ExecutionStatus::Failed, Some("unknown node".to_owned()))
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
                    (ExecutionStatus::Success, None)
                };
                let ran_checks =
                    status == ExecutionStatus::Success && request.mode != ExecutionMode::Run;
                NodeExecution::new(n.id.clone(), status, Some(finished), message)
                    .with_checks_passed(if ran_checks { checks } else { Vec::new() })
            })
            .collect();
        Ok(ExecutionReport::new(
            run_id,
            Some(started),
            finished,
            nodes,
            Vec::new(),
        ))
    }
}
