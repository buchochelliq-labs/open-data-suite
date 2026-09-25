//! Column lineage from real SQL, across the constructs that matter for impact analysis.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};
use ods_sdk::contracts::sql_lineage::{
    AnalyzeRequest, MapSchema, QueryLineage, SqlLineageAnalyzer,
};

const ID: EdgeKind = EdgeKind::Direct(DirectKind::Identity);
const XF: EdgeKind = EdgeKind::Direct(DirectKind::Transformation);
const AGG: EdgeKind = EdgeKind::Direct(DirectKind::Aggregation);

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn col(relation: &str, column: &str) -> ColumnRef {
    ColumnRef::new(rel(relation), column)
}

fn schema() -> MapSchema {
    let table =
        |name: &str, cols: &[&str]| (rel(name), cols.iter().map(|c| (*c).to_owned()).collect());
    MapSchema(BTreeMap::from([
        table(
            "orders",
            &["id", "customer_id", "status", "amount", "ordered_at"],
        ),
        table("customers", &["id", "name", "email", "tier"]),
        table("payments", &["id", "order_id", "method", "cents"]),
        table("events", &["id", "payload", "items"]),
    ]))
}

fn analyze_with(dialect: SqlDialect, sql: &str) -> QueryLineage {
    SqlparserAnalyzer::new(dialect)
        .analyze(&AnalyzeRequest {
            sql,
            schema: &schema(),
        })
        .unwrap()
}

fn analyze(sql: &str) -> QueryLineage {
    analyze_with(SqlDialect::Databricks, sql)
}

fn inputs(lineage: &QueryLineage, output: &str) -> BTreeSet<(ColumnRef, EdgeKind)> {
    lineage
        .output(output)
        .unwrap_or_else(|| panic!("no output `{output}` in {:?}", lineage.outputs))
        .inputs
        .clone()
}

fn names(lineage: &QueryLineage) -> Vec<&str> {
    lineage.outputs.iter().map(|o| o.name.as_str()).collect()
}

#[test]
fn classifies_identity_transformation_and_aggregation() {
    let l = analyze(
        "select customer_id as cust, upper(status) as status, sum(amount) as total
         from db.orders where status <> 'returned' group by customer_id, status",
    );
    assert!(!l.opaque, "{:?}", l.diagnostics);
    assert_eq!(names(&l), ["cust", "status", "total"]);
    assert_eq!(
        inputs(&l, "cust"),
        [(col("orders", "customer_id"), ID)].into()
    );
    assert_eq!(inputs(&l, "status"), [(col("orders", "status"), XF)].into());
    assert_eq!(inputs(&l, "total"), [(col("orders", "amount"), AGG)].into());
    assert!(
        l.row_inputs
            .contains(&(col("orders", "status"), IndirectKind::Filter))
    );
    assert!(
        l.row_inputs
            .contains(&(col("orders", "customer_id"), IndirectKind::GroupBy))
    );
    assert_eq!(l.confidence, Confidence::Exact);
    assert_eq!(l.relations_read, [rel("orders")].into());
}

#[test]
fn traces_through_ctes_to_physical_columns() {
    let l = analyze(
        "with completed as (select id, customer_id, amount from db.orders where status = 'completed'),
              per_customer as (select customer_id, sum(amount) as ltv from completed group by customer_id)
         select c.name, p.ltv from db.customers c join per_customer p on c.id = p.customer_id",
    );
    assert!(!l.opaque, "{:?}", l.diagnostics);
    assert_eq!(inputs(&l, "name"), [(col("customers", "name"), ID)].into());
    // Identity over an aggregation is still an aggregation.
    assert_eq!(inputs(&l, "ltv"), [(col("orders", "amount"), AGG)].into());
    // The CTE's filter and grouping shape the final rows, as does the join.
    for expected in [
        (col("orders", "status"), IndirectKind::Filter),
        (col("orders", "customer_id"), IndirectKind::GroupBy),
        (col("customers", "id"), IndirectKind::Join),
        (col("orders", "customer_id"), IndirectKind::Join),
    ] {
        assert!(
            l.row_inputs.contains(&expected),
            "missing {expected:?} in {:?}",
            l.row_inputs
        );
    }
    assert_eq!(l.relations_read, [rel("customers"), rel("orders")].into());
}

#[test]
fn resolves_unqualified_columns_across_joined_tables() {
    let l = analyze(
        "select name, amount, method from db.orders o
         join db.customers c on o.customer_id = c.id
         left join db.payments p on p.order_id = o.id",
    );
    assert_eq!(inputs(&l, "name"), [(col("customers", "name"), ID)].into());
    assert_eq!(
        inputs(&l, "method"),
        [(col("payments", "method"), ID)].into()
    );
    assert_eq!(l.confidence, Confidence::Exact);
}

#[test]
fn ambiguous_columns_attach_to_every_candidate_and_lower_confidence() {
    let l = analyze("select id from db.orders join db.customers on true");
    assert_eq!(
        inputs(&l, "id"),
        [(col("customers", "id"), ID), (col("orders", "id"), ID)].into()
    );
    assert_eq!(l.confidence, Confidence::Inferred);
}

#[test]
fn case_conditions_are_conditional_and_results_are_values() {
    let l = analyze(
        "select case when status = 'completed' then amount else 0 end as paid,
                sum(case when method = 'coupon' then cents end) as coupon_cents
         from db.orders join db.payments on payments.order_id = orders.id group by 1",
    );
    assert_eq!(
        inputs(&l, "paid"),
        [
            (col("orders", "amount"), XF),
            (
                col("orders", "status"),
                EdgeKind::Indirect(IndirectKind::Conditional)
            ),
        ]
        .into()
    );
    assert_eq!(
        inputs(&l, "coupon_cents"),
        [
            (col("payments", "cents"), AGG),
            (
                col("payments", "method"),
                EdgeKind::Indirect(IndirectKind::Conditional)
            ),
        ]
        .into()
    );
    // `group by 1` groups by the first projection's inputs.
    assert!(
        l.row_inputs
            .contains(&(col("orders", "status"), IndirectKind::GroupBy))
    );
}

#[test]
fn window_partitioning_is_an_indirect_input_of_that_column_only() {
    let l = analyze(
        "select id, row_number() over (partition by customer_id order by ordered_at) as seq,
                sum(amount) over w as running
         from db.orders window w as (partition by customer_id)",
    );
    assert_eq!(
        inputs(&l, "seq"),
        [
            (
                col("orders", "customer_id"),
                EdgeKind::Indirect(IndirectKind::Window)
            ),
            (
                col("orders", "ordered_at"),
                EdgeKind::Indirect(IndirectKind::Window)
            ),
        ]
        .into()
    );
    assert_eq!(
        inputs(&l, "running"),
        [
            (col("orders", "amount"), XF),
            (
                col("orders", "customer_id"),
                EdgeKind::Indirect(IndirectKind::Window)
            ),
        ]
        .into()
    );
    assert!(l.row_inputs.is_empty());
}

#[test]
fn set_operations_match_by_position() {
    let l = analyze(
        "select id as order_id, ordered_at as at from db.orders
         union all
         select order_id, null from db.payments",
    );
    assert_eq!(names(&l), ["order_id", "at"]);
    assert_eq!(
        inputs(&l, "order_id"),
        [(col("orders", "id"), ID), (col("payments", "order_id"), ID)].into()
    );
    assert!(l.row_inputs.is_empty(), "UNION ALL does not deduplicate");
    let distinct = analyze("select id from db.orders union select id from db.customers");
    assert!(
        distinct
            .row_inputs
            .contains(&(col("orders", "id"), IndirectKind::GroupBy))
    );
}

#[test]
fn stars_expand_from_known_schemas() {
    let l = analyze("select * except (payload) from db.events");
    assert_eq!(names(&l), ["id", "items"]);
    assert_eq!(l.wildcard_relations, [rel("events")].into());
    let qualified =
        analyze("select o.*, c.name from db.orders o join db.customers c on o.customer_id = c.id");
    assert_eq!(
        names(&qualified),
        [
            "id",
            "customer_id",
            "status",
            "amount",
            "ordered_at",
            "name"
        ]
    );
    // A star over a CTE with explicit columns is not a wildcard over the base table.
    let cte = analyze("with x as (select id, amount from db.orders) select * from x");
    assert_eq!(names(&cte), ["id", "amount"]);
    assert!(cte.wildcard_relations.is_empty());
}

#[test]
fn a_star_over_an_unknown_relation_is_opaque() {
    let l = analyze("select * from db.unknown_table");
    assert!(l.opaque);
    assert_eq!(l.relations_read, [rel("unknown_table")].into());
    assert!(l.diagnostics[0].contains("unknown"), "{:?}", l.diagnostics);
}

#[test]
fn subqueries_feed_values_and_filters() {
    let l = analyze(
        "select c.id,
                (select max(o.ordered_at) from db.orders o where o.customer_id = c.id) as last_order
         from db.customers c
         where exists (select 1 from db.payments p where p.method = 'coupon')",
    );
    let last = inputs(&l, "last_order");
    assert!(
        last.contains(&(col("orders", "ordered_at"), AGG)),
        "{last:?}"
    );
    // The correlated predicate shapes the subquery's rows, so it conditions the value.
    assert!(last.contains(&(
        col("customers", "id"),
        EdgeKind::Indirect(IndirectKind::Filter)
    )));
    assert!(
        l.row_inputs
            .contains(&(col("payments", "method"), IndirectKind::Filter))
    );
}

#[test]
fn databricks_syntax_is_understood() {
    let l = analyze(
        "select id, payload:customer.email::string as email, item,
                transform(items, x -> x.sku) as skus
         from db.events lateral view explode(items) t as item
         qualify row_number() over (partition by id order by id) = 1",
    );
    assert!(!l.opaque, "{:?}", l.diagnostics);
    assert_eq!(inputs(&l, "email"), [(col("events", "payload"), XF)].into());
    assert_eq!(inputs(&l, "item"), [(col("events", "items"), XF)].into());
    assert_eq!(inputs(&l, "skus"), [(col("events", "items"), XF)].into());
    assert!(
        l.row_inputs
            .contains(&(col("events", "id"), IndirectKind::Filter))
    );
}

#[test]
fn using_joins_record_both_sides() {
    let l = analyze(
        "select * from (select id as order_id, amount from db.orders) o
         join (select order_id, method from db.payments) p using (order_id)",
    );
    assert!(
        l.row_inputs
            .contains(&(col("orders", "id"), IndirectKind::Join))
    );
    assert!(
        l.row_inputs
            .contains(&(col("payments", "order_id"), IndirectKind::Join))
    );
}

#[test]
fn digests_ignore_aliases_and_formatting_but_not_logic() {
    let a = analyze("select o.amount * 2 as x from db.orders o where o.status = 'a'");
    let b = analyze("SELECT   t.amount*2 AS x\nFROM db.orders AS t -- note\nWHERE t.status='a'");
    let c = analyze("select o.amount * 3 as x from db.orders o where o.status = 'a'");
    let d = analyze("select o.amount * 2 as x from db.orders o where o.status = 'b'");
    assert_eq!(
        a.outputs[0].expression_digest,
        b.outputs[0].expression_digest
    );
    assert_eq!(a.row_digest, b.row_digest);
    assert_ne!(
        a.outputs[0].expression_digest,
        c.outputs[0].expression_digest
    );
    assert_eq!(a.row_digest, c.row_digest);
    assert_ne!(a.row_digest, d.row_digest);
}

#[test]
fn a_change_inside_a_cte_changes_the_outer_digest() {
    let a = analyze("with x as (select amount * 2 as v from db.orders) select v from x");
    let b = analyze("with x as (select amount * 3 as v from db.orders) select v from x");
    assert_ne!(
        a.outputs[0].expression_digest,
        b.outputs[0].expression_digest
    );
}

#[test]
fn unparseable_and_non_query_sql_is_opaque() {
    for sql in [
        "select ((( from",
        "create table x (id int)",
        "select 1; select 2",
    ] {
        let l = analyze(sql);
        assert!(l.opaque, "{sql}");
        assert_eq!(l.confidence, Confidence::Unknown);
    }
}

#[test]
fn identifier_case_follows_the_dialect() {
    // Postgres folds unquoted names to lower case but keeps quoted ones: `AMOUNT` is
    // `amount`, while `"Amount"` names a column that doesn't exist, so the query can't be
    // analyzed safely.
    let folded = analyze_with(SqlDialect::Postgres, "select AMOUNT from db.orders");
    assert_eq!(
        inputs(&folded, "amount"),
        [(col("orders", "amount"), ID)].into()
    );
    let quoted = analyze_with(SqlDialect::Postgres, r#"select "Amount" from db.orders"#);
    assert!(quoted.opaque, "{:?}", quoted.diagnostics);
    let dbx = analyze_with(SqlDialect::Databricks, "select `Amount` from db.orders");
    assert_eq!(
        inputs(&dbx, "amount"),
        [(col("orders", "amount"), ID)].into()
    );
}

// Regression tests for the architecture review of the column-lineage slice: each case
// used to lose an input or keep a digest unchanged when the meaning changed.

fn row_digest(sql: &str) -> String {
    analyze(sql).row_digest
}

fn digest_of(sql: &str, output: &str) -> String {
    analyze(sql)
        .output(output)
        .unwrap()
        .expression_digest
        .clone()
}

#[test]
fn review_sort_direction_windows_and_grouping_modifiers_change_digests() {
    assert_ne!(
        row_digest("select id from db.orders order by amount desc limit 5"),
        row_digest("select id from db.orders order by amount asc limit 5"),
    );
    assert_ne!(
        digest_of(
            "select sum(amount) over w as s from db.orders window w as (order by id)",
            "s"
        ),
        digest_of(
            "select sum(amount) over w as s from db.orders window w as (order by ordered_at)",
            "s"
        ),
    );
    assert_ne!(
        row_digest("select status, sum(amount) from db.orders group by status with rollup"),
        row_digest("select status, sum(amount) from db.orders group by status"),
    );
}

#[test]
fn review_lateral_struct_aggregate_order_and_windowed_arithmetic_keep_inputs() {
    let lateral = analyze("select v from db.orders o, lateral (select o.amount * 2 as v) x");
    assert!(!lateral.opaque, "{:?}", lateral.diagnostics);
    assert_eq!(
        inputs(&lateral, "v"),
        [(col("orders", "amount"), XF)].into()
    );

    let field = analyze("select e.payload.customer as c from db.events e");
    assert_eq!(inputs(&field, "c"), [(col("events", "payload"), XF)].into());

    let ordered = analyze("select array_agg(id order by ordered_at) as ids from db.orders");
    assert!(inputs(&ordered, "ids").contains(&(
        col("orders", "ordered_at"),
        EdgeKind::Indirect(IndirectKind::Sort)
    )));

    let windowed = analyze(
        "select sum(amount) over w * 2 as s from db.orders window w as (partition by customer_id)",
    );
    assert!(inputs(&windowed, "s").contains(&(
        col("orders", "customer_id"),
        EdgeKind::Indirect(IndirectKind::Window)
    )));
}

#[test]
fn review_unresolvable_references_make_the_query_opaque_but_keywords_do_not() {
    let unknown = analyze("select no_such_column from db.orders");
    assert!(unknown.opaque);
    assert_eq!(unknown.relations_read, [rel("orders")].into());
    let keyword = analyze("select id, current_date as today from db.orders");
    assert!(!keyword.opaque, "{:?}", keyword.diagnostics);
}

#[test]
fn review_wildcards_inside_ctes_and_distinct_on_are_recorded() {
    let cte = analyze("with c as (select distinct * from db.orders) select count(*) as n from c");
    assert_eq!(cte.wildcard_relations, [rel("orders")].into());
    assert!(
        cte.row_inputs
            .contains(&(col("orders", "status"), IndirectKind::GroupBy))
    );

    let on = analyze_with(
        SqlDialect::Postgres,
        "select distinct on (customer_id) id from db.orders order by customer_id, ordered_at",
    );
    assert!(
        on.row_inputs
            .contains(&(col("orders", "customer_id"), IndirectKind::GroupBy))
    );
}

#[test]
fn review_tables_with_unknown_columns_do_not_capture_outer_references() {
    let l = analyze(
        "select id from db.orders o
         where exists (select 1 from db.nocatalog n where n.id = o.id and n.status = status)",
    );
    // The bare `status` belongs to `orders` (known), not the unknown inner table.
    assert!(
        l.row_inputs
            .contains(&(col("orders", "status"), IndirectKind::Filter)),
        "{:?}",
        l.row_inputs
    );
}

#[test]
fn review_opaque_results_keep_tables_shadowed_by_cte_names() {
    let l = analyze("with orders as (select * from orders) select * from orders");
    assert!(l.opaque);
    let orders = RelationName::new(["orders"]).unwrap();
    assert!(l.relations_read.contains(&orders));
}

fn joins(lineage: &QueryLineage) -> Vec<(Vec<ColumnRef>, Vec<ColumnRef>)> {
    lineage
        .join_keys
        .iter()
        .map(|j| (j.left.clone(), j.right.clone()))
        .collect()
}

#[test]
fn join_keys_are_traced_to_physical_columns() {
    let lineage = analyze(
        "with paid as (select order_id, sum(cents) as cents from db.payments group by order_id)
         select o.id, c.name, p.cents
         from db.orders o
         join db.customers as c on o.customer_id = c.id
         left join paid p on (p.order_id = o.id)",
    );
    assert_eq!(
        joins(&lineage),
        [
            (
                vec![col("customers", "id")],
                vec![col("orders", "customer_id")]
            ),
            (vec![col("orders", "id")], vec![col("payments", "order_id")]),
        ],
        "canonical order, through the CTE's group-by key"
    );
}

#[test]
fn composite_and_using_joins_and_what_is_not_a_key() {
    let composite = analyze(
        "select 1 from db.orders o join db.payments p
         on o.id = p.order_id and cast(o.status as string) = p.method and o.amount > 0",
    );
    assert_eq!(
        joins(&composite),
        [(
            vec![col("orders", "id"), col("orders", "status")],
            vec![col("payments", "order_id"), col("payments", "method")]
        )]
    );
    let using = analyze("select 1 from db.orders join db.payments using (id)");
    assert_eq!(
        joins(&using),
        [(vec![col("orders", "id")], vec![col("payments", "id")])]
    );
    // Transformed keys and self-joins say nothing about keys.
    let transformed = analyze(
        "select 1 from db.orders o join db.customers c on lower(o.status) = c.tier
         join db.orders o2 on o2.id = o.id",
    );
    assert!(
        transformed.join_keys.is_empty(),
        "{:?}",
        transformed.join_keys
    );
}
