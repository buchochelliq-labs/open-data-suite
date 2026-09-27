//! `ods state explain`, `why-build`, `why-skip`, `diff`, `graph` and `history <node>`
//! (#21): State decisions, past builds and differences, explained from the plan and
//! from what snapshots record.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::{NodeState, PlanAction, PlanEntry, SnapshotId, StateSnapshot, Timestamp};
use ods_sdk::contracts::state_store::StateStore;
use ods_state::{Change, Explanation, NodeEvent, StateDiff};
use serde::Serialize;

use super::state_plan::{
    PlanReport, Sources, Workspace, block_on, common, display_name, store_error,
};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, TreeItem, ViewNode};

/// What an explaining command asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Question {
    /// Why it gets its action.
    Explain,
    /// Why it builds.
    WhyBuild,
    /// Why it is reused (skipped).
    WhySkip,
}

impl Question {
    pub(super) const ALL: [Question; 3] =
        [Question::Explain, Question::WhyBuild, Question::WhySkip];

    pub(super) fn name(self) -> &'static str {
        match self {
            Question::Explain => "explain",
            Question::WhyBuild => "why-build",
            Question::WhySkip => "why-skip",
        }
    }

    pub(super) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.name() == name)
    }

    fn about(self) -> &'static str {
        match self {
            Question::Explain => {
                "Explain why a node would be built or reused, traced upstream to the root causes, with the evidence"
            }
            Question::WhyBuild => {
                "Why a node would be built: its reasons, traced upstream (or why it isn't)"
            }
            Question::WhySkip => "Why a node would be reused instead of built (or why it isn't)",
        }
    }
}

fn with_now(command: Command) -> Command {
    command.arg(
        Arg::new("now")
            .long("now")
            .value_name("TIMESTAMP")
            .hide(true)
            .help("Plan as of this time (RFC 3339), for reproducible output"),
    )
}

/// `explain`, `why-build` and `why-skip`.
pub(super) fn question_command(question: Question) -> Command {
    with_now(common(
        Command::new(question.name()).about(question.about()),
    ))
    .arg(
        Arg::new("node")
            .value_name("NODE")
            .required(true)
            .help("A model, seed or snapshot: its name or unique id"),
    )
}

/// `ods state diff`.
pub(super) fn diff_command() -> Command {
    with_now(common(Command::new("diff").about(
        "What changed since the recorded state (code, source data, nodes added or removed), or between two snapshots",
    )))
    .arg(
        Arg::new("from")
            .long("from")
            .value_name("SNAPSHOT")
            .value_parser(clap::value_parser!(u64))
            .requires("to")
            .help("Compare two recorded snapshots instead (see `ods state history`): the older one"),
    )
    .arg(
        Arg::new("to")
            .long("to")
            .value_name("SNAPSHOT")
            .value_parser(clap::value_parser!(u64))
            .requires("from")
            .help("The newer one"),
    )
}

/// `ods state graph`.
pub(super) fn graph_command() -> Command {
    with_now(common(Command::new("graph").about(
        "The plan as a graph: each node, its action and why, and what it reads",
    )))
    .arg(
        Arg::new("changed")
            .long("changed")
            .action(ArgAction::SetTrue)
            .help("Only the nodes that would be built"),
    )
    .arg(
        Arg::new("format")
            .long("format")
            .value_name("FORMAT")
            .value_parser(["mermaid", "dot"])
            .default_value("mermaid")
            .help("mermaid or dot (Graphviz)"),
    )
}

/// The node `spec` names among `ids`: its unique id, or a name that only one has.
fn resolve<'a>(spec: &str, ids: impl IntoIterator<Item = &'a str>) -> Result<String, CliError> {
    let ids: BTreeSet<&str> = ids.into_iter().collect();
    if ids.contains(spec) {
        return Ok(spec.to_owned());
    }
    let matching: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|id| display_name(id) == spec)
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(CliError::new(
            ExitStatus::Usage,
            codes::LINEAGE_TARGET,
            format!("no model, seed or snapshot is named `{spec}`"),
        )
        .with_hint("use its name or unique id, as `ods state plan` lists them")),
        many => Err(CliError::new(
            ExitStatus::Usage,
            codes::LINEAGE_TARGET,
            format!("`{spec}` names more than one node: {}", many.join(", ")),
        )
        .with_hint("use the unique id")),
    }
}

// ---------------------------------------------------------------------------- explain

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ExplainReport {
    scope: String,
    based_on: Option<SnapshotId>,
    question: Question,
    /// The answer in a sentence.
    verdict: String,
    explanation: Explanation,
    /// Its recorded build, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_build: Option<LastBuild>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct LastBuild {
    run_id: String,
    built_at: Timestamp,
    tested_in: Option<String>,
}

impl ExplainReport {
    pub(super) fn build(
        question: Question,
        args: &ArgMatches,
        config: &Loaded,
    ) -> Result<Self, CliError> {
        let plan = PlanReport::build(args, config)?;
        let spec = args.get_one::<String>("node").map_or("", String::as_str);
        let node = resolve(spec, plan.plan.entries.iter().map(|e| e.node.as_str()))?;
        let explanation = ods_state::explain(&plan.plan, &node).ok_or_else(|| {
            CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("`{spec}` isn't planned"),
            )
        })?;
        let name = explanation.entry.name.clone();
        let builds = explanation.entry.action == PlanAction::Build;
        let verdict = match (question, builds) {
            (Question::WhySkip, true) => format!("{name} isn't reused: it would be built"),
            (Question::WhyBuild, false) => format!("{name} isn't built: it would be reused"),
            (_, true) => format!("{name} would be built"),
            (_, false) => format!("{name} would be reused"),
        };
        let last_build = recorded_state(&plan, &node)?.map(|s| LastBuild {
            run_id: s.run_id.clone(),
            built_at: s.built_at,
            tested_in: s.tested.as_ref().map(|t| t.run_id.clone()),
        });
        Ok(Self {
            scope: plan.scope,
            based_on: plan.based_on,
            question,
            verdict,
            explanation,
            last_build,
            warnings: plan.warnings,
        })
    }
}

/// The node's state in the snapshot the plan compared with.
fn recorded_state(plan: &PlanReport, node: &str) -> Result<Option<NodeState>, CliError> {
    let Some(id) = plan.based_on else {
        return Ok(None);
    };
    let store = block_on(ods_store_sqlite::SqliteStateStore::open_existing(
        &plan.state_db,
    ))?
    .map_err(|e| store_error(&e))?;
    let scope = ods_sdk::contracts::state_store::StateScope::new(
        plan.scope.split_once('/').map_or("", |(p, _)| p),
        plan.scope.split_once('/').map_or("", |(_, e)| e),
    )
    .map_err(|e| CliError::new(ExitStatus::Failure, codes::STATE_INPUT, e))?;
    let snapshot = block_on(store.get(&scope, id))?.map_err(|e| store_error(&e))?;
    Ok(snapshot.and_then(|s| s.snapshot.nodes.get(node).cloned()))
}

fn action_span(action: PlanAction) -> Span {
    match action {
        PlanAction::Build => Span::toned("build", Tone::Warning),
        _ => Span::toned("reuse", Tone::Success),
    }
}

fn explanation_tree(e: &Explanation) -> TreeItem {
    let entry = &e.entry;
    let mut label = vec![
        Span::toned(entry.name.as_str(), Tone::Code),
        Span::plain(": "),
        action_span(entry.action),
    ];
    if e.repeated {
        label.push(Span::toned(" (see above)", Tone::Muted));
        return TreeItem::leaf(label);
    }
    let mut children: Vec<TreeItem> = entry
        .reasons
        .iter()
        .map(|r| TreeItem::leaf(vec![Span::plain(r.message.as_str())]))
        .collect();
    if !entry.changed_components.is_empty() {
        children.push(TreeItem::leaf(vec![
            Span::toned("changed: ", Tone::Muted),
            Span::plain(entry.changed_components.join(", ")),
        ]));
    }
    for evidence in &entry.evidence {
        children.push(TreeItem::leaf(vec![Span::toned(
            format!(
                "evidence: {} of {}{}",
                evidence.kind,
                display_name(&evidence.subject),
                evidence
                    .value
                    .as_ref()
                    .map(|v| format!(" = {}", short(v)))
                    .unwrap_or_default()
            ),
            Tone::Muted,
        )]));
    }
    children.extend(e.causes.iter().map(explanation_tree));
    TreeItem { label, children }
}

/// Digests are long; people only need to tell them apart.
fn short(value: &str) -> String {
    if value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{}…", &value[..12])
    } else {
        value.to_owned()
    }
}

impl Present for ExplainReport {
    const COMMAND: &'static str = "state.explain";

    fn view(&self) -> ViewNode {
        let mut summary = vec![
            (
                "scope".into(),
                vec![Span::toned(self.scope.as_str(), Tone::Code)],
            ),
            (
                "compared with".into(),
                vec![Span::plain(self.based_on.map_or_else(
                    || "no recorded state".to_owned(),
                    |id| format!("snapshot {id}"),
                ))],
            ),
        ];
        if let Some(last) = &self.last_build {
            summary.push((
                "last built".into(),
                vec![Span::plain(format!(
                    "{} in run {}{}",
                    last.built_at,
                    last.run_id,
                    last.tested_in
                        .as_ref()
                        .map(|t| format!(", tested in run {t}"))
                        .unwrap_or_default()
                ))],
            ));
        }
        let mut blocks = vec![
            ViewNode::Heading(self.verdict.clone()),
            ViewNode::KeyValue(summary),
            ViewNode::Tree(explanation_tree(&self.explanation)),
        ];
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        ViewNode::Group(blocks)
    }
}

// ---------------------------------------------------------------------------- history

/// Every snapshot of the scope, oldest first.
fn all_snapshots(ws: &Workspace) -> Result<Vec<(SnapshotId, StateSnapshot)>, CliError> {
    if !ws.state_db.is_file() {
        return Ok(Vec::new());
    }
    let store = ws.open_store()?;
    let summaries = block_on(store.history(&ws.scope, usize::MAX))?.map_err(|e| store_error(&e))?;
    let mut snapshots = Vec::new();
    for summary in summaries.into_iter().rev() {
        if let Some(stored) =
            block_on(store.get(&ws.scope, summary.id))?.map_err(|e| store_error(&e))?
        {
            snapshots.push((stored.id, stored.snapshot));
        }
    }
    Ok(snapshots)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct NodeHistoryReport {
    state_db: PathBuf,
    scope: String,
    node: String,
    /// Newest first.
    events: Vec<NodeEvent>,
}

impl NodeHistoryReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
        let snapshots = all_snapshots(&ws)?;
        let spec = args.get_one::<String>("node").map_or("", String::as_str);
        let known: BTreeSet<&str> = ws
            .project
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .chain(
                snapshots
                    .iter()
                    .flat_map(|(_, s)| s.nodes.keys().map(String::as_str)),
            )
            .collect();
        let node = resolve(spec, known)?;
        let refs: Vec<(SnapshotId, &StateSnapshot)> =
            snapshots.iter().map(|(id, s)| (*id, s)).collect();
        let mut events = ods_state::node_history(&refs, &node);
        events.truncate(args.get_one::<usize>("limit").copied().unwrap_or(20));
        Ok(Self {
            state_db: ws.state_db,
            scope: ws.scope.to_string(),
            node,
            events,
        })
    }
}

fn change_text(change: &Change) -> String {
    match change {
        Change::Code {
            changed,
            added,
            removed,
        } => {
            let mut parts = Vec::new();
            if !changed.is_empty() {
                parts.push(format!("{} changed", changed.join(", ")));
            }
            if !added.is_empty() {
                parts.push(format!("{} added", added.join(", ")));
            }
            if !removed.is_empty() {
                parts.push(format!("{} removed", removed.join(", ")));
            }
            format!("code: {}", parts.join("; "))
        }
        Change::CodeUnknown { why } => format!("code can't be fingerprinted completely: {why}"),
        Change::Data {
            source,
            before,
            after,
        } => format!(
            "data of {}: {} → {}",
            display_name(source),
            before.as_deref().unwrap_or("unknown"),
            after.as_deref().unwrap_or("unknown")
        ),
        Change::Upstream { parent, .. } => format!("{} was rebuilt", display_name(parent)),
        Change::Target { before, after } => format!(
            "target: {} → {}",
            before.as_deref().unwrap_or("not recorded"),
            after.as_deref().unwrap_or("not recorded")
        ),
        _ => "changed".to_owned(),
    }
}

fn changes_text(changes: &[Change]) -> String {
    changes
        .iter()
        .map(change_text)
        .collect::<Vec<_>>()
        .join("; ")
}

impl Present for NodeHistoryReport {
    const COMMAND: &'static str = "state.history";

    fn view(&self) -> ViewNode {
        let rows = self
            .events
            .iter()
            .map(|event| match event {
                NodeEvent::Built {
                    snapshot,
                    run_id,
                    built_at,
                    first,
                    changes,
                } => vec![
                    vec![Span::plain(snapshot.to_string())],
                    vec![Span::toned("built", Tone::Warning)],
                    vec![Span::plain(built_at.to_string())],
                    vec![Span::toned(run_id.as_str(), Tone::Code)],
                    vec![Span::plain(if *first {
                        "first recorded build".to_owned()
                    } else if changes.is_empty() {
                        "nothing recorded changed: rebuilt for a reason snapshots don't keep (e.g. a full refresh or a missing relation)".to_owned()
                    } else {
                        changes_text(changes)
                    })],
                ],
                NodeEvent::Tested {
                    snapshot,
                    run_id,
                    at,
                } => vec![
                    vec![Span::plain(snapshot.to_string())],
                    vec![Span::toned("tested", Tone::Success)],
                    vec![Span::plain(at.to_string())],
                    vec![Span::toned(run_id.as_str(), Tone::Code)],
                    vec![Span::plain("its build passed its tests")],
                ],
                NodeEvent::Dropped { snapshot } => vec![
                    vec![Span::plain(snapshot.to_string())],
                    vec![Span::toned("dropped", Tone::Removed)],
                    vec![],
                    vec![],
                    vec![Span::plain(
                        "no longer recorded: gone from the project, or its state started afresh",
                    )],
                ],
                _ => vec![vec![], vec![], vec![], vec![], vec![]],
            })
            .collect();
        ViewNode::Group(vec![
            ViewNode::Heading(format!(
                "History of {} in {}",
                display_name(&self.node),
                self.scope
            )),
            ViewNode::Table {
                title: None,
                columns: vec![
                    "snapshot".into(),
                    "event".into(),
                    "at".into(),
                    "run".into(),
                    "why".into(),
                ],
                rows,
            },
        ])
    }
}

// ------------------------------------------------------------------------------- diff

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct DiffReport {
    scope: String,
    /// The older side: a snapshot.
    from: Option<SnapshotId>,
    /// The newer side: a snapshot, or `None` for the project now.
    to: Option<SnapshotId>,
    diff: StateDiff,
}

impl DiffReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
        let scope = ws.scope.to_string();
        let ids = (
            args.get_one::<u64>("from").copied(),
            args.get_one::<u64>("to").copied(),
        );
        if let (Some(from), Some(to)) = ids {
            let (from, to) = (SnapshotId(from), SnapshotId(to));
            let store = ws.open_store()?;
            let get = |id: SnapshotId| {
                block_on(store.get(&ws.scope, id))?
                    .map_err(|e| store_error(&e))?
                    .ok_or_else(|| {
                        CliError::new(
                            ExitStatus::Usage,
                            codes::STATE_INPUT,
                            format!("snapshot {id} isn't in {scope}"),
                        )
                        .with_hint("`ods state history` lists them")
                    })
            };
            let (before, after) = (get(from)?, get(to)?);
            return Ok(Self {
                diff: ods_state::diff_states(&before.snapshot, &after.snapshot),
                scope,
                from: Some(from),
                to: Some(to),
            });
        }
        let latest = if ws.state_db.is_file() {
            ws.latest(&ws.open_store()?)?
        } else {
            None
        };
        let Some(latest) = latest else {
            return Err(CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                format!("{scope} has no recorded state to compare with"),
            )
            .with_hint(
                "build first with `ods state run`, or record a dbt run with `ods state record`",
            ));
        };
        Ok(Self {
            diff: ods_state::diff_project(&latest.snapshot, &ws.project),
            scope,
            from: Some(latest.id),
            to: None,
        })
    }
}

impl Present for DiffReport {
    const COMMAND: &'static str = "state.diff";

    fn view(&self) -> ViewNode {
        let side = |id: Option<SnapshotId>| {
            id.map_or_else(
                || "the project now".to_owned(),
                |id| format!("snapshot {id}"),
            )
        };
        let mut blocks = vec![
            ViewNode::Heading(format!(
                "{} compared with {}",
                side(self.to),
                side(self.from)
            )),
            ViewNode::KeyValue(vec![(
                "scope".into(),
                vec![Span::toned(self.scope.as_str(), Tone::Code)],
            )]),
        ];
        if self.diff.is_empty() {
            blocks.push(ViewNode::Paragraph(vec![Span::plain("nothing differs")]));
            return ViewNode::Group(blocks);
        }
        let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
        for id in &self.diff.added {
            rows.push(vec![
                vec![Span::toned(display_name(id), Tone::Code)],
                vec![Span::toned("added", Tone::Added)],
                vec![],
            ]);
        }
        for id in &self.diff.removed {
            rows.push(vec![
                vec![Span::toned(display_name(id), Tone::Code)],
                vec![Span::toned("removed", Tone::Removed)],
                vec![],
            ]);
        }
        for node in &self.diff.changed {
            rows.push(vec![
                vec![Span::toned(display_name(&node.node), Tone::Code)],
                vec![Span::toned(
                    if node.rebuilt { "rebuilt" } else { "changed" },
                    Tone::Warning,
                )],
                vec![Span::plain(if node.changes.is_empty() {
                    "nothing recorded changed".to_owned()
                } else {
                    changes_text(&node.changes)
                })],
            ]);
        }
        blocks.push(ViewNode::Table {
            title: None,
            columns: vec!["node".into(), "change".into(), "what".into()],
            rows,
        });
        ViewNode::Group(blocks)
    }
}

// ------------------------------------------------------------------------------ graph

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct GraphNode {
    node: String,
    name: String,
    action: PlanAction,
    /// The main reason.
    why: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct GraphReport {
    scope: String,
    format: String,
    changed_only: bool,
    nodes: Vec<GraphNode>,
    /// `[from, to]`: `to` reads `from`.
    edges: Vec<[String; 2]>,
    /// The graph in `format`.
    text: String,
}

impl GraphReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let plan = PlanReport::build(args, config)?;
        let changed_only = args.get_flag("changed");
        let entries: Vec<&PlanEntry> = plan
            .plan
            .entries
            .iter()
            .filter(|e| !changed_only || e.action == PlanAction::Build)
            .collect();
        let included: BTreeSet<&str> = entries.iter().map(|e| e.node.as_str()).collect();
        let nodes: Vec<GraphNode> = entries
            .iter()
            .map(|e| GraphNode {
                node: e.node.clone(),
                name: e.name.clone(),
                action: e.action,
                why: e
                    .reasons
                    .first()
                    .map(|r| r.message.clone())
                    .unwrap_or_default(),
            })
            .collect();
        let edges: Vec<[String; 2]> = entries
            .iter()
            .flat_map(|e| {
                e.depends_on
                    .iter()
                    .filter(|p| included.contains(p.as_str()))
                    .map(|p| [p.clone(), e.node.clone()])
            })
            .collect();
        let format = args
            .get_one::<String>("format")
            .cloned()
            .unwrap_or_else(|| "mermaid".to_owned());
        let text = if format == "dot" {
            dot(&nodes, &edges)
        } else {
            mermaid(&nodes, &edges)
        };
        Ok(Self {
            scope: plan.scope,
            format,
            changed_only,
            nodes,
            edges,
            text,
        })
    }
}

fn ids(nodes: &[GraphNode]) -> BTreeMap<&str, String> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.node.as_str(), format!("n{i}")))
        .collect()
}

fn label(n: &GraphNode) -> String {
    let action = if n.action == PlanAction::Build {
        "build"
    } else {
        "reuse"
    };
    format!("{}: {action}", n.name)
}

fn mermaid(nodes: &[GraphNode], edges: &[[String; 2]]) -> String {
    let ids = ids(nodes);
    let mut out = String::from("graph LR\n");
    for n in nodes {
        let class = if n.action == PlanAction::Build {
            "build"
        } else {
            "reuse"
        };
        let _ = writeln!(
            out,
            "  {}[\"{}\"]:::{class}",
            ids[n.node.as_str()],
            label(n).replace('"', "#quot;")
        );
    }
    for [from, to] in edges {
        let _ = writeln!(out, "  {} --> {}", ids[from.as_str()], ids[to.as_str()]);
    }
    out.push_str("  classDef build fill:#fde2c4,stroke:#c2410c\n");
    out.push_str("  classDef reuse fill:#dcfce7,stroke:#15803d\n");
    out
}

fn dot(nodes: &[GraphNode], edges: &[[String; 2]]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from("digraph state {\n  rankdir=LR;\n  node [shape=box];\n");
    for n in nodes {
        let color = if n.action == PlanAction::Build {
            "#c2410c"
        } else {
            "#15803d"
        };
        let _ = writeln!(
            out,
            "  {} [label={}, tooltip={}, color=\"{color}\"];",
            quote(&n.node),
            quote(&label(n)),
            quote(&n.why)
        );
    }
    for [from, to] in edges {
        let _ = writeln!(out, "  {} -> {};", quote(from), quote(to));
    }
    out.push_str("}\n");
    out
}

impl GraphReport {
    /// The graph, as written without `--json`.
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

impl Present for GraphReport {
    const COMMAND: &'static str = "state.graph";

    fn view(&self) -> ViewNode {
        // Without `--json` the text is written as it is; this is for completeness.
        ViewNode::Paragraph(vec![Span::plain(self.text.trim_end())])
    }
}
