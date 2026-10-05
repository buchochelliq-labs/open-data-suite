//! Property tests for the State planner and recorder (#192), over random project
//! graphs: a plan doesn't depend on node order or selection, a change rebuilds exactly
//! the changed nodes and everything downstream of them (never reuse on top of new
//! data, rule 3), and a failed build is never recorded as built (rule 5).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::FreshnessPolicy;
use ods_core::state::{
    DataVersion, Exactness, Fingerprint, PlanAction, ReasonCode, SnapshotId, StateSnapshot,
    Timestamp,
};
use ods_state::{Node, Outcome, Project, RunResult, Source, plan, record, select};
use proptest::prelude::*;

const T0: i64 = 1_000_000;

/// Node `i`'s parents: earlier nodes, and maybe the one source.
#[derive(Debug, Clone)]
struct Graph(Vec<(BTreeSet<usize>, bool)>);

fn graph() -> impl Strategy<Value = Graph> {
    (1_usize..9).prop_flat_map(|n| {
        let nodes: Vec<_> = (0..n)
            .map(|i| {
                let parents = if i == 0 {
                    Just(BTreeSet::new()).boxed()
                } else {
                    prop::collection::btree_set(0..i, 0..3).boxed()
                };
                (parents, any::<bool>())
            })
            .collect();
        nodes.prop_map(Graph)
    })
}

fn id(i: usize) -> String {
    format!("model.p.n{i}")
}

/// The project, with node `i`'s code `codes[i]`, listed in `order`.
fn project(graph: &Graph, codes: &[String], order: &[usize]) -> Project {
    let nodes = order
        .iter()
        .map(|&i| {
            let (parents, reads_source) = &graph.0[i];
            let mut parent_ids: Vec<String> = parents.iter().map(|p| id(*p)).collect();
            if *reads_source {
                parent_ids.push("source.p.raw".to_owned());
            }
            let alone = parent_ids.is_empty();
            let node = Node::new(
                id(i),
                format!("n{i}"),
                "model",
                parent_ids,
                Ok(Fingerprint::from_content([("file", codes[i].as_str())])),
                FreshnessPolicy::conservative(),
            );
            // A node that reads nothing would otherwise always build: its data would be
            // unexplained (rule 3). Here it is static rows, like a seed.
            if alone { node.self_contained() } else { node }
        })
        .collect();
    Project::new(
        nodes,
        vec![
            Source::new(
                "source.p.raw",
                "raw",
                Some(DataVersion::new("v1", Exactness::Semantic, "sources.json")),
            )
            .observed_at(Some(Timestamp::from_unix(T0 + 50))),
        ],
    )
}

fn codes(graph: &Graph, changed: &BTreeSet<usize>) -> Vec<String> {
    (0..graph.0.len())
        .map(|i| {
            if changed.contains(&i) {
                format!("new{i}")
            } else {
                format!("old{i}")
            }
        })
        .collect()
}

fn in_order(graph: &Graph) -> Vec<usize> {
    (0..graph.0.len()).collect()
}

/// Everything built successfully at T0.
fn built(project: &Project) -> StateSnapshot {
    let results: Vec<RunResult> = project
        .nodes
        .iter()
        .map(|n| RunResult::new(&n.id, Outcome::Success, Some(Timestamp::from_unix(T0))))
        .collect();
    record(
        project,
        None,
        &results,
        "run-1",
        Timestamp::from_unix(T0),
        true,
    )
    .snapshot
}

fn decisions(
    project: &Project,
    state: Option<&StateSnapshot>,
    selected: &BTreeSet<String>,
) -> BTreeMap<String, (PlanAction, ReasonCode)> {
    plan(
        project,
        state.map(|s| (SnapshotId(1), s)),
        selected,
        Timestamp::from_unix(T0 + 60),
    )
    .unwrap()
    .entries
    .into_iter()
    .map(|e| (e.node, (e.action, e.reasons[0].code)))
    .collect()
}

/// `changed` and everything downstream of it.
fn downstream(graph: &Graph, changed: &BTreeSet<usize>) -> BTreeSet<usize> {
    let mut out = changed.clone();
    for (i, (parents, _)) in graph.0.iter().enumerate() {
        if parents.iter().any(|p| out.contains(p)) {
            out.insert(i);
        }
    }
    out
}

fn shuffled(mut order: Vec<usize>, mut seed: u64) -> Vec<usize> {
    for i in (1..order.len()).rev() {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = usize::try_from(seed >> 33).unwrap() % (i + 1);
        order.swap(i, j);
    }
    order
}

fn subset(n: usize) -> impl Strategy<Value = BTreeSet<usize>> {
    prop::collection::btree_set(0..n, 0..=n)
}

proptest! {
    #[test]
    fn a_change_rebuilds_exactly_it_and_what_is_downstream(
        (graph, changed) in graph().prop_flat_map(|g| { let s = subset(g.0.len()); (Just(g), s) }),
    ) {
        let before = project(&graph, &codes(&graph, &BTreeSet::new()), &in_order(&graph));
        let state = built(&before);
        let after = project(&graph, &codes(&graph, &changed), &in_order(&graph));
        let all = select(&after, &[]).unwrap();
        let rebuild = downstream(&graph, &changed);
        for (node, (action, code)) in decisions(&after, Some(&state), &all) {
            let i: usize = node.trim_start_matches("model.p.n").parse().unwrap();
            if rebuild.contains(&i) {
                prop_assert_eq!(action, PlanAction::Build, "{} ({:?}) should build", node, code);
            } else {
                prop_assert_eq!((action, code), (PlanAction::Reuse, ReasonCode::Unchanged), "{}", node);
            }
        }
        // Without state, everything builds.
        for (node, (action, _)) in decisions(&after, None, &all) {
            prop_assert_eq!(action, PlanAction::Build, "{}", node);
        }
    }

    #[test]
    fn neither_order_nor_selection_changes_a_decision(
        (graph, changed, selected) in graph().prop_flat_map(|g| { let n = g.0.len(); (Just(g), subset(n), subset(n)) }),
        seed in any::<u64>(),
    ) {
        let before = project(&graph, &codes(&graph, &BTreeSet::new()), &in_order(&graph));
        let state = built(&before);
        let codes = codes(&graph, &changed);
        let forward = project(&graph, &codes, &in_order(&graph));
        let mixed = project(&graph, &codes, &shuffled(in_order(&graph), seed));
        let all = select(&forward, &[]).unwrap();
        let full = decisions(&forward, Some(&state), &all);
        prop_assert_eq!(&decisions(&mixed, Some(&state), &all), &full);
        let some: BTreeSet<String> = selected.iter().map(|i| id(*i)).collect();
        let part = decisions(&forward, Some(&state), &some);
        prop_assert_eq!(part.keys().cloned().collect::<BTreeSet<_>>(), some.clone());
        for (node, decision) in part {
            prop_assert_eq!(&decision, &full[&node], "{}", node);
        }
    }

    #[test]
    fn a_failed_build_is_never_recorded_as_built(
        (graph, changed, failed) in graph().prop_flat_map(|g| { let n = g.0.len(); (Just(g), subset(n), subset(n)) }),
    ) {
        let before = project(&graph, &codes(&graph, &BTreeSet::new()), &in_order(&graph));
        let state = built(&before);
        let after = project(&graph, &codes(&graph, &changed), &in_order(&graph));
        let rebuild = downstream(&graph, &changed);
        // The run builds what the plan says; some of it fails, and what is downstream
        // of a failure is skipped.
        let mut broken = BTreeSet::new();
        let results: Vec<RunResult> = (0..graph.0.len())
            .filter(|i| rebuild.contains(i))
            .map(|i| {
                let outcome = if graph.0[i].0.iter().any(|p| broken.contains(p)) {
                    broken.insert(i);
                    Outcome::Skipped
                } else if failed.contains(&i) {
                    broken.insert(i);
                    Outcome::Failed
                } else {
                    Outcome::Success
                };
                RunResult::new(id(i), outcome, Some(Timestamp::from_unix(T0 + 30)))
            })
            .collect();
        let recorded = record(&after, Some((SnapshotId(1), &state)), &results, "run-2", Timestamp::from_unix(T0 + 40), true);
        let all = select(&after, &[]).unwrap();
        for (node, (action, code)) in decisions(&after, Some(&recorded.snapshot), &all) {
            let i: usize = node.trim_start_matches("model.p.n").parse().unwrap();
            if broken.contains(&i) {
                prop_assert_eq!(action, PlanAction::Build, "{} failed or was skipped, yet {:?}", node, code);
            }
            if !rebuild.contains(&i) {
                prop_assert_eq!(action, PlanAction::Reuse, "{} was untouched, yet {:?}", node, code);
            }
        }
    }
}
