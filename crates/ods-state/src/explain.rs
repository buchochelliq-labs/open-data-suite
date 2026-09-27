//! Explaining decisions (#21): why a node builds or is reused, traced to the upstream
//! causes; what changed between two recorded builds of a node; and how two states, or
//! a state and the project now, differ.
//!
//! History is explained from what snapshots record (fingerprints with their
//! components, the data versions a build read, the runs of the parents it read), so a
//! past rebuild stays explainable without keeping past plans. What those records can't
//! show (a full refresh, a missing relation) is said to be unrecorded,
//! never guessed (AGENTS.md rule 3).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{
    ExecutionPlan, NodeState, PlanAction, PlanEntry, ReasonCode, SnapshotId, StateSnapshot,
    Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::Project;
use crate::planner::is_code_change;

/// Whether a parent built for `parent` is what the planner counts for a child built
/// for `child`, as it decides: only those parents are the child's causes. A parent that
/// builds for another reason isn't one (e.g. a parent with new data, for a child built
/// because another parent's code changed).
fn caused_by(child: ReasonCode, parent: ReasonCode) -> bool {
    let refresh = |c| {
        matches!(
            c,
            ReasonCode::FullRefreshRequested | ReasonCode::UpstreamFullRefresh
        )
    };
    match child {
        ReasonCode::UpstreamCodeChanged => is_code_change(parent),
        ReasonCode::UpstreamFullRefresh => refresh(parent),
        ReasonCode::NewUpstreamData => !is_code_change(parent) && !refresh(parent),
        // Its unknown parents aren't planned, so no planned parent explains it.
        _ => false,
    }
}

/// A node's decision, with the decisions upstream that caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Explanation {
    /// The node's plan entry: action, reasons and evidence.
    pub entry: PlanEntry,
    /// The parents that build and are why this one does, each explained in turn.
    pub causes: Vec<Explanation>,
    /// Already explained above, through another path: its causes aren't repeated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeated: bool,
}

/// Why `node` gets its action in `plan`, traced upstream to the root causes: a node
/// built because a parent is built is explained by that parent's reasons. `None` if
/// the plan doesn't have the node.
pub fn explain(plan: &ExecutionPlan, node: &str) -> Option<Explanation> {
    let entries: BTreeMap<&str, &PlanEntry> =
        plan.entries.iter().map(|e| (e.node.as_str(), e)).collect();
    let root = entries.get(node)?;
    let mut seen = BTreeSet::new();
    Some(trace(root, &entries, &mut seen))
}

fn trace<'a>(
    entry: &'a PlanEntry,
    entries: &BTreeMap<&str, &'a PlanEntry>,
    seen: &mut BTreeSet<&'a str>,
) -> Explanation {
    let repeated = !seen.insert(entry.node.as_str());
    // The planner gives a build one reason: the one that decided it.
    let reason = entry.reasons.first().map(|r| r.code);
    let causes = match reason {
        Some(child) if !repeated && entry.action == PlanAction::Build => entry
            .depends_on
            .iter()
            .filter_map(|p| entries.get(p.as_str()))
            .filter(|p| {
                p.action == PlanAction::Build
                    && p.reasons.first().is_some_and(|r| caused_by(child, r.code))
            })
            .map(|p| trace(p, entries, seen))
            .collect(),
        _ => Vec::new(),
    };
    Explanation {
        entry: entry.clone(),
        causes,
        repeated,
    }
}

/// One thing that differs between two recorded builds of a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum Change {
    /// Its code: fingerprint components that changed, were added or removed.
    Code {
        /// Components with other content.
        changed: Vec<String>,
        /// New components.
        added: Vec<String>,
        /// Components that went away.
        removed: Vec<String>,
    },
    /// Its code can't be fingerprinted completely now, and why.
    CodeUnknown {
        /// Why.
        why: String,
    },
    /// The version of a source's data it read.
    Data {
        /// The source.
        source: String,
        /// The version before, if one was known.
        before: Option<String>,
        /// The version after, if one is known.
        after: Option<String>,
    },
    /// The target it was built in (another host, account, database or profile), as
    /// the snapshots record it: its state then started afresh.
    Target {
        /// The target before, if one was recorded.
        before: Option<String>,
        /// The target after, if one was recorded.
        after: Option<String>,
    },
    /// A parent it read was rebuilt.
    Upstream {
        /// The parent.
        parent: String,
        /// The run of the parent's build it read before.
        before: Option<String>,
        /// The run of the parent's build it read after.
        after: Option<String>,
    },
}

/// What differs between two recorded builds of a node, from what they record.
pub fn changes(before: &NodeState, after: &NodeState) -> Vec<Change> {
    let mut changes = Vec::new();
    let code = after.fingerprint.diff(&before.fingerprint);
    if !code.is_empty() {
        changes.push(Change::Code {
            changed: code.changed,
            added: code.added,
            removed: code.removed,
        });
    }
    changes.extend(data_changes(&before.inputs, |source| {
        after
            .inputs
            .get(source)
            .map(|v| v.as_ref().map(|v| v.value.clone()))
    }));
    for (source, version) in &after.inputs {
        if !before.inputs.contains_key(source) {
            changes.push(Change::Data {
                source: source.clone(),
                before: None,
                after: version.as_ref().map(|v| v.value.clone()),
            });
        }
    }
    let parents: BTreeSet<&String> = before.parents.keys().chain(after.parents.keys()).collect();
    for parent in parents {
        let (b, a) = (before.parents.get(parent), after.parents.get(parent));
        if b != a {
            changes.push(Change::Upstream {
                parent: parent.clone(),
                before: b.cloned(),
                after: a.cloned(),
            });
        }
    }
    changes
}

/// Data versions `recorded` differ from, where `now` gives the version now of a source
/// (`None`: the source isn't read any more; `Some(None)`: read, version unknown).
fn data_changes(
    recorded: &BTreeMap<String, Option<ods_core::state::DataVersion>>,
    now: impl Fn(&str) -> Option<Option<String>>,
) -> Vec<Change> {
    recorded
        .iter()
        .filter_map(|(source, version)| {
            let before = version.as_ref().map(|v| v.value.clone());
            let after = now(source)?;
            (before != after).then(|| Change::Data {
                source: source.clone(),
                before,
                after,
            })
        })
        .collect()
}

/// Something that happened to a node, as snapshots record it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum NodeEvent {
    /// A build was recorded.
    Built {
        /// The snapshot that recorded it.
        snapshot: SnapshotId,
        /// The run that built it.
        run_id: String,
        /// When the build finished.
        built_at: Timestamp,
        /// Its first recorded build: nothing to compare with.
        first: bool,
        /// What differs from the build before. Empty for a rebuild the records can't
        /// explain (e.g. a full refresh or a missing relation).
        changes: Vec<Change>,
    },
    /// Its build passed its checks in a later run.
    Tested {
        /// The snapshot that recorded it.
        snapshot: SnapshotId,
        /// The run that ran the checks.
        run_id: String,
        /// When they finished.
        at: Timestamp,
    },
    /// Its state was dropped: the node was gone from the project, or its state was
    /// rebuilt from nothing (e.g. another target).
    Dropped {
        /// The snapshot without it.
        snapshot: SnapshotId,
    },
}

/// What happened to `node` over `snapshots` (oldest first): each build and why, each
/// later test, and when its state was dropped. Newest first.
pub fn node_history(snapshots: &[(SnapshotId, &StateSnapshot)], node: &str) -> Vec<NodeEvent> {
    let mut events = Vec::new();
    // The node's last recorded state, and the snapshot that recorded it.
    let mut last: Option<(&NodeState, &StateSnapshot)> = None;
    for (id, snapshot) in snapshots {
        let Some(state) = snapshot.nodes.get(node) else {
            if last.take().is_some() {
                events.push(NodeEvent::Dropped { snapshot: *id });
            }
            continue;
        };
        match last {
            Some((before, _)) if before.run_id == state.run_id => {
                let was = before.tested.as_ref().map(|t| &t.run_id);
                if let Some(t) = &state.tested
                    && was != Some(&t.run_id)
                {
                    events.push(NodeEvent::Tested {
                        snapshot: *id,
                        run_id: t.run_id.clone(),
                        at: t.at,
                    });
                }
            }
            _ => events.push(NodeEvent::Built {
                snapshot: *id,
                run_id: state.run_id.clone(),
                built_at: state.built_at,
                first: last.is_none(),
                changes: last
                    .map(|(b, then)| {
                        let mut found = changes(b, state);
                        found.extend(target_change(then, snapshot));
                        found
                    })
                    .unwrap_or_default(),
            }),
        }
        last = Some((state, snapshot));
    }
    events.reverse();
    events
}

/// The target change between two snapshots, if their recorded targets differ.
fn target_change(before: &StateSnapshot, after: &StateSnapshot) -> Option<Change> {
    (before.target != after.target).then(|| Change::Target {
        before: before.target.as_ref().map(ToString::to_string),
        after: after.target.as_ref().map(ToString::to_string),
    })
}

/// How a node differs between two states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeDiff {
    /// The node.
    pub node: String,
    /// Whether it was built again in between (always false against the project now).
    pub rebuilt: bool,
    /// What differs.
    pub changes: Vec<Change>,
}

/// How two states, or a state and the project now, differ.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct StateDiff {
    /// Nodes only in the newer one.
    pub added: Vec<String>,
    /// Nodes only in the older one.
    pub removed: Vec<String>,
    /// Nodes in both that differ, by id.
    pub changed: Vec<NodeDiff>,
}

impl StateDiff {
    /// Whether nothing differs.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// How `after` differs from `before`: nodes added, removed, and rebuilt or retested
/// with what changed.
pub fn diff_states(before: &StateSnapshot, after: &StateSnapshot) -> StateDiff {
    let mut diff = StateDiff {
        added: keys_missing(&after.nodes, &before.nodes),
        removed: keys_missing(&before.nodes, &after.nodes),
        changed: Vec::new(),
    };
    for (id, b) in &before.nodes {
        let Some(a) = after.nodes.get(id) else {
            continue;
        };
        if b.run_id != a.run_id {
            let mut found = changes(b, a);
            found.extend(target_change(before, after));
            diff.changed.push(NodeDiff {
                node: id.clone(),
                rebuilt: true,
                changes: found,
            });
        }
    }
    diff
}

/// How the project now differs from `recorded`: new and removed nodes; for the others,
/// code that changed and source data versions that differ from the ones their builds
/// read.
pub fn diff_project(recorded: &StateSnapshot, project: &Project) -> StateDiff {
    let now: BTreeMap<&str, &crate::Node> =
        project.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let sources: BTreeMap<&str, Option<String>> = project
        .sources
        .iter()
        .map(|s| (s.id.as_str(), s.version.as_ref().map(|v| v.value.clone())))
        .collect();
    let reads = sources_read(project);
    let mut diff = StateDiff {
        added: now
            .keys()
            .filter(|id| !recorded.nodes.contains_key(**id))
            .map(|id| (*id).to_owned())
            .collect(),
        removed: recorded
            .nodes
            .keys()
            .filter(|id| !now.contains_key(id.as_str()))
            .cloned()
            .collect(),
        changed: Vec::new(),
    };
    for (id, state) in &recorded.nodes {
        let Some(node) = now.get(id.as_str()) else {
            continue;
        };
        let mut node_changes = Vec::new();
        match &node.fingerprint {
            Ok(fingerprint) => {
                let code = fingerprint.diff(&state.fingerprint);
                if !code.is_empty() {
                    node_changes.push(Change::Code {
                        changed: code.changed,
                        added: code.added,
                        removed: code.removed,
                    });
                }
            }
            Err(why) => node_changes.push(Change::CodeUnknown { why: why.clone() }),
        }
        let reads = reads.get(id.as_str());
        node_changes.extend(data_changes(&state.inputs, |source| {
            // Only sources it still reads: another node's source isn't its data.
            reads
                .is_some_and(|r| r.contains(source))
                .then(|| sources.get(source).cloned())
                .flatten()
        }));
        if !node_changes.is_empty() {
            diff.changed.push(NodeDiff {
                node: id.clone(),
                rebuilt: false,
                changes: node_changes,
            });
        }
    }
    diff
}

/// The sources each node reads now, directly or through its ancestors.
fn sources_read(project: &Project) -> BTreeMap<&str, BTreeSet<&str>> {
    fn visit<'a>(
        id: &'a str,
        nodes: &BTreeMap<&'a str, &'a crate::Node>,
        sources: &BTreeSet<&'a str>,
        done: &mut BTreeMap<&'a str, BTreeSet<&'a str>>,
        visiting: &mut BTreeSet<&'a str>,
    ) -> BTreeSet<&'a str> {
        if let Some(found) = done.get(id) {
            return found.clone();
        }
        let mut found = BTreeSet::new();
        // A cycle can't be planned; stop rather than loop.
        if !visiting.insert(id) {
            return found;
        }
        if let Some(node) = nodes.get(id) {
            for parent in &node.parents {
                if sources.contains(parent.as_str()) {
                    found.insert(parent.as_str());
                } else {
                    found.extend(visit(parent, nodes, sources, done, visiting));
                }
            }
        }
        visiting.remove(id);
        done.insert(id, found.clone());
        found
    }
    let nodes: BTreeMap<&str, &crate::Node> =
        project.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let sources: BTreeSet<&str> = project.sources.iter().map(|s| s.id.as_str()).collect();
    let mut done = BTreeMap::new();
    for id in nodes.keys() {
        visit(id, &nodes, &sources, &mut done, &mut BTreeSet::new());
    }
    done
}

fn keys_missing<V>(from: &BTreeMap<String, V>, other: &BTreeMap<String, V>) -> Vec<String> {
    from.keys()
        .filter(|k| !other.contains_key(*k))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use ods_core::FreshnessPolicy;
    use ods_core::state::{DataVersion, Exactness, Fingerprint, Reason, TestRecord};

    use super::*;
    use crate::{Node, Source};

    fn state(code: &str, run: &str, data: &str) -> NodeState {
        let mut s = NodeState::new(
            Fingerprint::from_content([("file", code), ("config", "{}")]),
            Timestamp::from_unix(1),
            run,
            BTreeMap::from([(
                "source.p.raw".to_owned(),
                Some(DataVersion::new(data, Exactness::Semantic, "sources.json")),
            )]),
        );
        s.parents = BTreeMap::from([("model.p.up".to_owned(), run.to_owned())]);
        s
    }

    fn snapshot(nodes: Vec<(&str, NodeState)>) -> StateSnapshot {
        StateSnapshot::new(
            None,
            Timestamp::from_unix(2),
            "r",
            nodes.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        )
    }

    fn entry(node: &str, action: PlanAction, code: ReasonCode, parents: &[&str]) -> PlanEntry {
        let mut e = PlanEntry::new(
            node,
            node,
            "model",
            action,
            vec![Reason::new(code, "why")],
            FreshnessPolicy::conservative(),
            0,
        );
        e.depends_on = parents.iter().map(|p| (*p).to_owned()).collect();
        e
    }

    #[test]
    fn a_build_is_traced_to_its_root_causes() {
        let plan = ExecutionPlan::new(
            None,
            Timestamp::from_unix(0),
            vec![
                entry("a", PlanAction::Build, ReasonCode::CodeChanged, &[]),
                entry("b", PlanAction::Reuse, ReasonCode::Unchanged, &[]),
                entry(
                    "c",
                    PlanAction::Build,
                    ReasonCode::UpstreamCodeChanged,
                    &["a", "b"],
                ),
                entry(
                    "d",
                    PlanAction::Build,
                    ReasonCode::UpstreamCodeChanged,
                    &["a", "c"],
                ),
            ],
        );
        let d = explain(&plan, "d").unwrap();
        let causes: Vec<&str> = d.causes.iter().map(|c| c.entry.node.as_str()).collect();
        assert_eq!(causes, ["a", "c"]);
        // `c` reads `a` too: it isn't explained twice, and the reused `b` isn't a cause.
        let c = &d.causes[1];
        assert_eq!(c.causes.len(), 1);
        assert!(c.causes[0].repeated);
        assert!(explain(&plan, "b").unwrap().causes.is_empty());
        assert!(explain(&plan, "nope").is_none());
    }

    #[test]
    fn history_explains_each_rebuild_from_what_was_recorded() {
        let s1 = snapshot(vec![("m", state("v1", "r1", "d1"))]);
        let s2 = snapshot(vec![("m", state("v2", "r2", "d1"))]);
        let mut tested = state("v2", "r2", "d1");
        tested.tested = Some(TestRecord::new("r3", Timestamp::from_unix(3), "c"));
        let s3 = snapshot(vec![("m", tested)]);
        let s4 = snapshot(vec![("m", state("v2", "r4", "d2"))]);
        let s5 = snapshot(vec![]);
        let snapshots = [
            (SnapshotId(1), &s1),
            (SnapshotId(2), &s2),
            (SnapshotId(3), &s3),
            (SnapshotId(4), &s4),
            (SnapshotId(5), &s5),
        ];
        let events = node_history(&snapshots, "m");
        assert_eq!(events.len(), 5, "{events:#?}");
        assert!(matches!(
            events[0],
            NodeEvent::Dropped {
                snapshot: SnapshotId(5)
            }
        ));
        let NodeEvent::Built { changes, .. } = &events[1] else {
            panic!("{events:#?}")
        };
        // Data changed, and the parent's build it read changed run.
        assert!(changes.iter().any(|c| matches!(c, Change::Data { before: Some(b), after: Some(a), .. } if b == "d1" && a == "d2")));
        assert!(changes.iter().any(|c| matches!(c, Change::Upstream { .. })));
        assert!(matches!(
            events[2],
            NodeEvent::Tested {
                snapshot: SnapshotId(3),
                ..
            }
        ));
        let NodeEvent::Built { changes, .. } = &events[3] else {
            panic!("{events:#?}")
        };
        assert!(matches!(&changes[0], Change::Code { changed, .. } if changed == &["file"]));
        assert!(matches!(events[4], NodeEvent::Built { first: true, .. }));
    }

    #[test]
    fn a_rebuild_with_nothing_recorded_changed_says_so() {
        let s1 = snapshot(vec![("m", state("v1", "r1", "d1"))]);
        let mut again = state("v1", "r2", "d1");
        again.parents = s1.nodes["m"].parents.clone();
        let s2 = snapshot(vec![("m", again)]);
        let events = node_history(&[(SnapshotId(1), &s1), (SnapshotId(2), &s2)], "m");
        assert!(
            matches!(&events[0], NodeEvent::Built { first: false, changes, .. } if changes.is_empty())
        );
    }

    #[test]
    fn states_and_the_project_now_are_diffed() {
        let before = snapshot(vec![
            ("m", state("v1", "r1", "d1")),
            ("gone", state("x", "r1", "d1")),
        ]);
        let after = snapshot(vec![
            ("m", state("v2", "r2", "d1")),
            ("new", state("y", "r2", "d1")),
        ]);
        let diff = diff_states(&before, &after);
        assert_eq!(diff.added, ["new"]);
        assert_eq!(diff.removed, ["gone"]);
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.changed[0].rebuilt);

        let project = Project::new(
            vec![Node::new(
                "m",
                "m",
                "model",
                vec!["source.p.raw".into()],
                Ok(Fingerprint::from_content([
                    ("file", "v1"),
                    ("config", "{}"),
                ])),
                FreshnessPolicy::conservative(),
            )],
            vec![Source::new(
                "source.p.raw",
                "raw",
                Some(DataVersion::new("d9", Exactness::Semantic, "sources.json")),
            )],
        );
        let diff = diff_project(&before, &project);
        assert_eq!(diff.removed, ["gone"]);
        assert!(diff.added.is_empty());
        assert_eq!(
            diff.changed[0].changes,
            [Change::Data {
                source: "source.p.raw".into(),
                before: Some("d1".into()),
                after: Some("d9".into())
            }]
        );
        let same = snapshot(vec![("m", state("v1", "r1", "d9"))]);
        assert!(diff_project(&same, &project).is_empty());
    }

    #[test]
    fn only_the_parents_behind_the_reason_are_causes() {
        // `c` builds because `a`'s code changed; `b` builds for new data, which
        // doesn't decide `c` (its lag tolerance might even have waited for it).
        let plan = ExecutionPlan::new(
            None,
            Timestamp::from_unix(0),
            vec![
                entry("a", PlanAction::Build, ReasonCode::CodeChanged, &[]),
                entry("b", PlanAction::Build, ReasonCode::NewUpstreamData, &[]),
                entry(
                    "c",
                    PlanAction::Build,
                    ReasonCode::UpstreamCodeChanged,
                    &["a", "b"],
                ),
                entry("d", PlanAction::Build, ReasonCode::NewUpstreamData, &["b"]),
                entry(
                    "r",
                    PlanAction::Build,
                    ReasonCode::FullRefreshRequested,
                    &[],
                ),
                entry(
                    "e",
                    PlanAction::Build,
                    ReasonCode::UpstreamFullRefresh,
                    &["a", "r"],
                ),
            ],
        );
        let causes = |n: &str| -> Vec<String> {
            explain(&plan, n)
                .unwrap()
                .causes
                .into_iter()
                .map(|c| c.entry.node)
                .collect()
        };
        assert_eq!(causes("c"), ["a"]);
        assert_eq!(causes("d"), ["b"]);
        assert_eq!(causes("e"), ["r"]);
    }

    #[test]
    fn a_recorded_target_change_explains_a_rebuild() {
        let s1 = snapshot(vec![("m", state("v1", "r1", "d1"))])
            .with_target(Some(ods_core::state::TargetIdentity::new("dev")));
        let mut again = state("v1", "r2", "d1");
        again.parents = s1.nodes["m"].parents.clone();
        let s2 = snapshot(vec![("m", again)])
            .with_target(Some(ods_core::state::TargetIdentity::new("prod")));
        let events = node_history(&[(SnapshotId(1), &s1), (SnapshotId(2), &s2)], "m");
        let NodeEvent::Built { changes, .. } = &events[0] else {
            panic!("{events:#?}")
        };
        assert!(
            matches!(&changes[..], [Change::Target { before: Some(b), after: Some(a) }] if b.starts_with("dev") && a.starts_with("prod")),
            "{changes:#?}"
        );
        let diff = diff_states(&s1, &s2);
        assert!(
            diff.changed[0]
                .changes
                .iter()
                .any(|c| matches!(c, Change::Target { .. }))
        );
    }

    #[test]
    fn a_source_the_node_no_longer_reads_isnt_its_data() {
        let recorded = snapshot(vec![("m", state("v1", "r1", "d1"))]);
        // `m` now reads nothing; another node still reads the source, which has new data.
        let project = Project::new(
            vec![
                Node::new(
                    "m",
                    "m",
                    "model",
                    vec![],
                    Ok(Fingerprint::from_content([
                        ("file", "v1"),
                        ("config", "{}"),
                    ])),
                    FreshnessPolicy::conservative(),
                ),
                Node::new(
                    "other",
                    "other",
                    "model",
                    vec!["source.p.raw".into()],
                    Ok(Fingerprint::from_content([("file", "x")])),
                    FreshnessPolicy::conservative(),
                ),
            ],
            vec![Source::new(
                "source.p.raw",
                "raw",
                Some(DataVersion::new("d9", Exactness::Semantic, "sources.json")),
            )],
        );
        let diff = diff_project(&recorded, &project);
        assert!(diff.changed.is_empty(), "{diff:#?}");
        assert_eq!(diff.added, ["other"]);
    }

    #[test]
    fn results_read_back_from_json() {
        let s1 = snapshot(vec![("m", state("v1", "r1", "d1"))]);
        let s2 = snapshot(vec![("m", state("v2", "r2", "d2"))]);
        let diff = diff_states(&s1, &s2);
        let json = serde_json::to_string(&diff).unwrap();
        assert_eq!(serde_json::from_str::<StateDiff>(&json).unwrap(), diff);
        let events = node_history(&[(SnapshotId(1), &s1), (SnapshotId(2), &s2)], "m");
        let json = serde_json::to_string(&events).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<NodeEvent>>(&json).unwrap(),
            events
        );
    }
}
