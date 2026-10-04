//! Which consumers would fail if columns were removed (#347): the SQL that names a
//! column that no longer exists.
//!
//! [`ColumnGraph::impact`] answers "what must run"; this answers "what would break". A
//! removed column breaks every node whose SQL names it (in an output, a join, a filter,
//! …). A node that only passes it through `select *` doesn't break, but loses the column
//! too, so its own readers are checked in turn. A reader whose lineage is unknown may
//! name it or not: it is reported as unknown, never as safe (AGENTS.md rule 3).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ods_core::{ColumnRef, DirectKind, EdgeKind, RelationName};
use serde::Serialize;

use crate::graph::ColumnGraph;

/// The result of [`ColumnGraph::breaks`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Breaks {
    /// Nodes whose SQL names a removed column, by id, with the columns it names.
    pub broken: BTreeMap<String, BTreeSet<ColumnRef>>,
    /// Columns that disappear downstream because a node passes a removed column
    /// through `select *`, by the node that loses them.
    pub dropped: BTreeMap<String, BTreeSet<ColumnRef>>,
    /// Nodes that can't be told to break or not, by id.
    pub unknown: BTreeMap<String, Uncertain>,
}

/// Why a node can't be told to break or not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Uncertain {
    /// The removed columns it might name, directly or once passed on.
    pub columns: BTreeSet<ColumnRef>,
    /// The relations it reads whose columns are unknown after the change (an opaque
    /// model that may pass a removed column on, or a node whose columns were never
    /// known). Empty when its own lineage is what is unknown.
    pub through: BTreeSet<RelationName>,
}

/// What travels downstream.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Loss {
    /// This column no longer exists.
    Removed(ColumnRef),
    /// `relation` may no longer have `origin` (or a column carrying it).
    Uncertain {
        relation: RelationName,
        origin: ColumnRef,
    },
}

impl ColumnGraph {
    /// Follows `removed` columns downstream and reports which nodes would fail.
    ///
    /// Past a node whose lineage is unknown, nothing is certain: its readers, and the
    /// readers of anything that passes its columns on with `select *`, are unknown,
    /// whatever else they read (AGENTS.md rule 3). So are the readers of a node whose
    /// columns were never known.
    pub fn breaks(&self, removed: &[ColumnRef]) -> Breaks {
        let mut breaks = Breaks::default();
        let mut queue: VecDeque<Loss> = removed.iter().cloned().map(Loss::Removed).collect();
        let mut seen: BTreeSet<Loss> = BTreeSet::new();
        while let Some(loss) = queue.pop_front() {
            if !seen.insert(loss.clone()) {
                continue;
            }
            match loss {
                Loss::Removed(column) => self.removed(&column, &mut breaks, &mut queue),
                Loss::Uncertain { relation, origin } => {
                    for reader in self.readers_of(&relation) {
                        let Some(node) = self.nodes.get(reader) else {
                            continue;
                        };
                        let entry = breaks.unknown.entry(reader.to_owned()).or_default();
                        entry.columns.insert(origin.clone());
                        entry.through.insert(relation.clone());
                        let star = node
                            .lineage
                            .as_ref()
                            .is_some_and(|l| l.wildcard_relations.contains(&relation));
                        if node.is_opaque() || star {
                            queue.push_back(Loss::Uncertain {
                                relation: node.relation.clone(),
                                origin: origin.clone(),
                            });
                        }
                    }
                }
            }
        }
        // What is certain wins: a node that names a removed column breaks, whatever
        // else is unknown about it or whatever it passes through.
        breaks
            .dropped
            .retain(|id, _| !breaks.broken.contains_key(id));
        breaks
            .unknown
            .retain(|id, _| !breaks.broken.contains_key(id));
        breaks
    }

    fn removed(&self, column: &ColumnRef, breaks: &mut Breaks, queue: &mut VecDeque<Loss>) {
        // Readers of a node whose columns were never known may name it in ways the
        // lineage couldn't resolve.
        if self
            .node_for(&column.relation)
            .is_some_and(|n| n.columns.is_empty())
        {
            queue.push_back(Loss::Uncertain {
                relation: column.relation.clone(),
                origin: column.clone(),
            });
        }
        for reader in self.readers_of(&column.relation) {
            let Some(node) = self.nodes.get(reader) else {
                continue;
            };
            // As in `impact`: a node that declares the relation but whose SQL doesn't
            // read it uses it in a way nothing records.
            let lineage = node
                .lineage
                .as_ref()
                .filter(|l| !node.is_opaque() && l.relations_read.contains(&column.relation));
            let Some(lineage) = lineage else {
                breaks
                    .unknown
                    .entry(reader.to_owned())
                    .or_default()
                    .columns
                    .insert(column.clone());
                queue.push_back(Loss::Uncertain {
                    relation: node.relation.clone(),
                    origin: column.clone(),
                });
                continue;
            };
            let star = lineage.wildcard_relations.contains(&column.relation);
            for used in self.uses_of(column).filter(|u| u.node == reader) {
                let passed_through = star
                    && used.output.as_deref() == Some(column.column.as_str())
                    && used.edge == EdgeKind::Direct(DirectKind::Identity);
                if passed_through {
                    let lost = ColumnRef::new(node.relation.clone(), column.column.clone());
                    breaks
                        .dropped
                        .entry(reader.to_owned())
                        .or_default()
                        .insert(lost.clone());
                    queue.push_back(Loss::Removed(lost));
                } else {
                    breaks
                        .broken
                        .entry(reader.to_owned())
                        .or_default()
                        .insert(column.clone());
                }
            }
        }
    }
}
