//! Whether SQL is one read-only query (ADR-0030 §4a): what a health probe may send to a
//! warehouse. Only a single query statement passes: a `SELECT`, `WITH … SELECT` or a set
//! operation of them. Anything that could write, lock or change a session is refused,
//! wherever it is nested, and so is anything that doesn't parse.

use std::ops::ControlFlow;

use sqlparser::ast::{Query, SetExpr, Statement, Visit, Visitor};

use crate::SqlDialect;

/// Checks that `sql` is exactly one read-only query in `dialect`; `Err` says why not.
///
/// # Errors
/// `sql` doesn't parse, isn't exactly one statement, isn't a query, or a query in it
/// writes (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, `SELECT … INTO`) or locks rows.
pub fn read_only_query(dialect: SqlDialect, sql: &str) -> Result<(), String> {
    let statements = dialect
        .parse(sql)
        .map_err(|e| format!("it doesn't parse as {} SQL: {e}", dialect.name()))?;
    let statement = match statements.as_slice() {
        [statement] => statement,
        [] => return Err("it has no statement".to_owned()),
        many => {
            return Err(format!(
                "it is {} statements; a probe is exactly one query",
                many.len()
            ));
        }
    };
    if !matches!(statement, Statement::Query(_)) {
        return Err(format!(
            "it is a {} statement, not a query",
            keyword(statement)
        ));
    }
    let mut check = Check(None);
    let _ = statement.visit(&mut check);
    check.0.map_or(Ok(()), Err)
}

/// Walks every statement and query in the tree, nested ones included, and stops at the
/// first that isn't read-only.
struct Check(Option<String>);

impl Visitor for Check {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        if matches!(statement, Statement::Query(_)) {
            ControlFlow::Continue(())
        } else {
            self.0 = Some(format!("it contains a {} statement", keyword(statement)));
            ControlFlow::Break(())
        }
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if !query.locks.is_empty() {
            self.0 = Some("it locks rows (`FOR UPDATE`/`FOR SHARE`)".to_owned());
            return ControlFlow::Break(());
        }
        match writes(&query.body) {
            Some(why) => {
                self.0 = Some(why);
                ControlFlow::Break(())
            }
            None => ControlFlow::Continue(()),
        }
    }
}

/// Why a query body writes, if it does: data-modifying CTEs and set operations
/// included.
fn writes(body: &SetExpr) -> Option<String> {
    match body {
        SetExpr::Select(select) if select.into.is_some() => {
            Some("it writes a table (`SELECT … INTO`)".to_owned())
        }
        SetExpr::Insert(statement)
        | SetExpr::Update(statement)
        | SetExpr::Delete(statement)
        | SetExpr::Merge(statement) => {
            Some(format!("it contains a {} statement", keyword(statement)))
        }
        SetExpr::SetOperation { left, right, .. } => writes(left).or_else(|| writes(right)),
        SetExpr::Query(query) => writes(&query.body),
        _ => None,
    }
}

/// The statement's first keyword, e.g. `DELETE`, for messages.
fn keyword(statement: &Statement) -> String {
    statement
        .to_string()
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(sql: &str) -> Result<(), String> {
        read_only_query(SqlDialect::Databricks, sql)
    }

    #[test]
    fn queries_pass() {
        for sql in [
            "select count(*) as n from shop.sales.orders",
            "SELECT max(_loaded_at) AS at FROM t WHERE x > 1",
            "with recent as (select * from t where d > current_date - 7) select count(*) as n from recent",
            "select a from t union all select a from u",
            "select (select max(x) from u) as m from t",
        ] {
            assert_eq!(check(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn anything_that_could_write_or_change_a_session_is_refused() {
        for (sql, why) in [
            ("delete from t", "DELETE statement"),
            ("insert into t select * from u", "INSERT statement"),
            ("update t set x = 1", "UPDATE statement"),
            (
                "merge into t using u on t.id = u.id when matched then delete",
                "MERGE statement",
            ),
            ("drop table t", "DROP statement"),
            ("create table x as select * from t", "CREATE statement"),
            ("truncate table t", "TRUNCATE statement"),
            ("set spark.sql.ansi.enabled = true", "SET statement"),
            ("use catalog main", "USE statement"),
            ("select 1; delete from t", "2 statements"),
            ("select * into backup from t", "SELECT … INTO"),
            ("select * from t for update", "locks rows"),
            ("", "no statement"),
            ("select (", "doesn't parse"),
        ] {
            let error = check(sql).expect_err(sql);
            assert!(error.contains(why), "{sql}: {error}");
        }
    }

    #[test]
    fn a_write_hidden_in_a_cte_is_refused() {
        let error = read_only_query(
            SqlDialect::Postgres,
            "with gone as (delete from t returning *) select count(*) from gone",
        )
        .unwrap_err();
        assert!(error.contains("DELETE"), "{error}");
    }
}
