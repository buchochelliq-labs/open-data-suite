//! `SqlLineageAnalyzer`: column-level lineage of one SQL query (#73, #74, ADR-0008).
//!
//! An analyzer parses a query in its dialect and reports, for every output column, which
//! input columns feed it and how ([`EdgeKind`]), plus the input columns that shape the
//! row set (joins, filters, grouping). Dialect rules (identifier case, quoting,
//! functions) live entirely in the provider; everything it returns is neutral.
//!
//! Unlike I/O contracts, this one is synchronous: analysis is pure CPU work, and callers
//! run many analyses in parallel on their own threads.
//!
//! # Conservative results (AGENTS.md rule 3)
//! When an analyzer cannot resolve something (an unknown `select *`, a UDF with unknown
//! semantics, a parse failure of part of the query), it must say so through
//! [`Confidence`] and [`QueryLineage::diagnostics`], never silently drop an input. A
//! whole-query failure is [`QueryLineage::opaque`]: consumers then assume every input
//! column affects every output column.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::{ColumnRef, Confidence, EdgeKind, IndirectKind, RelationName, SchemaVersion};
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `sql_lineage_analyzer` contract.
pub const SQL_LINEAGE_ANALYZER: Contract = Contract {
    name: "sql_lineage_analyzer",
    version: SchemaVersion::new(0, 1),
};

/// Column names of relations the query reads, for resolving `select *` and unqualified
/// columns. Names must be normalized the way the analyzer normalizes identifiers.
pub trait SchemaLookup: Sync {
    /// The relation's columns in order, or `None` if unknown.
    fn columns(&self, relation: &RelationName) -> Option<Vec<String>>;
}

/// A [`SchemaLookup`] backed by a map.
#[derive(Debug, Clone, Default)]
pub struct MapSchema(pub BTreeMap<RelationName, Vec<String>>);

impl SchemaLookup for MapSchema {
    fn columns(&self, relation: &RelationName) -> Option<Vec<String>> {
        self.0.get(relation).cloned()
    }
}

/// One output column and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct OutputColumn {
    /// The normalized output column name.
    pub name: String,
    /// Input columns that feed this column, with how. Row-shaping inputs that apply to
    /// every column are in [`QueryLineage::row_inputs`] instead.
    pub inputs: BTreeSet<(ColumnRef, EdgeKind)>,
    /// A digest of the column's normalized expression, with inputs resolved to
    /// [`ColumnRef`]s: equal digests mean the column is computed the same way.
    pub expression_digest: String,
    /// How well this column was resolved.
    pub confidence: Confidence,
}

impl OutputColumn {
    /// An output column.
    pub fn new(
        name: impl Into<String>,
        inputs: BTreeSet<(ColumnRef, EdgeKind)>,
        expression_digest: impl Into<String>,
        confidence: Confidence,
    ) -> Self {
        Self {
            name: name.into(),
            inputs,
            expression_digest: expression_digest.into(),
            confidence,
        }
    }
}

/// The column-level lineage of one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct QueryLineage {
    /// Output columns in select-list order.
    pub outputs: Vec<OutputColumn>,
    /// Input columns that decide which rows exist, their grouping or order; they affect
    /// every output column.
    pub row_inputs: BTreeSet<(ColumnRef, IndirectKind)>,
    /// Every relation the query reads.
    pub relations_read: BTreeSet<RelationName>,
    /// Relations whose columns reach the output through `*`: a column added to one of
    /// them adds an output column.
    pub wildcard_relations: BTreeSet<RelationName>,
    /// A digest of the row-shaping parts of the query (joins, filters, grouping,
    /// distinct, set operations, limits), with inputs resolved: equal digests mean the
    /// same rows are produced from the same inputs.
    pub row_digest: String,
    /// The lowest confidence of anything in this result.
    pub confidence: Confidence,
    /// `true` when the query could not be analyzed at all; consumers must then treat
    /// every column of [`Self::relations_read`] as feeding every output.
    pub opaque: bool,
    /// Human-readable notes on anything not fully resolved. Never contains data values.
    pub diagnostics: Vec<String>,
}

impl QueryLineage {
    /// A result for a query that could not be analyzed.
    pub fn opaque(relations_read: BTreeSet<RelationName>, reason: impl Into<String>) -> Self {
        Self {
            outputs: Vec::new(),
            row_inputs: BTreeSet::new(),
            relations_read,
            wildcard_relations: BTreeSet::new(),
            row_digest: String::new(),
            confidence: Confidence::Unknown,
            opaque: true,
            diagnostics: vec![reason.into()],
        }
    }

    /// A result from analyzed parts. `confidence` is the minimum over `outputs`.
    pub fn new(
        outputs: Vec<OutputColumn>,
        row_inputs: BTreeSet<(ColumnRef, IndirectKind)>,
        relations_read: BTreeSet<RelationName>,
        row_digest: impl Into<String>,
        diagnostics: Vec<String>,
    ) -> Self {
        let confidence = outputs
            .iter()
            .map(|o| o.confidence)
            .min()
            .unwrap_or(Confidence::Exact);
        Self {
            outputs,
            row_inputs,
            relations_read,
            wildcard_relations: BTreeSet::new(),
            row_digest: row_digest.into(),
            confidence,
            opaque: false,
            diagnostics,
        }
    }

    /// Records relations expanded through `*`.
    #[must_use]
    pub fn with_wildcards(mut self, relations: BTreeSet<RelationName>) -> Self {
        self.wildcard_relations = relations;
        self
    }

    /// The output column called `name`.
    pub fn output(&self, name: &str) -> Option<&OutputColumn> {
        self.outputs.iter().find(|o| o.name == name)
    }
}

/// What to analyze.
pub struct AnalyzeRequest<'a> {
    /// The SQL text, fully rendered (e.g. dbt `compiled_code`).
    pub sql: &'a str,
    /// Schemas of relations the query may read.
    pub schema: &'a dyn SchemaLookup,
}

/// Column-level lineage for SQL in one dialect (contract [`SQL_LINEAGE_ANALYZER`]).
pub trait SqlLineageAnalyzer: Provider {
    /// Identifies the analyzer's behaviour, e.g. `sqlparser-0.59/databricks/1`. It is part
    /// of every cache key, so a new version invalidates cached lineage.
    fn analyzer_version(&self) -> String;

    /// Normalizes a qualified relation name as written in SQL (e.g. a dbt
    /// `relation_name` such as `"db"."schema"."table"`).
    ///
    /// # Errors
    /// Returns [`ProviderError::Other`] if the name cannot be parsed.
    fn relation_name(&self, qualified: &str) -> Result<RelationName, ProviderError>;

    /// Normalizes a column name as written in SQL or declared in a schema.
    fn column_name(&self, name: &str) -> String;

    /// Analyzes one query. A query that cannot be parsed is an [`QueryLineage::opaque`]
    /// result, not an error; errors are for failures of the analyzer itself.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the analyzer fails.
    fn analyze(&self, request: &AnalyzeRequest<'_>) -> Result<QueryLineage, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::DirectKind;

    fn rel(name: &str) -> RelationName {
        RelationName::new(name.split('.')).unwrap()
    }

    #[test]
    fn confidence_is_the_minimum_of_the_outputs() {
        let orders = rel("db.main.orders");
        let exact = OutputColumn::new(
            "id",
            [(
                ColumnRef::new(orders.clone(), "id"),
                EdgeKind::Direct(DirectKind::Identity),
            )]
            .into(),
            "d1",
            Confidence::Exact,
        );
        let inferred = OutputColumn::new("x", BTreeSet::new(), "d2", Confidence::Inferred);
        let lineage = QueryLineage::new(
            vec![exact, inferred],
            BTreeSet::new(),
            [orders].into(),
            "r",
            vec![],
        );
        assert_eq!(lineage.confidence, Confidence::Inferred);
        assert!(lineage.output("id").is_some());
        assert!(!lineage.opaque);
    }

    #[test]
    fn opaque_results_carry_their_reason() {
        let lineage = QueryLineage::opaque([rel("a.b")].into(), "parse error at 3:14");
        assert!(lineage.opaque);
        assert_eq!(lineage.confidence, Confidence::Unknown);
        assert_eq!(lineage.diagnostics, ["parse error at 3:14"]);
    }
}
