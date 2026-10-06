//! Conformance suite for [`RelationProbe`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::probe::{ProbeAnswer, ProbeReport, ProbeRequest, ProbeTarget, RelationProbe};

/// What the suite needs from a probe under test.
#[async_trait]
pub trait ProbeHarness: Send + Sync {
    /// A fresh probe over a warehouse where every [`matching`](Self::matching)
    /// target's relation matches [`request`](Self::request)'s filter and answers its
    /// statements. Called once per case.
    async fn probe(&self) -> Arc<dyn RelationProbe>;

    /// A request the warehouse answers.
    fn request(&self) -> ProbeRequest;

    /// At least two targets whose relations match the request's filter.
    fn matching(&self) -> Vec<ProbeTarget>;

    /// A target whose relation exists but doesn't match the filter (e.g. a view when
    /// the filter asks for tables), if the harness has one. `None` skips the case that
    /// needs it.
    fn excluded(&self) -> Option<ProbeTarget>;

    /// A matching target that names a relation other than the one the implementation
    /// finds for it, if the harness has one. `None` (the default) skips the case.
    fn elsewhere(&self) -> Option<ProbeTarget> {
        None
    }

    /// A request with a [by-name](crate::contracts::probe::ProbeStatement::by_name)
    /// statement, and a matching target whose database, schema or name has a quote,
    /// backslash or brace, if the harness has one. `None` (the default) skips the case.
    fn unsafe_name(&self) -> Option<(ProbeRequest, ProbeTarget)> {
        None
    }
}

fn ids(targets: &[ProbeTarget]) -> Vec<String> {
    targets.iter().map(|s| s.id.clone()).collect()
}

fn listed(report: &ProbeReport) -> Vec<String> {
    report.targets.iter().map(|(id, _)| id.clone()).collect()
}

fn answer<'a>(report: &'a ProbeReport, id: &str) -> &'a ProbeAnswer {
    &report
        .targets
        .iter()
        .find(|(target, _)| target == id)
        .unwrap_or_else(|| panic!("{id} is not in the report: {report:?}"))
        .1
}

async fn run_probe(
    case: &str,
    probe: &dyn RelationProbe,
    request: &ProbeRequest,
    targets: &[ProbeTarget],
) -> ProbeReport {
    probe
        .probe(request, targets)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"))
}

async fn answers_every_target_once_with_a_row_per_statement(harness: &dyn ProbeHarness) {
    let case = "answers_every_target_once_with_a_row_per_statement";
    let probe = harness.probe().await;
    let request = harness.request();
    let mut targets = harness.matching();
    assert!(
        targets.len() >= 2,
        "{case}: the harness needs two matching targets"
    );
    targets.reverse();
    let report = run_probe(case, probe.as_ref(), &request, &targets).await;
    assert_eq!(listed(&report), ids(&targets), "{case}: listed targets");
    for target in &targets {
        let ProbeAnswer::Rows(rows) = answer(&report, &target.id) else {
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

async fn unknown_targets_are_never_probed(harness: &dyn ProbeHarness) {
    let case = "unknown_targets_are_never_probed";
    let probe = harness.probe().await;
    let unknown = ProbeTarget::new("model.ods_conformance.no_such_model", "no_such_model");
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
    let targets = harness.matching();
    let first = run_probe(case, probe.as_ref(), &request, &targets).await;
    let again = run_probe(case, probe.as_ref(), &request, &targets).await;
    assert_eq!(again, first, "{case}: probing changed the warehouse");
}

async fn relations_the_filter_excludes_are_skipped(harness: &dyn ProbeHarness) -> bool {
    let case = "relations_the_filter_excludes_are_skipped";
    let Some(excluded) = harness.excluded() else {
        return false;
    };
    let probe = harness.probe().await;
    let mut targets = harness.matching();
    targets.insert(1, excluded.clone());
    let report = run_probe(case, probe.as_ref(), &harness.request(), &targets).await;
    assert_eq!(listed(&report), ids(&targets), "{case}: listed targets");
    assert!(
        matches!(answer(&report, &excluded.id), ProbeAnswer::Skipped(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
    assert!(
        matches!(answer(&report, &targets[0].id), ProbeAnswer::Rows(_)),
        "{case}: the others are still probed: {report:?}"
    );
    true
}

async fn a_target_found_elsewhere_is_never_probed(harness: &dyn ProbeHarness) -> bool {
    let case = "a_target_found_elsewhere_is_never_probed";
    let Some(elsewhere) = harness.elsewhere() else {
        return false;
    };
    let probe = harness.probe().await;
    let report = run_probe(
        case,
        probe.as_ref(),
        &harness.request(),
        std::slice::from_ref(&elsewhere),
    )
    .await;
    assert!(
        matches!(answer(&report, &elsewhere.id), ProbeAnswer::Unknown(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
    true
}

async fn a_name_that_cant_be_a_literal_is_never_probed(harness: &dyn ProbeHarness) -> bool {
    let case = "a_name_that_cant_be_a_literal_is_never_probed";
    let Some((request, target)) = harness.unsafe_name() else {
        return false;
    };
    let probe = harness.probe().await;
    let report = run_probe(
        case,
        probe.as_ref(),
        &request,
        std::slice::from_ref(&target),
    )
    .await;
    assert!(
        matches!(answer(&report, &target.id), ProbeAnswer::Unknown(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
    true
}

/// Runs every case against `harness`.
pub async fn run(harness: &dyn ProbeHarness) -> Report {
    let mut report = Report::default();
    answers_every_target_once_with_a_row_per_statement(harness).await;
    report
        .passed
        .push("answers_every_target_once_with_a_row_per_statement");
    unknown_targets_are_never_probed(harness).await;
    report.passed.push("unknown_targets_are_never_probed");
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
    if a_name_that_cant_be_a_literal_is_never_probed(harness).await {
        report
            .passed
            .push("a_name_that_cant_be_a_literal_is_never_probed");
    } else {
        report.skipped.push((
            "a_name_that_cant_be_a_literal_is_never_probed",
            "the harness has no relation whose name can't be a literal".to_owned(),
        ));
    }
    if a_target_found_elsewhere_is_never_probed(harness).await {
        report
            .passed
            .push("a_target_found_elsewhere_is_never_probed");
    } else {
        report.skipped.push((
            "a_target_found_elsewhere_is_never_probed",
            "the harness has no target found under another relation".to_owned(),
        ));
    }
    report
}
