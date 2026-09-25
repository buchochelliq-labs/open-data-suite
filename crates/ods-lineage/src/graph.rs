//! The analyzed column graph and its indexes.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::{ColumnRef, EdgeKind, RelationName};
use ods_sdk::contracts::sql_lineage::QueryLineage;
use serde::Serialize;

use crate::project::NodeKind;

/// A node with its analysis result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeLineage {
    /// The node id.
    pub id: String,
    /// The relation it builds or reads.
    pub relation: RelationName,
    /// What it is.
    pub kind: NodeKind,
    /// Its columns as used for resolving downstream queries: analyzed outputs for
    /// models, otherwise the known columns. Empty if unknown.
    pub columns: Vec<String>,
    /// The analysis result; `None` for nodes without SQL.
    pub lineage: Option<QueryLineage>,
    /// The content-addressed key the result is cached under.
    pub cache_key: Option<String>,
    /// Relations of the nodes it declares as dependencies. Used as what it reads when
    /// its SQL can't tell (no SQL, or SQL that couldn't be analyzed).
    pub depends_on: Vec<RelationName>,
}

impl NodeLineage {
    /// Whether nothing is known about how this node uses its inputs: it has SQL that
    /// couldn't be analyzed, is a model without (SQL) lineage, or has upstreams but no
    /// lineage at all (e.g. a snapshot).
    pub fn is_opaque(&self) -> bool {
        match &self.lineage {
            Some(lineage) => lineage.opaque,
            None => self.kind == NodeKind::Model || !self.depends_on.is_empty(),
        }
    }
}

/// How a node uses an upstream column.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ColumnUse {
    /// The consuming node.
    pub node: String,
    /// The output column it feeds, or `None` if it shapes the node's rows.
    pub output: Option<String>,
    /// How.
    pub edge: EdgeKind,
}

/// Column-level lineage for a whole project.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ColumnGraph {
    pub(crate) nodes: BTreeMap<String, NodeLineage>,
    pub(crate) by_relation: BTreeMap<RelationName, String>,
    /// Upstream column → every place it is used.
    #[serde(skip)]
    pub(crate) consumers: BTreeMap<ColumnRef, BTreeSet<ColumnUse>>,
    /// Relation → nodes that read it.
    #[serde(skip)]
    pub(crate) readers: BTreeMap<RelationName, BTreeSet<String>>,
}

impl ColumnGraph {
    pub(crate) fn from_nodes(nodes: BTreeMap<String, NodeLineage>) -> Self {
        let by_relation = nodes
            .values()
            .map(|n| (n.relation.clone(), n.id.clone()))
            .collect();
        let mut consumers: BTreeMap<ColumnRef, BTreeSet<ColumnUse>> = BTreeMap::new();
        let mut readers: BTreeMap<RelationName, BTreeSet<String>> = BTreeMap::new();
        for node in nodes.values() {
            // A node reads what its SQL reads and, conservatively, everything it declares:
            // an opaque or SQL-less node must still be found as a reader.
            let sql_reads = node.lineage.iter().flat_map(|l| &l.relations_read);
            for relation in sql_reads.chain(&node.depends_on) {
                readers
                    .entry(relation.clone())
                    .or_default()
                    .insert(node.id.clone());
            }
            let Some(lineage) = &node.lineage else {
                continue;
            };
            for output in &lineage.outputs {
                for (column, edge) in &output.inputs {
                    consumers
                        .entry(column.clone())
                        .or_default()
                        .insert(ColumnUse {
                            node: node.id.clone(),
                            output: Some(output.name.clone()),
                            edge: *edge,
                        });
                }
            }
            for (column, kind) in &lineage.row_inputs {
                consumers
                    .entry(column.clone())
                    .or_default()
                    .insert(ColumnUse {
                        node: node.id.clone(),
                        output: None,
                        edge: EdgeKind::Indirect(*kind),
                    });
            }
        }
        Self {
            nodes,
            by_relation,
            consumers,
            readers,
        }
    }

    /// Every node, by id.
    pub fn nodes(&self) -> impl Iterator<Item = &NodeLineage> {
        self.nodes.values()
    }

    /// The node with this id.
    pub fn node(&self, id: &str) -> Option<&NodeLineage> {
        self.nodes.get(id)
    }

    /// The node that builds or reads `relation`.
    pub fn node_for(&self, relation: &RelationName) -> Option<&NodeLineage> {
        self.by_relation
            .get(relation)
            .and_then(|id| self.nodes.get(id))
    }

    /// Where an upstream column is used, sorted.
    pub fn uses_of(&self, column: &ColumnRef) -> impl Iterator<Item = &ColumnUse> {
        self.consumers.get(column).into_iter().flatten()
    }

    /// Nodes that read `relation`, sorted.
    pub fn readers_of(&self, relation: &RelationName) -> impl Iterator<Item = &str> {
        self.readers
            .get(relation)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// The immediate inputs of an output column, including row-shaping inputs that
    /// apply to every column of its node.
    pub fn inputs_of(&self, column: &ColumnRef) -> Vec<(ColumnRef, EdgeKind)> {
        let Some(lineage) = self
            .node_for(&column.relation)
            .and_then(|n| n.lineage.as_ref())
        else {
            return Vec::new();
        };
        let mut inputs: BTreeSet<(ColumnRef, EdgeKind)> = lineage
            .output(&column.column)
            .map(|o| o.inputs.clone())
            .unwrap_or_default();
        inputs.extend(
            lineage
                .row_inputs
                .iter()
                .map(|(c, k)| (c.clone(), EdgeKind::Indirect(*k))),
        );
        inputs.into_iter().collect()
    }

    /// Every column that `column` transitively depends on through `Direct` edges, i.e.
    /// where its values come from.
    pub fn value_sources(&self, column: &ColumnRef) -> BTreeSet<ColumnRef> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![column.clone()];
        let mut sources = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let direct: Vec<ColumnRef> = self
                .inputs_of(&current)
                .into_iter()
                .filter(|(_, e)| matches!(e, EdgeKind::Direct(_)))
                .map(|(c, _)| c)
                .collect();
            if direct.is_empty() && current != *column {
                sources.insert(current);
            } else {
                stack.extend(direct);
            }
        }
        sources
    }
}
