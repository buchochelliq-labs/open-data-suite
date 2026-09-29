//! Conformance suite for [`ChangeProvider`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::changes::{ChangeProvider, RequestedSource, SourceVersion, VersionReport};

/// What the suite needs from a change provider under test.
#[async_trait]
pub trait ChangeHarness: Send + Sync {
    /// A fresh provider over a warehouse where every [`readable`](Self::readable)
    /// source has a version. Called once per case.
    async fn provider(&self) -> Arc<dyn ChangeProvider>;

    /// At least two sources whose versions can be read.
    fn readable(&self) -> Vec<RequestedSource>;

    /// Changes `source`'s data in the warehouse of the last
    /// [`provider`](Self::provider). Returns `false` if the harness can't, which skips
    /// the case that needs it.
    async fn commit(&self, source: &RequestedSource) -> bool;

    /// A source that exists but whose version can't be read (e.g. a relation without
    /// one), if the harness has one. `None` skips the case that needs it.
    fn unreadable(&self) -> Option<RequestedSource>;
}

fn ids(sources: &[RequestedSource]) -> Vec<String> {
    sources.iter().map(|s| s.id.clone()).collect()
}

fn listed(report: &VersionReport) -> Vec<String> {
    report.sources.iter().map(|(id, _)| id.clone()).collect()
}

fn version<'a>(report: &'a VersionReport, id: &str) -> &'a SourceVersion {
    &report
        .sources
        .iter()
        .find(|(source, _)| source == id)
        .unwrap_or_else(|| panic!("{id} is not in the report: {report:?}"))
        .1
}

async fn read(
    case: &str,
    provider: &dyn ChangeProvider,
    sources: &[RequestedSource],
) -> VersionReport {
    provider
        .versions(sources)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"))
}

async fn lists_every_source_once_in_request_order(harness: &dyn ChangeHarness) {
    let case = "lists_every_source_once_in_request_order";
    let provider = harness.provider().await;
    let mut sources = harness.readable();
    assert!(
        sources.len() >= 2,
        "{case}: the harness needs two readable sources"
    );
    sources.reverse();
    let report = read(case, provider.as_ref(), &sources).await;
    assert_eq!(listed(&report), ids(&sources), "{case}: listed sources");
    for source in &sources {
        assert!(
            matches!(version(&report, &source.id), SourceVersion::Version(_)),
            "{case}: {report:?}"
        );
    }
}

async fn unknown_sources_have_no_version(harness: &dyn ChangeHarness) {
    let case = "unknown_sources_have_no_version";
    let provider = harness.provider().await;
    let unknown = RequestedSource::new(
        "source.ods_conformance.none.no_such_source",
        "none.no_such_source",
    );
    let report = read(case, provider.as_ref(), std::slice::from_ref(&unknown)).await;
    assert_eq!(
        listed(&report),
        std::slice::from_ref(&unknown.id),
        "{case}: listed"
    );
    assert!(
        matches!(version(&report, &unknown.id), SourceVersion::Unknown(_)),
        "{case}: {report:?}"
    );
}

async fn reading_is_repeatable(harness: &dyn ChangeHarness) {
    let case = "reading_is_repeatable";
    let provider = harness.provider().await;
    let sources = harness.readable();
    let first = read(case, provider.as_ref(), &sources).await;
    // Read-only: asking again sees the same data.
    let again = read(case, provider.as_ref(), &sources).await;
    assert_eq!(again, first, "{case}: reading changed the versions");
}

async fn a_commit_moves_only_its_version(harness: &dyn ChangeHarness) -> bool {
    let case = "a_commit_moves_only_its_version";
    let provider = harness.provider().await;
    let sources = harness.readable();
    let before = read(case, provider.as_ref(), &sources).await;
    if !harness.commit(&sources[0]).await {
        return false;
    }
    let after = read(case, provider.as_ref(), &sources).await;
    let (SourceVersion::Version(then), SourceVersion::Version(now)) = (
        version(&before, &sources[0].id),
        version(&after, &sources[0].id),
    ) else {
        panic!("{case}: {before:?} then {after:?}");
    };
    assert_ne!(now, then, "{case}: new data kept its version");
    assert_eq!(
        version(&after, &sources[1].id),
        version(&before, &sources[1].id),
        "{case}: the others didn't change"
    );
    true
}

async fn an_unreadable_source_is_unknown(harness: &dyn ChangeHarness) -> bool {
    let case = "an_unreadable_source_is_unknown";
    let Some(unreadable) = harness.unreadable() else {
        return false;
    };
    let provider = harness.provider().await;
    let mut sources = harness.readable();
    sources.insert(1, unreadable.clone());
    let report = read(case, provider.as_ref(), &sources).await;
    assert_eq!(listed(&report), ids(&sources), "{case}: listed sources");
    assert!(
        matches!(version(&report, &unreadable.id), SourceVersion::Unknown(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
    assert!(
        matches!(version(&report, &sources[0].id), SourceVersion::Version(_)),
        "{case}: the others are still read: {report:?}"
    );
    true
}

/// Runs every case against `harness`.
pub async fn run(harness: &dyn ChangeHarness) -> Report {
    let mut report = Report::default();
    lists_every_source_once_in_request_order(harness).await;
    report
        .passed
        .push("lists_every_source_once_in_request_order");
    unknown_sources_have_no_version(harness).await;
    report.passed.push("unknown_sources_have_no_version");
    reading_is_repeatable(harness).await;
    report.passed.push("reading_is_repeatable");
    if a_commit_moves_only_its_version(harness).await {
        report.passed.push("a_commit_moves_only_its_version");
    } else {
        report.skipped.push((
            "a_commit_moves_only_its_version",
            "the harness can't change a source's data".to_owned(),
        ));
    }
    if an_unreadable_source_is_unknown(harness).await {
        report.passed.push("an_unreadable_source_is_unknown");
    } else {
        report.skipped.push((
            "an_unreadable_source_is_unknown",
            "the harness has no unreadable source".to_owned(),
        ));
    }
    report
}
