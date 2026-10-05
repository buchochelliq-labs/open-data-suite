//! Conformance suite for [`ObservedLineageSource`] (#99, ADR-0006 §5).
//!
//! The harness gives a source over recorded lineage it ships with (a fixture, never a
//! live platform) and, if it can, one over malformed input. The suite checks what every
//! source must do: read the same thing every time, keep its edges consistent, and fail
//! on bad input with an error rather than a panic or a partial result.

use std::sync::Arc;

use super::Report;
use crate::contracts::observed_lineage::{ObservedLineage, ObservedLineageSource};

/// What the suite tests against.
pub trait ObservedLineageHarness: Send + Sync {
    /// A source over recorded lineage with at least one column edge.
    fn source(&self) -> Arc<dyn ObservedLineageSource>;

    /// A source over input it can't read (e.g. a malformed export), if the harness has
    /// one; `None` skips the case.
    fn malformed(&self) -> Option<Arc<dyn ObservedLineageSource>> {
        None
    }
}

fn read(source: &dyn ObservedLineageSource) -> ObservedLineage {
    source
        .observed_lineage()
        .unwrap_or_else(|e| panic!("observed_lineage: the harness's source reads: {e}"))
}

fn reading_again_gives_the_same_lineage(source: &dyn ObservedLineageSource) {
    assert_eq!(
        read(source),
        read(source),
        "observed_lineage: the same source reads the same every time"
    );
}

fn edges_are_consistent(source: &dyn ObservedLineageSource) {
    let observed = read(source);
    assert!(
        !observed.column_edges.is_empty(),
        "observed_lineage: the harness's source has a column edge"
    );
    for (from, to) in &observed.column_edges {
        assert!(
            observed
                .relation_edges
                .contains(&(from.relation.clone(), to.relation.clone())),
            "observed_lineage: the column edge {from} → {to} has its relation edge"
        );
    }
    for (from, to) in &observed.row_inputs {
        assert!(
            observed
                .relation_edges
                .contains(&(from.relation.clone(), to.clone())),
            "observed_lineage: the row input {from} → {to} has its relation edge"
        );
    }
    assert!(
        observed.skipped <= observed.records,
        "observed_lineage: it can't skip more records than it read ({} of {})",
        observed.skipped,
        observed.records
    );
    if let Some((first, last)) = &observed.observed_between {
        assert!(
            !first.is_empty() && !last.is_empty(),
            "observed_lineage: the time span, when given, names both ends"
        );
    }
}

fn malformed_input_is_an_error(source: &dyn ObservedLineageSource) {
    assert!(
        source.observed_lineage().is_err(),
        "observed_lineage: input it can't read is an error, never a partial result"
    );
}

/// Runs every case against the sources `harness` gives.
///
/// # Panics
/// Panics with the case name when a source breaks the contract.
pub fn run(harness: &dyn ObservedLineageHarness) -> Report {
    let mut report = Report::default();
    let source = harness.source();
    reading_again_gives_the_same_lineage(source.as_ref());
    report.passed.push("reading_again_gives_the_same_lineage");
    edges_are_consistent(source.as_ref());
    report.passed.push("edges_are_consistent");
    match harness.malformed() {
        Some(malformed) => {
            malformed_input_is_an_error(malformed.as_ref());
            report.passed.push("malformed_input_is_an_error");
        }
        None => report.skipped.push((
            "malformed_input_is_an_error",
            "the harness has no malformed input".to_owned(),
        )),
    }
    report
}
