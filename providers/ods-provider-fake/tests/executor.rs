//! The in-memory executor passes the `Executor` conformance suite.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_fake::{FakeClock, FakeExecutor};
use ods_sdk::conformance::executor::{ExecutorHarness, run};
use ods_sdk::contracts::executor::{Executor, RequestedNode};

#[derive(Default)]
struct Harness {
    last: Mutex<Option<FakeExecutor>>,
}

#[async_trait]
impl ExecutorHarness for Harness {
    async fn executor(&self) -> Arc<dyn Executor> {
        let executor = FakeExecutor::new(FakeClock::new(), ["model.suite.a", "model.suite.b"])
            .failing("model.suite.broken");
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
    assert_eq!(report.passed.len(), 7, "{report:?}");
}
