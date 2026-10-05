//! Property tests for the SQL analyzer (#192): whatever SQL it is given, in any dialect,
//! it never panics, gives the same answer every time, and never claims more than it
//! knows (rule 3).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::{Confidence, RelationName};
use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};
use ods_sdk::contracts::sql_lineage::{
    AnalyzeRequest, MapSchema, QueryLineage, SqlLineageAnalyzer,
};
use proptest::prelude::*;

const TABLES: [(&str, &[&str]); 2] = [
    ("orders", &["id", "customer_id", "status", "amount"]),
    ("customers", &["id", "name", "tier"]),
];

fn schema(analyzer: &SqlparserAnalyzer) -> MapSchema {
    MapSchema(
        TABLES
            .iter()
            .map(|(table, columns)| {
                (
                    RelationName::new(["db", *table]).unwrap(),
                    columns.iter().map(|c| analyzer.column_name(c)).collect(),
                )
            })
            .collect(),
    )
}

fn analyze(dialect: SqlDialect, sql: &str) -> QueryLineage {
    let analyzer = SqlparserAnalyzer::new(dialect);
    analyzer
        .analyze(&AnalyzeRequest {
            sql,
            schema: &schema(&analyzer),
        })
        .unwrap_or_else(|e| panic!("analyze is infallible here, got {e} for {sql:?}"))
}

/// What every result must hold, whatever the SQL.
fn check(lineage: &QueryLineage, sql: &str) -> Result<(), TestCaseError> {
    if lineage.opaque {
        prop_assert_eq!(lineage.confidence, Confidence::Unknown, "{:?}", sql);
        prop_assert!(lineage.outputs.is_empty(), "{:?}", sql);
        prop_assert!(
            !lineage.diagnostics.is_empty(),
            "an opaque result says why: {:?}",
            sql
        );
    } else {
        let lowest = lineage.outputs.iter().map(|o| o.confidence).min();
        if let Some(lowest) = lowest {
            prop_assert!(lineage.confidence <= lowest, "{:?}", sql);
        }
    }
    let known: BTreeMap<RelationName, BTreeSet<&str>> = TABLES
        .iter()
        .map(|(t, cols)| {
            (
                RelationName::new(["db", *t]).unwrap(),
                cols.iter().copied().collect(),
            )
        })
        .collect();
    for output in &lineage.outputs {
        for (input, _) in &output.inputs {
            // An input names a column of a relation it read, and, where the schema
            // knows that relation, a column it has.
            prop_assert!(
                lineage.relations_read.contains(&input.relation),
                "{} isn't among the relations read in {:?}",
                input,
                sql
            );
            if let Some(columns) = known.get(&input.relation) {
                prop_assert!(
                    columns.contains(input.column.as_str()),
                    "{} isn't a column of {} in {:?}",
                    input.column,
                    input.relation,
                    sql
                );
            }
        }
    }
    Ok(())
}

/// SQL built from the constructs the analyzer handles, so generated cases reach past
/// the parser into the analysis.
fn query() -> impl Strategy<Value = String> {
    let column = prop::sample::select(vec![
        "id",
        "customer_id",
        "status",
        "amount",
        "name",
        "tier",
        "missing",
        "*",
    ]);
    let expr = (column.clone(), 0_usize..6).prop_map(|(c, form)| match (c, form) {
        ("*", _) => "*".to_owned(),
        (c, 0) => c.to_owned(),
        (c, 1) => format!("o.{c}"),
        (c, 2) => format!("sum({c}) as total_{c}"),
        (c, 3) => format!("case when {c} > 1 then {c} end as x_{c}"),
        (c, 4) => format!("coalesce({c}, 0) + 1 as y_{c}"),
        (c, _) => format!("count(distinct {c})"),
    });
    let from = prop::sample::select(vec![
        "db.orders o",
        "db.orders o join db.customers c on o.customer_id = c.id",
        "db.orders o left join db.customers c using (id)",
        "(select id, amount from db.orders) o",
        "db.unknown o",
    ]);
    let tail = prop::sample::select(vec![
        "",
        " where status = 'x'",
        " group by 1",
        " order by 1 limit 10",
        " qualify row_number() over (partition by id order by amount) = 1",
    ]);
    let wrap = prop::sample::select(vec![
        "{}",
        "with q as ({}) select * from q",
        "{} union all {}",
    ]);
    (prop::collection::vec(expr, 1..4), from, tail, wrap).prop_map(|(exprs, from, tail, wrap)| {
        let select = format!("select {} from {from}{tail}", exprs.join(", "));
        wrap.replace("{}", &select)
    })
}

proptest! {
    #[test]
    fn any_text_is_analyzed_without_panicking(
        sql in any::<String>(),
        dialect in prop::sample::select(SqlDialect::ALL.to_vec()),
    ) {
        let lineage = analyze(dialect, &sql);
        check(&lineage, &sql)?;
        prop_assert_eq!(analyze(dialect, &sql), lineage);
    }

    #[test]
    fn generated_queries_hold_up(
        sql in query(),
        dialect in prop::sample::select(SqlDialect::ALL.to_vec()),
    ) {
        let lineage = analyze(dialect, &sql);
        check(&lineage, &sql)?;
        prop_assert_eq!(analyze(dialect, &sql), lineage);
    }

    #[test]
    fn a_cut_or_mangled_query_never_panics(
        sql in query(),
        cut in any::<prop::sample::Index>(),
        junk in "[()',;. a-z0-9*]{0,6}",
        dialect in prop::sample::select(SqlDialect::ALL.to_vec()),
    ) {
        let chars: Vec<char> = sql.chars().collect();
        let at = cut.index(chars.len() + 1);
        let mangled: String = chars[..at].iter().copied().chain(junk.chars()).chain(chars[at..].iter().copied()).collect();
        check(&analyze(dialect, &mangled), &mangled)?;
        let truncated: String = chars[..at].iter().collect();
        check(&analyze(dialect, &truncated), &truncated)?;
    }

    #[test]
    fn names_never_panic_and_normalize_once(
        name in any::<String>(),
        dialect in prop::sample::select(SqlDialect::ALL.to_vec()),
    ) {
        let analyzer = SqlparserAnalyzer::new(dialect);
        let once = analyzer.column_name(&name);
        prop_assert_eq!(analyzer.column_name(&once), once.clone(), "{:?}", name);
        if let Ok(relation) = analyzer.relation_name(&name) {
            prop_assert!(relation.parts().iter().all(|p| !p.is_empty()));
            prop_assert_eq!(analyzer.relation_name(&name).ok(), Some(relation));
        }
    }
}

/// The generator is worth something only if most of what it makes is analyzed, not
/// opaque: otherwise `generated_queries_hold_up` checks nothing.
#[test]
fn most_generated_queries_are_analyzed() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = query();
    let total = 200;
    let analyzed = (0..total)
        .filter(|_| {
            let sql = strategy.new_tree(&mut runner).unwrap().current();
            !analyze(SqlDialect::Databricks, &sql).opaque
        })
        .count();
    assert!(analyzed * 2 > total, "only {analyzed} of {total} analyzed");
}
