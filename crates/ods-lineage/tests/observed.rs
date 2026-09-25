//! Comparing static lineage with observed lineage, and stitching observed lineage into
//! opaque (e.g. Python) models.

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_lineage::{
    Agreement, Change, ColumnChangeKind, ColumnGraph, ImpactReason, LineageNode, LineageProject,
    MemoryCache, NodeKind, build,
};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::observed_lineage::ObservedLineage;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn col(relation: &str, column: &str) -> ColumnRef {
    ColumnRef::new(rel(relation), column)
}

/// `raw` (seed) → `orders` (SQL: id, amount; filtered on status) → `scores` (Python).
fn graph() -> ColumnGraph {
    let direct = |c: &str| {
        OutputColumn::new(
            c,
            [(col("raw", c), EdgeKind::Direct(DirectKind::Identity))].into(),
            c,
            Confidence::Exact,
        )
    };
    let orders = QueryLineage::new(
        vec![direct("id"), direct("amount")],
        [(col("raw", "status"), IndirectKind::Filter)].into(),
        [rel("raw")].into(),
        "rows",
        vec![],
    );
    let analyzer = FakeSqlLineageAnalyzer::new().with("orders.sql", orders);
    let project = LineageProject::new(vec![
        LineageNode::new("raw", rel("raw"), NodeKind::Seed)
            .with_columns(["id", "amount", "status"]),
        LineageNode::new("orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["raw"]),
        // A Python model: no SQL, so no static lineage.
        LineageNode::new("scores", rel("scores"), NodeKind::Model).with_depends_on(["orders"]),
    ]);
    build(&project, &analyzer, &MemoryCache::default())
        .unwrap()
        .0
}

fn observed() -> ObservedLineage {
    let mut observed = ObservedLineage::default();
    observed.add_column_edge(col("raw", "id"), col("orders", "id"));
    observed.add_column_edge(col("raw", "amount"), col("orders", "amount"));
    observed.add_column_edge(col("orders", "amount"), col("scores", "total"));
    observed
}

#[test]
fn matching_lineage_agrees_and_opaque_models_are_stitch_candidates() {
    let comparison = graph().compare_observed(&observed());
    let verdict = |id: &str| {
        comparison
            .models
            .iter()
            .find(|m| m.node == id)
            .unwrap()
            .agreement
    };
    assert_eq!(verdict("orders"), Agreement::Agrees);
    assert_eq!(verdict("scores"), Agreement::OpaqueObserved);
    assert_eq!(comparison.precision(), Some(1.0));
    assert_eq!(comparison.recall(), Some(1.0));
}

#[test]
fn unpredicted_edges_are_misses_and_row_inputs_count_as_agreement() {
    let mut observed = observed();
    observed.add_column_edge(col("raw", "status"), col("orders", "id"));
    observed.add_column_edge(col("raw", "discount"), col("orders", "amount"));
    let comparison = graph().compare_observed(&observed);
    let orders = comparison
        .models
        .iter()
        .find(|m| m.node == "orders")
        .unwrap();
    assert_eq!(orders.agreement, Agreement::Misses);
    assert_eq!((orders.matched, orders.matched_indirect), (2, 1));
    assert_eq!(orders.missing.len(), 1);
    assert_eq!(orders.missing[0].source, col("raw", "discount"));
    assert_eq!(orders.recall, Some(0.75));
}

#[test]
fn predicted_edges_that_did_not_run_are_reported_not_counted_as_misses() {
    let mut observed = ObservedLineage::default();
    observed.add_column_edge(col("raw", "id"), col("orders", "id"));
    let comparison = graph().compare_observed(&observed);
    let orders = comparison
        .models
        .iter()
        .find(|m| m.node == "orders")
        .unwrap();
    assert_eq!(orders.agreement, Agreement::Covers);
    assert_eq!(orders.unobserved.len(), 1);
    assert_eq!(orders.precision, Some(0.5));
    let scores = comparison
        .models
        .iter()
        .find(|m| m.node == "scores")
        .unwrap();
    assert_eq!(scores.agreement, Agreement::NotObserved);
}

#[test]
fn stitched_lineage_is_shown_but_stays_opaque_for_impact_unless_trusted() {
    let change = Change::Column {
        column: col("orders", "id"),
        kind: ColumnChangeKind::Modified,
    };

    let (shown, stitched) = graph().with_observed(&observed(), false);
    assert_eq!(stitched.nodes, ["scores"]);
    let scores = shown.node("scores").unwrap();
    assert!(scores.is_opaque());
    let lineage = scores.lineage.as_ref().unwrap();
    assert_eq!(lineage.confidence, Confidence::Observed);
    assert!(lineage.output("total").is_some());
    assert!(scores.columns.contains(&"total".to_owned()));
    let impact = shown.impact(std::slice::from_ref(&change));
    assert!(
        impact.nodes["scores"]
            .reasons
            .contains(&ImpactReason::Opaque {
                upstream: rel("orders")
            }),
        "untrusted observed lineage never prunes"
    );

    let (trusted, _) = graph().with_observed(&observed(), true);
    let impact = trusted.impact(std::slice::from_ref(&change));
    assert!(
        !impact.nodes.contains_key("scores"),
        "scores only uses orders.amount"
    );
    assert!(impact.pruned.iter().any(|p| p.node == "scores"));
    let amount = Change::Column {
        column: col("orders", "amount"),
        kind: ColumnChangeKind::Modified,
    };
    let impact = trusted.impact(&[amount]);
    assert!(impact.nodes["scores"].changed_columns.contains("total"));
}

#[test]
fn models_the_analyzer_can_read_are_never_overwritten() {
    let mut observed = observed();
    observed.add_column_edge(col("raw", "status"), col("orders", "flag"));
    let (graph, stitched) = graph().with_observed(&observed, true);
    assert_eq!(stitched.nodes, ["scores"]);
    let orders = graph.node("orders").unwrap().lineage.as_ref().unwrap();
    assert_eq!(orders.confidence, Confidence::Exact);
    assert!(orders.output("flag").is_none());
}
