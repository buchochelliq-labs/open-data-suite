//! Downstream impact of column changes: which models must run, and why.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ods_core::{ColumnRef, EdgeKind, RelationName};
use serde::Serialize;

use crate::diff::{Change, ColumnChangeKind};
use crate::graph::ColumnGraph;

/// One step of evidence for why a node is impacted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
#[non_exhaustive]
pub enum ImpactReason {
    /// An upstream column it uses changed.
    Column {
        /// The upstream column.
        upstream: ColumnRef,
        /// How it changed.
        change: ColumnChangeKind,
        /// The output column it feeds, or `None` if it shapes the rows.
        output: Option<String>,
        /// How it is used.
        edge: EdgeKind,
    },
    /// The rows of an upstream relation it reads may differ.
    Rows {
        /// The upstream relation.
        upstream: RelationName,
    },
    /// A column was added to a relation it reads with `*`.
    Wildcard {
        /// The new upstream column.
        upstream: ColumnRef,
    },
    /// A column was added to a relation it reads, with the same name as a column it
    /// uses from elsewhere: an unqualified reference may now bind to the new column.
    NameCapture {
        /// The new upstream column.
        upstream: ColumnRef,
    },
    /// Its lineage is unknown, so any change to what it reads impacts it.
    Opaque {
        /// The upstream relation that changed.
        upstream: RelationName,
    },
}

/// How one node is impacted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeImpact {
    /// Every piece of evidence, sorted.
    pub reasons: BTreeSet<ImpactReason>,
    /// Its output columns whose values may change.
    pub changed_columns: BTreeSet<String>,
    /// Whether its rows (and so every column) may change.
    pub rows_changed: bool,
}

/// A reader of a changed relation that is **not** impacted, with the reason. This is what
/// saves work, so it is reported, never silent (AGENTS.md rule 4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Pruned {
    /// The node that doesn't need to run.
    pub node: String,
    /// The changed relation it reads.
    pub upstream: RelationName,
    /// The changed columns of that relation, none of which it uses.
    pub unused_changed_columns: BTreeSet<String>,
}

/// The result of [`ColumnGraph::impact`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Impact {
    /// Impacted downstream nodes, by id. Nodes whose own code changed are not included
    /// unless something upstream impacts them too.
    pub nodes: BTreeMap<String, NodeImpact>,
    /// Readers of changed relations that are provably unaffected.
    pub pruned: BTreeSet<Pruned>,
}

impl Impact {
    /// Ids of the impacted nodes, sorted.
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.keys().map(String::as_str)
    }
}

impl ColumnGraph {
    /// Propagates `changes` downstream and reports every impacted node with the evidence
    /// chain, plus the readers that were pruned.
    ///
    /// Rules, most conservative first:
    /// - a reader whose lineage is unknown is impacted by any change to what it reads;
    /// - a [`Change::Rows`] change impacts every reader, and changes its rows;
    /// - a modified or removed column impacts each output it feeds `Direct`ly (that
    ///   output is modified), and changes the rows of each node where it is a
    ///   row-shaping (`Indirect`) input;
    /// - an added column impacts only readers that select `*` from the relation.
    pub fn impact(&self, changes: &[Change]) -> Impact {
        let mut impact = Impact::default();
        let mut queue: VecDeque<Change> = changes.iter().cloned().collect();
        let mut seen: BTreeSet<Change> = BTreeSet::new();
        // Changed columns per relation, for reporting pruned readers.
        let mut changed_by_relation: BTreeMap<RelationName, BTreeSet<String>> = BTreeMap::new();

        while let Some(change) = queue.pop_front() {
            if !seen.insert(change.clone()) {
                continue;
            }
            let relation = change.relation().clone();
            let entry = changed_by_relation.entry(relation.clone()).or_default();
            if let Change::Column { column, .. } = &change {
                entry.insert(column.column.clone());
            }
            for reader in self.readers_of(&relation) {
                let Some(node) = self.nodes.get(reader) else {
                    continue;
                };
                // Declared but not read by its SQL (e.g. a `-- depends_on:` hint for a macro
                // that queries the table at run time): nothing says how it's used, so it's
                // treated as opaque for this relation.
                let declared_only = node
                    .lineage
                    .as_ref()
                    .is_some_and(|l| !l.relations_read.contains(&relation));
                if node.is_opaque() || declared_only {
                    let target = impact.nodes.entry(reader.to_owned()).or_default();
                    target.reasons.insert(ImpactReason::Opaque {
                        upstream: relation.clone(),
                    });
                    rows_changed(target, &node.relation, &mut queue);
                    continue;
                }
                match &change {
                    Change::Rows { .. } => {
                        let target = impact.nodes.entry(reader.to_owned()).or_default();
                        target.reasons.insert(ImpactReason::Rows {
                            upstream: relation.clone(),
                        });
                        rows_changed(target, &node.relation, &mut queue);
                    }
                    Change::Column {
                        column,
                        kind: ColumnChangeKind::Added,
                    } => {
                        let Some(lineage) = node.lineage.as_ref() else {
                            continue;
                        };
                        if lineage.wildcard_relations.contains(&relation) {
                            let target = impact.nodes.entry(reader.to_owned()).or_default();
                            target.reasons.insert(ImpactReason::Wildcard {
                                upstream: column.clone(),
                            });
                            // If the reader also shapes rows with that relation's columns
                            // (DISTINCT *, UNION, GROUP BY over *), the new column can
                            // change which rows exist.
                            if lineage
                                .row_inputs
                                .iter()
                                .any(|(c, _)| c.relation == relation)
                            {
                                rows_changed(target, &node.relation, &mut queue);
                            }
                            if target.changed_columns.insert(column.column.clone()) {
                                queue.push_back(Change::Column {
                                    column: ColumnRef::new(
                                        node.relation.clone(),
                                        column.column.clone(),
                                    ),
                                    kind: ColumnChangeKind::Added,
                                });
                            }
                        }
                        let uses_same_name = lineage
                            .outputs
                            .iter()
                            .flat_map(|o| o.inputs.iter().map(|(c, _)| c))
                            .chain(lineage.row_inputs.iter().map(|(c, _)| c))
                            .any(|c| c.column == column.column && c.relation != relation);
                        if uses_same_name {
                            let target = impact.nodes.entry(reader.to_owned()).or_default();
                            target.reasons.insert(ImpactReason::NameCapture {
                                upstream: column.clone(),
                            });
                            rows_changed(target, &node.relation, &mut queue);
                        }
                    }
                    Change::Column { column, kind } => {
                        self.column_changed(column, *kind, reader, &mut impact, &mut queue);
                    }
                }
            }
        }

        for (relation, columns) in &changed_by_relation {
            for reader in self.readers_of(relation) {
                if !impact.nodes.contains_key(reader) {
                    impact.pruned.insert(Pruned {
                        node: reader.to_owned(),
                        upstream: relation.clone(),
                        unused_changed_columns: columns.clone(),
                    });
                }
            }
        }
        impact
    }

    fn column_changed(
        &self,
        column: &ColumnRef,
        kind: ColumnChangeKind,
        reader: &str,
        impact: &mut Impact,
        queue: &mut VecDeque<Change>,
    ) {
        let Some(node) = self.nodes.get(reader) else {
            return;
        };
        for used in self.uses_of(column).filter(|u| u.node == reader) {
            let target = impact.nodes.entry(reader.to_owned()).or_default();
            target.reasons.insert(ImpactReason::Column {
                upstream: column.clone(),
                change: kind,
                output: used.output.clone(),
                edge: used.edge,
            });
            match &used.output {
                // Direct inputs change the value; indirect ones attached to an output
                // (a CASE condition) change which value is chosen. Either way the
                // output is modified.
                Some(output) => {
                    if target.changed_columns.insert(output.clone()) {
                        queue.push_back(Change::Column {
                            column: ColumnRef::new(node.relation.clone(), output.clone()),
                            kind: ColumnChangeKind::Modified,
                        });
                    }
                }
                None => rows_changed(target, &node.relation, queue),
            }
        }
    }
}

fn rows_changed(target: &mut NodeImpact, relation: &RelationName, queue: &mut VecDeque<Change>) {
    if !target.rows_changed {
        target.rows_changed = true;
        queue.push_back(Change::Rows {
            relation: relation.clone(),
        });
    }
}
