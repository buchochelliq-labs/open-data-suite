//! Property tests for impact analysis (#192). Over random projects with scripted
//! lineage, impact never misses a node that a brute-force propagation says may change
//! (rule 3: never under-report), never drops a reader of a changed relation silently
//! (rule 4: an unaffected reader is reported as pruned), and doesn't depend on the order
//! nodes are listed in. An exported graph document never names a node it doesn't hold,
//! whatever it is focused on.

use std::collections::BTreeSet;

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_lineage::export::Endpoint;
use ods_lineage::{
    Change, ColumnChangeKind, ColumnGraph, GraphFilter, Impact, LineageNode, LineageProject,
    MemoryCache, NodeKind, build,
};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use proptest::prelude::*;

const COLUMNS: usize = 3;

fn rel(i: usize) -> RelationName {
    RelationName::new(["db", &format!("n{i}")]).unwrap()
}

fn col(i: usize, c: usize) -> ColumnRef {
    ColumnRef::new(rel(i), format!("c{c}"))
}

/// One model: what it reads, and per output column the upstream columns feeding it.
#[derive(Debug, Clone)]
struct Model {
    reads: Vec<usize>,
    /// `outputs[c]`: `(upstream node, upstream column, direct?)`.
    outputs: Vec<Vec<(usize, usize, bool)>>,
    /// Upstream columns that shape the rows (filters, joins).
    rows: Vec<(usize, usize)>,
    opaque: bool,
}

/// `seeds` seed nodes, then models, each reading only nodes before it: acyclic.
#[derive(Debug, Clone)]
struct Project {
    seeds: usize,
    models: Vec<Model>,
}

impl Project {
    fn len(&self) -> usize {
        self.seeds + self.models.len()
    }

    fn model(&self, i: usize) -> Option<&Model> {
        i.checked_sub(self.seeds).map(|m| &self.models[m])
    }
}

fn model(before: usize) -> impl Strategy<Value = Model> {
    let column = (0..before, 0..COLUMNS);
    (
        prop::collection::vec(
            (column.clone(), any::<bool>()).prop_map(|((n, c), d)| (n, c, d)),
            0..3,
        ),
        prop::collection::vec(
            prop::collection::vec(
                (column.clone(), any::<bool>()).prop_map(|((n, c), d)| (n, c, d)),
                0..3,
            ),
            COLUMNS,
        ),
        prop::collection::vec(column, 0..2),
        prop::bool::weighted(0.15),
        prop::collection::btree_set(0..before, 1..3),
    )
        .prop_map(|(_, outputs, rows, opaque, extra)| {
            // It reads every relation it takes a column from, and maybe more.
            let mut reads: BTreeSet<usize> = extra;
            reads.extend(outputs.iter().flatten().map(|(n, _, _)| *n));
            reads.extend(rows.iter().map(|(n, _)| *n));
            Model {
                reads: reads.into_iter().collect(),
                outputs,
                rows,
                opaque,
            }
        })
}

fn project() -> impl Strategy<Value = Project> {
    (1_usize..3, 1_usize..6).prop_flat_map(|(seeds, models)| {
        let strategies: Vec<_> = (0..models).map(|m| model(seeds + m).boxed()).collect();
        (Just(seeds), strategies).prop_map(|(seeds, models)| Project { seeds, models })
    })
}

fn lineage(model: &Model) -> QueryLineage {
    let reads: BTreeSet<RelationName> = model.reads.iter().map(|n| rel(*n)).collect();
    if model.opaque {
        return QueryLineage::opaque(reads, "scripted as opaque");
    }
    let outputs = model
        .outputs
        .iter()
        .enumerate()
        .map(|(c, inputs)| {
            OutputColumn::new(
                format!("c{c}"),
                inputs
                    .iter()
                    .map(|(n, uc, direct)| {
                        let kind = if *direct {
                            EdgeKind::Direct(DirectKind::Transformation)
                        } else {
                            EdgeKind::Indirect(IndirectKind::Conditional)
                        };
                        (col(*n, *uc), kind)
                    })
                    .collect(),
                format!("digest:{c}:{inputs:?}"),
                Confidence::Exact,
            )
        })
        .collect();
    let rows = model
        .rows
        .iter()
        .map(|(n, c)| (col(*n, *c), IndirectKind::Filter))
        .collect();
    QueryLineage::new(outputs, rows, reads, "rows", vec![])
}

/// The project as nodes, in `order`.
fn graph(project: &Project, order: &[usize]) -> ColumnGraph {
    let mut analyzer = FakeSqlLineageAnalyzer::new();
    let mut nodes = Vec::new();
    for &i in order {
        let id = format!("n{i}");
        match project.model(i) {
            None => nodes.push(
                LineageNode::new(&id, rel(i), NodeKind::Seed)
                    .with_columns((0..COLUMNS).map(|c| format!("c{c}"))),
            ),
            Some(model) => {
                let sql = format!("{id}.sql");
                analyzer = analyzer.with(sql.clone(), lineage(model));
                nodes.push(
                    LineageNode::new(&id, rel(i), NodeKind::Model)
                        .with_sql(sql)
                        .with_depends_on(model.reads.iter().map(|n| format!("n{n}"))),
                );
            }
        }
    }
    build(
        &LineageProject::new(nodes),
        &analyzer,
        &MemoryCache::default(),
    )
    .unwrap()
    .0
}

/// Which nodes may change, by brute force: a column changes if any input to it does,
/// and a node's rows change if a row input changes, it reads a relation whose rows
/// change, or its lineage is unknown and it reads anything that changes.
fn expected(project: &Project, change: &Change) -> BTreeSet<usize> {
    let mut columns: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut rows: BTreeSet<usize> = BTreeSet::new();
    let mut touched: BTreeSet<usize> = BTreeSet::new();
    match change {
        Change::Column { column, .. } => {
            let n = (0..project.len())
                .find(|n| rel(*n) == column.relation)
                .unwrap();
            let c: usize = column.column[1..].parse().unwrap();
            columns.insert((n, c));
            touched.insert(n);
        }
        Change::Rows { relation } => {
            let n = (0..project.len()).find(|n| rel(*n) == *relation).unwrap();
            rows.insert(n);
            touched.insert(n);
        }
    }
    let changed_col = |columns: &BTreeSet<(usize, usize)>, rows: &BTreeSet<usize>, n, c| {
        columns.contains(&(n, c)) || rows.contains(&n)
    };
    let mut impacted = BTreeSet::new();
    for i in project.seeds..project.len() {
        let model = project.model(i).unwrap();
        if model.opaque {
            if model.reads.iter().any(|n| touched.contains(n)) {
                impacted.insert(i);
                rows.insert(i);
                touched.insert(i);
            }
            continue;
        }
        let row_change = model.reads.iter().any(|n| rows.contains(n))
            || model
                .rows
                .iter()
                .any(|(n, c)| changed_col(&columns, &rows, *n, *c));
        let mut any = row_change;
        if row_change {
            rows.insert(i);
        }
        for (c, inputs) in model.outputs.iter().enumerate() {
            if inputs
                .iter()
                .any(|(n, uc, _)| changed_col(&columns, &rows, *n, *uc))
            {
                columns.insert((i, c));
                any = true;
            }
        }
        if any {
            impacted.insert(i);
            touched.insert(i);
        }
    }
    impacted
}

fn impacted(impact: &Impact) -> BTreeSet<usize> {
    impact
        .node_ids()
        .map(|id| id[1..].parse().unwrap())
        .collect()
}

fn change() -> impl Strategy<Value = (usize, Option<usize>)> {
    (0_usize..8, prop::option::of(0..COLUMNS))
}

proptest! {
    #[test]
    fn impact_never_misses_a_node_that_may_change(
        project in project(),
        (node, column) in change(),
    ) {
        let node = node % project.len();
        let change = match column {
            Some(c) => Change::Column { column: col(node, c), kind: ColumnChangeKind::Modified },
            None => Change::Rows { relation: rel(node) },
        };
        let order: Vec<usize> = (0..project.len()).collect();
        let impact = graph(&project, &order).impact(std::slice::from_ref(&change));
        let got = impacted(&impact);
        let want = expected(&project, &change);
        prop_assert!(want.is_subset(&got), "missed {:?} for {:?} in {:?}", want.difference(&got).collect::<Vec<_>>(), change, project);

        // Every direct reader of the changed relation is impacted or pruned, never
        // silently left out.
        let pruned: BTreeSet<usize> = impact.pruned.iter().map(|p| p.node[1..].parse().unwrap()).collect();
        for i in project.seeds..project.len() {
            if project.model(i).unwrap().reads.contains(&node) {
                prop_assert!(got.contains(&i) || pruned.contains(&i), "n{} reads n{} but is neither impacted nor pruned", i, node);
            }
        }
        prop_assert!(got.is_disjoint(&pruned), "a node is both impacted and pruned");
    }

    #[test]
    fn listing_order_changes_nothing(
        project in project(),
        (node, column) in change(),
        seed in any::<u64>(),
    ) {
        let node = node % project.len();
        let change = match column {
            Some(c) => Change::Column { column: col(node, c), kind: ColumnChangeKind::Modified },
            None => Change::Rows { relation: rel(node) },
        };
        let forward: Vec<usize> = (0..project.len()).collect();
        let mut shuffled = forward.clone();
        // A deterministic shuffle from `seed`.
        let mut state = seed;
        for i in (1..shuffled.len()).rev() {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let j = usize::try_from(state >> 33).unwrap() % (i + 1);
            shuffled.swap(i, j);
        }
        let a = graph(&project, &forward).impact(std::slice::from_ref(&change));
        let b = graph(&project, &shuffled).impact(std::slice::from_ref(&change));
        prop_assert_eq!(a, b);
    }
}

/// The oracle checks something only if changes often reach a model.
#[test]
fn most_changes_reach_a_model() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = (project(), change());
    let total = 200;
    let reached = (0..total)
        .filter(|_| {
            let (project, (node, column)) = strategy.new_tree(&mut runner).unwrap().current();
            let node = node % project.len();
            let change = match column {
                Some(c) => Change::Column {
                    column: col(node, c),
                    kind: ColumnChangeKind::Modified,
                },
                None => Change::Rows {
                    relation: rel(node),
                },
            };
            !expected(&project, &change).is_empty()
        })
        .count();
    assert!(
        reached * 4 > total,
        "only {reached} of {total} changes reach a model"
    );
}

proptest! {
    #[test]
    fn a_graph_document_has_no_phantom_nodes(
        project in project(),
        focus in prop::option::of((0_usize..8, prop::option::of(0..COLUMNS))),
        upstream in prop::option::of(0_usize..3),
        downstream in prop::option::of(0_usize..3),
    ) {
        let order: Vec<usize> = (0..project.len()).collect();
        let built = graph(&project, &order);
        let name = |id: &str| id.to_owned();
        let full = built.document(&name, &GraphFilter::default());
        let filter = match focus {
            None => GraphFilter::default(),
            Some((node, column)) => GraphFilter::focused(vec![Endpoint {
                node: format!("n{}", node % project.len()),
                column: column.map(|c| format!("c{c}")),
            }])
            .with_depth(upstream, downstream),
        };
        let reversed: Vec<usize> = order.iter().rev().copied().collect();
        let document = built.document(&name, &filter);
        prop_assert_eq!(
            &document,
            &graph(&project, &reversed).document(&name, &filter),
            "listing order changes the document"
        );

        let ids: Vec<&str> = document.nodes.iter().map(|n| n.id.as_str()).collect();
        let held: BTreeSet<&str> = ids.iter().copied().collect();
        prop_assert_eq!(held.len(), ids.len(), "a node is listed twice");
        prop_assert!(ids.windows(2).all(|w| w[0] < w[1]), "nodes aren't sorted by id");
        let all: BTreeSet<&str> = full.nodes.iter().map(|n| n.id.as_str()).collect();
        prop_assert!(held.is_subset(&all), "the filter invented a node");
        prop_assert_eq!(all.len(), project.len(), "the unfiltered document drops a node");
        for edge in &document.node_edges {
            prop_assert!(held.contains(edge.from.as_str()) && held.contains(edge.to.as_str()), "{:?}", edge);
        }
        for edge in &document.column_edges {
            prop_assert!(held.contains(edge.from.node.as_str()) && held.contains(edge.to.node.as_str()), "{:?}", edge);
        }
    }
}
