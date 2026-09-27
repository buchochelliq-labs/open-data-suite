//! Dependency order over ids, on [`petgraph`]: which ids come first, how deep each one
//! is, and, when there is no order, the cycle that prevents one.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::{astar, tarjan_scc, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::{EdgeRef, NodeFiltered};
use petgraph::{Direction, Graph};

/// A dependency cycle: each id depends on the one before it, and the first on the last.
/// It starts at its smallest id and is a shortest cycle through it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", display(.0))]
pub struct Cycle(pub Vec<String>);

fn display(ids: &[String]) -> String {
    let mut out = ids.join(" → ");
    if let Some(first) = ids.first() {
        out.push_str(" → ");
        out.push_str(first);
    }
    out
}

/// Orders `ids` so that each comes after everything it depends on, with its depth: 0
/// for an id that depends on nothing among `ids`, otherwise one more than its deepest
/// dependency. The result is sorted by depth, then id.
///
/// `edges` are `(dependency, dependent)` pairs; pairs naming an id not in `ids` are
/// ignored, and an id that depends on itself is a cycle.
///
/// # Errors
/// Returns a [`Cycle`] when the dependencies go round in a circle.
pub fn layers<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    edges: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<Vec<(&'a str, u32)>, Cycle> {
    let ids: BTreeSet<&str> = ids.into_iter().collect();
    let mut graph: DiGraph<&str, ()> = Graph::with_capacity(ids.len(), 0);
    let index: BTreeMap<&str, NodeIndex> = ids.iter().map(|id| (*id, graph.add_node(id))).collect();
    let edges: BTreeSet<(NodeIndex, NodeIndex)> = edges
        .into_iter()
        .filter_map(|(from, to)| Some((*index.get(from)?, *index.get(to)?)))
        .collect();
    for (from, to) in edges {
        graph.add_edge(from, to, ());
    }
    let order = toposort(&graph, None).map_err(|_| cycle(&graph))?;
    let mut depth = vec![0_u32; graph.node_count()];
    for node in order {
        depth[node.index()] = graph
            .edges_directed(node, Direction::Incoming)
            .map(|e| depth[e.source().index()] + 1)
            .max()
            .unwrap_or(0);
    }
    let mut out: Vec<(&str, u32)> = graph
        .node_indices()
        .map(|n| (graph[n], depth[n.index()]))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    Ok(out)
}

/// A shortest cycle through the smallest id that is on any cycle, so the same graph
/// always reports the same cycle.
fn cycle(graph: &DiGraph<&str, ()>) -> Cycle {
    let name = |n: &NodeIndex| graph[*n];
    let Some(component) = tarjan_scc(graph)
        .into_iter()
        .filter(|c| c.len() > 1 || graph.contains_edge(c[0], c[0]))
        .min_by_key(|c| c.iter().map(name).min())
    else {
        return Cycle(Vec::new());
    };
    let Some(start) = component.iter().copied().min_by_key(name) else {
        return Cycle(Vec::new());
    };
    if graph.contains_edge(start, start) {
        return Cycle(vec![graph[start].to_owned()]);
    }
    let on_cycle: BTreeSet<NodeIndex> = component.into_iter().collect();
    let inside = NodeFiltered::from_fn(graph, |n| on_cycle.contains(&n));
    let back = graph
        .neighbors(start)
        .filter(|n| on_cycle.contains(n))
        .filter_map(|next| astar(&inside, next, |n| n == start, |_| 1_u32, |_| 0).map(|(_, p)| p))
        .min_by(|a, b| {
            a.len()
                .cmp(&b.len())
                .then_with(|| a.iter().map(name).cmp(b.iter().map(name)))
        })
        .unwrap_or_default();
    Cycle(
        std::iter::once(start)
            .chain(back.into_iter().filter(|n| *n != start))
            .map(|n| graph[n].to_owned())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_order_by_depth_then_id() {
        let order = layers(
            ["d", "c", "b", "a", "x"],
            [
                ("a", "b"),
                ("a", "c"),
                ("b", "d"),
                ("c", "d"),
                ("a", "d"),
                ("zz", "a"),
            ],
        )
        .unwrap();
        assert_eq!(order, [("a", 0), ("x", 0), ("b", 1), ("c", 1), ("d", 2)]);
    }

    #[test]
    fn a_cycle_is_named_from_its_smallest_id() {
        // `d` is stuck behind the cycle but isn't on it; `a → a2` is not on it either.
        let err = layers(
            ["a", "a2", "b", "c", "d", "e"],
            [
                ("a", "a2"),
                ("b", "c"),
                ("c", "e"),
                ("e", "b"),
                ("c", "b"),
                ("e", "d"),
            ],
        )
        .unwrap_err();
        assert_eq!(err, Cycle(vec!["b".into(), "c".into()]));
        assert_eq!(err.to_string(), "b → c → b");

        let err = layers(["a", "b"], [("b", "b"), ("a", "b"), ("b", "a")]).unwrap_err();
        assert_eq!(err, Cycle(vec!["a".into(), "b".into()]));
        let err = layers(["a"], [("a", "a")]).unwrap_err();
        assert_eq!(err.to_string(), "a → a");
    }
}
