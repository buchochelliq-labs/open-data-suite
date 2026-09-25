//! Building, caching, diffing and impact on a jaffle-shaped project with scripted lineage.

use std::collections::BTreeSet;

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_lineage::{
    Change, ColumnChangeKind, ImpactReason, LineageNode, LineageProject, MemoryCache, NodeKind,
    build, diff,
};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn col(relation: &str, column: &str) -> ColumnRef {
    ColumnRef::new(rel(relation), column)
}

const ID: EdgeKind = EdgeKind::Direct(DirectKind::Identity);
const AGG: EdgeKind = EdgeKind::Direct(DirectKind::Aggregation);
const XFORM: EdgeKind = EdgeKind::Direct(DirectKind::Transformation);

fn out(name: &str, inputs: &[(ColumnRef, EdgeKind)]) -> OutputColumn {
    OutputColumn::new(
        name,
        inputs.iter().cloned().collect(),
        format!("digest:{name}:{}", inputs.len()),
        Confidence::Exact,
    )
}

fn query(
    outputs: Vec<OutputColumn>,
    rows: &[(ColumnRef, IndirectKind)],
    reads: &[&str],
) -> QueryLineage {
    QueryLineage::new(
        outputs,
        rows.iter().cloned().collect(),
        reads.iter().map(|r| rel(r)).collect(),
        "rows",
        vec![],
    )
}

/// `stg_orders`/`stg_payments` → `orders` → {`customers`, `rank`}; `events` reads staging;
/// `customers_all` selects `*` from `customers`; `legacy_report` is opaque.
#[allow(
    clippy::too_many_lines,
    reason = "one fixture, easiest to read top to bottom"
)]
fn project_and_analyzer() -> (LineageProject, FakeSqlLineageAnalyzer) {
    let orders = query(
        vec![
            out("order_id", &[(col("stg_orders", "order_id"), ID)]),
            out("customer_id", &[(col("stg_orders", "customer_id"), ID)]),
            out("order_date", &[(col("stg_orders", "order_date"), ID)]),
            out("status", &[(col("stg_orders", "status"), ID)]),
            out(
                "credit_card_amount",
                &[
                    (col("stg_payments", "amount"), AGG),
                    (
                        col("stg_payments", "payment_method"),
                        EdgeKind::Indirect(IndirectKind::Conditional),
                    ),
                ],
            ),
            out("amount", &[(col("stg_payments", "amount"), AGG)]),
        ],
        &[
            (col("stg_orders", "order_id"), IndirectKind::Join),
            (col("stg_payments", "order_id"), IndirectKind::Join),
            (col("stg_payments", "order_id"), IndirectKind::GroupBy),
        ],
        &["stg_orders", "stg_payments"],
    );
    let customers = query(
        vec![
            out("customer_id", &[(col("orders", "customer_id"), ID)]),
            out("first_order", &[(col("orders", "order_date"), AGG)]),
            out("lifetime_value", &[(col("orders", "amount"), AGG)]),
        ],
        &[
            (col("orders", "status"), IndirectKind::Filter),
            (col("orders", "customer_id"), IndirectKind::GroupBy),
        ],
        &["orders"],
    );
    let rank = query(
        vec![
            out("order_id", &[(col("orders", "order_id"), ID)]),
            out(
                "order_seq",
                &[
                    (
                        col("orders", "customer_id"),
                        EdgeKind::Indirect(IndirectKind::Window),
                    ),
                    (
                        col("orders", "order_date"),
                        EdgeKind::Indirect(IndirectKind::Window),
                    ),
                ],
            ),
        ],
        &[(col("orders", "status"), IndirectKind::Filter)],
        &["orders"],
    );
    let events = query(
        vec![
            out("order_id", &[(col("stg_orders", "order_id"), ID)]),
            out("event_date", &[(col("stg_orders", "order_date"), XFORM)]),
        ],
        &[],
        &["stg_orders"],
    );
    let customers_all = query(
        vec![
            out("customer_id", &[(col("customers", "customer_id"), ID)]),
            out("first_order", &[(col("customers", "first_order"), ID)]),
            out(
                "lifetime_value",
                &[(col("customers", "lifetime_value"), ID)],
            ),
        ],
        &[],
        &["customers"],
    )
    .with_wildcards([rel("customers")].into());

    let analyzer = FakeSqlLineageAnalyzer::new()
        .with("orders.sql", orders)
        .with("customers.sql", customers)
        .with("rank.sql", rank)
        .with("events.sql", events)
        .with("customers_all.sql", customers_all);

    let model = |name: &str, deps: &[&str]| {
        LineageNode::new(name, rel(name), NodeKind::Model)
            .with_sql(format!("{name}.sql"))
            .with_depends_on(deps.iter().copied())
    };
    let staging = |name: &str, columns: &[&str]| {
        LineageNode::new(name, rel(name), NodeKind::Seed).with_columns(columns.iter().copied())
    };
    let mut legacy = LineageNode::new("legacy_report", rel("legacy_report"), NodeKind::Model)
        .with_sql("select something unparseable from orders")
        .with_depends_on(["orders"]);
    legacy.columns = Some(vec!["x".into()]);

    let project = LineageProject::new(vec![
        staging(
            "stg_orders",
            &["order_id", "customer_id", "order_date", "status"],
        ),
        staging(
            "stg_payments",
            &["payment_id", "order_id", "payment_method", "amount"],
        ),
        model("orders", &["stg_orders", "stg_payments"]),
        model("customers", &["orders"]),
        model("rank", &["orders"]),
        model("events", &["stg_orders"]),
        model("customers_all", &["customers"]),
        legacy,
    ]);
    (project, analyzer)
}

fn opaque_reads_orders(analyzer: FakeSqlLineageAnalyzer) -> FakeSqlLineageAnalyzer {
    analyzer.with(
        "select something unparseable from orders",
        QueryLineage::opaque([rel("orders")].into(), "parse error"),
    )
}

fn graph() -> ods_lineage::ColumnGraph {
    let (project, analyzer) = project_and_analyzer();
    let analyzer = opaque_reads_orders(analyzer);
    build(&project, &analyzer, &MemoryCache::default())
        .unwrap()
        .0
}

fn modified(relation: &str, column: &str) -> Change {
    Change::Column {
        column: col(relation, column),
        kind: ColumnChangeKind::Modified,
    }
}

fn ids(impact: &ods_lineage::Impact) -> Vec<&str> {
    impact.node_ids().collect()
}

#[test]
fn builds_in_dependency_waves_and_reports_opaque_models() {
    let (project, analyzer) = project_and_analyzer();
    let analyzer = opaque_reads_orders(analyzer);
    let (graph, stats) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    assert_eq!(stats.waves, 4, "{stats:?}");
    assert_eq!((stats.analyzed, stats.cached, stats.opaque), (6, 0, 1));
    assert!(graph.node("legacy_report").unwrap().is_opaque());
    // Opaque models keep their declared columns; analyzed ones use their outputs.
    assert_eq!(graph.node("legacy_report").unwrap().columns, ["x"]);
    assert_eq!(
        graph.node("rank").unwrap().columns,
        ["order_id", "order_seq"]
    );
}

#[test]
fn unchanged_models_are_served_from_the_cache() {
    let (project, analyzer) = project_and_analyzer();
    let cache = MemoryCache::default();
    build(&project, &analyzer, &cache).unwrap();
    let first = analyzer.calls();
    let (_, stats) = build(&project, &analyzer, &cache).unwrap();
    assert_eq!(analyzer.calls(), first, "nothing re-analyzed");
    assert_eq!(stats.analyzed, 0);
    // The opaque result is cached too: it reads only what the model declares.
    assert_eq!(stats.cached, 6);
}

#[test]
fn a_column_change_impacts_only_its_consumers() {
    // payment_method only feeds orders.credit_card_amount, which nothing downstream uses.
    let impact = graph().impact(&[modified("stg_payments", "payment_method")]);
    assert_eq!(ids(&impact), ["legacy_report", "orders"]);
    let orders = &impact.nodes["orders"];
    assert_eq!(
        orders.changed_columns,
        BTreeSet::from(["credit_card_amount".to_owned()])
    );
    assert!(!orders.rows_changed);
    // customers and rank read orders but none of its changed columns: pruned, with why.
    let pruned: Vec<&str> = impact.pruned.iter().map(|p| p.node.as_str()).collect();
    assert_eq!(pruned, ["customers", "rank"]);
    assert!(
        impact
            .pruned
            .iter()
            .all(|p| p.unused_changed_columns == BTreeSet::from(["credit_card_amount".into()]))
    );
    // The opaque model is impacted by any change to what it reads.
    assert!(
        impact.nodes["legacy_report"]
            .reasons
            .contains(&ImpactReason::Opaque {
                upstream: rel("orders")
            })
    );
}

#[test]
fn changes_propagate_through_values_windows_and_wildcards() {
    let impact = graph().impact(&[modified("stg_orders", "order_date")]);
    assert_eq!(
        ids(&impact),
        [
            "customers",
            "customers_all",
            "events",
            "legacy_report",
            "orders",
            "rank"
        ]
    );
    assert_eq!(
        impact.nodes["customers"].changed_columns,
        BTreeSet::from(["first_order".to_owned()])
    );
    assert_eq!(
        impact.nodes["customers_all"].changed_columns,
        BTreeSet::from(["first_order".to_owned()])
    );
    // A window ordering input changes the windowed column, not the rows.
    assert_eq!(
        impact.nodes["rank"].changed_columns,
        BTreeSet::from(["order_seq".to_owned()])
    );
    assert!(!impact.nodes["rank"].rows_changed);
}

#[test]
fn row_shaping_inputs_change_every_downstream_row() {
    let impact = graph().impact(&[modified("stg_orders", "status")]);
    // orders.status is modified; customers and rank filter on it, so their rows change,
    // which reaches customers_all.
    assert!(impact.nodes["customers"].rows_changed);
    assert!(impact.nodes["rank"].rows_changed);
    assert!(impact.nodes["customers_all"].rows_changed);
    assert!(!impact.nodes.contains_key("events"));
}

#[test]
fn an_added_column_impacts_only_wildcard_readers() {
    let impact = graph().impact(&[Change::Column {
        column: col("customers", "value_tier"),
        kind: ColumnChangeKind::Added,
    }]);
    assert_eq!(ids(&impact), ["customers_all"]);
    assert!(
        impact.nodes["customers_all"]
            .reasons
            .contains(&ImpactReason::Wildcard {
                upstream: col("customers", "value_tier")
            })
    );
}

#[test]
fn diff_reports_added_removed_and_modified_columns() {
    let before = query(
        vec![
            out("a", &[(col("t", "a"), ID)]),
            out("b", &[(col("t", "b"), ID)]),
        ],
        &[],
        &["t"],
    );
    let mut after = query(
        vec![
            out("a", &[(col("t", "a"), ID)]),
            out("c", &[(col("t", "c"), ID)]),
        ],
        &[],
        &["t"],
    );
    after.outputs[0].expression_digest = "changed".into();
    let changes = diff(&rel("m"), Some(&before), &after);
    assert_eq!(
        changes,
        [
            Change::Column {
                column: col("m", "a"),
                kind: ColumnChangeKind::Modified
            },
            Change::Column {
                column: col("m", "b"),
                kind: ColumnChangeKind::Removed
            },
            Change::Column {
                column: col("m", "c"),
                kind: ColumnChangeKind::Added
            },
        ]
    );
    // New models and row-shape changes are conservative: every row may differ.
    assert_eq!(
        diff(&rel("m"), None, &after),
        [Change::Rows { relation: rel("m") }]
    );
    let mut reshaped = before.clone();
    reshaped.row_digest = "other".into();
    assert_eq!(
        diff(&rel("m"), Some(&before), &reshaped),
        [Change::Rows { relation: rel("m") }]
    );
}

#[test]
fn value_sources_trace_through_models() {
    let graph = graph();
    assert_eq!(
        graph.value_sources(&col("customers_all", "lifetime_value")),
        BTreeSet::from([col("stg_payments", "amount")])
    );
}
