//! Plans BUILD or REUSE for every node (ADR-0013, "Planning rules").

use std::collections::{BTreeMap, BTreeSet};

use ods_core::Quorum;
use ods_core::freshness::format_duration;
use ods_core::state::{
    Evidence, Exactness, ExecutionPlan, Fingerprint, NodeState, PlanAction, PlanEntry, Reason,
    ReasonCode, SnapshotId, StateSnapshot, Timestamp,
};

use crate::{Node, Project, RelationFact, Source};

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
            | ReasonCode::UnknownDependency
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
    /// Parents rebuilt from scratch by a full refresh, or downstream of one.
    refreshed: Vec<String>,
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
                Self::source_input(source, before, name, evidence, &mut inputs);
                continue;
            }
            let Some((action, code)) = self.decided.get(parent) else {
                inputs.unknown.push(parent.clone());
                continue;
            };
            evidence.push(Evidence::new(
                "parent_decision",
                parent.clone(),
                Some(format!(
                    "{}: {}",
                    if *action == PlanAction::Build {
                        "build"
                    } else {
                        "reuse"
                    },
                    serde_json_word(*code)
                )),
                Exactness::Exact,
            ));
            if *action == PlanAction::Build {
                if is_code_change(*code) {
                    inputs.code_changed.push(name);
                } else if matches!(
                    *code,
                    ReasonCode::FullRefreshRequested | ReasonCode::UpstreamFullRefresh
                ) {
                    inputs.refreshed.push(name);
                } else {
                    inputs.new_data.push(name);
                }
                continue;
            }
            // Reused: has it been rebuilt since this node read it? Runs are compared,
            // not clocks.
            let current = self
                .previous
                .and_then(|s| s.nodes.get(parent))
                .map(|p| p.run_id.as_str());
            match (before.parents.get(parent).map(String::as_str), current) {
                (Some(seen), Some(now)) if seen == now => inputs.unchanged.push(name),
                (Some(_), Some(_)) => inputs.new_data.push(name),
                _ => inputs.missing.push(format!(
                    "{name} (which of its builds this node read is unknown)"
                )),
            }
        }
        inputs
    }

    fn source_input(
        source: &Source,
        before: &NodeState,
        name: String,
        evidence: &mut Vec<Evidence>,
        inputs: &mut Inputs,
    ) {
        let now = source.version.as_ref();
        evidence.push(Evidence::new(
            "source_data_version",
            source.id.clone(),
            now.map(|v| v.value.clone()),
            now.map_or(Exactness::None, |v| v.exactness),
        ));
        let then = before.inputs.get(&source.id).and_then(Option::as_ref);
        // A version observed before the node's last build can't show data that arrived
        // after it.
        let current = source.observed_at.is_some_and(|at| at > before.built_at);
        match (now, then) {
            (Some(now), Some(then))
                if current && now.exactness.allows_reuse() && then.exactness.allows_reuse() =>
            {
                if now == then {
                    inputs.unchanged.push(name);
                } else {
                    inputs.new_data.push(name);
                }
            }
            (Some(_), _) if !current => inputs.missing.push(format!(
                "{name} (its version was measured before the last build)"
            )),
            _ => inputs.missing.push(name),
        }
    }
}

/// A reason code as it is serialized, for evidence values.
fn serde_json_word(code: ReasonCode) -> String {
    format!("{code:?}")
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            let lower = c.to_ascii_lowercase();
            if c.is_ascii_uppercase() && i > 0 {
                vec!['_', lower]
            } else {
                vec![lower]
            }
        })
        .collect()
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
        if changed_components.iter().any(|c| c == Fingerprint::SCHEME) {
            return build(
                ReasonCode::CodeChanged,
                format!(
                    "ODS fingerprints code differently since run {}, so it can't be compared: built once",
                    before.run_id
                ),
            );
        }
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
    // Like a code change, a full refresh reaches everything downstream now: it is how
    // data gets corrected, so waiting out a lag tolerance would defeat it.
    if !inputs.refreshed.is_empty() {
        return build(
            ReasonCode::UpstreamFullRefresh,
            format!(
                "upstream full refresh: {} will be rebuilt from scratch",
                inputs.refreshed.join(", ")
            ),
        );
    }
    if !inputs.unknown.is_empty() {
        return build(
            ReasonCode::UnknownDependency,
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
    if node.parents.is_empty() && !node.self_contained {
        return build(
            ReasonCode::MissingDataEvidence,
            "declares no inputs, so ODS can't tell when the data it reads changes".into(),
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
                "no usable evidence of the data in {}; measure source freshness before planning",
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
    let cosmetic = node
        .fingerprint
        .as_ref()
        .map_or_else(|_| Vec::new(), |f| f.cosmetic_changes(&before.fingerprint));
    let message = if cosmetic.is_empty() {
        format!("code and inputs unchanged since run {}", before.run_id)
    } else {
        format!(
            "code and inputs unchanged since run {}; only formatting changed ({}): comments or whitespace",
            before.run_id,
            cosmetic.join(", ")
        )
    };
    (
        PlanAction::Reuse,
        vec![Reason::new(ReasonCode::Unchanged, message)],
    )
}

/// Plans every node against the last snapshot. Only `selected` nodes appear in the
/// plan, but every node is evaluated, so a selection never changes a decision (except
/// what a full refresh forces, see [`PlanOptions`]).
///
/// # Errors
/// Returns [`PlanError`] if the graph has a cycle or duplicate ids.
pub fn plan(
    project: &Project,
    previous: Option<(SnapshotId, &StateSnapshot)>,
    selected: &BTreeSet<String>,
    now: Timestamp,
) -> Result<ExecutionPlan, PlanError> {
    plan_with(project, previous, selected, now, PlanOptions::default())
}

/// How a run asks to be planned, beyond what changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlanOptions {
    /// A full refresh: selected nodes a full refresh builds differently
    /// ([`Node::full_refresh_rebuilds`]) are built even if unchanged. Their readers see
    /// new data, as after any rebuild.
    pub full_refresh: bool,
    /// Relations were checked (#230): a node that would be reused but wasn't checked
    /// ([`RelationFact::Unchecked`]) is built, as unverified, rather than reused on
    /// trust.
    pub relations_checked: bool,
}

impl PlanOptions {
    /// Asks for a full refresh.
    #[must_use]
    pub fn full_refresh(mut self) -> Self {
        self.full_refresh = true;
        self
    }

    /// Says relations were checked, so an unchecked one is never reused.
    #[must_use]
    pub fn relations_checked(mut self) -> Self {
        self.relations_checked = true;
        self
    }
}

/// Records what is known about a reused node's relation, and returns why it must be
/// built instead, if it must.
fn relation_check(node: &Node, checked: bool, evidence: &mut Vec<Evidence>) -> Option<Reason> {
    let (value, exactness, reason) = match &node.relation {
        RelationFact::Unchecked if checked => (
            None,
            Exactness::None,
            Some(Reason::new(
                ReasonCode::RelationUnverified,
                "couldn't check that its table is still in the warehouse: it wasn't checked",
            )),
        ),
        RelationFact::Unchecked => (None, Exactness::None, None),
        RelationFact::Present(kind) => (
            Some(kind.clone().unwrap_or_else(|| "present".to_owned())),
            Exactness::Exact,
            None,
        ),
        RelationFact::Missing => (
            Some("missing".to_owned()),
            Exactness::Exact,
            Some(Reason::new(
                ReasonCode::RelationMissing,
                "its table isn't in the warehouse",
            )),
        ),
        RelationFact::Unverified(why) => (
            None,
            Exactness::None,
            Some(Reason::new(
                ReasonCode::RelationUnverified,
                format!("couldn't check that its table is still in the warehouse: {why}"),
            )),
        ),
    };
    evidence.push(Evidence::new(
        "relation_exists",
        node.id.clone(),
        value,
        exactness,
    ));
    reason
}

/// The nodes a plan would reuse if their relations are all still there: the ones worth
/// checking (#230). It covers the whole project, and plans it without a full refresh,
/// which only adds builds and depends on the selection: so it is a superset of what
/// any selection reuses. Relation facts only ever turn a reuse into a build, so
/// checking these once is enough.
///
/// # Errors
/// As [`plan`].
pub fn reuse_candidates(
    project: &Project,
    previous: Option<(SnapshotId, &StateSnapshot)>,
    now: Timestamp,
    options: PlanOptions,
) -> Result<Vec<String>, PlanError> {
    let mut unchecked = project.clone();
    for node in &mut unchecked.nodes {
        node.relation = RelationFact::Unchecked;
    }
    let all = unchecked.nodes.iter().map(|n| n.id.clone()).collect();
    let mut options = options;
    options.full_refresh = false;
    options.relations_checked = false;
    Ok(plan_with(&unchecked, previous, &all, now, options)?
        .entries
        .into_iter()
        .filter(|e| e.action == PlanAction::Reuse)
        .map(|e| e.node)
        .collect())
}

/// [`plan`], with `options`.
///
/// # Errors
/// As [`plan`].
pub fn plan_with(
    project: &Project,
    previous: Option<(SnapshotId, &StateSnapshot)>,
    selected: &BTreeSet<String>,
    now: Timestamp,
    options: PlanOptions,
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
        let (action, reasons) =
            if options.full_refresh && node.full_refresh_rebuilds && selected.contains(&node.id) {
                (
                    PlanAction::Build,
                    vec![Reason::new(
                        ReasonCode::FullRefreshRequested,
                        "full refresh requested: rebuilt from scratch",
                    )],
                )
            } else {
                decide(node, &context, now, &mut evidence, &mut changed_components)
            };
        // Reuse vouches for a build that is still there: one that isn't, or can't be
        // shown to be, is built (#230). Decided before children look at it, so they
        // see the rebuild.
        let (action, reasons) = if action == PlanAction::Reuse {
            relation_check(node, options.relations_checked, &mut evidence)
                .map_or((action, reasons), |reason| {
                    (PlanAction::Build, vec![reason])
                })
        } else {
            (action, reasons)
        };
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
