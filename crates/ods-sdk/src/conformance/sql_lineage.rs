//! Conformance suite for [`SqlLineageAnalyzer`] (#99, ADR-0006 §5).
//!
//! The suite's queries are fixed ([`IDENTITY_SQL`], [`UNPARSEABLE_SQL`]) and read one
//! relation, [`RELATION`], whose columns the suite supplies through a [`MapSchema`]: no
//! warehouse, no network. A scripted analyzer (like `ods-provider-fake`'s) scripts
//! [`IDENTITY_SQL`]'s lineage with [`identity_lineage`]; a real one parses it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};

use super::Report;
use crate::contracts::sql_lineage::{
    AnalyzeRequest, MapSchema, OutputColumn, QueryLineage, SqlLineageAnalyzer,
};

/// The relation the suite's queries read, as SQL writes it.
pub const RELATION: &str = "db.orders";

/// Reads one column unchanged: `id` is the identity of `db.orders.id`.
pub const IDENTITY_SQL: &str = "select id from db.orders";

/// Not SQL in any dialect: an analyzer must say it is opaque, not fail.
pub const UNPARSEABLE_SQL: &str = "select from where ((( orders";

/// What the suite tests against.
pub trait SqlLineageHarness: Send + Sync {
    /// The analyzer under test.
    fn analyzer(&self) -> Arc<dyn SqlLineageAnalyzer>;
}

/// The lineage [`IDENTITY_SQL`] has, for an analyzer that normalizes names with
/// `relation` and `column`: for a scripted analyzer to return.
pub fn identity_lineage(relation: RelationName, column: &str) -> QueryLineage {
    let input = ColumnRef::new(relation.clone(), column);
    QueryLineage::new(
        vec![OutputColumn::new(
            column,
            BTreeSet::from([(input, EdgeKind::Direct(DirectKind::Identity))]),
            "identity",
            Confidence::Exact,
        )],
        BTreeSet::new(),
        BTreeSet::from([relation]),
        "rows",
        Vec::new(),
    )
}

/// The suite's schema: `db.orders (id, amount)`, named as `analyzer` normalizes names.
fn schema(analyzer: &dyn SqlLineageAnalyzer) -> MapSchema {
    let relation = analyzer
        .relation_name(RELATION)
        .expect("relation_name: the suite's relation parses");
    MapSchema(BTreeMap::from([(
        relation,
        vec![analyzer.column_name("id"), analyzer.column_name("amount")],
    )]))
}

fn analyze(analyzer: &dyn SqlLineageAnalyzer, sql: &str) -> QueryLineage {
    let schema = schema(analyzer);
    analyzer
        .analyze(&AnalyzeRequest {
            sql,
            schema: &schema,
        })
        .unwrap_or_else(|e| panic!("analyze: `{sql}` failed: {e}"))
}

fn the_version_is_stable_and_named(analyzer: &dyn SqlLineageAnalyzer) {
    let version = analyzer.analyzer_version();
    assert!(
        !version.trim().is_empty(),
        "analyzer_version: names the analyzer's behaviour"
    );
    assert_eq!(
        analyzer.analyzer_version(),
        version,
        "analyzer_version: the same on every call (it keys caches)"
    );
}

fn names_normalize_once(analyzer: &dyn SqlLineageAnalyzer) {
    for name in ["id", "Amount", "customer_id"] {
        let once = analyzer.column_name(name);
        assert!(!once.is_empty(), "column_name: `{name}` keeps a name");
        assert_eq!(
            analyzer.column_name(&once),
            once,
            "column_name: normalizing `{name}` twice changes nothing"
        );
    }
    let relation = analyzer
        .relation_name(RELATION)
        .expect("relation_name: a qualified name parses");
    assert_eq!(
        analyzer.relation_name(RELATION).ok(),
        Some(relation),
        "relation_name: the same name each time"
    );
    assert!(
        analyzer.relation_name("").is_err(),
        "relation_name: an empty name is an error, not a relation"
    );
}

fn unparseable_sql_is_opaque_not_an_error(analyzer: &dyn SqlLineageAnalyzer) {
    let lineage = analyze(analyzer, UNPARSEABLE_SQL);
    assert!(
        lineage.opaque,
        "analyze: SQL that can't be parsed is opaque"
    );
    assert_eq!(
        lineage.confidence,
        Confidence::Unknown,
        "analyze: an opaque result claims nothing (rule 3)"
    );
    assert!(
        lineage.outputs.is_empty(),
        "analyze: an opaque result names no outputs"
    );
    assert!(
        !lineage.diagnostics.is_empty(),
        "analyze: an opaque result says why"
    );
}

fn a_selected_column_is_its_identity(analyzer: &dyn SqlLineageAnalyzer) {
    let lineage = analyze(analyzer, IDENTITY_SQL);
    assert!(!lineage.opaque, "analyze: `{IDENTITY_SQL}` is analyzed");
    let relation = analyzer.relation_name(RELATION).unwrap();
    let id = analyzer.column_name("id");
    assert!(
        lineage.relations_read.contains(&relation),
        "analyze: the relation read is listed: {lineage:?}"
    );
    let output = lineage
        .output(&id)
        .unwrap_or_else(|| panic!("analyze: the output `{id}` is listed: {lineage:?}"));
    // Exactly that input: an extra one (`amount`) would be a false dependency in
    // lineage and impact.
    assert_eq!(
        output.inputs,
        BTreeSet::from([(
            ColumnRef::new(relation, id.clone()),
            EdgeKind::Direct(DirectKind::Identity)
        )]),
        "analyze: `{id}` is the identity of its one input, and depends on nothing else"
    );
}

fn the_same_query_gives_the_same_lineage(analyzer: &dyn SqlLineageAnalyzer) {
    for sql in [IDENTITY_SQL, UNPARSEABLE_SQL] {
        assert_eq!(
            analyze(analyzer, sql),
            analyze(analyzer, sql),
            "analyze: `{sql}` gives the same lineage every time (it is cached by version)"
        );
    }
}

/// Runs every case against the analyzer `harness` gives.
///
/// # Panics
/// Panics with the case name when the analyzer breaks the contract.
pub fn run(harness: &dyn SqlLineageHarness) -> Report {
    let mut report = Report::default();
    let analyzer = harness.analyzer();
    let analyzer = analyzer.as_ref();
    the_version_is_stable_and_named(analyzer);
    report.passed.push("the_version_is_stable_and_named");
    names_normalize_once(analyzer);
    report.passed.push("names_normalize_once");
    unparseable_sql_is_opaque_not_an_error(analyzer);
    report.passed.push("unparseable_sql_is_opaque_not_an_error");
    a_selected_column_is_its_identity(analyzer);
    report.passed.push("a_selected_column_is_its_identity");
    the_same_query_gives_the_same_lineage(analyzer);
    report.passed.push("the_same_query_gives_the_same_lineage");
    report
}
