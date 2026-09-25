//! Graph exports of a [`ColumnGraph`]: a documented JSON document (the contract the
//! local viewer and the VS Code extension read), plus DOT, Mermaid and `GraphML` for
//! Graphviz, Markdown and graph tools (Gephi, yEd, Neo4j).
//!
//! Every export is deterministic: nodes and edges are sorted, so the same graph always
//! produces byte-identical output.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

use ods_core::{ColumnRef, Confidence, EdgeKind};
use serde::Serialize;

use crate::graph::ColumnGraph;
use crate::project::NodeKind;

/// Version of the [`GraphDocument`] JSON format.
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

/// One end of a column edge: a node, and a column of it (`None` = the node's rows).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Endpoint {
    /// Node id.
    pub node: String,
    /// Column name; `None` for dataset-wide (row-shaping) edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

/// A column-level edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ColumnEdge {
    /// The input column.
    pub from: Endpoint,
    /// The output column, or the output node for row-shaping inputs.
    pub to: Endpoint,
    /// How the input is used.
    pub kind: EdgeKind,
}

/// A node in the exported graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GraphNode {
    /// Stable id, e.g. `model.jaffle.orders`.
    pub id: String,
    /// Display name, e.g. `orders`.
    pub name: String,
    /// What it is.
    pub kind: NodeKind,
    /// The relation it builds or reads.
    pub relation: String,
    /// Columns, in order.
    pub columns: Vec<String>,
    /// Lineage confidence; `None` for nodes without SQL.
    pub confidence: Option<Confidence>,
    /// Whether the node's SQL couldn't be analyzed.
    pub opaque: bool,
    /// Analyzer notes.
    pub diagnostics: Vec<String>,
    /// Dependency depth (0 = no upstream in the graph), for layered layouts.
    pub layer: usize,
}

/// A node-level edge (the node reads the other).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct NodeEdge {
    /// Upstream node id.
    pub from: String,
    /// Downstream node id.
    pub to: String,
}

/// The whole graph as one JSON document (format version [`GRAPH_SCHEMA_VERSION`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GraphDocument {
    /// Format version.
    pub schema_version: u32,
    /// Nodes, sorted by id.
    pub nodes: Vec<GraphNode>,
    /// Node-level edges, sorted.
    pub node_edges: Vec<NodeEdge>,
    /// Column-level edges, sorted.
    pub column_edges: Vec<ColumnEdge>,
}

/// What to include.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GraphFilter {
    /// Keep only nodes connected to these (node id, optional column) focus points.
    pub focus: Vec<Endpoint>,
    /// Maximum hops upstream from the focus (`None` = unlimited).
    pub upstream: Option<usize>,
    /// Maximum hops downstream from the focus (`None` = unlimited).
    pub downstream: Option<usize>,
}

impl GraphFilter {
    /// A filter focused on `focus`, walking without limit in both directions.
    pub fn focused(focus: Vec<Endpoint>) -> Self {
        Self {
            focus,
            upstream: None,
            downstream: None,
        }
    }

    /// Limits the walk.
    #[must_use]
    pub fn with_depth(mut self, upstream: Option<usize>, downstream: Option<usize>) -> Self {
        self.upstream = upstream;
        self.downstream = downstream;
        self
    }
}

impl ColumnGraph {
    /// The graph as a [`GraphDocument`]. `name` gives each node's display name.
    pub fn document(&self, name: &dyn Fn(&str) -> String, filter: &GraphFilter) -> GraphDocument {
        let mut column_edges = BTreeSet::new();
        let mut node_edges = BTreeSet::new();
        for node in self.nodes() {
            let Some(lineage) = &node.lineage else {
                continue;
            };
            for relation in &lineage.relations_read {
                if let Some(upstream) = self.node_for(relation) {
                    node_edges.insert(NodeEdge {
                        from: upstream.id.clone(),
                        to: node.id.clone(),
                    });
                }
            }
            let endpoint = |column: &ColumnRef| {
                self.node_for(&column.relation).map(|n| Endpoint {
                    node: n.id.clone(),
                    column: Some(column.column.clone()),
                })
            };
            for output in &lineage.outputs {
                for (column, kind) in &output.inputs {
                    if let Some(from) = endpoint(column) {
                        column_edges.insert(ColumnEdge {
                            from,
                            to: Endpoint {
                                node: node.id.clone(),
                                column: Some(output.name.clone()),
                            },
                            kind: *kind,
                        });
                    }
                }
            }
            for (column, kind) in &lineage.row_inputs {
                if let Some(from) = endpoint(column) {
                    column_edges.insert(ColumnEdge {
                        from,
                        to: Endpoint {
                            node: node.id.clone(),
                            column: None,
                        },
                        kind: EdgeKind::Indirect(*kind),
                    });
                }
            }
        }

        let keep = if filter.focus.is_empty() {
            None
        } else {
            Some(Self::reachable(filter, &column_edges, &node_edges))
        };
        let kept = |id: &str| keep.as_ref().is_none_or(|k| k.contains(id));
        let node_edges: Vec<NodeEdge> = node_edges
            .into_iter()
            .filter(|e| kept(&e.from) && kept(&e.to))
            .collect();
        let column_edges: Vec<ColumnEdge> = column_edges
            .into_iter()
            .filter(|e| kept(&e.from.node) && kept(&e.to.node))
            .collect();
        let layers = layers(self.nodes().map(|n| n.id.as_str()), &node_edges);
        let nodes = self
            .nodes()
            .filter(|n| kept(&n.id))
            .map(|n| GraphNode {
                id: n.id.clone(),
                name: name(&n.id),
                kind: n.kind,
                relation: n.relation.to_string(),
                columns: n.columns.clone(),
                confidence: n.lineage.as_ref().map(|l| l.confidence),
                opaque: n.is_opaque(),
                diagnostics: n
                    .lineage
                    .as_ref()
                    .map(|l| l.diagnostics.clone())
                    .unwrap_or_default(),
                layer: layers.get(n.id.as_str()).copied().unwrap_or(0),
            })
            .collect();
        GraphDocument {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes,
            node_edges,
            column_edges,
        }
    }

    /// Node ids reachable from the focus, following column edges when the focus names
    /// a column and node edges otherwise.
    fn reachable(
        filter: &GraphFilter,
        column_edges: &BTreeSet<ColumnEdge>,
        node_edges: &BTreeSet<NodeEdge>,
    ) -> BTreeSet<String> {
        let mut keep: BTreeSet<String> = filter.focus.iter().map(|f| f.node.clone()).collect();
        for downstream in [true, false] {
            let limit = if downstream {
                filter.downstream
            } else {
                filter.upstream
            };
            let mut seen: BTreeSet<Endpoint> = BTreeSet::new();
            let mut queue: VecDeque<(Endpoint, usize)> =
                filter.focus.iter().map(|f| (f.clone(), 0)).collect();
            while let Some((at, depth)) = queue.pop_front() {
                if !seen.insert(at.clone()) || limit.is_some_and(|l| depth >= l) {
                    continue;
                }
                let next: Vec<Endpoint> = if at.column.is_some() {
                    // A column reaches what it feeds, or what feeds it. A row-shaping
                    // edge reaches every column of the node.
                    column_edges
                        .iter()
                        .filter_map(|e| {
                            let (here, there) = if downstream {
                                (&e.from, &e.to)
                            } else {
                                (&e.to, &e.from)
                            };
                            let matches = here.node == at.node
                                && (here.column == at.column || here.column.is_none());
                            matches.then(|| there.clone())
                        })
                        .collect()
                } else {
                    node_edges
                        .iter()
                        .filter_map(|e| {
                            if downstream && e.from == at.node {
                                Some(e.to.clone())
                            } else if !downstream && e.to == at.node {
                                Some(e.from.clone())
                            } else {
                                None
                            }
                        })
                        .map(|node| Endpoint { node, column: None })
                        .collect()
                };
                // A row-shaping target (no column) continues at node level: all of
                // its rows, so everything downstream of it, may change.
                for endpoint in next {
                    keep.insert(endpoint.node.clone());
                    queue.push_back((endpoint, depth + 1));
                }
            }
        }
        keep
    }
}

/// Longest-path depth of every node.
fn layers<'a>(ids: impl Iterator<Item = &'a str>, edges: &[NodeEdge]) -> BTreeMap<&'a str, usize> {
    let ids: Vec<&str> = ids.collect();
    let mut upstream: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        if let (Some(from), Some(to)) = (
            ids.iter().find(|i| **i == edge.from),
            ids.iter().find(|i| **i == edge.to),
        ) {
            upstream.entry(*to).or_default().push(*from);
        }
    }
    let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
    let mut stack = BTreeSet::new();
    for id in &ids {
        visit(id, &upstream, &mut depth, &mut stack);
    }
    depth
}

/// Depth of `id`: one more than its deepest upstream.
fn visit<'a>(
    id: &'a str,
    upstream: &BTreeMap<&'a str, Vec<&'a str>>,
    depth: &mut BTreeMap<&'a str, usize>,
    stack: &mut BTreeSet<&'a str>,
) -> usize {
    if let Some(d) = depth.get(id) {
        return *d;
    }
    if !stack.insert(id) {
        return 0; // a cycle; builds reject these, so this is only defensive
    }
    let d = upstream
        .get(id)
        .into_iter()
        .flatten()
        .map(|u| visit(u, upstream, depth, stack) + 1)
        .max()
        .unwrap_or(0);
    stack.remove(id);
    depth.insert(id, d);
    d
}

fn edge_label(kind: EdgeKind) -> String {
    match kind {
        EdgeKind::Direct(k) => k.openlineage_subtype().to_lowercase(),
        EdgeKind::Indirect(k) => k.openlineage_subtype().to_lowercase(),
    }
}

fn dot_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn port(index: usize) -> String {
    format!("c{index}")
}

impl GraphDocument {
    fn column_index(&self) -> BTreeMap<(&str, &str), usize> {
        self.nodes
            .iter()
            .flat_map(|n| {
                n.columns
                    .iter()
                    .enumerate()
                    .map(move |(i, c)| ((n.id.as_str(), c.as_str()), i))
            })
            .collect()
    }

    /// Graphviz DOT. With `columns`, each node is a table of its columns and edges run
    /// between columns (indirect edges dashed); otherwise, a model-level graph.
    pub fn to_dot(&self, columns: bool) -> String {
        let mut out = String::from(
            "digraph lineage {\n  rankdir=LR;\n  node [shape=plaintext, fontname=\"Helvetica\"];\n  edge [color=\"#607080\"];\n",
        );
        let index = self.column_index();
        for node in &self.nodes {
            let header = format!(
                "<tr><td bgcolor=\"{}\"><b>{}</b></td></tr>",
                if node.opaque { "#f6d7a7" } else { "#dbe7f3" },
                dot_escape(&node.name)
            );
            let mut rows = String::new();
            if columns {
                for (i, c) in node.columns.iter().enumerate() {
                    let _ = write!(
                        rows,
                        "<tr><td port=\"{}\" align=\"left\">{}</td></tr>",
                        port(i),
                        dot_escape(c)
                    );
                }
            }
            let _ = writeln!(
                out,
                "  \"{}\" [label=<<table border=\"0\" cellborder=\"1\" cellspacing=\"0\">{header}{rows}</table>>];",
                node.id
            );
        }
        if columns {
            for edge in &self.column_edges {
                let from = edge
                    .from
                    .column
                    .as_deref()
                    .and_then(|c| index.get(&(edge.from.node.as_str(), c)))
                    .map_or(String::new(), |i| format!(":{}", port(*i)));
                let to = edge
                    .to
                    .column
                    .as_deref()
                    .and_then(|c| index.get(&(edge.to.node.as_str(), c)))
                    .map_or(String::new(), |i| format!(":{}", port(*i)));
                let style = match edge.kind {
                    EdgeKind::Direct(_) => "solid",
                    EdgeKind::Indirect(_) => "dashed",
                };
                let _ = writeln!(
                    out,
                    "  \"{}\"{from} -> \"{}\"{to} [style={style}, tooltip=\"{}\"];",
                    edge.from.node,
                    edge.to.node,
                    edge_label(edge.kind)
                );
            }
        } else {
            for edge in &self.node_edges {
                let _ = writeln!(out, "  \"{}\" -> \"{}\";", edge.from, edge.to);
            }
        }
        out.push_str("}\n");
        out
    }

    /// A Mermaid flowchart of the node-level graph (column graphs are too dense for
    /// Mermaid to lay out legibly).
    pub fn to_mermaid(&self) -> String {
        let ids: BTreeMap<&str, String> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), format!("n{i}")))
            .collect();
        let mut out = String::from("flowchart LR\n");
        for node in &self.nodes {
            let label = node.name.replace('"', "'");
            let shape = match node.kind {
                NodeKind::Source | NodeKind::Seed => format!("[(\"{label}\")]"),
                _ => format!("[\"{label}\"]"),
            };
            let _ = writeln!(out, "  {}{shape}", ids[node.id.as_str()]);
        }
        for edge in &self.node_edges {
            let _ = writeln!(
                out,
                "  {} --> {}",
                ids[edge.from.as_str()],
                ids[edge.to.as_str()]
            );
        }
        let opaque: Vec<&str> = self
            .nodes
            .iter()
            .filter(|n| n.opaque)
            .map(|n| ids[n.id.as_str()].as_str())
            .collect();
        if !opaque.is_empty() {
            let _ = writeln!(out, "  classDef opaque fill:#f6d7a7,stroke:#b07a2a");
            let _ = writeln!(out, "  class {} opaque", opaque.join(","));
        }
        out
    }

    /// `GraphML` with one vertex per node and per column, `contains` edges from nodes to
    /// their columns, and `lineage` edges between columns.
    pub fn to_graphml(&self) -> String {
        let esc = |t: &str| {
            t.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let mut out = String::from(concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n",
            "  <key id=\"label\" for=\"node\" attr.name=\"label\" attr.type=\"string\"/>\n",
            "  <key id=\"type\" for=\"node\" attr.name=\"type\" attr.type=\"string\"/>\n",
            "  <key id=\"relation\" for=\"node\" attr.name=\"relation\" attr.type=\"string\"/>\n",
            "  <key id=\"edge\" for=\"edge\" attr.name=\"kind\" attr.type=\"string\"/>\n",
            "  <key id=\"subtype\" for=\"edge\" attr.name=\"subtype\" attr.type=\"string\"/>\n",
            "  <graph id=\"lineage\" edgedefault=\"directed\">\n"
        ));
        let kind_name = |k: NodeKind| match k {
            NodeKind::Model => "model",
            NodeKind::Seed => "seed",
            NodeKind::Snapshot => "snapshot",
            NodeKind::Source => "source",
        };
        let mut edge_id = 0usize;
        for node in &self.nodes {
            let _ = writeln!(
                out,
                "    <node id=\"{}\"><data key=\"label\">{}</data><data key=\"type\">{}</data><data key=\"relation\">{}</data></node>",
                esc(&node.id),
                esc(&node.name),
                kind_name(node.kind),
                esc(&node.relation)
            );
            for column in &node.columns {
                let _ = writeln!(
                    out,
                    "    <node id=\"{}#{}\"><data key=\"label\">{}</data><data key=\"type\">column</data></node>",
                    esc(&node.id),
                    esc(column),
                    esc(column)
                );
                let _ = writeln!(
                    out,
                    "    <edge id=\"e{edge_id}\" source=\"{}\" target=\"{}#{}\"><data key=\"edge\">contains</data></edge>",
                    esc(&node.id),
                    esc(&node.id),
                    esc(column)
                );
                edge_id += 1;
            }
        }
        let vertex = |e: &Endpoint| match &e.column {
            Some(c) => format!("{}#{}", esc(&e.node), esc(c)),
            None => esc(&e.node),
        };
        for edge in &self.column_edges {
            let kind = match edge.kind {
                EdgeKind::Direct(_) => "direct",
                EdgeKind::Indirect(_) => "indirect",
            };
            let _ = writeln!(
                out,
                "    <edge id=\"e{edge_id}\" source=\"{}\" target=\"{}\"><data key=\"edge\">{kind}</data><data key=\"subtype\">{}</data></edge>",
                vertex(&edge.from),
                vertex(&edge.to),
                edge_label(edge.kind)
            );
            edge_id += 1;
        }
        out.push_str("  </graph>\n</graphml>\n");
        out
    }
}
