//! Plans BUILD or REUSE for every node (ADR-0013, "Planning rules").

use std::collections::{BTreeMap, BTreeSet};

use ods_core::Quorum;
use ods_core::freshness::format_duration;
use ods_core::state::{
    DataVersion, Evidence, Exactness, ExecutionPlan, NodeState, PlanAction, PlanEntry, Reason,
    ReasonCode, SnapshotId, StateSnapshot, Timestamp,
};

use crate::{Node, Project, Source};

/// Why no plan could be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// The dependency graph has a cycle through these nodes.
    #[error("the dependency graph has a cycle through {}", .0.join(", "))]
    Cycle(Vec<String>),
    /// Two nodes share an id.
    #[error("node `{0}` is defined twice")]
    Duplicate(String),
}

/// Reasons that mean a node's output may differ in shape, not just in data: its
/// children rebuild whatever their lag tolerance.
fn is_code_change(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::NeverBuilt
            | ReasonCode::CodeEvidenceIncomplete
            | ReasonCode::CodeChanged
            | ReasonCode::UpstreamCodeChanged
    )
}

/// Nodes in dependency order (parents first; ties by id), with their depth.
fn order(project: &Project) -> Result<Vec<(&Node, u32)>, PlanError> {
    let mut by_id: BTreeMap<&str, &Node> = BTreeMap::new();
    for node in &project.nodes {
        if by_id.insert(&node.id, node).is_some() {
            return Err(PlanError::Duplicate(node.id.clone()));
        }
    }
    let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in &project.nodes {
        let parents: BTreeSet<&str> = node
            .parents
            .iter()
            .map(String::as_str)
            .filter(|p| by_id.contains_key(p))
            .collect();
        waiting.insert(&node.id, parents.len());
        for parent in parents {
            children.entry(parent).or_default().push(&node.id);
        }
    }
    let mut depth: BTreeMap<&str, u32> = BTreeMap::new();
    let mut ready: BTreeSet<&str> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut out = Vec::with_capacity(project.nodes.len());
    while let Some(id) = ready.pop_first() {
        let node = by_id[id];
        let d = node
            .parents
            .iter()
            .filter_map(|p| depth.get(p.as_str()))
            .max()
            .map_or(0, |d| d + 1);
        depth.insert(id, d);
        out.push((node, d));
        for child in children.get(id).into_iter().flatten() {
            let n = waiting.get_mut(child).expect("every child is a node");
            *n -= 1;
            if *n == 0 {
                ready.insert(child);
            }
        }
    }
    if out.len() < project.nodes.len() {
        let stuck = waiting
            .iter()
            .filter(|(id, _)| !depth.contains_key(*id))
            .map(|(id, _)| (*id).to_owned())
            .collect();
        return Err(PlanError::Cycle(stuck));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.id.cmp(&b.0.id)));
    Ok(out)
}

/// What a node's parents say, and what is known about its sources.
#[derive(Default)]
struct Inputs {
    /// Parents that will be built because their code changed.
    code_changed: Vec<String>,
    /// Parents with data newer than what the node was built from.
    new_data: Vec<String>,
    /// Parents without new data.
    unchanged: Vec<String>,
    /// Sources whose data version is unknown, now or when the node was built.
    missing: Vec<String>,
    /// Parents that aren't known nodes or sources.
    unknown: Vec<String>,
}

struct Context<'a> {
    sources: BTreeMap<&'a str, &'a Source>,
    names: BTreeMap<&'a str, &'a str>,
    previous: Option<&'a StateSnapshot>,
    decided: BTreeMap<String, (PlanAction, ReasonCode)>,
}

impl Context<'_> {
    fn name<'s>(&'s self, id: &'s str) -> &'s str {
        self.names.get(id).copied().unwrap_or(id)
    }

    fn inputs(&self, node: &Node, before: &NodeState, evidence: &mut Vec<Evidence>) -> Inputs {
        let mut inputs = Inputs::default();
        for parent in &node.parents {
            let name = self.name(parent).to_owned();
            if let Some(source) = self.sources.get(parent.as_str()) {
                let now = source.version.as_ref();
                let then: Option<&DataVersion> = before.inputs.get(parent).and_then(Option::as_ref);
                evidence.push(Evidence::new(
                    "source_data_version",
                    parent.clone(),
                    now.map(|v| v.value.clone()),
                    now.map_or(Exactness::None, |v| v.exactness),
                ));
                match (now, then) {
                    (Some(now), Some(then)) if now.exactness.allows_reuse() => {
                        if now.value == then.value {
                            inputs.unchanged.push(name);
                        } else {
                            inputs.new_data.push(name);
                        }
                    }
                    _ => inputs.missing.push(name),
                }
                continue;
            }
            let Some((action, code)) = self.decided.get(parent) else {
                inputs.unknown.push(parent.clone());
                continue;
            };
            if *action == PlanAction::Build {
                if is_code_change(*code) {
                    inputs.code_changed.push(name);
                } else {
                    inputs.new_data.push(name);
                }
                continue;
            }
            // Reused, but built after this node was: this node hasn't seen that data
            // (e.g. it failed in the run that rebuilt its parent).
            let parent_built = self
                .previous
                .and_then(|s| s.nodes.get(parent))
                .map(|p| p.built_at);
            if parent_built.is_some_and(|t| t > before.built_at) {
                inputs.new_data.push(name);
            } else {
                inputs.unchanged.push(name);
            }
        }
        inputs
    }
}

/// Decides one node; returns the action and reasons.
fn decide(
    node: &Node,
    context: &Context<'_>,
    now: Timestamp,
    evidence: &mut Vec<Evidence>,
    changed_components: &mut Vec<String>,
) -> (PlanAction, Vec<Reason>) {
    let build = |code, message: String| (PlanAction::Build, vec![Reason::new(code, message)]);
    let before = context.previous.and_then(|s| s.nodes.get(&node.id));
    let Some(before) = before else {
        return build(
            ReasonCode::NeverBuilt,
            "no successful build recorded by ODS".into(),
        );
    };
    let fingerprint = match &node.fingerprint {
        Ok(fingerprint) => fingerprint,
        Err(why) => return build(ReasonCode::CodeEvidenceIncomplete, why.clone()),
    };
    evidence.push(Evidence::new(
        "fingerprint",
        node.id.clone(),
        Some(fingerprint.digest.clone()),
        Exactness::Exact,
    ));
    let diff = fingerprint.diff(&before.fingerprint);
    if !diff.is_empty() {
        *changed_components = diff.all();
        return build(
            ReasonCode::CodeChanged,
            format!(
                "code changed since run {}: {}",
                before.run_id,
                changed_components.join(", ")
            ),
        );
    }
    let inputs = context.inputs(node, before, evidence);
    if !inputs.code_changed.is_empty() {
        return build(
            ReasonCode::UpstreamCodeChanged,
            format!(
                "upstream code changed: {} will be rebuilt",
                inputs.code_changed.join(", ")
            ),
        );
    }
    if !inputs.unknown.is_empty() {
        return build(
            ReasonCode::MissingDataEvidence,
            format!(
                "depends on {}, which ODS doesn't know",
                inputs.unknown.join(", ")
            ),
        );
    }
    if !node.policy.allows_reuse() {
        let settings: Vec<String> = node
            .policy
            .unknown
            .iter()
            .cloned()
            .chain(
                node.policy
                    .unapplied
                    .iter()
                    .filter(|s| s.blocks_reuse)
                    .map(|s| s.setting.clone()),
            )
            .collect();
        return build(
            ReasonCode::PolicyBlocksReuse,
            format!("never reused: ODS can't honour {}", settings.join(", ")),
        );
    }
    decide_on_data(node, before, &inputs, now)
}

/// The decision once code is unchanged: it rests on upstream data and the policy.
fn decide_on_data(
    node: &Node,
    before: &NodeState,
    inputs: &Inputs,
    now: Timestamp,
) -> (PlanAction, Vec<Reason>) {
    let build = |code, message: String| (PlanAction::Build, vec![Reason::new(code, message)]);
    if !inputs.missing.is_empty() {
        return build(
            ReasonCode::MissingDataEvidence,
            format!(
                "no usable data version for {}; measure source freshness before planning",
                inputs.missing.join(", ")
            ),
        );
    }
    if !inputs.new_data.is_empty() {
        let new = inputs.new_data.join(", ");
        if node.policy.require_fresh_data_from == Quorum::All && !inputs.unchanged.is_empty() {
            return (
                PlanAction::Reuse,
                vec![Reason::new(
                    ReasonCode::QuorumNotMet,
                    format!(
                        "new data in {new}, but the policy waits for all parents; no new data yet in {}",
                        inputs.unchanged.join(", ")
                    ),
                )],
            );
        }
        let lag = node.policy.lag_tolerance_secs;
        let due = before
            .built_at
            .unix()
            .saturating_add(i64::try_from(lag).unwrap_or(i64::MAX));
        if lag > 0 && now.unix() < due {
            return (
                PlanAction::Reuse,
                vec![Reason::new(
                    ReasonCode::WithinLagTolerance,
                    format!(
                        "new data in {new}, but it was built at {} and tolerates {} of lag: due at {}",
                        before.built_at,
                        format_duration(lag),
                        Timestamp::from_unix(due)
                    ),
                )],
            );
        }
        return build(
            ReasonCode::NewUpstreamData,
            format!("new upstream data in {new}"),
        );
    }
    (
        PlanAction::Reuse,
        vec![Reason::new(
            ReasonCode::Unchanged,
            format!("code and inputs unchanged since run {}", before.run_id),
        )],
    )
}

/// Plans every node against the last snapshot. Only `selected` nodes appear in the
/// plan, but every node is evaluated, so a selection never changes a decision.
///
/// # Errors
/// Returns [`PlanError`] if the graph has a cycle or duplicate ids.
pub fn plan(
    project: &Project,
    previous: Option<(SnapshotId, &StateSnapshot)>,
    selected: &BTreeSet<String>,
    now: Timestamp,
) -> Result<ExecutionPlan, PlanError> {
    let ordered = order(project)?;
    let mut context = Context {
        sources: project.sources.iter().map(|s| (s.id.as_str(), s)).collect(),
        names: project
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.name.as_str()))
            .chain(
                project
                    .sources
                    .iter()
                    .map(|s| (s.id.as_str(), s.name.as_str())),
            )
            .collect(),
        previous: previous.map(|(_, s)| s),
        decided: BTreeMap::new(),
    };
    let mut entries = Vec::new();
    for (node, depth) in ordered {
        let mut evidence = Vec::new();
        let mut changed_components = Vec::new();
        let (action, reasons) = decide(node, &context, now, &mut evidence, &mut changed_components);
        if action == PlanAction::Reuse {
            evidence.push(Evidence::new(
                "relation_exists",
                node.id.clone(),
                None,
                Exactness::None,
            ));
        }
        context
            .decided
            .insert(node.id.clone(), (action, reasons[0].code));
        if !selected.contains(&node.id) {
            continue;
        }
        let before = context.previous.and_then(|s| s.nodes.get(&node.id));
        let mut entry = PlanEntry::new(
            node.id.clone(),
            node.name.clone(),
            node.kind.clone(),
            action,
            reasons,
            node.policy.clone(),
            depth,
        );
        entry.evidence = evidence;
        entry.depends_on.clone_from(&node.parents);
        entry.before = before.map(|b| b.fingerprint.digest.clone());
        entry.after = node.fingerprint.as_ref().ok().map(|f| f.digest.clone());
        entry.changed_components = changed_components;
        entries.push(entry);
    }
    Ok(ExecutionPlan::new(previous.map(|(id, _)| id), now, entries))
}
