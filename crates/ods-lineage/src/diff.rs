//! Turning two versions of a model's lineage into column changes.

use std::collections::BTreeMap;

use ods_core::{ColumnRef, RelationName};
use ods_sdk::contracts::sql_lineage::QueryLineage;
use serde::Serialize;

/// How a column changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnChangeKind {
    /// The column is new.
    Added,
    /// The column no longer exists.
    Removed,
    /// The column is computed differently, so its values may differ.
    Modified,
}

/// A change to a relation's data or shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case", tag = "change")]
pub enum Change {
    /// One column changed.
    Column {
        /// The column.
        column: ColumnRef,
        /// How.
        kind: ColumnChangeKind,
    },
    /// The set of rows may differ (filters, joins, grouping, new data), so every column
    /// may differ.
    Rows {
        /// The relation.
        relation: RelationName,
    },
}

impl Change {
    /// The relation that changed.
    pub fn relation(&self) -> &RelationName {
        match self {
            Change::Column { column, .. } => &column.relation,
            Change::Rows { relation } => relation,
        }
    }
}

/// The changes to `relation` between a previous and a current analysis of its SQL.
///
/// Conservative: a new model, or an opaque analysis on either side, is a [`Change::Rows`]
/// change: nothing can be proven unchanged. Changes are sorted.
pub fn diff(
    relation: &RelationName,
    before: Option<&QueryLineage>,
    after: &QueryLineage,
) -> Vec<Change> {
    let rows = || {
        vec![Change::Rows {
            relation: relation.clone(),
        }]
    };
    let Some(before) = before else {
        return rows();
    };
    if before.opaque || after.opaque || before.row_digest != after.row_digest {
        return rows();
    }
    let old: BTreeMap<&str, &str> = before
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), o.expression_digest.as_str()))
        .collect();
    let new: BTreeMap<&str, &str> = after
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), o.expression_digest.as_str()))
        .collect();
    let column = |name: &str, kind| Change::Column {
        column: ColumnRef::new(relation.clone(), name),
        kind,
    };
    let mut changes = Vec::new();
    for (name, digest) in &new {
        match old.get(name) {
            None => changes.push(column(name, ColumnChangeKind::Added)),
            Some(previous) if previous != digest => {
                changes.push(column(name, ColumnChangeKind::Modified));
            }
            Some(_) => {}
        }
    }
    for name in old.keys().filter(|n| !new.contains_key(*n)) {
        changes.push(column(name, ColumnChangeKind::Removed));
    }
    changes.sort();
    changes
}
