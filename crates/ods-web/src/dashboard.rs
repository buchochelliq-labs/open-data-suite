//! The dashboard's data: what the binary hands over ([`Dashboard`]), and the view
//! models the page and the JSON API are built from ([`ShellView`], [`HomeView`]).
//!
//! The binary owns the providers, so it reads the project, the state store and the plan
//! and fills a [`Dashboard`] with neutral facts (ADR-0001, ADR-0009). This module turns
//! them into view models (ADR-0003): what each tile, row and pill says. The HTML page
//! and `/api/shell` and `/api/home` render the same view models, so the API carries
//! everything the page shows.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use ods_core::state::{ExecutionPlan, PlanAction, ReasonCode, StateSnapshot, Timestamp};
use serde::Serialize;

pub mod state;

/// Version of the dashboard view models in `/api/shell` and `/api/home`. Additive
/// fields don't change it; a removed or retyped field does.
pub const DASHBOARD_SCHEMA_VERSION: u32 = 1;

/// How many recent runs Home lists.
pub const RECENT_RUNS: usize = 5;

/// How many "needs attention" items Home lists; the rest are counted.
const ATTENTION_LIMIT: usize = 5;

// ------------------------------------------------------------------------- inputs

/// Everything the dashboard shows about a project, filled in by the binary.
///
/// It is shown to anyone who can reach the server, so it must never carry
/// credentials or resolved secrets (AGENTS rule 9): names, counts, paths and reasons
/// only. Targets are named, never connected to.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Dashboard {
    /// The project's name.
    pub project: String,
    /// Whose state is kept, e.g. `dev` or `default`.
    pub environment: String,
    /// The scope the state is kept under, e.g. `shop/dev`.
    pub scope: String,
    /// The target builds go to, if known.
    pub target: Option<Target>,
    /// Planned nodes by kind (`model`, `seed`, `snapshot`, …).
    pub node_kinds: BTreeMap<String, usize>,
    /// What the state store holds.
    pub state: StateInput,
    /// Nodes whose column lineage is unknown, and why.
    pub opaque: Vec<OpaqueNode>,
    /// Which modules are set up.
    pub modules: Vec<ModuleStatus>,
    /// The project's nodes, for the Catalog and the model pages (#313).
    pub catalog: crate::catalog::CatalogInput,
}

impl Dashboard {
    /// A project with nothing known yet but its name and environment.
    pub fn new(project: impl Into<String>, environment: impl Into<String>) -> Self {
        let project = project.into();
        let environment = environment.into();
        Self {
            scope: format!("{project}/{environment}"),
            project,
            environment,
            target: None,
            node_kinds: BTreeMap::new(),
            state: StateInput::NoStore {
                store: StoreLocation::new("", ""),
            },
            opaque: Vec::new(),
            modules: Vec::new(),
            catalog: crate::catalog::CatalogInput::default(),
        }
    }

    /// Sets the state scope, when the binary names it differently.
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Sets the target.
    #[must_use]
    pub fn with_target(mut self, target: Option<Target>) -> Self {
        self.target = target;
        self
    }

    /// Sets the planned nodes by kind.
    #[must_use]
    pub fn with_node_kinds(mut self, kinds: BTreeMap<String, usize>) -> Self {
        self.node_kinds = kinds;
        self
    }

    /// Sets what the state store holds.
    #[must_use]
    pub fn with_state(mut self, state: StateInput) -> Self {
        self.state = state;
        self
    }

    /// Sets the nodes whose column lineage is unknown.
    #[must_use]
    pub fn with_opaque(mut self, opaque: Vec<OpaqueNode>) -> Self {
        self.opaque = opaque;
        self
    }

    /// Sets the modules and whether each is set up.
    #[must_use]
    pub fn with_modules(mut self, modules: Vec<ModuleStatus>) -> Self {
        self.modules = modules;
        self
    }

    /// Sets the project's nodes, for the Catalog (#313).
    #[must_use]
    pub fn with_catalog(mut self, catalog: crate::catalog::CatalogInput) -> Self {
        self.catalog = catalog;
        self
    }
}

/// Where builds go, without anything secret: a name and a kind of warehouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Target {
    /// Its name, e.g. `dev`.
    pub name: String,
    /// What kind of warehouse it is, if known, e.g. an adapter type.
    pub kind: Option<String>,
}

impl Target {
    /// A target.
    pub fn new(name: impl Into<String>, kind: Option<String>) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }
}

/// Where the state store is: as people read it (relative to the project, or with `~`),
/// and in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct StoreLocation {
    /// Short, e.g. `.ods/state.db`.
    pub shown: String,
    /// The full path.
    pub full: String,
}

impl StoreLocation {
    /// A location shown as `shown`, at `full`.
    pub fn new(shown: impl Into<String>, full: impl Into<String>) -> Self {
        Self {
            shown: shown.into(),
            full: full.into(),
        }
    }
}

impl From<&str> for StoreLocation {
    fn from(path: &str) -> Self {
        Self::new(path, path)
    }
}

/// What the state store holds for the project's scope.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StateInput {
    /// There is no state store yet: nothing was ever recorded.
    NoStore {
        /// Where it would be.
        store: StoreLocation,
    },
    /// The store couldn't be read.
    Unreadable {
        /// Where it is.
        store: StoreLocation,
        /// Why.
        error: String,
    },
    /// The project itself couldn't be read (e.g. its artifacts are missing or too
    /// old), so its state scope isn't known. Nothing is wrong with the store.
    ProjectUnreadable {
        /// Why.
        error: String,
        /// What to do, from the binary, which knows the project format.
        hint: Option<String>,
    },
    /// The store was read. `runs` may be empty: nothing recorded in this scope yet.
    Recorded(Box<Recorded>),
}

/// Plans the project against the recorded state as of a given time, returning the
/// plan and the warnings that qualify it, or why it couldn't. Supplied by the binary,
/// which owns the providers; offline and cheap.
///
/// Plans depend on time, not only on files: a lag tolerance can expire while nothing
/// changes on disk, turning a REUSE into a BUILD. So Home plans again on every request
/// rather than showing the plan made at load time (AGENTS rule 3).
pub type Planner =
    Arc<dyn Fn(Timestamp) -> Result<(ExecutionPlan, Vec<String>), String> + Send + Sync>;

/// A [`Planner`], for types that must be `Debug`.
#[derive(Clone)]
struct PlannerFn(Planner);

impl fmt::Debug for PlannerFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Planner")
    }
}

impl Recorded {
    /// The plan as of `now`, with what qualifies it: made again with the planner when
    /// there is one, else as made at load time. For pages outside this module, such as
    /// Lineage (#312).
    pub(crate) fn plan_at(&self, now: Timestamp) -> Result<(ExecutionPlan, Vec<String>), String> {
        match &self.planner {
            Some(PlannerFn(planner)) => planner(now),
            None => self.plan.clone().map(|plan| (plan, self.warnings.clone())),
        }
    }
}

/// A readable state store's contents for one scope.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Recorded {
    /// Where the store is.
    pub store: StoreLocation,
    /// The most recent runs, newest first.
    pub runs: Vec<RunRecord>,
    /// How many snapshots the scope has in all, or at least, when `snapshots_capped`.
    pub snapshots: usize,
    /// Whether the store was asked for no more than `snapshots`, and has that many.
    pub snapshots_capped: bool,
    /// The plan against the latest snapshot, or why it couldn't be made.
    pub plan: Result<ExecutionPlan, String>,
    /// What qualifies the plan, e.g. missing source freshness.
    pub warnings: Vec<String>,
    /// Plans again as of the time asked; without one, `plan` is shown as made.
    planner: Option<PlannerFn>,
    /// What the State pages list (#311): more snapshots, and the last run's outcome.
    history: Option<Arc<state::History>>,
    /// The last plan made, and the time bucket it was made for: shared by every
    /// clone of this snapshot's facts, so it lasts until the next reload.
    plan_memo: Arc<Mutex<Option<PlanMemo>>>,
}

/// A plan made for a time bucket: `(bucket, plan, warnings)`.
type PlanMemo = (i64, Result<ExecutionPlan, String>, Vec<String>);

/// How long a plan is reused, in seconds. Lag tolerances are whole minutes or more, so a
/// plan at most this old answers the same (#311 review: not planned on every request).
pub const PLAN_REUSED_FOR: i64 = 30;

impl Recorded {
    /// A readable store.
    pub fn new(
        store: impl Into<StoreLocation>,
        runs: Vec<RunRecord>,
        snapshots: usize,
        plan: Result<ExecutionPlan, String>,
    ) -> Self {
        Self {
            store: store.into(),
            runs,
            snapshots,
            snapshots_capped: false,
            plan,
            warnings: Vec::new(),
            planner: None,
            history: None,
            plan_memo: Arc::default(),
        }
    }

    /// Adds what the State pages list (#311).
    #[must_use]
    pub fn with_history(mut self, history: state::History) -> Self {
        self.history = Some(Arc::new(history));
        self
    }

    /// Plans again with `planner` whenever Home is shown.
    #[must_use]
    pub fn with_planner(mut self, planner: Planner) -> Self {
        self.planner = Some(PlannerFn(planner));
        self
    }

    /// Says `snapshots` is a lower bound: the store was only counted that far.
    #[must_use]
    pub fn capped(mut self, capped: bool) -> Self {
        self.snapshots_capped = capped;
        self
    }

    /// Adds what qualifies the plan.
    #[must_use]
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }
}

/// One recorded run: a committed snapshot, and which of its nodes the run built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunRecord {
    /// The snapshot it committed.
    pub snapshot: u64,
    /// The run's id.
    pub run_id: String,
    /// When it was committed.
    pub recorded_at: Timestamp,
    /// Ids of the nodes this run built, sorted.
    pub built: Vec<String>,
    /// How many recorded nodes kept an earlier run's build: not built successfully by
    /// this run. That covers reuse, but also nodes this run didn't select and nodes it
    /// failed to build (a snapshot keeps their last good build, AGENTS rule 5).
    pub kept: usize,
    /// The target it built in, if recorded.
    pub target: Option<Target>,
    /// For each node the snapshot holds, the names of its recorded fingerprint's
    /// components, so a page can say which were compared (#312).
    pub components: BTreeMap<String, Vec<String>>,
}

impl RunRecord {
    /// A run.
    pub fn new(
        snapshot: u64,
        run_id: impl Into<String>,
        recorded_at: Timestamp,
        built: Vec<String>,
        kept: usize,
    ) -> Self {
        Self {
            snapshot,
            run_id: run_id.into(),
            recorded_at,
            built,
            kept,
            target: None,
            components: BTreeMap::new(),
        }
    }

    /// The run a committed snapshot records: the nodes whose last build is this run's
    /// were built by it; every other node kept an earlier build, for whatever reason
    /// (reused, not selected, or failed: the snapshot can't tell them apart).
    pub fn of(id: u64, snapshot: &StateSnapshot) -> Self {
        let built: Vec<String> = snapshot
            .nodes
            .iter()
            .filter(|(_, node)| node.run_id == snapshot.run_id)
            .map(|(id, _)| id.clone())
            .collect();
        let kept = snapshot.nodes.len() - built.len();
        Self {
            snapshot: id,
            run_id: snapshot.run_id.clone(),
            recorded_at: snapshot.created_at,
            built,
            kept,
            target: snapshot
                .target
                .as_ref()
                .map(|t| Target::new(t.name.clone(), t.kind.clone())),
            components: snapshot
                .nodes
                .iter()
                .map(|(id, node)| {
                    let names = node.fingerprint.components.keys().cloned().collect();
                    (id.clone(), names)
                })
                .collect(),
        }
    }
}

/// A node whose column lineage is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpaqueNode {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Why, for people.
    pub why: String,
}

impl OpaqueNode {
    /// An opaque node.
    pub fn new(id: impl Into<String>, name: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            why: why.into(),
        }
    }
}

/// Whether a module is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModuleState {
    /// It works for this project, and this page shows it.
    Ready,
    /// It works from the CLI; the dashboard doesn't check it for this project.
    Available,
    /// It works, but needs something first (e.g. a first recorded run).
    NotSetUp,
    /// It isn't built yet.
    Planned,
}

/// A module and whether it is set up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ModuleStatus {
    /// Its name, for people.
    pub name: String,
    /// Whether it is set up.
    pub state: ModuleState,
    /// What to know, e.g. how to set it up.
    pub note: Option<String>,
}

impl ModuleStatus {
    /// A module.
    pub fn new(name: impl Into<String>, state: ModuleState, note: Option<String>) -> Self {
        Self {
            name: name.into(),
            state,
            note,
        }
    }
}

// -------------------------------------------------------------------- view models

/// The frame around every page: navigation, pickers, and the current snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ShellView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The project shown.
    pub project: String,
    /// The target shown.
    pub target: TargetView,
    /// The latest snapshot, if any.
    pub snapshot: Option<SnapshotBadge>,
    /// The navigation, in order.
    pub sections: Vec<NavSection>,
    /// Always `local`: there is no server mode yet.
    pub mode: &'static str,
    /// Always true: nothing here writes configuration or state.
    pub read_only: bool,
}

/// The target picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TargetView {
    /// Its name, or the environment when no target is known.
    pub name: String,
    /// The kind of warehouse, if known.
    pub kind: Option<String>,
    /// Whether state is recorded for it.
    pub recorded: bool,
}

/// The latest snapshot, for the header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SnapshotBadge {
    /// Its id.
    pub id: u64,
    /// When it was recorded.
    pub recorded_at: Timestamp,
}

/// Whether a section can be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStatus {
    /// The page shown now.
    Current,
    /// Built: it links somewhere.
    Available,
    /// Not built yet: greyed out, links nowhere.
    Planned,
}

/// A navigation entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NavSection {
    /// Stable key, e.g. `home`.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// Whether it can be opened.
    pub status: SectionStatus,
    /// Where it is, relative to the dashboard's root; `None` when planned.
    pub href: Option<&'static str>,
    /// What to know about it, e.g. that its module already works from the CLI.
    pub note: Option<&'static str>,
    /// Its pages, shown under it while it is the current section.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<NavItem>,
}

/// A page of a section in the navigation, e.g. State's Runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NavItem {
    /// Stable key, e.g. `runs`.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// Where it is, relative to the dashboard's root; `None` when planned.
    pub href: Option<&'static str>,
}

/// A section's pages: its key, then each page's key, label, and href when built.
type SectionPages = (
    &'static str,
    &'static [(&'static str, &'static str, Option<&'static str>)],
);

/// Each section's pages.
const SECTION_ITEMS: [SectionPages; 2] = [
    (
        "catalog",
        &[
            ("models", "Models", Some("catalog")),
            // Sources and freshness evidence, and the semantic layer (#309).
            ("freshness", "Freshness evidence", None),
            ("semantic", "Semantic layer", None),
        ],
    ),
    (
        "state",
        &[
            ("plan", "Plan", Some("state/plan")),
            ("runs", "Runs", Some("state/runs")),
            ("history", "History", None),
            ("policies", "Policies", None),
        ],
    ),
];

/// A section of the design: key, label, href when built, and a note.
type Section = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

/// The sections of the design, in order. Settings is last and sits apart at the bottom
/// of the navigation.
const SECTIONS: [Section; 9] = [
    ("home", "Home", Some("./"), None),
    ("catalog", "Catalog", Some("catalog"), None),
    ("lineage", "Lineage", Some("lineage"), None),
    ("state", "State", Some("state/plan"), None),
    (
        "erd",
        "ERD",
        None,
        Some("ERD pages are planned; the ERD module works from the CLI (ods erd …)"),
    ),
    ("usage", "Usage", None, None),
    ("ci", "CI · Impact", None, None),
    ("agent", "Agent", None, None),
    ("settings", "Settings", None, None),
];

/// Where the state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStatus {
    /// Runs are recorded.
    Recorded,
    /// The store exists but holds nothing for this scope.
    NoRuns,
    /// There is no store yet.
    NoStore,
    /// The store couldn't be read.
    Unreadable,
    /// The project couldn't be read, so neither was the store.
    ProjectUnreadable,
}

/// Home: the project's health at a glance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HomeView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The state scope, e.g. `shop/dev`.
    pub scope: String,
    /// Where the state is.
    pub state: StateStatus,
    /// The state store's location; only shown on loopback.
    pub store: Option<StoreLocation>,
    /// What to do when there are no runs to show.
    pub empty: Option<EmptyState>,
    /// The latest run.
    pub last_run: Option<RunView>,
    /// The four tiles.
    pub tiles: Vec<Tile>,
    /// Recent runs, newest first.
    pub runs: Vec<RunView>,
    /// Nodes to look at, most important first.
    pub attention: Vec<AttentionItem>,
    /// How many more there are than listed.
    pub attention_more: usize,
    /// The plan the attention list comes from.
    pub plan: Option<PlanSummary>,
    /// Health signals; counts are `None` until they exist (#117).
    pub health: Vec<CountRow>,
    /// Test and documentation coverage; counts are `None` until they exist (#117).
    pub coverage: Vec<CoverageRow>,
    /// Which modules are set up.
    pub modules: Vec<ModuleStatus>,
}

/// What Home says instead of runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EmptyState {
    /// A heading.
    pub title: String,
    /// What's going on.
    pub message: String,
    /// Commands to run, each with what it does.
    pub commands: Vec<CommandHint>,
}

/// A command to copy into a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CommandHint {
    /// The command line.
    pub command: String,
    /// What it does.
    pub does: String,
}

fn hint(command: &str, does: &str) -> CommandHint {
    CommandHint {
        command: command.to_owned(),
        does: does.to_owned(),
    }
}

/// A recorded run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunView {
    /// The snapshot it committed.
    pub snapshot: u64,
    /// The run's id.
    pub run_id: String,
    /// Its first eight characters, for people.
    pub short_run_id: String,
    /// When it was committed.
    pub recorded_at: Timestamp,
    /// The command that ran; `None` until runs record it.
    pub command: Option<String>,
    /// How many nodes it built.
    pub built: usize,
    /// How many recorded nodes kept an earlier build: reused, not selected, or failed
    /// this run. The snapshot can't tell which, so this is not a reuse count.
    pub kept: usize,
    /// Names of the nodes it built, in plan order where known.
    pub built_names: Vec<String>,
    /// Always `recorded`: a run commits a snapshot when at least one node succeeded,
    /// keeping only its successful builds (AGENTS rule 5). Whether others failed isn't
    /// stored, so this doesn't say the run succeeded.
    pub outcome: &'static str,
}

/// A tile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Tile {
    /// Stable key.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// The number; `None` when unknown.
    pub value: Option<usize>,
    /// A line under it.
    pub note: String,
}

/// Why a node needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// Its code or checks changed since its last build: it builds next run.
    Changed,
    /// The evidence to reuse it is missing: it builds next run.
    Unknown,
    /// Its column lineage is unknown.
    Opaque,
}

/// A node to look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AttentionItem {
    /// Why.
    pub kind: AttentionKind,
    /// Its name.
    pub node: String,
    /// Its id.
    pub node_id: String,
    /// The reason, for people.
    pub why: String,
}

/// The plan against the latest snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PlanSummary {
    /// The snapshot it compares against.
    pub based_on: Option<u64>,
    /// How many nodes it builds.
    pub build: usize,
    /// How many it reuses.
    pub reuse: usize,
    /// Why it builds what it builds: each built node counted once, by its first (most
    /// important) reason, most common first. Covers every build, including the ones
    /// the attention list doesn't show.
    pub builds_for: Vec<ReasonCount>,
    /// Why it couldn't be made, if it couldn't.
    pub error: Option<String>,
    /// What qualifies it.
    pub warnings: Vec<String>,
}

/// How many planned builds have a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ReasonCount {
    /// The planner's stable reason code.
    pub code: ReasonCode,
    /// The code for people, e.g. `target changed`.
    pub label: String,
    /// How many built nodes have it as their first reason.
    pub count: usize,
}

/// `upstream_code_changed` → `upstream code changed`: the planner's stable code, as
/// words, so new codes read without a table to keep in step.
pub(crate) fn reason_label(code: ReasonCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
        .unwrap_or_else(|| format!("{code:?}"))
}

/// Each built node counted by its first reason; most common first, then by code.
fn builds_for(plan: &ExecutionPlan) -> Vec<ReasonCount> {
    let mut counts: BTreeMap<ReasonCode, usize> = BTreeMap::new();
    for entry in plan.with_action(PlanAction::Build) {
        // A build with no reason would be a planner bug; it still counts, as a build
        // whose reason is unknown is exactly what this must not hide.
        let code = entry
            .reasons
            .first()
            .map_or(ReasonCode::UnknownDependency, |r| r.code);
        *counts.entry(code).or_default() += 1;
    }
    let mut counts: Vec<ReasonCount> = counts
        .into_iter()
        .map(|(code, count)| ReasonCount {
            code,
            label: reason_label(code),
            count,
        })
        .collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then(a.code.cmp(&b.code)));
    counts
}

/// A labelled count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CountRow {
    /// Stable key.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// The count; `None` until it is measured.
    pub count: Option<usize>,
}

/// A coverage bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CoverageRow {
    /// Stable key.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// What the placeholder stands for, e.g. `tested`.
    pub placeholder: &'static str,
    /// How many are covered; `None` until it is measured.
    pub count: Option<usize>,
    /// Out of how many.
    pub total: usize,
}

// ----------------------------------------------------------------------- building

/// `4c0b5c8f-…` → `4c0b5c8f`.
pub(crate) fn short(run_id: &str) -> String {
    run_id.chars().take(8).collect()
}

/// `code changed` → `Code changed`.
pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

impl Dashboard {
    fn recorded(&self) -> Option<&Recorded> {
        match &self.state {
            StateInput::Recorded(recorded) => Some(recorded),
            _ => None,
        }
    }

    fn latest(&self) -> Option<&RunRecord> {
        self.recorded().and_then(|r| r.runs.first())
    }

    /// The shell, with `current` as the page shown.
    pub fn shell(&self, current: &str) -> ShellView {
        let latest = self.latest();
        let target = latest
            .and_then(|r| r.target.clone())
            .or_else(|| self.target.clone());
        ShellView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            project: self.project.clone(),
            target: TargetView {
                name: target
                    .as_ref()
                    .map_or_else(|| self.environment.clone(), |t| t.name.clone()),
                kind: target.and_then(|t| t.kind),
                recorded: latest.is_some(),
            },
            snapshot: latest.map(|r| SnapshotBadge {
                id: r.snapshot,
                recorded_at: r.recorded_at,
            }),
            sections: SECTIONS
                .iter()
                .map(|&(key, label, href, note)| NavSection {
                    key,
                    label,
                    status: match href {
                        _ if key == current => SectionStatus::Current,
                        Some(_) => SectionStatus::Available,
                        None => SectionStatus::Planned,
                    },
                    href,
                    note,
                    items: SECTION_ITEMS
                        .iter()
                        .filter(|(section, _)| *section == key)
                        .flat_map(|(_, items)| items.iter())
                        .map(|&(key, label, href)| NavItem { key, label, href })
                        .collect(),
                })
                .collect(),
            mode: "local",
            read_only: true,
        }
    }

    /// Home as of now. `details` shows local paths and error text (on loopback only).
    pub fn home(&self, details: bool) -> HomeView {
        self.home_at(details, Timestamp::now())
    }

    /// The plan as of `now` and what qualifies it, or `None` without a readable store.
    /// With a [`Planner`], it is planned again once per [`PLAN_REUSED_FOR`] seconds and
    /// reload, and shared by every page (Home, State, and later ones) until then.
    pub(crate) fn plan_at(
        &self,
        now: Timestamp,
    ) -> Option<(Result<ExecutionPlan, String>, Vec<String>)> {
        let recorded = self.recorded()?;
        let Some(PlannerFn(planner)) = &recorded.planner else {
            return Some((recorded.plan.clone(), recorded.warnings.clone()));
        };
        let bucket = now.unix().div_euclid(PLAN_REUSED_FOR);
        let mut memo = recorded
            .plan_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((at, plan, warnings)) = memo.as_ref()
            && *at == bucket
        {
            return Some((plan.clone(), warnings.clone()));
        }
        let (plan, warnings) = match planner(now) {
            Ok((plan, warnings)) => (Ok(plan), warnings),
            Err(error) => (Err(error), Vec::new()),
        };
        *memo = Some((bucket, plan.clone(), warnings.clone()));
        Some((plan, warnings))
    }

    /// Home as of `now`: with a [`Planner`], the plan is made for `now`, at most once
    /// per [`PLAN_REUSED_FOR`] seconds and reload.
    pub fn home_at(&self, details: bool, now: Timestamp) -> HomeView {
        match &self.state {
            StateInput::Recorded(recorded) if recorded.planner.is_some() => {
                let mut fresh = (**recorded).clone();
                if let Some((plan, warnings)) = self.plan_at(now) {
                    fresh.plan = plan;
                    fresh.warnings = warnings;
                }
                fresh.planner = None;
                let mut this = self.clone();
                this.state = StateInput::Recorded(Box::new(fresh));
                this.render_home(details)
            }
            _ => self.render_home(details),
        }
    }

    fn render_home(&self, details: bool) -> HomeView {
        let names = self.names();
        let name_of = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
        let order = self.plan_order();
        let runs: Vec<RunView> = self
            .recorded()
            .map(|r| r.runs.iter().take(RECENT_RUNS).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|run| {
                let mut built = run.built.clone();
                // In plan order; nodes no longer planned (removed since) last.
                built.sort_by(|a, b| {
                    let at = |id: &String| order.get(id.as_str()).copied().unwrap_or(usize::MAX);
                    at(a).cmp(&at(b)).then_with(|| a.cmp(b))
                });
                RunView {
                    snapshot: run.snapshot,
                    run_id: run.run_id.clone(),
                    short_run_id: short(&run.run_id),
                    recorded_at: run.recorded_at,
                    command: None,
                    built: run.built.len(),
                    kept: run.kept,
                    built_names: built.iter().map(|id| name_of(id)).collect(),
                    outcome: "recorded",
                }
            })
            .collect();
        let (state, empty) = self.state_status(details);
        let (attention, attention_more) = self.attention();
        let models = self.node_kinds.get("model").copied().unwrap_or(0);
        HomeView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            scope: self.scope.clone(),
            state,
            store: details
                .then(|| match &self.state {
                    StateInput::NoStore { store } | StateInput::Unreadable { store, .. } => {
                        store.clone()
                    }
                    StateInput::Recorded(r) => r.store.clone(),
                    StateInput::ProjectUnreadable { .. } => StoreLocation::new("", ""),
                })
                .filter(|s| !s.full.is_empty()),
            empty,
            last_run: runs.first().cloned(),
            tiles: self.tiles(runs.first()),
            runs,
            attention,
            attention_more,
            plan: self.plan_summary(details),
            health: [
                ("healthy", "Healthy"),
                ("warning", "Warning"),
                ("failing", "Failing"),
                ("unknown", "Unknown"),
            ]
            .map(|(key, label)| CountRow {
                key,
                label,
                count: None,
            })
            .to_vec(),
            coverage: vec![
                CoverageRow {
                    key: "tests",
                    label: "Models with tests",
                    placeholder: "tested",
                    count: None,
                    total: models,
                },
                CoverageRow {
                    key: "descriptions",
                    label: "Models with descriptions",
                    placeholder: "described",
                    count: None,
                    total: models,
                },
            ],
            modules: self.modules.clone(),
        }
    }

    /// The plan against the latest snapshot, or why there is none.
    fn plan_summary(&self, details: bool) -> Option<PlanSummary> {
        self.recorded().map(|r| match &r.plan {
            Ok(plan) => PlanSummary {
                based_on: plan.based_on.map(|s| s.0),
                build: plan.with_action(PlanAction::Build).count(),
                reuse: plan.with_action(PlanAction::Reuse).count(),
                builds_for: builds_for(plan),
                error: None,
                warnings: r.warnings.clone(),
            },
            Err(error) => PlanSummary {
                based_on: None,
                build: 0,
                reuse: 0,
                builds_for: Vec::new(),
                error: Some(if details {
                    error.clone()
                } else {
                    "the plan couldn't be made; see the server log".to_owned()
                }),
                warnings: r.warnings.clone(),
            },
        })
    }

    /// Node id → name, from the plan and the opaque list.
    fn names(&self) -> BTreeMap<String, String> {
        let mut names: BTreeMap<String, String> = self
            .opaque
            .iter()
            .map(|n| (n.id.clone(), n.name.clone()))
            .collect();
        if let Some(Ok(plan)) = self.recorded().map(|r| &r.plan) {
            names.extend(
                plan.entries
                    .iter()
                    .map(|e| (e.node.clone(), e.name.clone())),
            );
        }
        names
    }

    /// Node id → position in the plan (by depth, then id).
    fn plan_order(&self) -> BTreeMap<&str, usize> {
        match self.recorded().map(|r| &r.plan) {
            Some(Ok(plan)) => plan
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node.as_str(), i))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    fn state_status(&self, details: bool) -> (StateStatus, Option<EmptyState>) {
        let first_run = vec![
            hint(
                "ods state build",
                "plans, builds what changed, and records the run",
            ),
            hint(
                "ods state record",
                "records a build you ran yourself, from its run results",
            ),
        ];
        match &self.state {
            StateInput::NoStore { .. } => (
                StateStatus::NoStore,
                Some(EmptyState {
                    title: "No runs recorded yet".to_owned(),
                    message: "ODS keeps the last successful build of every node in a local \
                              state store. There isn't one yet, so there are no runs, reuse or \
                              plan to show. Record a first run in a terminal; this page \
                              updates when it's done."
                        .to_owned(),
                    commands: first_run,
                }),
            ),
            StateInput::Unreadable { error, .. } => (
                StateStatus::Unreadable,
                Some(EmptyState {
                    title: "The state store can't be read".to_owned(),
                    message: if details {
                        format!("{error}. Nothing was changed.")
                    } else {
                        "See the server log. Nothing was changed.".to_owned()
                    },
                    commands: vec![hint(
                        "ods state doctor",
                        "says what is wrong with the store and how to recover",
                    )],
                }),
            ),
            StateInput::ProjectUnreadable { error, hint } => (
                StateStatus::ProjectUnreadable,
                Some(EmptyState {
                    title: "The project can't be read".to_owned(),
                    message: format!(
                        "{} The state store wasn't opened: without the project, which \
                         state to show isn't known.{}",
                        if details {
                            format!("{error}.")
                        } else {
                            "See the server log.".to_owned()
                        },
                        hint.as_ref()
                            .map_or_else(String::new, |h| format!(" To fix it: {h}.")),
                    ),
                    commands: Vec::new(),
                }),
            ),
            StateInput::Recorded(r) if r.runs.is_empty() => (
                StateStatus::NoRuns,
                Some(EmptyState {
                    title: "No runs recorded yet".to_owned(),
                    message: format!(
                        "The state store has no runs for {} yet. Record a first run in a \
                         terminal; this page updates when it's done.",
                        self.scope
                    ),
                    commands: first_run,
                }),
            ),
            StateInput::Recorded(_) => (StateStatus::Recorded, None),
        }
    }

    fn tiles(&self, last: Option<&RunView>) -> Vec<Tile> {
        let total: usize = self.node_kinds.values().sum();
        let kinds = self
            .node_kinds
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(kind, n)| plural(*n, kind))
            .collect::<Vec<_>>();
        let snapshots = match &self.state {
            StateInput::Recorded(r) => Some(r.snapshots),
            StateInput::NoStore { .. } => Some(0),
            // Unknown, not zero: the store (or which scope to read) couldn't be read.
            StateInput::Unreadable { .. } | StateInput::ProjectUnreadable { .. } => None,
        };
        let capped = self.recorded().is_some_and(|r| r.snapshots_capped);
        vec![
            Tile {
                key: "nodes",
                label: "Nodes",
                value: Some(total),
                note: if kinds.is_empty() {
                    "nothing to build".to_owned()
                } else {
                    kinds.join(" · ")
                },
            },
            Tile {
                key: "kept_last_run",
                label: "Kept earlier build",
                value: last.map(|r| r.kept),
                // What the snapshot shows, and no more: it can't tell reuse from a node
                // left out or failed.
                note: last.map_or_else(
                    || "nothing to compare with yet".to_owned(),
                    |_| "not rebuilt by the last run".to_owned(),
                ),
            },
            Tile {
                key: "built_last_run",
                label: "Built last run",
                value: last.map(|r| r.built),
                note: last.map_or_else(
                    || "record a first run".to_owned(),
                    |r| match r.built_names.as_slice() {
                        [] => "nothing built".to_owned(),
                        [one] => one.clone(),
                        [first, second] => format!("{first} and {second}"),
                        [first, rest @ ..] => format!("{first} + {} more", rest.len()),
                    },
                ),
            },
            Tile {
                key: "snapshots",
                label: "Snapshots",
                value: snapshots,
                note: match snapshots {
                    Some(n) if capped => format!("at least {n}; each keeps only successful builds"),
                    Some(n) if n > 0 => "each keeps only successful builds".to_owned(),
                    Some(_) => "none recorded".to_owned(),
                    None => "unknown".to_owned(),
                },
            },
        ]
    }

    /// Changed and unknown-evidence nodes from the plan, then opaque nodes. Only once a
    /// run is recorded: before that every node builds, and listing them all says
    /// nothing.
    fn attention(&self) -> (Vec<AttentionItem>, usize) {
        let Some(recorded) = self.recorded().filter(|r| !r.runs.is_empty()) else {
            return (Vec::new(), 0);
        };
        let mut items = Vec::new();
        if let Ok(plan) = &recorded.plan {
            // Long run ids read better short, as in the runs table.
            let shorten = |message: &str| {
                recorded.runs.iter().fold(message.to_owned(), |m, run| {
                    m.replace(&run.run_id, &short(&run.run_id))
                })
            };
            for entry in plan.with_action(PlanAction::Build) {
                let kind = entry.reasons.iter().find_map(|r| match r.code {
                    ReasonCode::CodeChanged | ReasonCode::ChecksChanged => {
                        Some((AttentionKind::Changed, r))
                    }
                    ReasonCode::MissingDataEvidence
                    | ReasonCode::CodeEvidenceIncomplete
                    | ReasonCode::UnknownDependency
                    | ReasonCode::RelationUnverified => Some((AttentionKind::Unknown, r)),
                    _ => None,
                });
                if let Some((kind, reason)) = kind {
                    items.push(AttentionItem {
                        kind,
                        node: entry.name.clone(),
                        node_id: entry.node.clone(),
                        why: sentence(&shorten(&reason.message)),
                    });
                }
            }
        }
        items.extend(self.opaque.iter().map(|n| AttentionItem {
            kind: AttentionKind::Opaque,
            node: n.name.clone(),
            node_id: n.id.clone(),
            why: n.why.clone(),
        }));
        // Every kind gets a place before any kind gets a second, so many changed nodes
        // can't hide an opaque one. Then by kind, and in plan order within a kind.
        let mut seen: BTreeMap<AttentionKind, usize> = BTreeMap::new();
        let mut ranked: Vec<(usize, AttentionItem)> = items
            .into_iter()
            .map(|item| {
                let n = seen.entry(item.kind).or_default();
                *n += 1;
                (*n, item)
            })
            .collect();
        ranked.sort_by_key(|(n, item)| (*n, item.kind));
        let more = ranked.len().saturating_sub(ATTENTION_LIMIT);
        let mut shown: Vec<(usize, AttentionItem)> =
            ranked.into_iter().take(ATTENTION_LIMIT).collect();
        shown.sort_by_key(|(n, item)| (item.kind, *n));
        (shown.into_iter().map(|(_, item)| item).collect(), more)
    }
}
