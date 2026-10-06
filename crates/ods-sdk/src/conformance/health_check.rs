//! Conformance suite for [`HealthCheck`] (ADR-0030 §5).

use std::collections::BTreeMap;
use std::sync::Arc;

use ods_core::Capability;

use super::Report;
use crate::contracts::health_check::{CheckFinding, CheckInfo, CheckScope, HealthCheck};

/// What the suite needs from a check under test.
pub trait HealthCheckHarness: Send + Sync {
    /// The check, ready to run.
    fn check(&self) -> Arc<dyn HealthCheck>;

    /// A scope of a few nodes, ids unique, that the check can decide about.
    fn scope(&self) -> CheckScope;

    /// A scope the check can't decide about (e.g. its connection unavailable), or
    /// `None` if nothing makes it undecided, which skips the case that needs it.
    fn undecidable(&self) -> Option<CheckScope>;
}

fn sorted(mut findings: Vec<CheckFinding>) -> Vec<CheckFinding> {
    findings.sort_by(|a, b| a.node.cmp(&b.node).then_with(|| a.reason.cmp(&b.reason)));
    findings
}

/// Every node in `scope` answered once, and no other.
fn answers_exactly(case: &str, scope: &CheckScope, findings: &[CheckFinding]) {
    let mut answered: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in findings {
        *answered.entry(finding.node.as_str()).or_default() += 1;
        assert!(
            !finding.reason.trim().is_empty(),
            "{case}: no reason for {}",
            finding.node
        );
    }
    for node in &scope.nodes {
        assert_eq!(
            answered.remove(node.id.as_str()),
            Some(1),
            "{case}: {} must be answered exactly once",
            node.id
        );
    }
    assert!(
        answered.is_empty(),
        "{case}: answered nodes outside the scope: {answered:?}"
    );
}

fn advertises_the_capability(harness: &dyn HealthCheckHarness) {
    let info = harness.check().info();
    assert!(
        info.capabilities.contains(&Capability::HealthCheck),
        "advertises_the_capability: {info:?}"
    );
}

fn describes_itself(harness: &dyn HealthCheckHarness) {
    let info = harness.check().describe();
    assert!(
        CheckInfo::valid_id(&info.id),
        "describes_itself: id `{}`",
        info.id
    );
    assert!(!info.about.trim().is_empty(), "describes_itself: no about");
    assert_eq!(
        harness.check().describe(),
        info,
        "describes_itself: not stable"
    );
}

async fn answers_every_node_once(harness: &dyn HealthCheckHarness) {
    let case = "answers_every_node_once";
    let scope = harness.scope();
    assert!(
        !scope.nodes.is_empty(),
        "{case}: the harness's scope is empty"
    );
    let findings = harness
        .check()
        .check(&scope)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    answers_exactly(case, &scope, &findings);
}

async fn an_empty_scope_has_no_findings(harness: &dyn HealthCheckHarness) {
    let case = "an_empty_scope_has_no_findings";
    let scope = CheckScope::new(Vec::new(), harness.scope().last_run);
    let findings = harness
        .check()
        .check(&scope)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert!(findings.is_empty(), "{case}: {findings:?}");
}

async fn is_deterministic(harness: &dyn HealthCheckHarness) {
    let case = "is_deterministic";
    let scope = harness.scope();
    let check = harness.check();
    let first = check
        .check(&scope)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let second = check
        .check(&scope)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(sorted(first), sorted(second), "{case}");
}

/// Undecided means unknown or an error, never a pass (AGENTS rule 3).
async fn never_passes_what_it_cant_decide(harness: &dyn HealthCheckHarness) -> bool {
    let case = "never_passes_what_it_cant_decide";
    let Some(scope) = harness.undecidable() else {
        return false;
    };
    if let Ok(findings) = harness.check().check(&scope).await {
        answers_exactly(case, &scope, &findings);
        assert!(
            findings
                .iter()
                .all(|f| f.status != crate::contracts::health_check::Status::Pass),
            "{case}: {findings:?}"
        );
    }
    true
}

/// Runs every case. Panics with the case's name on the first failure.
pub async fn run(harness: &dyn HealthCheckHarness) -> Report {
    let mut report = Report::default();
    advertises_the_capability(harness);
    report.passed.push("advertises_the_capability");
    describes_itself(harness);
    report.passed.push("describes_itself");
    answers_every_node_once(harness).await;
    report.passed.push("answers_every_node_once");
    an_empty_scope_has_no_findings(harness).await;
    report.passed.push("an_empty_scope_has_no_findings");
    is_deterministic(harness).await;
    report.passed.push("is_deterministic");
    if never_passes_what_it_cant_decide(harness).await {
        report.passed.push("never_passes_what_it_cant_decide");
    } else {
        report.skipped.push((
            "never_passes_what_it_cant_decide",
            "the harness has no scope the check can't decide about".to_owned(),
        ));
    }
    report
}
