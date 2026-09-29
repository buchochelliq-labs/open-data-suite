//! The in-memory executor passes the `Executor` conformance suite.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_fake::{FakeClock, FakeExecutor};
use ods_sdk::conformance::executor::{ExecutorHarness, run};
use ods_sdk::contracts::executor::{Executor, RequestedNode};

#[derive(Default)]
struct Harness {
    last: Mutex<Option<FakeExecutor>>,
    /// Without the `run_events` capability: events rebuilt from the report.
    without_run_events: bool,
}

#[async_trait]
impl ExecutorHarness for Harness {
    async fn executor(&self) -> Arc<dyn Executor> {
        let executor = FakeExecutor::new(FakeClock::new(), ["model.suite.a", "model.suite.b"])
            .failing("model.suite.broken")
            .with_checks("model.suite.checked", ["test.suite.checked_unique"])
            .with_source(
                "source.suite.raw.good",
                ["test.suite.source_not_null_good"],
                ["model.suite.a"],
            )
            .with_source(
                "source.suite.raw.bad",
                ["test.suite.source_not_null_bad"],
                ["model.suite.b"],
            )
            .failing_source("source.suite.raw.bad");
        let executor = if self.without_run_events {
            executor.without_run_events()
        } else {
            executor
        };
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(executor.clone());
        Arc::new(executor)
    }

    fn buildable(&self) -> Vec<RequestedNode> {
        vec![
            RequestedNode::new("model.suite.a", "a"),
            RequestedNode::new("model.suite.b", "b"),
        ]
    }

    fn failing(&self) -> Option<RequestedNode> {
        Some(RequestedNode::new("model.suite.broken", "broken"))
    }

    fn checked_and_unchecked(&self) -> Option<(RequestedNode, RequestedNode)> {
        Some((
            RequestedNode::new("model.suite.checked", "checked"),
            RequestedNode::new("model.suite.a", "a"),
        ))
    }

    fn checked_source(&self) -> Option<RequestedNode> {
        Some(RequestedNode::new("source.suite.raw.good", "raw.good"))
    }

    fn failing_source(&self) -> Option<(RequestedNode, RequestedNode)> {
        Some((
            RequestedNode::new("source.suite.raw.bad", "raw.bad"),
            RequestedNode::new("model.suite.b", "b"),
        ))
    }

    async fn built(&self) -> Option<Vec<String>> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(FakeExecutor::built)
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 14, "{report:?}");
}

/// Without `run_events`, the events rebuilt from the report follow the same rules
/// (#322).
#[tokio::test]
async fn conforms_without_run_events() {
    let report = run(&Harness {
        without_run_events: true,
        ..Harness::default()
    })
    .await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 14, "{report:?}");
}
