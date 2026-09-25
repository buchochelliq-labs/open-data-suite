//! Scope-based column lineage over the sqlparser AST.
//!
//! Every query level becomes a [`Scope`] of named sources. Each source is either a
//! physical table (its columns come from the upstream schema) or a derived query (a CTE,
//! subquery or lateral view) that was analyzed first into a [`QueryOut`]. Column
//! references resolve against the scope, then outward for correlated subqueries, and
//! references to derived columns are replaced by those columns' own inputs, so every
//! edge in the result points at a physical column.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::rc::Rc;

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage, SchemaLookup};
use sha2::{Digest, Sha256};
use sqlparser::ast::{
    Distinct, Expr, FunctionArg, FunctionArgExpr, FunctionArgumentClause, FunctionArguments,
    GroupByExpr, Ident, JoinConstraint, JoinOperator, NamedWindowExpr, OrderByKind, Query, Select,
    SelectItem, SelectItemQualifiedWildcardKind, SetExpr, SetQuantifier, Statement, TableFactor,
    TableWithJoins, WildcardAdditionalOptions, WindowSpec, WindowType, visit_expressions,
    visit_expressions_mut,
};

use crate::dialect::SqlDialect;

type Edges = BTreeSet<(ColumnRef, EdgeKind)>;
type Rows = BTreeSet<(ColumnRef, IndirectKind)>;

/// Stops analysis: the query can't be analyzed safely.
#[derive(Debug)]
struct Opaque(String);

type Result<T> = std::result::Result<T, Opaque>;

/// One analyzed output column.
#[derive(Debug, Clone)]
struct Col {
    name: String,
    inputs: Edges,
    digest: String,
    confidence: Confidence,
}

/// An analyzed query (or query level).
#[derive(Debug, Clone, Default)]
struct QueryOut {
    outputs: Vec<Col>,
    rows: Rows,
    reads: BTreeSet<RelationName>,
    wildcards: BTreeSet<RelationName>,
    row_digest: String,
    diagnostics: Vec<String>,
}

impl QueryOut {
    fn output(&self, name: &str) -> Option<&Col> {
        self.outputs.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone)]
enum SourceKind {
    Table {
        relation: RelationName,
        columns: Option<Vec<String>>,
    },
    Derived(Rc<QueryOut>),
}

#[derive(Debug, Clone)]
struct Source {
    alias: String,
    kind: SourceKind,
}

/// Names visible at one query level.
struct Scope<'a> {
    sources: Vec<Source>,
    ctes: BTreeMap<String, Rc<QueryOut>>,
    windows: BTreeMap<String, WindowSpec>,
    /// Outputs defined so far, for lateral column aliases (`select a + 1 as b, b * 2`).
    aliases: Vec<Col>,
    /// The named windows as written, part of every column digest at this level so a
    /// changed `window w as (…)` changes the columns that use it.
    windows_text: String,
    outer: Option<&'a Scope<'a>>,
}

impl<'a> Scope<'a> {
    fn child(ctes: BTreeMap<String, Rc<QueryOut>>, outer: Option<&'a Scope<'a>>) -> Self {
        Self {
            sources: Vec::new(),
            ctes,
            windows: BTreeMap::new(),
            aliases: Vec::new(),
            windows_text: String::new(),
            outer,
        }
    }
}

/// What a column reference resolved to.
struct Resolved {
    /// Physical input columns, with the edge kind of the path through derived sources.
    edges: Edges,
    /// A stable token for digests: the physical column, or the derived column's digest.
    token: String,
    confidence: Confidence,
}

/// How a column reference is being used where it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    Value(DirectKind),
    Indirect(IndirectKind),
}

impl Use {
    /// Entering a function or operator: a bare value becomes a transformation.
    fn transformed(self) -> Self {
        match self {
            Use::Value(DirectKind::Identity) => Use::Value(DirectKind::Transformation),
            other => other,
        }
    }

    fn aggregated(self) -> Self {
        match self {
            Use::Value(_) => Use::Value(DirectKind::Aggregation),
            indirect @ Use::Indirect(_) => indirect,
        }
    }
}

/// Combines the kind at the point of use with the kind along a derived column's path.
///
/// Where the reference is used indirectly (e.g. in a filter), that use decides the kind;
/// otherwise an indirect step on the path (e.g. a CASE condition inside a CTE) does.
fn compose(outer: Use, inner: EdgeKind) -> EdgeKind {
    match (outer, inner) {
        (Use::Indirect(k), _) | (Use::Value(_), EdgeKind::Indirect(k)) => EdgeKind::Indirect(k),
        (Use::Value(a), EdgeKind::Direct(b)) => EdgeKind::Direct(a.max(b)),
    }
}

/// Everything collected while walking one expression.
#[derive(Default)]
struct Acc {
    edges: Edges,
    /// Row-shaping inputs found inside subqueries of the expression.
    rows: Rows,
    reads: BTreeSet<RelationName>,
    confidence: Option<Confidence>,
    diagnostics: Vec<String>,
    /// Lambda parameters currently in scope; they are not columns.
    shadowed: Vec<String>,
}

impl Acc {
    fn lower(&mut self, confidence: Confidence) {
        self.confidence = Some(self.confidence.map_or(confidence, |c| c.min(confidence)));
    }
}

struct Analyzer<'s> {
    dialect: SqlDialect,
    schema: &'s dyn SchemaLookup,
}

/// Niladic keywords some dialects parse as identifiers.
const KEYWORDS: &[&str] = &[
    "current_date",
    "current_time",
    "current_timestamp",
    "localtime",
    "localtimestamp",
    "current_user",
    "session_user",
    "user",
    "current_role",
    "current_catalog",
    "current_schema",
    "current_database",
    "sysdate",
    "systimestamp",
    "null",
    "true",
    "false",
    "default",
];

const AGGREGATES: &[&str] = &[
    "sum",
    "count",
    "min",
    "max",
    "avg",
    "mean",
    "median",
    "any_value",
    "first",
    "last",
    "first_value",
    "last_value",
    "array_agg",
    "collect_list",
    "collect_set",
    "string_agg",
    "listagg",
    "group_concat",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "variance",
    "var_pop",
    "var_samp",
    "bool_and",
    "bool_or",
    "every",
    "count_if",
    "countif",
    "approx_count_distinct",
    "approx_distinct",
    "percentile",
    "percentile_cont",
    "percentile_disc",
    "approx_percentile",
    "max_by",
    "min_by",
    "arg_max",
    "arg_min",
    "corr",
    "covar_pop",
    "covar_samp",
    "bit_and",
    "bit_or",
    "bit_xor",
    "hll_sketch_agg",
];

/// Analyzes one SQL text into lineage; never fails, returning an opaque result instead.
pub(crate) fn analyze(dialect: SqlDialect, sql: &str, schema: &dyn SchemaLookup) -> QueryLineage {
    let statements = match dialect.parse(sql) {
        Ok(statements) => statements,
        Err(error) => {
            return QueryLineage::opaque(BTreeSet::new(), format!("SQL did not parse: {error}"));
        }
    };
    let query = match statements.as_slice() {
        [Statement::Query(query)] => query,
        [_] => {
            return QueryLineage::opaque(
                BTreeSet::new(),
                "only a single SELECT query can be analyzed",
            );
        }
        _ => {
            return QueryLineage::opaque(BTreeSet::new(), "expected exactly one SQL statement");
        }
    };
    let analyzer = Analyzer { dialect, schema };
    match analyzer.query(query, &BTreeMap::new(), None) {
        Ok(out) => {
            let outputs = out
                .outputs
                .iter()
                .map(|c| OutputColumn::new(&c.name, c.inputs.clone(), &c.digest, c.confidence))
                .collect();
            QueryLineage::new(
                outputs,
                out.rows,
                out.reads,
                out.row_digest,
                out.diagnostics,
            )
            .with_wildcards(out.wildcards)
        }
        Err(Opaque(reason)) => {
            let reads = analyzer.relations_in(query);
            QueryLineage::opaque(reads, reason)
        }
    }
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl Analyzer<'_> {
    /// Every physical relation named anywhere in the query (for opaque results).
    fn relations_in(&self, query: &Query) -> BTreeSet<RelationName> {
        // Every name is kept, CTE names included: `with orders as (select * from orders)`
        // reads the physical `orders`, and an extra name only ever over-reports.
        let mut reads = BTreeSet::new();
        let _ = sqlparser::ast::visit_relations(query, |name| {
            if let Ok(relation) = RelationName::new(self.dialect.object_name(name)) {
                reads.insert(relation);
            }
            ControlFlow::<()>::Continue(())
        });
        reads
    }

    fn query(
        &self,
        query: &Query,
        inherited: &BTreeMap<String, Rc<QueryOut>>,
        outer: Option<&Scope<'_>>,
    ) -> Result<QueryOut> {
        if !query.pipe_operators.is_empty() {
            return Err(Opaque("pipe syntax is not supported".into()));
        }
        let mut ctes = inherited.clone();
        if let Some(with) = &query.with {
            if with.recursive {
                return Err(Opaque("recursive CTEs are not supported".into()));
            }
            for cte in &with.cte_tables {
                let mut out = self.query(&cte.query, &ctes, outer)?;
                rename(
                    &mut out,
                    &cte.alias
                        .columns
                        .iter()
                        .map(|c| self.dialect.ident(&c.name))
                        .collect::<Vec<_>>(),
                )?;
                ctes.insert(self.dialect.ident(&cte.alias.name), Rc::new(out));
            }
        }
        let mut out = self.set_expr(&query.body, &ctes, outer)?;

        // ORDER BY only shapes the rows when a LIMIT/FETCH keeps some of them.
        let limited = query.limit_clause.is_some() || query.fetch.is_some();
        let mut row_parts = vec![out.row_digest.clone()];
        if let Some(limit) = &query.limit_clause {
            row_parts.push(limit.to_string());
        }
        if let Some(fetch) = &query.fetch {
            row_parts.push(fetch.to_string());
        }
        if limited && let Some(order_by) = &query.order_by {
            match &order_by.kind {
                OrderByKind::All(_) => {
                    let all: Vec<(ColumnRef, IndirectKind)> = out
                        .outputs
                        .iter()
                        .flat_map(|c| {
                            c.inputs
                                .iter()
                                .map(|(r, _)| (r.clone(), IndirectKind::Sort))
                        })
                        .collect();
                    out.rows.extend(all);
                    row_parts.push(format!("order by all {order_by}"));
                }
                OrderByKind::Expressions(exprs) => {
                    for order in exprs {
                        // Output aliases first (`order by amount`), then the sources.
                        if let Some(col) = self.output_alias(&order.expr, &out.outputs) {
                            let sorted: Vec<_> = col
                                .inputs
                                .iter()
                                .map(|(r, _)| (r.clone(), IndirectKind::Sort))
                                .collect();
                            row_parts.push(format!("{} {}", col.digest, order.options));
                            out.rows.extend(sorted);
                        } else {
                            row_parts.push(order.to_string());
                            out.diagnostics.push(format!(
                                "ORDER BY `{}` over a set operation or subquery was not resolved; \
                                 the sort is recorded by text only",
                                order.expr
                            ));
                        }
                    }
                }
            }
        }
        let parts: Vec<&str> = row_parts.iter().map(String::as_str).collect();
        out.row_digest = digest(&parts);
        Ok(out)
    }

    fn output_alias<'c>(&self, expr: &Expr, outputs: &'c [Col]) -> Option<&'c Col> {
        match expr {
            Expr::Identifier(ident) => {
                let name = self.dialect.ident(ident);
                outputs.iter().find(|c| c.name == name)
            }
            Expr::Value(value) => {
                let position: usize = value.to_string().parse().ok()?;
                outputs.get(position.checked_sub(1)?)
            }
            _ => None,
        }
    }

    fn set_expr(
        &self,
        body: &SetExpr,
        ctes: &BTreeMap<String, Rc<QueryOut>>,
        outer: Option<&Scope<'_>>,
    ) -> Result<QueryOut> {
        match body {
            SetExpr::Select(select) => self.select(select, ctes, outer),
            SetExpr::Query(query) => self.query(query, ctes, outer),
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                let left = self.set_expr(left, ctes, outer)?;
                let right = self.set_expr(right, ctes, outer)?;
                if left.outputs.len() != right.outputs.len() {
                    return Err(Opaque(format!(
                        "{op} branches have {} and {} columns",
                        left.outputs.len(),
                        right.outputs.len()
                    )));
                }
                let distinct = !matches!(
                    set_quantifier,
                    SetQuantifier::All | SetQuantifier::AllByName
                );
                let mut out = QueryOut {
                    rows: &left.rows | &right.rows,
                    reads: &left.reads | &right.reads,
                    wildcards: &left.wildcards | &right.wildcards,
                    diagnostics: [left.diagnostics.clone(), right.diagnostics.clone()].concat(),
                    ..QueryOut::default()
                };
                // Columns match by position; names come from the first branch.
                for (l, r) in left.outputs.iter().zip(&right.outputs) {
                    out.outputs.push(Col {
                        name: l.name.clone(),
                        inputs: &l.inputs | &r.inputs,
                        digest: digest(&[&l.digest, &op.to_string(), &r.digest]),
                        confidence: l.confidence.min(r.confidence),
                    });
                }
                if distinct {
                    // Deduplication (and EXCEPT/INTERSECT matching) depends on every column.
                    let all: Vec<_> = out
                        .outputs
                        .iter()
                        .flat_map(|c| {
                            c.inputs
                                .iter()
                                .map(|(r, _)| (r.clone(), IndirectKind::GroupBy))
                        })
                        .collect();
                    out.rows.extend(all);
                }
                out.row_digest = digest(&[
                    &left.row_digest,
                    &op.to_string(),
                    &set_quantifier.to_string(),
                    &right.row_digest,
                ]);
                Ok(out)
            }
            SetExpr::Values(values) => {
                let width = values.rows.first().map_or(0, |row| row.content.len());
                let outputs = (1..=width)
                    .map(|i| Col {
                        name: format!("column{i}"),
                        inputs: Edges::new(),
                        digest: digest(&["values", &i.to_string()]),
                        confidence: Confidence::Exact,
                    })
                    .collect();
                Ok(QueryOut {
                    outputs,
                    row_digest: digest(&["values", &values.to_string()]),
                    ..QueryOut::default()
                })
            }
            other => Err(Opaque(format!(
                "unsupported query body: {}",
                first_words(&other.to_string())
            ))),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the clauses of a SELECT, in SQL order"
    )]
    fn select(
        &self,
        select: &Select,
        ctes: &BTreeMap<String, Rc<QueryOut>>,
        outer: Option<&Scope<'_>>,
    ) -> Result<QueryOut> {
        let mut scope = Scope::child(ctes.clone(), outer);
        let mut out = QueryOut::default();
        let mut row_parts: Vec<String> = Vec::new();
        let mut rows = Rows::new();

        for named in &select.named_window {
            if let NamedWindowExpr::WindowSpec(spec) = &named.1 {
                scope
                    .windows
                    .insert(self.dialect.ident(&named.0), spec.clone());
            }
        }

        scope.windows_text = select
            .named_window
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        if !scope.windows_text.is_empty() {
            row_parts.push(format!("window {}", scope.windows_text));
        }

        // FROM and JOINs.
        for table in &select.from {
            self.table_with_joins(table, &mut scope, &mut out, &mut rows, &mut row_parts)?;
        }
        for lateral in &select.lateral_views {
            let mut acc = Acc::default();
            self.walk(
                &lateral.lateral_view,
                Use::Value(DirectKind::Transformation),
                &scope,
                &mut acc,
            )?;
            let alias = lateral
                .lateral_view_name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .map_or_else(String::new, |i| self.dialect.ident(i));
            let token = self.canon(&lateral.lateral_view, &scope);
            let outputs = lateral
                .lateral_col_alias
                .iter()
                .map(|name| Col {
                    name: self.dialect.ident(name),
                    inputs: acc.edges.clone(),
                    digest: digest(&["lateral", &token, &name.value]),
                    confidence: acc.confidence.unwrap_or(Confidence::Exact),
                })
                .collect();
            row_parts.push(format!("lateral view {} {token}", lateral.outer));
            out.reads.extend(acc.reads);
            scope.sources.push(Source {
                alias,
                kind: SourceKind::Derived(Rc::new(QueryOut {
                    outputs,
                    ..QueryOut::default()
                })),
            });
        }

        // Row filters.
        for (clause, expr) in [("prewhere", &select.prewhere), ("where", &select.selection)] {
            if let Some(expr) = expr {
                self.rows_from(expr, IndirectKind::Filter, &scope, &mut out, &mut rows)?;
                row_parts.push(format!("{clause} {}", self.canon(expr, &scope)));
            }
        }

        // Projection.
        let mut outputs: Vec<Col> = Vec::new();
        for item in &select.projection {
            for col in self.select_item(item, &scope, &mut out)? {
                scope.aliases.push(col.clone());
                outputs.push(col);
            }
        }

        // Grouping, then group filters.
        match &select.group_by {
            GroupByExpr::All(modifiers) => {
                row_parts.push(format!("group by all {}", modifiers_text(modifiers)));
                for col in &outputs {
                    if !col
                        .inputs
                        .iter()
                        .any(|(_, k)| *k == EdgeKind::Direct(DirectKind::Aggregation))
                    {
                        rows.extend(
                            col.inputs
                                .iter()
                                .map(|(r, _)| (r.clone(), IndirectKind::GroupBy)),
                        );
                    }
                }
            }
            GroupByExpr::Expressions(exprs, modifiers) => {
                row_parts.push(format!("group modifiers {}", modifiers_text(modifiers)));
                for expr in exprs {
                    if let Some(col) = self.group_ref(expr, &outputs, &scope) {
                        rows.extend(
                            col.inputs
                                .iter()
                                .map(|(r, _)| (r.clone(), IndirectKind::GroupBy)),
                        );
                        row_parts.push(format!("group {}", col.digest));
                    } else {
                        self.rows_from(expr, IndirectKind::GroupBy, &scope, &mut out, &mut rows)?;
                        row_parts.push(format!("group {}", self.canon(expr, &scope)));
                    }
                }
            }
        }
        for (clause, expr) in [("having", &select.having), ("qualify", &select.qualify)] {
            if let Some(expr) = expr {
                self.rows_from(expr, IndirectKind::Filter, &scope, &mut out, &mut rows)?;
                row_parts.push(format!("{clause} {}", self.canon(expr, &scope)));
            }
        }
        if let Some(Distinct::On(exprs)) = &select.distinct {
            for expr in exprs {
                self.rows_from(expr, IndirectKind::GroupBy, &scope, &mut out, &mut rows)?;
                row_parts.push(format!("distinct on {}", self.canon(expr, &scope)));
            }
        }
        if select.distinct.is_some() {
            row_parts.push("distinct".into());
            for col in &outputs {
                rows.extend(
                    col.inputs
                        .iter()
                        .map(|(r, _)| (r.clone(), IndirectKind::GroupBy)),
                );
            }
        }
        if let Some(top) = &select.top {
            row_parts.push(format!("top {top}"));
        }

        // Derived sources contribute their own row shaping and relations.
        for source in &scope.sources {
            if let SourceKind::Derived(derived) = &source.kind {
                rows.extend(derived.rows.iter().cloned());
                out.reads.extend(derived.reads.iter().cloned());
                out.diagnostics.extend(derived.diagnostics.iter().cloned());
                row_parts.push(format!("derived {} {}", source.alias, derived.row_digest));
            }
        }

        out.outputs = outputs;
        out.rows.extend(rows);
        let parts: Vec<&str> = row_parts.iter().map(String::as_str).collect();
        out.row_digest = digest(&parts);
        Ok(out)
    }

    /// `GROUP BY 1` or `GROUP BY alias` refer to projection items.
    fn group_ref<'c>(&self, expr: &Expr, outputs: &'c [Col], scope: &Scope<'_>) -> Option<&'c Col> {
        match expr {
            Expr::Value(_) => self.output_alias(expr, outputs),
            // A source column wins over an output alias of the same name.
            Expr::Identifier(ident)
                if self.resolve(std::slice::from_ref(ident), scope).is_none() =>
            {
                self.output_alias(expr, outputs)
            }
            _ => None,
        }
    }

    fn rows_from(
        &self,
        expr: &Expr,
        kind: IndirectKind,
        scope: &Scope<'_>,
        out: &mut QueryOut,
        rows: &mut Rows,
    ) -> Result<()> {
        let mut acc = Acc::default();
        self.walk(expr, Use::Indirect(kind), scope, &mut acc)?;
        rows.extend(acc.edges.into_iter().map(|(r, e)| match e {
            EdgeKind::Indirect(k) => (r, k),
            EdgeKind::Direct(_) => (r, kind),
        }));
        rows.extend(acc.rows);
        out.reads.extend(acc.reads);
        out.diagnostics.extend(acc.diagnostics);
        Ok(())
    }

    fn table_with_joins(
        &self,
        table: &TableWithJoins,
        scope: &mut Scope<'_>,
        out: &mut QueryOut,
        rows: &mut Rows,
        row_parts: &mut Vec<String>,
    ) -> Result<()> {
        self.table_factor(&table.relation, scope, out, row_parts)?;
        for join in &table.joins {
            self.table_factor(&join.relation, scope, out, row_parts)?;
            let (name, constraint) = join_parts(&join.join_operator)?;
            row_parts.push(name.to_owned());
            match constraint {
                Some(JoinConstraint::On(expr)) => {
                    self.rows_from(expr, IndirectKind::Join, scope, out, rows)?;
                    row_parts.push(format!("on {}", self.canon(expr, scope)));
                }
                Some(JoinConstraint::Using(names)) => {
                    for name in names {
                        let parts = self.dialect.object_name(name);
                        let column = parts.last().cloned().unwrap_or_default();
                        // USING matches the column on both sides.
                        for source in &scope.sources {
                            if let Some(resolved) = Self::source_column(source, &column) {
                                rows.extend(
                                    resolved
                                        .edges
                                        .into_iter()
                                        .map(|(r, _)| (r, IndirectKind::Join)),
                                );
                            }
                        }
                        row_parts.push(format!("using {column}"));
                    }
                }
                Some(JoinConstraint::Natural) => {
                    return Err(Opaque("NATURAL joins are not supported".into()));
                }
                Some(JoinConstraint::None) | None => {}
            }
        }
        Ok(())
    }

    fn table_factor(
        &self,
        factor: &TableFactor,
        scope: &mut Scope<'_>,
        out: &mut QueryOut,
        row_parts: &mut Vec<String>,
    ) -> Result<()> {
        match factor {
            TableFactor::Table {
                name, alias, args, ..
            } => {
                if args.is_some() {
                    return Err(Opaque(format!("table function `{name}` is not supported")));
                }
                let parts = self.dialect.object_name(name);
                let alias_name = alias.as_ref().map_or_else(
                    || parts.last().cloned().unwrap_or_default(),
                    |a| self.dialect.ident(&a.name),
                );
                let renames: Vec<String> = alias
                    .iter()
                    .flat_map(|a| &a.columns)
                    .map(|c| self.dialect.ident(&c.name))
                    .collect();
                if let [single] = parts.as_slice()
                    && let Some(cte) = scope.ctes.get(single)
                {
                    let mut derived = (**cte).clone();
                    rename(&mut derived, &renames)?;
                    row_parts.push(format!("cte {single}"));
                    out.wildcards.extend(derived.wildcards.iter().cloned());
                    scope.sources.push(Source {
                        alias: alias_name,
                        kind: SourceKind::Derived(Rc::new(derived)),
                    });
                    return Ok(());
                }
                let relation = RelationName::new(parts)
                    .map_err(|e| Opaque(format!("invalid table name `{name}`: {e}")))?;
                let columns = self.schema.columns(&relation);
                row_parts.push(format!("table {relation}"));
                out.reads.insert(relation.clone());
                if renames.is_empty() {
                    scope.sources.push(Source {
                        alias: alias_name,
                        kind: SourceKind::Table { relation, columns },
                    });
                } else {
                    let Some(columns) = columns else {
                        return Err(Opaque(format!(
                            "columns of `{relation}` are renamed by position but are unknown"
                        )));
                    };
                    let mut derived = table_as_query(&relation, &columns);
                    rename(&mut derived, &renames)?;
                    scope.sources.push(Source {
                        alias: alias_name,
                        kind: SourceKind::Derived(Rc::new(derived)),
                    });
                }
                Ok(())
            }
            TableFactor::Derived {
                lateral,
                subquery,
                alias,
                ..
            } => {
                // A LATERAL subquery sees the sources to its left.
                let mut derived = if *lateral {
                    self.query(subquery, &scope.ctes, Some(&*scope))?
                } else {
                    self.query(subquery, &scope.ctes, scope.outer)?
                };
                let alias_name = alias
                    .as_ref()
                    .map_or_else(String::new, |a| self.dialect.ident(&a.name));
                let renames: Vec<String> = alias
                    .iter()
                    .flat_map(|a| &a.columns)
                    .map(|c| self.dialect.ident(&c.name))
                    .collect();
                rename(&mut derived, &renames)?;
                out.wildcards.extend(derived.wildcards.iter().cloned());
                scope.sources.push(Source {
                    alias: alias_name,
                    kind: SourceKind::Derived(Rc::new(derived)),
                });
                Ok(())
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                let mut rows = Rows::new();
                self.table_with_joins(table_with_joins, scope, out, &mut rows, row_parts)?;
                out.rows.extend(rows);
                Ok(())
            }
            other => Err(Opaque(format!(
                "unsupported FROM source: {}",
                first_words(&other.to_string())
            ))),
        }
    }

    fn select_item(
        &self,
        item: &SelectItem,
        scope: &Scope<'_>,
        out: &mut QueryOut,
    ) -> Result<Vec<Col>> {
        match item {
            SelectItem::UnnamedExpr(expr) => {
                let name = match expr {
                    Expr::Identifier(ident) => self.dialect.ident(ident),
                    Expr::CompoundIdentifier(parts) => parts
                        .last()
                        .map_or_else(String::new, |i| self.dialect.ident(i)),
                    other => self.dialect.column_name(&other.to_string()),
                };
                Ok(vec![self.column(&name, expr, scope, out)?])
            }
            SelectItem::ExprWithAlias { expr, alias } => Ok(vec![self.column(
                &self.dialect.ident(alias),
                expr,
                scope,
                out,
            )?]),
            SelectItem::ExprWithAliases { .. } => {
                Err(Opaque("multi-alias select items are not supported".into()))
            }
            SelectItem::Wildcard(options) => {
                check_wildcard_options(options)?;
                let excluded = self.excluded(options);
                let mut cols = Vec::new();
                for source in &scope.sources {
                    cols.extend(Self::expand(source, &excluded, out)?);
                }
                Ok(cols)
            }
            SelectItem::QualifiedWildcard(kind, options) => {
                check_wildcard_options(options)?;
                let SelectItemQualifiedWildcardKind::ObjectName(name) = kind else {
                    return Err(Opaque(
                        "wildcards over expressions are not supported".into(),
                    ));
                };
                let parts = self.dialect.object_name(name);
                let source = Self::source_named(&parts, scope)
                    .ok_or_else(|| Opaque(format!("`{name}.*` names no source in scope")))?;
                let excluded = self.excluded(options);
                Self::expand(source, &excluded, out)
            }
        }
    }

    fn excluded(&self, options: &WildcardAdditionalOptions) -> BTreeSet<String> {
        let mut excluded = BTreeSet::new();
        if let Some(except) = &options.opt_except {
            excluded.insert(self.dialect.ident(&except.first_element));
            excluded.extend(
                except
                    .additional_elements
                    .iter()
                    .map(|i| self.dialect.ident(i)),
            );
        }
        if let Some(exclude) = &options.opt_exclude {
            let names = match exclude {
                sqlparser::ast::ExcludeSelectItem::Single(name) => vec![name],
                sqlparser::ast::ExcludeSelectItem::Multiple(names) => names.iter().collect(),
            };
            for name in names {
                if let Some(last) = self.dialect.object_name(name).pop() {
                    excluded.insert(last);
                }
            }
        }
        excluded
    }

    /// `*` over one source.
    fn expand(
        source: &Source,
        excluded: &BTreeSet<String>,
        out: &mut QueryOut,
    ) -> Result<Vec<Col>> {
        match &source.kind {
            SourceKind::Table { relation, columns } => {
                let Some(columns) = columns else {
                    return Err(Opaque(format!(
                        "`select *` over `{relation}`, whose columns are unknown"
                    )));
                };
                out.wildcards.insert(relation.clone());
                Ok(columns
                    .iter()
                    .filter(|c| !excluded.contains(*c))
                    .map(|c| {
                        let column = ColumnRef::new(relation.clone(), c.clone());
                        Col {
                            name: c.clone(),
                            digest: digest(&["column", &column.to_string()]),
                            inputs: [(column, EdgeKind::Direct(DirectKind::Identity))].into(),
                            confidence: Confidence::Exact,
                        }
                    })
                    .collect())
            }
            SourceKind::Derived(derived) => {
                out.wildcards.extend(derived.wildcards.iter().cloned());
                Ok(derived
                    .outputs
                    .iter()
                    .filter(|c| !excluded.contains(&c.name))
                    .cloned()
                    .collect())
            }
        }
    }

    fn column(
        &self,
        name: &str,
        expr: &Expr,
        scope: &Scope<'_>,
        out: &mut QueryOut,
    ) -> Result<Col> {
        let mut acc = Acc::default();
        self.walk(expr, Use::Value(DirectKind::Identity), scope, &mut acc)?;
        out.rows.extend(acc.rows);
        out.reads.extend(acc.reads);
        out.diagnostics.extend(acc.diagnostics);
        Ok(Col {
            name: name.to_owned(),
            digest: digest(&["expr", &self.canon(expr, scope), &scope.windows_text]),
            inputs: acc.edges,
            confidence: acc.confidence.unwrap_or(Confidence::Exact),
        })
    }

    /// Collects the columns `expr` reads, classified by how they are used.
    #[allow(clippy::too_many_lines, reason = "one arm per expression shape")]
    fn walk(&self, expr: &Expr, how: Use, scope: &Scope<'_>, acc: &mut Acc) -> Result<()> {
        match expr {
            Expr::Identifier(ident) => self.reference(std::slice::from_ref(ident), how, scope, acc),
            Expr::CompoundIdentifier(parts) => self.reference(parts, how, scope, acc),
            Expr::Nested(inner) => self.walk(inner, how, scope, acc),
            Expr::CompoundFieldAccess { root, .. } => {
                self.walk(root, how.transformed(), scope, acc)
            }
            Expr::JsonAccess { value, .. } => self.walk(value, how.transformed(), scope, acc),
            Expr::Function(function) => {
                let name = self
                    .dialect
                    .object_name(&function.name)
                    .last()
                    .cloned()
                    .unwrap_or_default()
                    .to_lowercase();
                let args_use = if function.over.is_some() {
                    how.transformed()
                } else if AGGREGATES.contains(&name.as_str()) {
                    how.aggregated()
                } else {
                    how.transformed()
                };
                self.function_args(&function.args, args_use, scope, acc)?;
                self.function_args(&function.parameters, args_use, scope, acc)?;
                if let Some(filter) = &function.filter {
                    self.walk(filter, indirect_for(how, IndirectKind::Filter), scope, acc)?;
                }
                for order in &function.within_group {
                    self.walk(
                        &order.expr,
                        indirect_for(how, IndirectKind::Sort),
                        scope,
                        acc,
                    )?;
                }
                match &function.over {
                    Some(WindowType::WindowSpec(spec)) => self.window(spec, how, scope, acc)?,
                    Some(WindowType::NamedWindow(window)) => {
                        let key = self.dialect.ident(window);
                        let spec = scope
                            .windows
                            .get(&key)
                            .ok_or_else(|| Opaque(format!("unknown window `{window}`")))?;
                        self.window(spec, how, scope, acc)?;
                    }
                    None => {}
                }
                Ok(())
            }
            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let condition = indirect_for(how, IndirectKind::Conditional);
                if let Some(operand) = operand {
                    self.walk(operand, condition, scope, acc)?;
                }
                for when in conditions {
                    self.walk(&when.condition, condition, scope, acc)?;
                    self.walk(&when.result, how.transformed(), scope, acc)?;
                }
                if let Some(other) = else_result {
                    self.walk(other, how.transformed(), scope, acc)?;
                }
                Ok(())
            }
            Expr::Lambda(lambda) => {
                let params: Vec<String> = lambda_params(lambda)
                    .iter()
                    .map(|i| self.dialect.ident(i))
                    .collect();
                let before = acc.shadowed.len();
                acc.shadowed.extend(params);
                let result = self.walk(&lambda.body, how.transformed(), scope, acc);
                acc.shadowed.truncate(before);
                result
            }
            Expr::Subquery(query) => self.subquery(
                query,
                how.transformed(),
                indirect_for(how, IndirectKind::Filter),
                scope,
                acc,
            ),
            Expr::Exists { subquery, .. } => {
                let kind = indirect_for(how, IndirectKind::Filter);
                self.subquery(subquery, kind, kind, scope, acc)
            }
            Expr::InSubquery { expr, subquery, .. } => {
                self.walk(expr, how.transformed(), scope, acc)?;
                let kind = indirect_for(how, IndirectKind::Filter);
                self.subquery(subquery, kind, kind, scope, acc)
            }
            other => match children(other) {
                // Compound expressions (arithmetic, casts, predicates, …): walk each part
                // so windows, CASEs and subqueries inside keep their meaning.
                Some(parts) => {
                    for part in parts {
                        self.walk(part, how.transformed(), scope, acc)?;
                    }
                    Ok(())
                }
                None => self.generic(other, how.transformed(), scope, acc),
            },
        }
    }

    fn function_args(
        &self,
        args: &FunctionArguments,
        how: Use,
        scope: &Scope<'_>,
        acc: &mut Acc,
    ) -> Result<()> {
        match args {
            FunctionArguments::None => Ok(()),
            FunctionArguments::Subquery(query) => self.subquery(query, how, how, scope, acc),
            FunctionArguments::List(list) => {
                for arg in &list.args {
                    let (FunctionArg::Named { arg, .. }
                    | FunctionArg::ExprNamed { arg, .. }
                    | FunctionArg::Unnamed(arg)) = arg;
                    if let FunctionArgExpr::Expr(expr) = arg {
                        self.walk(expr, how, scope, acc)?;
                    }
                }
                // `array_agg(a order by b)`, `listagg(a) … where …`: the clauses decide
                // which values are combined, and in what order.
                for clause in &list.clauses {
                    match clause {
                        FunctionArgumentClause::OrderBy(orders) => {
                            for order in orders {
                                self.walk(
                                    &order.expr,
                                    indirect_for(how, IndirectKind::Sort),
                                    scope,
                                    acc,
                                )?;
                            }
                        }
                        FunctionArgumentClause::Where(expr) => {
                            self.walk(expr, indirect_for(how, IndirectKind::Filter), scope, acc)?;
                        }
                        FunctionArgumentClause::Limit(expr) => self.walk(expr, how, scope, acc)?,
                        _ => {}
                    }
                }
                Ok(())
            }
        }
    }

    fn window(&self, spec: &WindowSpec, how: Use, scope: &Scope<'_>, acc: &mut Acc) -> Result<()> {
        let kind = indirect_for(how, IndirectKind::Window);
        for expr in &spec.partition_by {
            self.walk(expr, kind, scope, acc)?;
        }
        for order in &spec.order_by {
            self.walk(&order.expr, kind, scope, acc)?;
        }
        if let Some(name) = &spec.window_name
            && let Some(base) = scope.windows.get(&self.dialect.ident(name))
        {
            self.window(base, how, scope, acc)?;
        }
        Ok(())
    }

    /// A subquery used as a value (`values`) and/or a row filter (`rows`).
    fn subquery(
        &self,
        query: &Query,
        values: Use,
        rows: Use,
        scope: &Scope<'_>,
        acc: &mut Acc,
    ) -> Result<()> {
        let out = self.query(query, &scope.ctes, Some(scope))?;
        for col in &out.outputs {
            for (column, edge) in &col.inputs {
                acc.edges.insert((column.clone(), compose(values, *edge)));
            }
        }
        // The subquery's own row shaping decides its result, so it becomes an indirect
        // input of whatever uses the subquery.
        let kind = match rows {
            Use::Indirect(kind) => kind,
            Use::Value(_) => IndirectKind::Filter,
        };
        for (column, _) in &out.rows {
            acc.edges.insert((column.clone(), EdgeKind::Indirect(kind)));
        }
        acc.reads.extend(out.reads);
        acc.diagnostics.extend(out.diagnostics);
        Ok(())
    }

    /// Any other expression shape: every column it mentions feeds it. Constructs whose
    /// meaning depends on structure (subqueries, lambdas, CASE, window functions) can't
    /// be classified here, so they make the query opaque instead of being flattened.
    fn generic(&self, expr: &Expr, how: Use, scope: &Scope<'_>, acc: &mut Acc) -> Result<()> {
        let mut references: Vec<Vec<Ident>> = Vec::new();
        let mut unsupported = None;
        let _ = visit_expressions(expr, |e| {
            match e {
                Expr::Identifier(ident) => references.push(vec![ident.clone()]),
                Expr::CompoundIdentifier(parts) => references.push(parts.clone()),
                Expr::Subquery(_)
                | Expr::Exists { .. }
                | Expr::InSubquery { .. }
                | Expr::Lambda(_)
                | Expr::Case { .. } => {
                    unsupported = Some(first_words(&e.to_string()));
                }
                Expr::Function(f) if f.over.is_some() => {
                    unsupported = Some(first_words(&e.to_string()));
                }
                _ => {}
            }
            ControlFlow::<()>::Continue(())
        });
        if let Some(what) = unsupported {
            return Err(Opaque(format!(
                "`{what}` inside `{}` is not supported",
                first_words(&expr.to_string())
            )));
        }
        for parts in references {
            self.reference(&parts, how, scope, acc)?;
        }
        Ok(())
    }

    fn reference(&self, parts: &[Ident], how: Use, scope: &Scope<'_>, acc: &mut Acc) -> Result<()> {
        // Lambda parameters (`x`, or a field of one: `x.sku`) are not columns.
        if let Some(first) = parts.first()
            && acc.shadowed.contains(&self.dialect.ident(first))
        {
            return Ok(());
        }
        if let Some(resolved) = self.resolve(parts, scope) {
            acc.lower(resolved.confidence);
            for (column, edge) in resolved.edges {
                acc.edges.insert((column, compose(how, edge)));
            }
            return Ok(());
        }
        let name = parts
            .iter()
            .map(|p| p.value.as_str())
            .collect::<Vec<_>>()
            .join(".");
        // Niladic keywords (`current_date`, `null`) can parse as identifiers; they read no
        // column. Anything else unresolved could be a real input we'd miss, so the query
        // is opaque rather than silently incomplete (AGENTS.md rule 3).
        if let [single] = parts
            && single.quote_style.is_none()
            && KEYWORDS.contains(&single.value.to_ascii_lowercase().as_str())
        {
            return Ok(());
        }
        Err(Opaque(format!(
            "`{name}` could not be resolved to a column"
        )))
    }

    /// Resolves a column reference: `col`, `source.col`, `db.table.col`, or any of
    /// those followed by struct field names (`source.col.field`).
    fn resolve(&self, parts: &[Ident], scope: &Scope<'_>) -> Option<Resolved> {
        let names: Vec<String> = parts.iter().map(|p| self.dialect.ident(p)).collect();
        // Prefer the longest qualifier: `a.b.c` is column `c` of source `a.b` before it is
        // field `c` of column `b` of source `a`, before field path `b.c` of column `a`.
        for split in (0..names.len()).rev() {
            let (qualifier, rest) = names.split_at(split);
            let Some(column) = rest.first() else {
                continue;
            };
            if let Some(mut resolved) = Self::resolve_column(column, qualifier, scope) {
                if rest.len() > 1 {
                    resolved.edges = resolved
                        .edges
                        .into_iter()
                        .map(|(c, e)| (c, compose(Use::Value(DirectKind::Transformation), e)))
                        .collect();
                }
                return Some(resolved);
            }
        }
        None
    }

    fn resolve_column(column: &str, qualifier: &[String], scope: &Scope<'_>) -> Option<Resolved> {
        if !qualifier.is_empty() {
            let source = Self::source_named(qualifier, scope)?;
            return Self::source_column(source, column).or_else(|| {
                // The qualifier names a table whose columns are unknown: trust the
                // qualifier (the SQL says so), but mark it inferred.
                match &source.kind {
                    SourceKind::Table {
                        relation,
                        columns: None,
                    } => Some(Resolved {
                        token: format!("{relation}.{column}"),
                        edges: [(
                            ColumnRef::new(relation.clone(), column),
                            EdgeKind::Direct(DirectKind::Identity),
                        )]
                        .into(),
                        confidence: Confidence::Inferred,
                    }),
                    _ => None,
                }
            });
        }
        // Known columns, innermost scope outward; lateral aliases only innermost.
        let mut current = Some(scope);
        let mut innermost = true;
        while let Some(level) = current {
            let known: Vec<Resolved> = level
                .sources
                .iter()
                .filter_map(|s| Self::source_column(s, column))
                .collect();
            match known.len() {
                1 => return known.into_iter().next(),
                n if n > 1 => return Some(merge(known, Confidence::Inferred)),
                _ => {}
            }
            if innermost && let Some(alias) = level.aliases.iter().rev().find(|c| c.name == column)
            {
                return Some(Resolved {
                    edges: alias.inputs.clone(),
                    token: alias.digest.clone(),
                    confidence: alias.confidence,
                });
            }
            innermost = false;
            current = level.outer;
        }
        // Only then: tables whose columns are unknown might have it (innermost first).
        let mut current = Some(scope);
        while let Some(level) = current {
            let unknown: Vec<Resolved> = level
                .sources
                .iter()
                .filter_map(|s| match &s.kind {
                    SourceKind::Table {
                        relation,
                        columns: None,
                    } => Some(Resolved {
                        token: format!("{relation}.{column}"),
                        edges: [(
                            ColumnRef::new(relation.clone(), column),
                            EdgeKind::Direct(DirectKind::Identity),
                        )]
                        .into(),
                        confidence: Confidence::Inferred,
                    }),
                    _ => None,
                })
                .collect();
            if !unknown.is_empty() {
                return Some(merge(unknown, Confidence::Inferred));
            }
            current = level.outer;
        }
        None
    }

    fn source_named<'s>(qualifier: &[String], scope: &'s Scope<'_>) -> Option<&'s Source> {
        let mut current = Some(scope);
        while let Some(scope) = current {
            let found = scope.sources.iter().find(|s| match qualifier {
                [alias] => s.alias == *alias,
                _ => matches!(&s.kind, SourceKind::Table { relation, .. } if relation.parts().ends_with(qualifier)),
            });
            if found.is_some() {
                return found;
            }
            current = scope.outer;
        }
        None
    }

    fn source_column(source: &Source, column: &str) -> Option<Resolved> {
        match &source.kind {
            SourceKind::Table {
                relation,
                columns: Some(columns),
            } => columns.iter().any(|c| c == column).then(|| Resolved {
                token: format!("{relation}.{column}"),
                edges: [(
                    ColumnRef::new(relation.clone(), column),
                    EdgeKind::Direct(DirectKind::Identity),
                )]
                .into(),
                confidence: Confidence::Exact,
            }),
            SourceKind::Table { columns: None, .. } => None,
            SourceKind::Derived(derived) => derived.output(column).map(|c| Resolved {
                edges: c.inputs.clone(),
                token: c.digest.clone(),
                confidence: c.confidence,
            }),
        }
    }

    /// The expression with every column reference replaced by what it resolves to, so
    /// aliases and formatting don't affect digests but a changed input does.
    fn canon(&self, expr: &Expr, scope: &Scope<'_>) -> String {
        let mut expr = expr.clone();
        let _ = visit_expressions_mut(&mut expr, |e| {
            let parts = match e {
                Expr::Identifier(ident) => Some(vec![ident.clone()]),
                Expr::CompoundIdentifier(parts) => Some(parts.clone()),
                _ => None,
            };
            if let Some(parts) = parts
                && let Some(resolved) = self.resolve(&parts, scope)
            {
                *e = Expr::Identifier(Ident::with_quote('"', resolved.token));
            }
            ControlFlow::<()>::Continue(())
        });
        expr.to_string()
    }
}

/// The direct sub-expressions of common compound expressions, or `None` for shapes
/// handled elsewhere or by the generic fallback.
fn children(expr: &Expr) -> Option<Vec<&Expr>> {
    Some(match expr {
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => vec![left, right],
        Expr::UnaryOp { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::Collate { expr, .. }
        | Expr::Extract { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. } => vec![expr],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr, low, high],
        Expr::InList { expr, list, .. } => std::iter::once(&**expr).chain(list).collect(),
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => vec![expr, pattern],
        Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => vec![timestamp, time_zone],
        Expr::Tuple(items) => items.iter().collect(),
        Expr::Value(_) | Expr::TypedString(_) => Vec::new(),
        _ => return None,
    })
}

fn modifiers_text(modifiers: &[sqlparser::ast::GroupByWithModifier]) -> String {
    modifiers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn merge(all: Vec<Resolved>, confidence: Confidence) -> Resolved {
    let mut tokens: Vec<String> = all.iter().map(|r| r.token.clone()).collect();
    tokens.sort();
    Resolved {
        edges: all.into_iter().flat_map(|r| r.edges).collect(),
        token: tokens.join("|"),
        confidence,
    }
}

/// In a value position, a row-shaping construct becomes the given indirect kind; in an
/// indirect position, it keeps the outer kind (a filter stays a filter).
fn indirect_for(how: Use, kind: IndirectKind) -> Use {
    match how {
        Use::Value(_) => Use::Indirect(kind),
        indirect @ Use::Indirect(_) => indirect,
    }
}

fn lambda_params(lambda: &sqlparser::ast::LambdaFunction) -> Vec<Ident> {
    match &lambda.params {
        sqlparser::ast::OneOrManyWithParens::One(param) => vec![param.name.clone()],
        sqlparser::ast::OneOrManyWithParens::Many(params) => {
            params.iter().map(|p| p.name.clone()).collect()
        }
    }
}

fn join_parts(operator: &JoinOperator) -> Result<(&'static str, Option<&JoinConstraint>)> {
    Ok(match operator {
        JoinOperator::Join(c) | JoinOperator::Inner(c) | JoinOperator::StraightJoin(c) => {
            ("inner", Some(c))
        }
        JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => ("left", Some(c)),
        JoinOperator::Right(c) | JoinOperator::RightOuter(c) => ("right", Some(c)),
        JoinOperator::FullOuter(c) => ("full", Some(c)),
        JoinOperator::CrossJoin(c) => ("cross", Some(c)),
        JoinOperator::Semi(c) | JoinOperator::LeftSemi(c) => ("semi", Some(c)),
        JoinOperator::RightSemi(c) => ("right semi", Some(c)),
        JoinOperator::Anti(c) | JoinOperator::LeftAnti(c) => ("anti", Some(c)),
        JoinOperator::RightAnti(c) => ("right anti", Some(c)),
        other => {
            return Err(Opaque(format!(
                "unsupported join: {}",
                first_words(&format!("{other:?}"))
            )));
        }
    })
}

fn check_wildcard_options(options: &WildcardAdditionalOptions) -> Result<()> {
    if options.opt_ilike.is_some()
        || options.opt_replace.is_some()
        || options.opt_rename.is_some()
        || options.opt_alias.is_some()
    {
        return Err(Opaque(
            "`*` with ILIKE, REPLACE, RENAME or an alias is not supported".into(),
        ));
    }
    Ok(())
}

/// Renames outputs by position (`cte(a, b) as (...)`, `t as x(a, b)`).
fn rename(out: &mut QueryOut, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    if names.len() > out.outputs.len() {
        return Err(Opaque(format!(
            "{} column names given for {} columns",
            names.len(),
            out.outputs.len()
        )));
    }
    for (col, name) in out.outputs.iter_mut().zip(names) {
        col.name.clone_from(name);
    }
    Ok(())
}

fn table_as_query(relation: &RelationName, columns: &[String]) -> QueryOut {
    QueryOut {
        outputs: columns
            .iter()
            .map(|c| {
                let column = ColumnRef::new(relation.clone(), c.clone());
                Col {
                    name: c.clone(),
                    digest: digest(&["column", &column.to_string()]),
                    inputs: [(column, EdgeKind::Direct(DirectKind::Identity))].into(),
                    confidence: Confidence::Exact,
                }
            })
            .collect(),
        reads: [relation.clone()].into(),
        row_digest: digest(&["table", &relation.to_string()]),
        ..QueryOut::default()
    }
}

fn first_words(text: &str) -> String {
    const MAX: usize = 60;
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > MAX {
        format!("{}…", line.chars().take(MAX).collect::<String>())
    } else {
        line.to_owned()
    }
}
