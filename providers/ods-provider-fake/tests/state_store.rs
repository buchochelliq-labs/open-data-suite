//! The in-memory state store passes the `StateStore` conformance suite.

use std::sync::Arc;

use async_trait::async_trait;
use ods_core::Capability;
use ods_core::state::{RunEntry, RunEntryOutcome, SnapshotId, StateSnapshot, Timestamp};
use ods_provider_fake::FakeStateStore;
use ods_sdk::conformance::state_store::{StateStoreHarness, run};
use ods_sdk::contracts::state_store::{
    SnapshotSummary, StateScope, StateStore, StoreCheck, StoredSnapshot,
};

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
    assert_eq!(report.passed.len(), 8, "{report:?}");
}

/// A store that implements only what contract 0.2 required: the run ledger methods
/// default to `Unsupported` (0.3, ADR-0029).
struct Before03;

impl ods_sdk::Provider for Before03 {
    fn info(&self) -> ods_sdk::ProviderInfo {
        ods_sdk::ProviderInfo::new("x", "x", "0", ods_core::CapabilitySet::new())
    }
}

#[async_trait]
impl StateStore for Before03 {
    async fn latest(
        &self,
        _: &StateScope,
    ) -> Result<Option<StoredSnapshot>, ods_sdk::ProviderError> {
        Ok(None)
    }
    async fn get(
        &self,
        _: &StateScope,
        _: SnapshotId,
    ) -> Result<Option<StoredSnapshot>, ods_sdk::ProviderError> {
        Ok(None)
    }
    async fn commit(
        &self,
        _: &StateScope,
        _: &StateSnapshot,
    ) -> Result<SnapshotId, ods_sdk::ProviderError> {
        Ok(SnapshotId(1))
    }
    async fn history(
        &self,
        _: &StateScope,
        _: usize,
    ) -> Result<Vec<SnapshotSummary>, ods_sdk::ProviderError> {
        Ok(Vec::new())
    }
    async fn check(&self) -> Result<StoreCheck, ods_sdk::ProviderError> {
        Ok(StoreCheck::new(None, Vec::new(), Vec::new()))
    }
}

#[tokio::test]
async fn a_store_without_a_ledger_still_conforms() {
    struct Old;
    #[async_trait]
    impl StateStoreHarness for Old {
        async fn store(&self) -> Arc<dyn StateStore> {
            Arc::new(Before03)
        }
    }
    // Only the ledger case runs meaningfully against a stub: it says unsupported.
    let scope = StateScope::new("p", "dev").unwrap();
    let store = Old.store().await;
    let entry = RunEntry::new(
        "run-1",
        Timestamp::from_unix(1),
        RunEntryOutcome::Succeeded,
        std::collections::BTreeMap::new(),
    );
    assert!(matches!(
        store.record_run(&scope, &entry).await,
        Err(ods_sdk::ProviderError::Unsupported(Capability::RunLedger))
    ));
    assert!(matches!(
        store.runs(&scope, None, 1).await,
        Err(ods_sdk::ProviderError::Unsupported(Capability::RunLedger))
    ));
}
