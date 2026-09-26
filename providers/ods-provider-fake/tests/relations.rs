//! The in-memory executor passes the `RelationInspector` conformance suite.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_fake::{FakeClock, FakeExecutor};
use ods_sdk::conformance::relations::{RelationHarness, run};
use ods_sdk::contracts::executor::RequestedNode;
use ods_sdk::contracts::relations::RelationInspector;

#[derive(Default)]
struct Harness {
    last: Mutex<Option<FakeExecutor>>,
}

#[async_trait]
impl RelationHarness for Harness {
    async fn inspector(&self) -> Arc<dyn RelationInspector> {
        let executor = FakeExecutor::new(FakeClock::new(), ["model.suite.a", "model.suite.b"]);
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(executor.clone());
        Arc::new(executor)
    }

    fn present(&self) -> Vec<RequestedNode> {
        vec![
            RequestedNode::new("model.suite.a", "a"),
            RequestedNode::new("model.suite.b", "b"),
        ]
    }

    async fn drop_relation(&self, node: &RequestedNode) -> bool {
        if let Some(executor) = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            executor.drop_relation(&node.id);
        }
        true
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 3, "{report:?}");
}

#[tokio::test]
async fn building_a_dropped_relation_brings_it_back_and_failures_verify_nothing() {
    use ods_sdk::contracts::executor::{ExecutionMode, ExecutionRequest, Executor};
    use ods_sdk::contracts::relations::RelationPresence;

    let executor = FakeExecutor::new(FakeClock::new(), ["model.suite.a"]);
    let a = RequestedNode::new("model.suite.a", "a");
    executor.drop_relation(&a.id);
    let report = executor.inspect(std::slice::from_ref(&a)).await.unwrap();
    assert_eq!(report.nodes[0].1, RelationPresence::Missing);
    executor
        .execute(&ExecutionRequest::new(vec![a.clone()], ExecutionMode::Run))
        .await
        .unwrap();
    let report = executor.inspect(std::slice::from_ref(&a)).await.unwrap();
    assert!(matches!(
        report.nodes[0].1,
        RelationPresence::Present { .. }
    ));

    let unreachable = executor.failing_inspection();
    assert!(unreachable.inspect(&[a]).await.is_err());
}
