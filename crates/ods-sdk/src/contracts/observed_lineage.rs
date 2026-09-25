//! `ObservedLineageSource`: column lineage a platform recorded while queries ran
//! (e.g. a catalog's lineage system tables, or `OpenLineage` events).
//!
//! Static analysis ([`SqlLineageAnalyzer`](super::sql_lineage::SqlLineageAnalyzer)) says
//! what a query *can* do. Observed lineage says what executed queries *did*. It checks the
//! analyzer, and it covers code the analyzer can't read, such as Python models.
//!
//! # What observed lineage can't say (AGENTS.md rule 3)
//! It only contains paths that actually ran within the platform's retention window. A
//! missing edge doesn't prove the edge can't exist. Consumers must not treat observed
//! lineage as complete unless a user explicitly opts in.

use std::collections::BTreeSet;

use ods_core::{ColumnRef, RelationName, SchemaVersion};
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `observed_lineage_source` contract.
pub const OBSERVED_LINEAGE_SOURCE: Contract = Contract {
    name: "observed_lineage_source",
    version: SchemaVersion::new(0, 1),
};

/// Lineage recorded by a platform, deduplicated and in a stable order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ObservedLineage {
    /// `source` column → `target` column, as written by some statement.
    pub column_edges: BTreeSet<(ColumnRef, ColumnRef)>,
    /// A source column read by a statement that wrote the target relation, with no
    /// target column recorded. Platforms report these for columns that shape rows
    /// (joins, filters) or for reads they couldn't attribute to a column.
    pub row_inputs: BTreeSet<(ColumnRef, RelationName)>,
    /// `source` relation → `target` relation.
    pub relation_edges: BTreeSet<(RelationName, RelationName)>,
    /// Records read.
    pub records: usize,
    /// Records ignored: no source table (e.g. file paths), or reads with no target.
    pub skipped: usize,
    /// Earliest and latest event time seen, as the platform wrote them.
    pub observed_between: Option<(String, String)>,
}

impl ObservedLineage {
    /// Adds `source → target`, recording the relation edge too.
    pub fn add_column_edge(&mut self, source: ColumnRef, target: ColumnRef) {
        self.relation_edges
            .insert((source.relation.clone(), target.relation.clone()));
        self.column_edges.insert((source, target));
    }

    /// Adds a row input of `target`, recording the relation edge too.
    pub fn add_row_input(&mut self, source: ColumnRef, target: RelationName) {
        self.relation_edges
            .insert((source.relation.clone(), target.clone()));
        self.row_inputs.insert((source, target));
    }

    /// Every relation written by some observed statement.
    pub fn targets(&self) -> BTreeSet<&RelationName> {
        self.relation_edges.iter().map(|(_, t)| t).collect()
    }

    /// Rewrites every relation and column name, e.g. to match an analyzer's identifier
    /// normalization, merging entries that become equal.
    #[must_use]
    pub fn normalized(
        &self,
        relation: &dyn Fn(&RelationName) -> RelationName,
        column: &dyn Fn(&str) -> String,
    ) -> Self {
        let col = |c: &ColumnRef| ColumnRef::new(relation(&c.relation), column(&c.column));
        Self {
            column_edges: self
                .column_edges
                .iter()
                .map(|(s, t)| (col(s), col(t)))
                .collect(),
            row_inputs: self
                .row_inputs
                .iter()
                .map(|(s, t)| (col(s), relation(t)))
                .collect(),
            relation_edges: self
                .relation_edges
                .iter()
                .map(|(s, t)| (relation(s), relation(t)))
                .collect(),
            records: self.records,
            skipped: self.skipped,
            observed_between: self.observed_between.clone(),
        }
    }
}

/// A source of observed lineage.
///
/// Synchronous like [`SqlLineageAnalyzer`](super::sql_lineage::SqlLineageAnalyzer):
/// today's implementations read exported files. A live source (a system-table query)
/// can wrap its I/O and hand back the same type.
pub trait ObservedLineageSource: Provider {
    /// Reads everything the source recorded.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the source can't be read or isn't in the expected
    /// format.
    fn observed_lineage(&self) -> Result<ObservedLineage, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(relation: &str, column: &str) -> ColumnRef {
        ColumnRef::new(RelationName::new(relation.split('.')).unwrap(), column)
    }

    #[test]
    fn edges_are_deduplicated_and_normalization_merges_names() {
        let mut observed = ObservedLineage::default();
        observed.add_column_edge(col("c.s.a", "ID"), col("c.s.b", "id"));
        observed.add_column_edge(col("c.s.a", "id"), col("c.s.b", "id"));
        observed.add_row_input(col("c.s.a", "status"), col("c.s.b", "x").relation);
        assert_eq!(observed.column_edges.len(), 2);
        assert_eq!(observed.relation_edges.len(), 1);
        let lower = observed.normalized(&Clone::clone, &str::to_lowercase);
        assert_eq!(lower.column_edges.len(), 1);
        assert_eq!(lower.targets().len(), 1);
    }
}
