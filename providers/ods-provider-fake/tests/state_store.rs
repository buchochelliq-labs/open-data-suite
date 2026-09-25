//! The in-memory state store passes the `StateStore` conformance suite.

use std::sync::Arc;

use async_trait::async_trait;
use ods_provider_fake::FakeStateStore;
use ods_sdk::conformance::state_store::{StateStoreHarness, run};
use ods_sdk::contracts::state_store::StateStore;

struct Harness;

#[async_trait]
impl StateStoreHarness for Harness {
    async fn store(&self) -> Arc<dyn StateStore> {
        Arc::new(FakeStateStore::new())
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 5, "{report:?}");
}
