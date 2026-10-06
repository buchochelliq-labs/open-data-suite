//! This crate's plugins pass ODS's conformance suites, as any provider must (#99): the
//! health check's, and the change provider's over the fake relation probe, standing in
//! for dbt's connection to a warehouse.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use custom_ods::{LATEST_BATCH, LoadBatches, OwnerTagged};
use ods_provider_fake::FakeRelationProbe;
use ods_sdk::conformance::changes::{self, ChangeHarness};
use ods_sdk::conformance::health_check::{self, HealthCheckHarness};
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource};
use ods_sdk::contracts::health_check::{CheckScope, HealthCheck, NodeFacts};

struct Checks;

impl HealthCheckHarness for Checks {
    fn check(&self) -> Arc<dyn HealthCheck> {
        Arc::new(OwnerTagged)
    }

    fn scope(&self) -> CheckScope {
        let mut owned = NodeFacts::new("model.shop.orders", "orders", "model");
        owned.tags = vec!["owner:finance".to_owned()];
        let unowned = NodeFacts::new("model.shop.customers", "customers", "model");
        CheckScope::new(vec![owned, unowned], None)
    }

    fn undecidable(&self) -> Option<CheckScope> {
        // It decides from the tags it is given, so nothing makes it undecided.
        None
    }
}

#[tokio::test]
async fn the_health_check_conforms() {
    let report = health_check::run(&Checks).await;
    assert_eq!(report.skipped.len(), 1, "{report:?}");
}

/// A warehouse with two loaded tables and a view.
fn warehouse() -> FakeRelationProbe {
    let loaded = |probe: FakeRelationProbe, source: &str| {
        probe.with_relation(source, "table", None).with_row(
            source,
            LATEST_BATCH,
            [("batch_id", "41")],
        )
    };
    let probe = loaded(FakeRelationProbe::new(), "source.shop.raw.orders");
    loaded(probe, "source.shop.raw.payments").with_relation("source.shop.raw.recent", "view", None)
}

#[derive(Default)]
struct Versions {
    last: Mutex<Option<FakeRelationProbe>>,
}

#[async_trait]
impl ChangeHarness for Versions {
    async fn provider(&self) -> Arc<dyn ChangeProvider> {
        let probe = warehouse();
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(probe.clone());
        Arc::new(LoadBatches::new(probe))
    }

    fn readable(&self) -> Vec<RequestedSource> {
        vec![
            RequestedSource::new("source.shop.raw.orders", "raw.orders"),
            RequestedSource::new("source.shop.raw.payments", "raw.payments"),
        ]
    }

    async fn commit(&self, source: &RequestedSource) -> bool {
        if let Some(probe) = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            probe.set_row(&source.id, LATEST_BATCH, [("batch_id", "42")]);
        }
        true
    }

    fn unreadable(&self) -> Option<RequestedSource> {
        Some(RequestedSource::new("source.shop.raw.recent", "raw.recent"))
    }
}

#[tokio::test]
async fn the_change_provider_conforms() {
    let report = changes::run(&Versions::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
}
