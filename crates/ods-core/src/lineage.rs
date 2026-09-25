//! Column-level lineage vocabulary (ADR-0008).
//!
//! These are the provider-neutral types that SQL analyzers produce and that State, CI and
//! Agent consume. Lineage describes how data *flows* through transformations; it is never
//! a PK/FK relationship (AGENTS.md rule 6, ERD ≠ lineage).
//!
//! Edge kinds follow the `OpenLineage` column-lineage facet, so export is lossless:
//! a [`EdgeKind::Direct`] input contributes to a column's *value*; an
//! [`EdgeKind::Indirect`] input decides *which rows* exist or how they are grouped or
//! ordered, so it affects every output column.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A relation (table or view), as normalized identifier parts, e.g. `analytics.main.orders`.
///
/// Parts are compared exactly; the SQL analyzer that produced them applies its dialect's
/// case and quoting rules, so core never needs to know the dialect.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationName(Vec<String>);

impl RelationName {
    /// A relation name from already-normalized parts.
    ///
    /// # Errors
    /// Returns a reason if there are no parts or a part is empty.
    pub fn new<I, S>(parts: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let parts: Vec<String> = parts.into_iter().map(Into::into).collect();
        if parts.is_empty() {
            return Err("a relation name needs at least one part".into());
        }
        if parts.iter().any(String::is_empty) {
            return Err("relation name parts must not be empty".into());
        }
        Ok(Self(parts))
    }

    /// The normalized parts, outermost first.
    pub fn parts(&self) -> &[String] {
        &self.0
    }

    /// The last part (the table or view name).
    pub fn object(&self) -> &str {
        self.0.last().map_or("", String::as_str)
    }
}

impl fmt::Display for RelationName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("."))
    }
}

/// A column of a relation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColumnRef {
    /// The relation that has the column.
    pub relation: RelationName,
    /// The normalized column name.
    pub column: String,
}

impl ColumnRef {
    /// A column of `relation`.
    pub fn new(relation: RelationName, column: impl Into<String>) -> Self {
        Self {
            relation,
            column: column.into(),
        }
    }
}

impl fmt::Display for ColumnRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.relation, self.column)
    }
}

/// How an input column contributes to an output column's value (`OpenLineage` `DIRECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DirectKind {
    /// Copied unchanged (possibly renamed or cast-free).
    Identity,
    /// Computed from the input by a non-aggregate expression.
    Transformation,
    /// Computed by an aggregate over many input rows.
    Aggregation,
}

impl DirectKind {
    /// The `OpenLineage` transformation subtype, e.g. `IDENTITY`.
    pub fn openlineage_subtype(self) -> &'static str {
        match self {
            DirectKind::Identity => "IDENTITY",
            DirectKind::Transformation => "TRANSFORMATION",
            DirectKind::Aggregation => "AGGREGATION",
        }
    }
}

/// How an input column affects the output's rows (`OpenLineage` `INDIRECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IndirectKind {
    /// Used in a join condition.
    Join,
    /// Used in a `WHERE`, `HAVING` or `QUALIFY` predicate.
    Filter,
    /// Used in `GROUP BY`.
    GroupBy,
    /// Used in `ORDER BY` (which matters with `LIMIT`).
    Sort,
    /// Used in a window's `PARTITION BY` or `ORDER BY`.
    Window,
    /// Used in a condition of a `CASE`/`IF` that selects between values.
    Conditional,
}

impl IndirectKind {
    /// The `OpenLineage` transformation subtype, e.g. `GROUP_BY`.
    pub fn openlineage_subtype(self) -> &'static str {
        match self {
            IndirectKind::Join => "JOIN",
            IndirectKind::Filter => "FILTER",
            IndirectKind::GroupBy => "GROUP_BY",
            IndirectKind::Sort => "SORT",
            IndirectKind::Window => "WINDOW",
            IndirectKind::Conditional => "CONDITIONAL",
        }
    }
}

/// How an input column relates to an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "subtype")]
pub enum EdgeKind {
    /// Contributes to the value.
    Direct(DirectKind),
    /// Affects which rows exist, their grouping or order.
    Indirect(IndirectKind),
}

/// How much to trust a piece of lineage (AGENTS.md rule 3: inference is never fact).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Confidence {
    /// Nothing is known: consumers must assume every input affects every output.
    Unknown,
    /// Derived with assumptions, e.g. an unqualified column matched to the only
    /// relation that could have it, or a `select *` expanded from a declared schema.
    Inferred,
    /// Statically resolved from SQL and a known schema.
    Exact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_names_are_validated_and_displayed() {
        let name = RelationName::new(["analytics", "main", "orders"]).unwrap();
        assert_eq!(name.to_string(), "analytics.main.orders");
        assert_eq!(name.object(), "orders");
        assert!(RelationName::new(Vec::<String>::new()).is_err());
        assert!(RelationName::new(["a", ""]).is_err());
    }

    #[test]
    fn edge_kinds_serialize_like_openlineage() {
        let edge = EdgeKind::Indirect(IndirectKind::GroupBy);
        assert_eq!(
            serde_json::to_string(&edge).unwrap(),
            r#"{"type":"indirect","subtype":"group_by"}"#
        );
    }

    #[test]
    fn confidence_orders_from_unknown_to_exact() {
        assert!(Confidence::Unknown < Confidence::Inferred);
        assert!(Confidence::Inferred < Confidence::Exact);
    }
}
