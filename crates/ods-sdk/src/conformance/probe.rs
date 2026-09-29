//! Conformance suite for [`RelationProbe`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::changes::RequestedSource;
use crate::contracts::probe::{ProbeAnswer, ProbeReport, ProbeRequest, RelationProbe};

/// What the suite needs from a probe under test.
#[async_trait]
pub trait ProbeHarness: Send + Sync {
    /// A fresh probe over a warehouse where every [`matching`](Self::matching)
    /// source's relation matches [`request`](Self::request)'s filter and answers its
    /// statements. Called once per case.
    async fn probe(&self) -> Arc<dyn RelationProbe>;

    /// A request the warehouse answers.
    fn request(&self) -> ProbeRequest;

    /// At least two sources whose relations match the request's filter.
    fn matching(&self) -> Vec<RequestedSource>;

    /// A source whose relation exists but doesn't match the filter (e.g. a view when
    /// the filter asks for tables), if the harness has one. `None` skips the case that
    /// needs it.
    fn excluded(&self) -> Option<RequestedSource>;
}

fn ids(sources: &[RequestedSource]) -> Vec<String> {
    sources.iter().map(|s| s.id.clone()).collect()
}

fn listed(report: &ProbeReport) -> Vec<String> {
    report.sources.iter().map(|(id, _)| id.clone()).collect()
}

fn answer<'a>(report: &'a ProbeReport, id: &str) -> &'a ProbeAnswer {
    &report
        .sources
        .iter()
        .find(|(source, _)| source == id)
        .unwrap_or_else(|| panic!("{id} is not in the report: {report:?}"))
        .1
}

async fn run_probe(
    case: &str,
    probe: &dyn RelationProbe,
    request: &ProbeRequest,
    sources: &[RequestedSource],
) -> ProbeReport {
    probe
        .probe(request, sources)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"))
}

async fn answers_every_source_once_with_a_row_per_statement(harness: &dyn ProbeHarness) {
    let case = "answers_every_source_once_with_a_row_per_statement";
    let probe = harness.probe().await;
    let request = harness.request();
    let mut sources = harness.matching();
    assert!(
        sources.len() >= 2,
        "{case}: the harness needs two matching sources"
    );
    sources.reverse();
    let report = run_probe(case, probe.as_ref(), &request, &sources).await;
    assert_eq!(listed(&report), ids(&sources), "{case}: listed sources");
    for source in &sources {
        let ProbeAnswer::Rows(rows) = answer(&report, &source.id) else {
            panic!("{case}: {report:?}");
        };
        assert_eq!(
            rows.len(),
            request.statements().len(),
            "{case}: one row per statement: {report:?}"
        );
        for (row, statement) in rows.iter().zip(request.statements()) {
            assert!(
                row.keys()
                    .all(|column| statement.columns().contains(column)),
                "{case}: only the requested columns: {row:?}"
            );
        }
    }
}

async fn unknown_sources_are_never_probed(harness: &dyn ProbeHarness) {
    let case = "unknown_sources_are_never_probed";
    let probe = harness.probe().await;
    let unknown = RequestedSource::new(
        "source.ods_conformance.none.no_such_source",
        "none.no_such_source",
    );
    let report = run_probe(
        case,
        probe.as_ref(),
        &harness.request(),
        std::slice::from_ref(&unknown),
    )
    .await;
    assert_eq!(
        listed(&report),
        std::slice::from_ref(&unknown.id),
        "{case}: listed"
    );
    // Skipped is for relations it recognised that don't match the filter.
    assert!(
        matches!(answer(&report, &unknown.id), ProbeAnswer::Unknown(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
}

async fn probing_is_repeatable(harness: &dyn ProbeHarness) {
    let case = "probing_is_repeatable";
    let probe = harness.probe().await;
    let request = harness.request();
    let sources = harness.matching();
    let first = run_probe(case, probe.as_ref(), &request, &sources).await;
    let again = run_probe(case, probe.as_ref(), &request, &sources).await;
    assert_eq!(again, first, "{case}: probing changed the warehouse");
}

async fn relations_the_filter_excludes_are_skipped(harness: &dyn ProbeHarness) -> bool {
    let case = "relations_the_filter_excludes_are_skipped";
    let Some(excluded) = harness.excluded() else {
        return false;
    };
    let probe = harness.probe().await;
    let mut sources = harness.matching();
    sources.insert(1, excluded.clone());
    let report = run_probe(case, probe.as_ref(), &harness.request(), &sources).await;
    assert_eq!(listed(&report), ids(&sources), "{case}: listed sources");
    assert!(
        matches!(answer(&report, &excluded.id), ProbeAnswer::Skipped(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
    assert!(
        matches!(answer(&report, &sources[0].id), ProbeAnswer::Rows(_)),
        "{case}: the others are still probed: {report:?}"
    );
    true
}

/// Runs every case against `harness`.
pub async fn run(harness: &dyn ProbeHarness) -> Report {
    let mut report = Report::default();
    answers_every_source_once_with_a_row_per_statement(harness).await;
    report
        .passed
        .push("answers_every_source_once_with_a_row_per_statement");
    unknown_sources_are_never_probed(harness).await;
    report.passed.push("unknown_sources_are_never_probed");
    probing_is_repeatable(harness).await;
    report.passed.push("probing_is_repeatable");
    if relations_the_filter_excludes_are_skipped(harness).await {
        report
            .passed
            .push("relations_the_filter_excludes_are_skipped");
    } else {
        report.skipped.push((
            "relations_the_filter_excludes_are_skipped",
            "the harness has no relation the filter excludes".to_owned(),
        ));
    }
    report
}
