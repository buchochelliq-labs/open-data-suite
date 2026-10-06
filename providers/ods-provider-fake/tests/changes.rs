//! The in-memory change provider and relation probe pass their conformance suites.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_fake::{FakeChangeProvider, FakeRelationProbe};
use ods_sdk::conformance::changes::{ChangeHarness, run as run_changes};
use ods_sdk::conformance::probe::{ProbeHarness, run as run_probe};
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource};
use ods_sdk::contracts::probe::{
    ProbeAnswer, ProbeFilter, ProbeRequest, ProbeStatement, ProbeTarget, RelationProbe,
};

#[derive(Default)]
struct Changes {
    last: Mutex<Option<FakeChangeProvider>>,
}

#[async_trait]
impl ChangeHarness for Changes {
    async fn provider(&self) -> Arc<dyn ChangeProvider> {
        let provider = FakeChangeProvider::new()
            .with_source("source.suite.raw.a")
            .with_source("source.suite.raw.b")
            .unreadable("source.suite.raw.view", "a view has no version");
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(provider.clone());
        Arc::new(provider)
    }

    fn readable(&self) -> Vec<RequestedSource> {
        vec![
            RequestedSource::new("source.suite.raw.a", "raw.a"),
            RequestedSource::new("source.suite.raw.b", "raw.b"),
        ]
    }

    async fn commit(&self, source: &RequestedSource) -> bool {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|p| p.commit(&source.id))
    }

    fn unreadable(&self) -> Option<RequestedSource> {
        Some(RequestedSource::new("source.suite.raw.view", "raw.view"))
    }
}

#[tokio::test]
async fn the_change_provider_conforms() {
    let report = run_changes(&Changes::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

#[tokio::test]
async fn a_failing_change_provider_reads_nothing() {
    let provider = FakeChangeProvider::new().with_source("a").failing();
    assert!(
        provider
            .versions(&[RequestedSource::new("a", "a")])
            .await
            .is_err()
    );
}

const DETAIL: &str = "describe {relation}";

struct Probe;

fn request() -> ProbeRequest {
    ProbeRequest::new(
        ProbeFilter::kinds(["table"])
            .unwrap()
            .with_format("columnar")
            .unwrap(),
        vec![
            ProbeStatement::new(DETAIL, ["id", "format"]).unwrap(),
            ProbeStatement::new("history of {relation}", ["version"]).unwrap(),
        ],
    )
    .unwrap()
}

#[async_trait]
impl ProbeHarness for Probe {
    async fn probe(&self) -> Arc<dyn RelationProbe> {
        Arc::new(
            FakeRelationProbe::new()
                .with_relation("source.suite.raw.a", "table", Some("columnar"))
                .with_row(
                    "source.suite.raw.a",
                    DETAIL,
                    [("id", "a1"), ("format", "columnar"), ("extra", "x")],
                )
                .with_relation("source.suite.raw.b", "table", Some("columnar"))
                .with_relation("source.suite.raw.view", "view", None)
                .named("source.suite.raw.a", "suite.raw.a")
                .with_parts("source.suite.raw.b", "suite", "raw", "it's"),
        )
    }

    fn request(&self) -> ProbeRequest {
        request()
    }

    fn matching(&self) -> Vec<ProbeTarget> {
        vec![
            ProbeTarget::new("source.suite.raw.a", "raw.a"),
            ProbeTarget::new("source.suite.raw.b", "raw.b"),
        ]
    }

    fn excluded(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("source.suite.raw.view", "raw.view"))
    }

    fn elsewhere(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("source.suite.raw.a", "raw.a").expecting("other.raw.a"))
    }

    fn unsafe_name(&self) -> Option<(ProbeRequest, ProbeTarget)> {
        let request = ProbeRequest::new(
            ProbeFilter::kinds(["table"]).unwrap(),
            vec![ProbeStatement::by_name("select {name} as n", ["n"]).unwrap()],
        )
        .unwrap();
        Some((request, ProbeTarget::new("source.suite.raw.b", "raw.b")))
    }
}

#[tokio::test]
async fn the_relation_probe_conforms() {
    let report = run_probe(&Probe).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 6, "{report:?}");
}

#[tokio::test]
async fn the_probe_returns_requested_columns_and_skips_unconfirmed_formats() {
    let probe = FakeRelationProbe::new()
        .with_relation("a", "table", Some("columnar"))
        .with_row("a", DETAIL, [("id", "a1"), ("extra", "x")])
        .with_relation("b", "table", None)
        .with_relation("c", "table", Some("rows"));
    let targets = ["a", "b", "c"].map(|s| ProbeTarget::new(s, s));
    let report = probe.probe(&request(), &targets).await.unwrap();
    assert_eq!(
        report.targets[0].1,
        ProbeAnswer::Rows(vec![
            [("id".to_owned(), "a1".to_owned())].into(),
            ods_sdk::contracts::probe::ProbeRow::new()
        ])
    );
    assert!(
        matches!(&report.targets[1].1, ProbeAnswer::Skipped(why) if why.contains("can't be confirmed"))
    );
    assert!(
        matches!(&report.targets[2].1, ProbeAnswer::Skipped(why) if why.contains("stored as rows"))
    );
    assert_eq!(
        probe.probed(),
        ["a"],
        "only relations that matched ran anything"
    );
    assert!(probe.failing().probe(&request(), &targets).await.is_err());
}
