//! `DeltaVersions` passes the `ChangeProvider` conformance suite over the fake relation
//! probe, standing in for the dbt executor on a Databricks workspace.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_core::Capability;
use ods_core::state::{DataVersion, Exactness};
use ods_provider_databricks::DeltaVersions;
use ods_provider_databricks::delta_versions::{DETAIL, HISTORY, ORIGIN};
use ods_provider_fake::FakeRelationProbe;
use ods_sdk::Provider;
use ods_sdk::conformance::changes::{ChangeHarness, run};
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource, SourceVersion};

/// A workspace with two Delta tables, a Parquet table and a view.
fn workspace() -> FakeRelationProbe {
    let delta = |probe: FakeRelationProbe, source: &str, id: &'static str| {
        probe
            .with_relation(source, "table", Some("delta"))
            .with_row(source, DETAIL, [("id", id), ("format", "delta")])
            .with_row(
                source,
                HISTORY,
                [("version", "3"), ("timestamp", "2026-09-29 10:00:00")],
            )
    };
    let probe = delta(FakeRelationProbe::new(), "source.p.raw.orders", "id-orders");
    delta(probe, "source.p.raw.payments", "id-payments")
        .with_relation("source.p.raw.files", "table", Some("parquet"))
        .with_relation("source.p.raw.view", "view", None)
}

#[derive(Default)]
struct Harness {
    last: Mutex<Option<FakeRelationProbe>>,
}

#[async_trait]
impl ChangeHarness for Harness {
    async fn provider(&self) -> Arc<dyn ChangeProvider> {
        let probe = workspace();
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(probe.clone());
        Arc::new(DeltaVersions::new(probe))
    }

    fn readable(&self) -> Vec<RequestedSource> {
        vec![
            RequestedSource::new("source.p.raw.orders", "raw.orders"),
            RequestedSource::new("source.p.raw.payments", "raw.payments"),
        ]
    }

    async fn commit(&self, source: &RequestedSource) -> bool {
        if let Some(probe) = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            probe.set_row(&source.id, HISTORY, [("version", "4")]);
        }
        true
    }

    fn unreadable(&self) -> Option<RequestedSource> {
        Some(RequestedSource::new("source.p.raw.files", "raw.files"))
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

#[tokio::test]
async fn versions_are_exact_and_everything_else_is_unknown_with_why() {
    let provider = DeltaVersions::new(workspace());
    assert!(
        provider
            .info()
            .capabilities
            .contains(&Capability::RelationVersions)
    );
    let asked = ["orders", "files", "view", "gone"]
        .map(|t| RequestedSource::new(format!("source.p.raw.{t}"), t));
    let report = provider.versions(&asked).await.unwrap();
    assert_eq!(
        report.sources[0].1,
        SourceVersion::Version(DataVersion::new("id-orders/3", Exactness::Exact, ORIGIN))
    );
    let why = |i: usize| match &report.sources[i].1 {
        SourceVersion::Unknown(why) => why.clone(),
        other => panic!("{other:?}"),
    };
    assert!(why(1).starts_with("not a Delta table"), "{}", why(1));
    assert!(why(2).starts_with("not a Delta table"), "{}", why(2));
    assert_eq!(why(3), "unknown source");

    // Dropped and created again: a new id, and versions start again.
    let probe = workspace();
    probe.set_row(
        "source.p.raw.orders",
        DETAIL,
        [("id", "id-new"), ("format", "delta")],
    );
    probe.set_row("source.p.raw.orders", HISTORY, [("version", "3")]);
    let again = DeltaVersions::new(probe)
        .versions(&asked[..1])
        .await
        .unwrap();
    assert_ne!(again.sources[0].1, report.sources[0].1);

    // A probe that fails reads nothing.
    let failing = DeltaVersions::new(workspace().failing());
    assert!(failing.versions(&asked).await.is_err());
}
