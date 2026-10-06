//! Conformance suite for [`RelationPrivileges`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::privileges::{Access, PrivilegeReport, RelationPrivileges};
use crate::contracts::probe::ProbeTarget;

/// What the suite needs from a provider under test.
#[async_trait]
pub trait PrivilegesHarness: Send + Sync {
    /// A fresh provider over a warehouse where its login can only read every
    /// [`read_only`](Self::read_only) target's relation. Called once per case.
    async fn provider(&self) -> Arc<dyn RelationPrivileges>;

    /// At least two targets the login can only read.
    fn read_only(&self) -> Vec<ProbeTarget>;

    /// A target the login can do more than read (e.g. one it owns), if the harness has
    /// one. `None` skips the case that needs it.
    fn elevated(&self) -> Option<ProbeTarget>;
}

fn ids(targets: &[ProbeTarget]) -> Vec<String> {
    targets.iter().map(|t| t.id.clone()).collect()
}

fn listed(report: &PrivilegeReport) -> Vec<String> {
    report.targets.iter().map(|(id, _)| id.clone()).collect()
}

fn access<'a>(report: &'a PrivilegeReport, id: &str) -> &'a Access {
    &report
        .targets
        .iter()
        .find(|(target, _)| target == id)
        .unwrap_or_else(|| panic!("{id} is not in the report: {report:?}"))
        .1
}

async fn read(
    case: &str,
    provider: &dyn RelationPrivileges,
    targets: &[ProbeTarget],
) -> PrivilegeReport {
    provider
        .privileges(targets)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"))
}

async fn answers_every_target_once_in_order(harness: &dyn PrivilegesHarness) {
    let case = "answers_every_target_once_in_order";
    let provider = harness.provider().await;
    let mut targets = harness.read_only();
    assert!(
        targets.len() >= 2,
        "{case}: the harness needs two read-only targets"
    );
    targets.reverse();
    let report = read(case, provider.as_ref(), &targets).await;
    assert_eq!(listed(&report), ids(&targets), "{case}: listed targets");
    for target in &targets {
        assert_eq!(
            access(&report, &target.id),
            &Access::ReadOnly,
            "{case}: {report:?}"
        );
    }
}

async fn unknown_targets_are_never_read_only(harness: &dyn PrivilegesHarness) {
    let case = "unknown_targets_are_never_read_only";
    let provider = harness.provider().await;
    let unknown = ProbeTarget::new("model.ods_conformance.no_such_model", "no_such_model");
    let report = read(case, provider.as_ref(), std::slice::from_ref(&unknown)).await;
    assert_eq!(
        listed(&report),
        std::slice::from_ref(&unknown.id),
        "{case}: listed"
    );
    assert!(
        matches!(access(&report, &unknown.id), Access::Unknown(why) if !why.is_empty()),
        "{case}: {report:?}"
    );
}

async fn reading_is_repeatable(harness: &dyn PrivilegesHarness) {
    let case = "reading_is_repeatable";
    let provider = harness.provider().await;
    let targets = harness.read_only();
    let first = read(case, provider.as_ref(), &targets).await;
    let again = read(case, provider.as_ref(), &targets).await;
    assert_eq!(again, first, "{case}: reading changed the warehouse");
}

async fn more_than_reading_is_named(harness: &dyn PrivilegesHarness) -> bool {
    let case = "more_than_reading_is_named";
    let Some(elevated) = harness.elevated() else {
        return false;
    };
    let provider = harness.provider().await;
    let mut targets = harness.read_only();
    targets.insert(1, elevated.clone());
    let report = read(case, provider.as_ref(), &targets).await;
    assert_eq!(listed(&report), ids(&targets), "{case}: listed targets");
    assert!(
        matches!(access(&report, &elevated.id), Access::Elevated(what) if !what.is_empty() && what.iter().all(|w| !w.is_empty())),
        "{case}: {report:?}"
    );
    assert_eq!(
        access(&report, &targets[0].id),
        &Access::ReadOnly,
        "{case}: the others are still read-only: {report:?}"
    );
    true
}

/// Runs every case against `harness`.
pub async fn run(harness: &dyn PrivilegesHarness) -> Report {
    let mut report = Report::default();
    answers_every_target_once_in_order(harness).await;
    report.passed.push("answers_every_target_once_in_order");
    unknown_targets_are_never_read_only(harness).await;
    report.passed.push("unknown_targets_are_never_read_only");
    reading_is_repeatable(harness).await;
    report.passed.push("reading_is_repeatable");
    if more_than_reading_is_named(harness).await {
        report.passed.push("more_than_reading_is_named");
    } else {
        report.skipped.push((
            "more_than_reading_is_named",
            "the harness has no target the login can do more than read".to_owned(),
        ));
    }
    report
}
