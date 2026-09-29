//! Where deferred references should point (#296, ADR-0020 §2): for each node of an
//! upstream state, this target's relation when ODS recorded a build of it here and can
//! show it still exists, and otherwise the upstream's own.
//!
//! An engine that defers references to an upstream state (a production deployment,
//! say) resolves an unselected node to the upstream's relation. That is wrong when this
//! target has since built the node: the reader then mixes environments. The rules here
//! decide, per node, which pointer an exported state carries. First match wins, and
//! anything ODS can't vouch for keeps the upstream pointer: exactly what the engine
//! would have done without ODS (AGENTS.md rule 3).
//!
//! | # | Condition | Pointer | Reason |
//! |---|---|---|---|
//! | 1 | The engine never defers it | unchanged | `not_deferrable` |
//! | 2 | Not in the current project | upstream | `not_in_project` |
//! | 3 | The latest snapshot is for another target, or names none | upstream | `target_changed` / `target_unknown` |
//! | 4 | No successful build recorded | upstream | `not_built_here` |
//! | 5 | The recorded relation differs from the current one | upstream | `relation_changed` |
//! | 6 | Any other fingerprint component differs | upstream | `code_changed_since_build` |
//! | 7 | The relation check says it's missing | upstream | `relation_missing` |
//! | 8 | The check couldn't tell, or didn't run | upstream | `relation_unverified` |
//! | 9 | Otherwise | this target | `built_here` |
//!
//! Everything here is pure: the caller reads the upstream state, the store and the
//! warehouse, and writes the result.

use std::collections::BTreeMap;

use ods_core::SchemaVersion;
use ods_core::state::{
    Evidence, Exactness, Fingerprint, NodeState, SnapshotId, StateSnapshot, TargetIdentity,
    Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::{Node, Project, RelationFact};

/// Version of [`ExportRecord`] documents.
pub const EXPORT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// A node of the upstream state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct UpstreamNode {
    /// Its unique id.
    pub id: String,
    /// Whether the engine defers references to it at all (e.g. a table or view it
    /// builds, not a test or code that is inlined into its readers).
    pub deferrable: bool,
}

impl UpstreamNode {
    /// A node of the upstream state.
    pub fn new(id: impl Into<String>, deferrable: bool) -> Self {
        Self {
            id: id.into(),
            deferrable,
        }
    }
}

/// Where a node's deferred references point in the exported state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Pointer {
    /// Left as the upstream has it: the engine never defers to it anyway.
    Unchanged,
    /// The upstream's relation, as the engine would have used without ODS.
    Upstream,
    /// This target's relation: ODS recorded a build of it here that still exists.
    ThisTarget,
}

/// Why a node points where it does (ADR-0020 §2). Codes are stable for machines;
/// [`PointerChoice::message`] is for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PointerReason {
    /// Rule 1: the engine never defers references to it.
    NotDeferrable,
    /// Rule 2: it isn't in the current project.
    NotInProject,
    /// Rule 3: the recorded state was built in another target.
    TargetChanged,
    /// Rule 3: the recorded state doesn't say which target it was built in.
    TargetUnknown,
    /// Rule 4: no successful build of it is recorded.
    NotBuiltHere,
    /// Rule 5: it now builds into another relation than the recorded build did.
    RelationChanged,
    /// Rule 6: its code differs from the recorded build's, or can't be fingerprinted.
    CodeChangedSinceBuild,
    /// Rule 7: its relation isn't in this target's warehouse.
    RelationMissing,
    /// Rule 8: nothing showed that its relation is in this target's warehouse.
    RelationUnverified,
    /// Rule 9: built here, and still there.
    BuiltHere,
}

impl PointerReason {
    /// Every reason, in rule order.
    pub const ALL: [PointerReason; 10] = [
        PointerReason::NotDeferrable,
        PointerReason::NotInProject,
        PointerReason::TargetChanged,
        PointerReason::TargetUnknown,
        PointerReason::NotBuiltHere,
        PointerReason::RelationChanged,
        PointerReason::CodeChangedSinceBuild,
        PointerReason::RelationMissing,
        PointerReason::RelationUnverified,
        PointerReason::BuiltHere,
    ];

    /// The stable code, as serialized, e.g. `built_here`.
    pub fn code(self) -> &'static str {
        match self {
            PointerReason::NotDeferrable => "not_deferrable",
            PointerReason::NotInProject => "not_in_project",
            PointerReason::TargetChanged => "target_changed",
            PointerReason::TargetUnknown => "target_unknown",
            PointerReason::NotBuiltHere => "not_built_here",
            PointerReason::RelationChanged => "relation_changed",
            PointerReason::CodeChangedSinceBuild => "code_changed_since_build",
            PointerReason::RelationMissing => "relation_missing",
            PointerReason::RelationUnverified => "relation_unverified",
            PointerReason::BuiltHere => "built_here",
        }
    }
}

/// Where one node points, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PointerChoice {
    /// The node's unique id.
    pub node: String,
    /// Where it points.
    pub pointer: Pointer,
    /// Why.
    pub reason: PointerReason,
    /// Why, for people.
    pub message: String,
    /// The run of the recorded build the choice looked at, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// When that build finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at: Option<Timestamp>,
    /// Fingerprint components that differ from the recorded build's (rules 5 and 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_components: Vec<String>,
    /// What the choice rests on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl PointerChoice {
    fn new(
        node: &str,
        pointer: Pointer,
        reason: PointerReason,
        message: impl Into<String>,
    ) -> Self {
        Self {
            node: node.to_owned(),
            pointer,
            reason,
            message: message.into(),
            run_id: None,
            built_at: None,
            changed_components: Vec::new(),
            evidence: Vec::new(),
        }
    }

    fn upstream(node: &str, reason: PointerReason, message: impl Into<String>) -> Self {
        Self::new(node, Pointer::Upstream, reason, message)
    }

    fn build(mut self, state: &NodeState) -> Self {
        self.run_id = Some(state.run_id.clone());
        self.built_at = Some(state.built_at);
        self
    }

    fn evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }
}

/// The ODS sidecar written next to an exported state (`ods-export.json`): what it was
/// made from, and every node's choice. Only the target identity's non-secret form is
/// kept (ADR-0017, AGENTS.md rule 9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ExportRecord {
    /// Document version ([`EXPORT_SCHEMA_VERSION`]).
    pub schema_version: SchemaVersion,
    /// When the export was made.
    pub generated_at: Timestamp,
    /// The snapshot the choices were made from, if there was one.
    pub snapshot: Option<SnapshotId>,
    /// The target the export points at, as identified when it was made.
    pub target: TargetIdentity,
    /// The upstream state's invocation, if it names one.
    pub upstream_invocation_id: Option<String>,
    /// SHA-256 of the exported state file this record describes.
    pub manifest_sha256: String,
    /// Every upstream node's choice, by id.
    pub nodes: Vec<PointerChoice>,
}

impl ExportRecord {
    /// A record of an export.
    pub fn new(
        generated_at: Timestamp,
        snapshot: Option<SnapshotId>,
        target: TargetIdentity,
        upstream_invocation_id: Option<String>,
        manifest_sha256: impl Into<String>,
        nodes: Vec<PointerChoice>,
    ) -> Self {
        Self {
            schema_version: EXPORT_SCHEMA_VERSION,
            generated_at,
            snapshot,
            target,
            upstream_invocation_id,
            manifest_sha256: manifest_sha256.into(),
            nodes,
        }
    }
}

/// Rules 1–6: a choice, or `Err` with the recorded build when only the relation
/// check (rules 7–9) is left to decide.
fn before_check<'a>(
    upstream: &UpstreamNode,
    nodes: &BTreeMap<&str, &Node>,
    snapshot: Option<&'a StateSnapshot>,
    current: &TargetIdentity,
) -> Result<PointerChoice, &'a NodeState> {
    let id = upstream.id.as_str();
    if !upstream.deferrable {
        return Ok(PointerChoice::new(
            id,
            Pointer::Unchanged,
            PointerReason::NotDeferrable,
            "never deferred to (not a table or view the project builds)",
        ));
    }
    let Some(node) = nodes.get(id) else {
        return Ok(PointerChoice::upstream(
            id,
            PointerReason::NotInProject,
            "not built by the project as it is now",
        ));
    };
    let Some(snapshot) = snapshot else {
        return Ok(PointerChoice::upstream(
            id,
            PointerReason::NotBuiltHere,
            "no build of it is recorded here",
        ));
    };
    match &snapshot.target {
        None => {
            return Ok(PointerChoice::upstream(
                id,
                PointerReason::TargetUnknown,
                "the recorded state doesn't say which target it was built in",
            ));
        }
        Some(recorded) if recorded != current => {
            return Ok(PointerChoice::upstream(
                id,
                PointerReason::TargetChanged,
                format!("the recorded state was built in target {recorded}, not {current}"),
            )
            .evidence(Evidence::new(
                "target",
                id,
                Some(recorded.to_string()),
                Exactness::Exact,
            )));
        }
        Some(_) => {}
    }
    let Some(state) = snapshot.nodes.get(id) else {
        return Ok(PointerChoice::upstream(
            id,
            PointerReason::NotBuiltHere,
            "no successful build of it is recorded in this target",
        ));
    };
    let now = match &node.fingerprint {
        Ok(fingerprint) => fingerprint,
        Err(why) => {
            return Ok(PointerChoice::upstream(
                id,
                PointerReason::CodeChangedSinceBuild,
                format!("its code can't be compared with the recorded build: {why}"),
            )
            .build(state)
            .evidence(Evidence::new(
                "fingerprint",
                id,
                Some(why.clone()),
                Exactness::None,
            )));
        }
    };
    let relation = |f: &Fingerprint| f.components.get(Fingerprint::RELATION).cloned();
    // A relation either side doesn't name can't be shown to be the same one.
    let (recorded, named) = (relation(&state.fingerprint), relation(now));
    if recorded.is_none() || named.is_none() || recorded != named {
        let mut choice = PointerChoice::upstream(
            id,
            PointerReason::RelationChanged,
            "it builds into another relation than the recorded build did",
        )
        .build(state)
        .evidence(Evidence::new(
            "fingerprint",
            id,
            Some(Fingerprint::RELATION.to_owned()),
            Exactness::Exact,
        ));
        choice.changed_components = vec![Fingerprint::RELATION.to_owned()];
        return Ok(choice);
    }
    let changed: Vec<String> = now
        .diff(&state.fingerprint)
        .all()
        .into_iter()
        .filter(|c| c != Fingerprint::RELATION)
        .collect();
    if !changed.is_empty() {
        let mut choice = PointerChoice::upstream(
            id,
            PointerReason::CodeChangedSinceBuild,
            format!(
                "its code changed since the recorded build ({})",
                changed.join(", ")
            ),
        )
        .build(state)
        .evidence(Evidence::new(
            "fingerprint",
            id,
            Some(changed.join(", ")),
            Exactness::Exact,
        ));
        choice.changed_components = changed;
        return Ok(choice);
    }
    Err(state)
}

fn by_id(project: &Project) -> BTreeMap<&str, &Node> {
    project.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
}

/// The upstream nodes that pass rules 1–6, by id: those whose relation in this target
/// is worth checking. Nothing else can point at this target, so nothing else needs a
/// warehouse query.
pub fn defer_candidates(
    project: &Project,
    upstream: &[UpstreamNode],
    snapshot: Option<&StateSnapshot>,
    current: &TargetIdentity,
) -> Vec<String> {
    let nodes = by_id(project);
    let mut ids: Vec<String> = upstream
        .iter()
        .filter(|u| before_check(u, &nodes, snapshot, current).is_err())
        .map(|u| u.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Every upstream node's pointer, sorted by id. `snapshot` is the scope's latest
/// snapshot, `current` the target the export is for, and `facts` what the relation
/// check found for the [candidates](defer_candidates); `None` when it didn't run.
pub fn choose_pointers(
    project: &Project,
    upstream: &[UpstreamNode],
    snapshot: Option<&StateSnapshot>,
    current: &TargetIdentity,
    facts: Option<&BTreeMap<String, RelationFact>>,
) -> Vec<PointerChoice> {
    let nodes = by_id(project);
    let mut choices: Vec<PointerChoice> = upstream
        .iter()
        .map(|u| match before_check(u, &nodes, snapshot, current) {
            Ok(choice) => choice,
            Err(state) => after_check(&u.id, state, facts),
        })
        .collect();
    choices.sort_by(|a, b| a.node.cmp(&b.node));
    choices.dedup_by(|a, b| a.node == b.node);
    choices
}

/// Rules 7–9, for a node whose recorded build is of its current code and relation.
fn after_check(
    id: &str,
    state: &NodeState,
    facts: Option<&BTreeMap<String, RelationFact>>,
) -> PointerChoice {
    let Some(facts) = facts else {
        return PointerChoice::upstream(
            id,
            PointerReason::RelationUnverified,
            "its relation in this target wasn't checked",
        )
        .build(state)
        .evidence(Evidence::new(
            "relation_exists",
            id,
            Some("not checked".to_owned()),
            Exactness::None,
        ));
    };
    match facts.get(id) {
        Some(RelationFact::Present(kind)) => PointerChoice::new(
            id,
            Pointer::ThisTarget,
            PointerReason::BuiltHere,
            format!(
                "built here by run {} at {}, and its relation is still there",
                state.run_id, state.built_at
            ),
        )
        .build(state)
        .evidence(Evidence::new(
            "relation_exists",
            id,
            Some(kind.clone().unwrap_or_else(|| "present".to_owned())),
            Exactness::Exact,
        )),
        Some(RelationFact::Missing) => PointerChoice::upstream(
            id,
            PointerReason::RelationMissing,
            "its relation isn't in this target's warehouse any more",
        )
        .build(state)
        .evidence(Evidence::new(
            "relation_exists",
            id,
            Some("missing".to_owned()),
            Exactness::Exact,
        )),
        Some(RelationFact::Unverified(why)) => PointerChoice::upstream(
            id,
            PointerReason::RelationUnverified,
            format!("its relation in this target couldn't be checked: {why}"),
        )
        .build(state)
        .evidence(Evidence::new(
            "relation_exists",
            id,
            Some(why.clone()),
            Exactness::None,
        )),
        Some(RelationFact::Unchecked) | None => PointerChoice::upstream(
            id,
            PointerReason::RelationUnverified,
            "the relation check didn't report on it",
        )
        .build(state)
        .evidence(Evidence::new(
            "relation_exists",
            id,
            Some("not reported".to_owned()),
            Exactness::None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::FreshnessPolicy;

    fn fp(sql: &str, relation: Option<&str>) -> Fingerprint {
        let mut components = vec![("sql", sql.to_owned())];
        if let Some(r) = relation {
            components.push((Fingerprint::RELATION, r.to_owned()));
        }
        Fingerprint::from_content(components)
    }

    fn node(id: &str, fingerprint: Result<Fingerprint, String>) -> Node {
        Node::new(
            id,
            id,
            "model",
            vec![],
            fingerprint,
            FreshnessPolicy::conservative(),
        )
    }

    fn dev() -> TargetIdentity {
        TargetIdentity::new("dev").location(Some("dev.db".into()))
    }

    fn snapshot(nodes: &[(&str, Fingerprint)], target: Option<TargetIdentity>) -> StateSnapshot {
        StateSnapshot::new(
            Some(SnapshotId(1)),
            Timestamp::from_unix(1_000),
            "run-1",
            nodes
                .iter()
                .map(|(id, f)| {
                    (
                        (*id).to_owned(),
                        NodeState::new(
                            f.clone(),
                            Timestamp::from_unix(900),
                            "run-1",
                            BTreeMap::new(),
                        ),
                    )
                })
                .collect(),
        )
        .with_target(target)
    }

    fn present() -> BTreeMap<String, RelationFact> {
        BTreeMap::from([("a".to_owned(), RelationFact::Present(Some("table".into())))])
    }

    /// The choice for `a`, a model built in dev with the same code, unless the
    /// arguments say otherwise.
    fn choose(
        upstream: UpstreamNode,
        current: Option<Result<Fingerprint, String>>,
        recorded: Option<Fingerprint>,
        target: Option<TargetIdentity>,
        facts: Option<&BTreeMap<String, RelationFact>>,
    ) -> PointerChoice {
        let project = Project::new(current.map(|f| node("a", f)).into_iter().collect(), vec![]);
        let snapshot = snapshot(
            &recorded.map(|f| ("a", f)).into_iter().collect::<Vec<_>>(),
            target,
        );
        let mut choices = choose_pointers(&project, &[upstream], Some(&snapshot), &dev(), facts);
        assert_eq!(choices.len(), 1);
        choices.remove(0)
    }

    fn same() -> Fingerprint {
        fp("select 1", Some("dev.a"))
    }

    fn deferrable() -> UpstreamNode {
        UpstreamNode::new("a", true)
    }

    #[test]
    fn rule_1_never_deferred_is_unchanged() {
        let c = choose(
            UpstreamNode::new("a", false),
            Some(Ok(same())),
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Unchanged, PointerReason::NotDeferrable)
        );
    }

    #[test]
    fn rule_2_not_in_project_points_upstream() {
        let c = choose(
            deferrable(),
            None,
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Upstream, PointerReason::NotInProject)
        );
    }

    #[test]
    fn rule_3_other_or_unknown_target_points_upstream() {
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            Some(same()),
            Some(TargetIdentity::new("prod")),
            Some(&present()),
        );
        assert_eq!(c.reason, PointerReason::TargetChanged);
        assert_eq!(c.pointer, Pointer::Upstream);
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            Some(same()),
            None,
            Some(&present()),
        );
        assert_eq!(c.reason, PointerReason::TargetUnknown);
    }

    #[test]
    fn rule_4_not_built_points_upstream() {
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            None,
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Upstream, PointerReason::NotBuiltHere)
        );
        // No state at all.
        let project = Project::new(vec![node("a", Ok(same()))], vec![]);
        let c = choose_pointers(&project, &[deferrable()], None, &dev(), Some(&present()));
        assert_eq!(c[0].reason, PointerReason::NotBuiltHere);
    }

    #[test]
    fn rule_5_relation_changed_or_unnamed_points_upstream() {
        let c = choose(
            deferrable(),
            Some(Ok(fp("select 1", Some("dev.other")))),
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Upstream, PointerReason::RelationChanged)
        );
        assert_eq!(c.changed_components, ["relation"]);
        assert_eq!(c.run_id.as_deref(), Some("run-1"));
        // Missing on either side counts as changed.
        let c = choose(
            deferrable(),
            Some(Ok(fp("select 1", None))),
            Some(fp("select 1", None)),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(c.reason, PointerReason::RelationChanged);
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            Some(fp("select 1", None)),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(c.reason, PointerReason::RelationChanged);
    }

    #[test]
    fn rule_6_code_changed_or_unfingerprinted_points_upstream() {
        let c = choose(
            deferrable(),
            Some(Ok(fp("select 2", Some("dev.a")))),
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Upstream, PointerReason::CodeChangedSinceBuild)
        );
        assert_eq!(c.changed_components, ["sql"]);
        let c = choose(
            deferrable(),
            Some(Err("no compiled SQL".to_owned())),
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(c.reason, PointerReason::CodeChangedSinceBuild);
        assert_eq!(
            c.evidence[0].value.as_deref(),
            Some("no compiled SQL"),
            "{c:?}"
        );
    }

    #[test]
    fn rule_7_missing_relation_points_upstream() {
        let facts = BTreeMap::from([("a".to_owned(), RelationFact::Missing)]);
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            Some(same()),
            Some(dev()),
            Some(&facts),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::Upstream, PointerReason::RelationMissing)
        );
    }

    #[test]
    fn rule_8_unverified_or_unchecked_points_upstream() {
        let cases = [
            Some(BTreeMap::from([(
                "a".to_owned(),
                RelationFact::Unverified("timeout".into()),
            )])),
            Some(BTreeMap::from([("a".to_owned(), RelationFact::Unchecked)])),
            Some(BTreeMap::new()),
            None,
        ];
        for facts in &cases {
            let c = choose(
                deferrable(),
                Some(Ok(same())),
                Some(same()),
                Some(dev()),
                facts.as_ref(),
            );
            assert_eq!(
                (c.pointer, c.reason),
                (Pointer::Upstream, PointerReason::RelationUnverified),
                "{facts:?}"
            );
        }
    }

    #[test]
    fn rule_9_built_here_points_at_this_target() {
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            Some(same()),
            Some(dev()),
            Some(&present()),
        );
        assert_eq!(
            (c.pointer, c.reason),
            (Pointer::ThisTarget, PointerReason::BuiltHere)
        );
        assert_eq!(c.run_id.as_deref(), Some("run-1"));
        assert_eq!(c.built_at, Some(Timestamp::from_unix(900)));
    }

    #[test]
    fn first_matching_rule_wins() {
        // Not deferrable beats everything, even a node that isn't in the project.
        let c = choose(UpstreamNode::new("a", false), None, None, None, None);
        assert_eq!(c.reason, PointerReason::NotDeferrable);
        // Not in the project beats another target.
        let c = choose(deferrable(), None, Some(same()), None, None);
        assert_eq!(c.reason, PointerReason::NotInProject);
        // Another target beats not built.
        let c = choose(
            deferrable(),
            Some(Ok(same())),
            None,
            Some(TargetIdentity::new("prod")),
            None,
        );
        assert_eq!(c.reason, PointerReason::TargetChanged);
        // A changed relation beats changed code, and a missing relation.
        let facts = BTreeMap::from([("a".to_owned(), RelationFact::Missing)]);
        let c = choose(
            deferrable(),
            Some(Ok(fp("select 2", Some("dev.other")))),
            Some(same()),
            Some(dev()),
            Some(&facts),
        );
        assert_eq!(c.reason, PointerReason::RelationChanged);
        // Changed code beats a missing relation.
        let c = choose(
            deferrable(),
            Some(Ok(fp("select 2", Some("dev.a")))),
            Some(same()),
            Some(dev()),
            Some(&facts),
        );
        assert_eq!(c.reason, PointerReason::CodeChangedSinceBuild);
    }

    #[test]
    fn only_nodes_passing_rules_1_to_6_are_candidates() {
        let project = Project::new(
            vec![
                node("a", Ok(same())),
                node("b", Ok(fp("select 2", Some("dev.b")))),
                node("c", Ok(fp("select 3", Some("dev.c")))),
            ],
            vec![],
        );
        let snapshot = snapshot(
            &[
                ("a", same()),
                ("b", fp("select 1", Some("dev.b"))),
                ("d", same()),
            ],
            Some(dev()),
        );
        let upstream = [
            UpstreamNode::new("c", true),
            UpstreamNode::new("b", true),
            UpstreamNode::new("a", true),
            UpstreamNode::new("t", false),
        ];
        assert_eq!(
            defer_candidates(&project, &upstream, Some(&snapshot), &dev()),
            ["a"]
        );
        let choices = choose_pointers(&project, &upstream, Some(&snapshot), &dev(), None);
        let ids: Vec<&str> = choices.iter().map(|c| c.node.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "t"]);
    }

    #[test]
    fn reasons_serialize_as_their_codes() {
        for reason in PointerReason::ALL {
            let json = serde_json::to_value(reason).unwrap();
            assert_eq!(json, reason.code());
        }
        assert_eq!(
            serde_json::to_value(Pointer::ThisTarget).unwrap(),
            "this_target"
        );
    }

    #[test]
    fn export_record_round_trips() {
        let record = ExportRecord::new(
            Timestamp::from_unix(1_000),
            Some(SnapshotId(3)),
            dev(),
            Some("inv".into()),
            "abc",
            vec![choose(
                deferrable(),
                Some(Ok(same())),
                Some(same()),
                Some(dev()),
                Some(&present()),
            )],
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json["schema_version"],
            serde_json::json!({"major": 1, "minor": 0})
        );
        assert_eq!(json["nodes"][0]["reason"], "built_here");
        let back: ExportRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, record);
    }
}
