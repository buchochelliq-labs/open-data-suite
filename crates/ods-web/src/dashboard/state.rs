//! The State pages' data (#311): the plan with its Why panel, the runs the state store
//! recorded, and one run.
//!
//! As for Home, the binary hands over neutral facts ([`History`], [`LastRun`]) and this
//! module turns them into view models that the HTML pages and `/api/state/...` both
//! render. Nothing here claims more than the store records (AGENTS rule 3):
//!
//! - a snapshot records which nodes a run built, not which it reused, failed or left
//!   out, so the rest are *kept earlier build*;
//! - failures are only known for the last run, from the file `ods state retry` keeps
//!   beside the store. It is shown only for the scope it names, and tied to the
//!   snapshot that records its run id; that it recorded nothing is only ever
//!   *inferred*;
//! - durations, start times and who ran a command aren't recorded: they are `None`,
//!   shown as placeholders.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use ods_core::state::{
    Evidence, Exactness, ExecutionPlan, NodeState, PlanAction, PlanEntry, ReasonCode,
    StateSnapshot, Timestamp,
};
use ods_state::{Change, Explanation};
use serde::Serialize;

use super::{
    CommandHint, DASHBOARD_SCHEMA_VERSION, Dashboard, EmptyState, StateInput, StateStatus,
    StoreLocation, Target, hint, plural, reason_label, short,
};

/// How many runs the Runs page lists, newest first. The binary reads one more
/// snapshot, so the oldest listed run can say what it replaced.
pub const RUNS_LISTED: usize = 50;

/// How many earlier runs a run's page lists.
const EARLIER_RUNS: usize = 5;

// ------------------------------------------------------------------------- inputs

/// What the State pages list beyond Home's recent runs, filled in by the binary.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct History {
    /// Snapshots, newest first: up to [`RUNS_LISTED`] + 1.
    pub snapshots: Vec<(u64, StateSnapshot)>,
    /// The last run started from this machine, if one is kept beside the store.
    pub last_run: Option<LastRun>,
    /// Their run ids longer than people read, computed once (see `shorten`).
    long_run_ids: OnceLock<Vec<String>>,
}

impl History {
    /// What the store holds: `snapshots`, newest first.
    pub fn new(snapshots: Vec<(u64, StateSnapshot)>) -> Self {
        Self {
            snapshots,
            last_run: None,
            long_run_ids: OnceLock::new(),
        }
    }

    /// Adds the last run.
    #[must_use]
    pub fn with_last_run(mut self, last_run: Option<LastRun>) -> Self {
        self.last_run = last_run;
        self
    }
}

/// The last run started against the store, as kept beside it for retrying. Only the
/// latest is kept, and it names no snapshot: it is tied to one by time.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LastRun {
    /// The command as typed, e.g. `ods state build --select +orders`.
    pub command: String,
    /// The command without its options, safe to show beyond loopback (options may
    /// name local paths), e.g. `ods state build`.
    pub command_name: String,
    /// When it started.
    pub started_at: Timestamp,
    /// How it ended; `None` if it stopped before the build finished, or the file is
    /// from an ODS that didn't keep it.
    pub outcome: Option<LastOutcome>,
    /// The command that builds only what failed, if this ODS has one.
    pub retry_failed: Option<String>,
    /// The command that runs it again, planned afresh, if this ODS has one.
    pub retry: Option<String>,
    /// Where it was read from.
    pub file: StoreLocation,
    /// The state scope it ran for, e.g. `shop/dev`; `None` in an older file.
    pub scope: Option<String>,
    /// The run's id, as the snapshot it commits records it; `None` if unknown.
    pub run_id: Option<String>,
}

impl LastRun {
    /// A last run.
    pub fn new(
        command: impl Into<String>,
        command_name: impl Into<String>,
        started_at: Timestamp,
        file: impl Into<StoreLocation>,
    ) -> Self {
        Self {
            command: command.into(),
            command_name: command_name.into(),
            started_at,
            outcome: None,
            retry_failed: None,
            retry: None,
            file: file.into(),
            scope: None,
            run_id: None,
        }
    }

    /// Sets how it ended.
    #[must_use]
    pub fn with_outcome(mut self, outcome: Option<LastOutcome>) -> Self {
        self.outcome = outcome;
        self
    }

    /// Sets the scope it ran for and its run id.
    #[must_use]
    pub fn with_run(mut self, scope: Option<String>, run_id: Option<String>) -> Self {
        self.scope = scope;
        self.run_id = run_id;
        self
    }

    /// Sets the retry commands: all of it, and only what failed.
    #[must_use]
    pub fn with_retry(mut self, retry: Option<String>, retry_failed: Option<String>) -> Self {
        self.retry = retry;
        self.retry_failed = retry_failed;
        self
    }
}

/// What failed in the last run: node and source ids, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LastOutcome {
    /// Nodes that failed, or whose tests failed.
    pub failed: Vec<String>,
    /// Nodes skipped because of a failure.
    pub skipped: Vec<String>,
    /// Sources whose tests failed.
    pub failed_source_tests: Vec<String>,
}

impl LastOutcome {
    /// An outcome.
    pub fn new(
        failed: Vec<String>,
        skipped: Vec<String>,
        failed_source_tests: Vec<String>,
    ) -> Self {
        Self {
            failed,
            skipped,
            failed_source_tests,
        }
    }

    /// Failures counted in a run's Failed column: nodes, and sources whose tests failed.
    fn failures(&self) -> usize {
        self.failed.len() + self.failed_source_tests.len()
    }

    fn is_clean(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty() && self.failed_source_tests.is_empty()
    }
}

// -------------------------------------------------------------------- view models

/// A node, named for people, with where to read more about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeRef {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
}

/// The plan page (`/state/plan`, `/api/state/plan`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PlanView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The state scope, e.g. `shop/dev`.
    pub scope: String,
    /// The environment the plan is for, e.g. `dev`.
    pub environment: String,
    /// The target it is for, as the shell names it: the target, else the environment.
    pub target: String,
    /// Where the state is.
    pub state: StateStatus,
    /// What to do when there is no plan to show.
    pub empty: Option<EmptyState>,
    /// The snapshot the plan compares with.
    pub based_on: Option<u64>,
    /// When it was made: now, as every request plans again.
    pub created_at: Option<Timestamp>,
    /// Always true: the plan is made offline and builds nothing.
    pub dry_run: bool,
    /// How many nodes build, are reused, and whose relation was checked.
    pub counts: PlanCounts,
    /// The rows shown: `all`, `build` or `reuse`.
    pub filter: &'static str,
    /// The decision table, in plan order, filtered.
    pub rows: Vec<PlanRow>,
    /// Commands to copy.
    pub commands: Vec<CommandHint>,
    /// Why the plan couldn't be made, if it couldn't.
    pub error: Option<String>,
    /// What qualifies it.
    pub warnings: Vec<String>,
    /// The Why panel of the selected node.
    pub selected: Option<WhyView>,
}

/// The plan's counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PlanCounts {
    /// Every planned node.
    pub total: usize,
    /// To build.
    pub build: usize,
    /// To reuse.
    pub reuse: usize,
    /// Reused nodes whose relation was found in the warehouse.
    pub relations_checked: usize,
}

/// One row of the decision table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PlanRow {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// `model`, `seed`, `snapshot`, …
    pub kind: String,
    /// Build or reuse.
    pub action: PlanAction,
    /// The decisive reason's code.
    pub code: Option<ReasonCode>,
    /// The code for people, e.g. `code changed`.
    pub reason: String,
    /// The decisive reason, for people.
    pub why: String,
    /// Whether it builds because evidence is missing, not because something changed.
    pub unknown_evidence: bool,
    /// Whether it is the node the Why panel shows.
    pub selected: bool,
}

/// The Why panel for one node (`/api/state/plan/<node>`): the same explanation as
/// `ods state explain <node>`, and the evidence behind it, grouped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct WhyView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// `model`, `seed`, …
    pub kind: String,
    /// Build or reuse.
    pub action: PlanAction,
    /// The answer in a sentence, as `ods state explain` gives it.
    pub verdict: String,
    /// The snapshot the plan compares with.
    pub based_on: Option<u64>,
    /// Its recorded build in that snapshot, if any.
    pub last_build: Option<LastBuild>,
    /// Its fingerprint, compared with its recorded build.
    pub fingerprint: FingerprintView,
    /// Whether its relation is still in the warehouse.
    pub relation: RelationView,
    /// The versions of the sources it reads, and where each came from (ADR-0022).
    pub sources: Vec<SourceVersionView>,
    /// The decisions of the planned parents it reads.
    pub parents: Vec<ParentView>,
    /// Planned nodes that read it and build too.
    pub readers: Vec<NodeRef>,
    /// Every piece of evidence the decision rests on, as the planner gave it.
    pub evidence: Vec<EvidenceView>,
    /// Why, most important first, with run ids shortened for people.
    pub reasons: Vec<ReasonView>,
    /// The reason chain for people: `explanation`, one line per node, depth first.
    pub chain: Vec<ChainLine>,
    /// The reason chain, traced upstream: exactly `ods state explain`'s.
    pub explanation: Explanation,
    /// Commands to copy.
    pub commands: Vec<CommandHint>,
    /// What qualifies the plan.
    pub warnings: Vec<String>,
}

/// One reason for a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ReasonView {
    /// The planner's stable code.
    pub code: ReasonCode,
    /// The code for people.
    pub label: String,
    /// The reason, for people.
    pub message: String,
}

/// A node in the reason chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ChainLine {
    /// How far upstream of the node asked about: 0 is the node itself.
    pub depth: usize,
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// Build or reuse.
    pub action: PlanAction,
    /// Already explained above, through another path.
    pub repeated: bool,
    /// Its reasons, for people.
    pub reasons: Vec<String>,
    /// Fingerprint components that changed.
    pub changed: Vec<String>,
}

/// A node's recorded build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LastBuild {
    /// The snapshot the plan compares with, which still records it.
    pub snapshot: u64,
    /// The snapshot its run committed, if listed: where it was first recorded.
    pub built_in: Option<u64>,
    /// The run that built it.
    pub run_id: String,
    /// Its first eight characters.
    pub short_run_id: String,
    /// When.
    pub built_at: Timestamp,
    /// The run whose tests it passed, if any.
    pub tested_in: Option<String>,
}

/// A fingerprint compared with the recorded one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FingerprintView {
    /// Whether the planner compared it: only with a recorded build and a complete
    /// fingerprint.
    pub compared: bool,
    /// The recorded digest, shortened.
    pub before: Option<String>,
    /// The digest now, shortened.
    pub after: Option<String>,
    /// Each component and whether it changed; empty when not compared.
    pub components: Vec<ComponentView>,
    /// What this says, for people.
    pub summary: String,
}

/// A fingerprint component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ComponentView {
    /// Its name, e.g. `sql`.
    pub name: String,
    /// Whether it differs from the recorded build.
    pub changed: bool,
}

/// What is known about a node's relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RelationView {
    /// `present`, `missing`, `not_checked`, `unverified` or `not_needed` (it builds).
    pub status: &'static str,
    /// What was found, e.g. the relation's kind.
    pub value: Option<String>,
    /// How exact it is, or `not_applicable` when it builds.
    pub grade: &'static str,
    /// For people.
    pub note: String,
}

/// A source's data version, as the plan read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SourceVersionView {
    /// Its id.
    pub source: String,
    /// Its name.
    pub name: String,
    /// The version now, if one is known.
    pub version: Option<String>,
    /// How exact the version is: `exact`, `semantic`, `proxy`, `inferred` or `unknown`.
    pub grade: &'static str,
    /// Whether the version is good enough to reuse on (semantic or exact). When not, it
    /// is inferred or unknown, and never shown as fact.
    pub usable: bool,
    /// The strategy chosen to read it, e.g. `table_version` or `freshness`.
    pub strategy: Option<String>,
    /// Where the version itself came from, e.g. a table's history.
    pub origin: Option<String>,
    /// Strategies passed over, each with why.
    pub skipped: Vec<String>,
}

/// A planned parent's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ParentView {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// Its decision and reason, e.g. `build: code_changed`.
    pub decision: String,
    /// Whether it builds.
    pub builds: bool,
}

/// One piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EvidenceView {
    /// What kind of fact, e.g. `fingerprint`.
    pub kind: String,
    /// What it is about.
    pub subject: String,
    /// Its name.
    pub subject_name: String,
    /// Its value, if any.
    pub value: Option<String>,
    /// How exact it is.
    pub exactness: Exactness,
    /// The same, as a grade for people.
    pub grade: &'static str,
    /// Whether it may be shown as fact (semantic or exact); otherwise it is inferred
    /// or unknown.
    pub fact: bool,
}

/// How a run ended, as far as ODS knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Its successful builds were recorded; whether others failed isn't stored.
    Recorded,
    /// Nothing failed, as the last run's outcome says.
    Succeeded,
    /// Some nodes failed or were skipped, as the last run's outcome says.
    Failed,
}

/// A recorded run, as the Runs page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunRow {
    /// The snapshot it committed.
    pub snapshot: u64,
    /// The snapshot it replaced.
    pub replaces: Option<u64>,
    /// The run's id.
    pub run_id: String,
    /// Its first eight characters.
    pub short_run_id: String,
    /// When it was committed.
    pub recorded_at: Timestamp,
    /// The command, if known: only for the last run, tied to it by time.
    pub command: Option<String>,
    /// The target it built in, if recorded.
    pub target: Option<Target>,
    /// Nodes it built.
    pub built: usize,
    /// Recorded nodes that kept an earlier build: reused, not selected, or failed.
    pub kept: usize,
    /// Nodes that failed; `None` unless recorded (the last run only).
    pub failed: Option<usize>,
    /// Nodes skipped because of a failure; `None` unless recorded.
    pub skipped: Option<usize>,
    /// How it ended, as far as ODS knows.
    pub outcome: RunOutcome,
    /// Whether the command and outcome come from the last run, whose run id this
    /// snapshot records: kept beside the store, not in the snapshot.
    pub from_last_run: bool,
    /// How long it took; not recorded yet.
    pub duration: Option<String>,
    /// Who ran it; not recorded yet.
    pub triggered_by: Option<String>,
}

/// The last run, and what it did to the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LastRunView {
    /// The command; its options only on loopback.
    pub command: String,
    /// The command without its options, e.g. `ods state build`.
    pub command_name: String,
    /// When it started.
    pub started_at: Timestamp,
    /// Whether its outcome is known.
    pub outcome_known: bool,
    /// Nodes that failed.
    pub failed: Vec<NodeRef>,
    /// Nodes skipped because of a failure.
    pub skipped: Vec<NodeRef>,
    /// Sources whose tests failed.
    pub failed_source_tests: Vec<NodeRef>,
    /// The scope it ran for, if the file says (since ODS kept it, #311).
    pub scope: Option<String>,
    /// The snapshot it recorded: the one that records its run id. `None` when none
    /// does, or it can't be told.
    pub snapshot: Option<u64>,
    /// Whether it probably recorded nothing. Inferred: no listed snapshot records its
    /// run id or, without one, none was recorded since it started. A clock step or a
    /// later `ods state record` could make this wrong, so it is never shown as fact.
    pub recorded_nothing_inferred: bool,
    /// The last good state: the latest snapshot. A failed run never replaces it.
    pub last_good: Option<u64>,
    /// What to run next, if anything failed.
    pub next: Vec<CommandHint>,
    /// Where it was read from; only on loopback.
    pub file: Option<StoreLocation>,
}

impl LastRunView {
    /// Whether anything failed: nodes, skipped nodes, or sources' tests.
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty() || !self.skipped.is_empty() || !self.failed_source_tests.is_empty()
    }

    /// What its Failed column counts: failed nodes and sources whose tests failed.
    pub fn failures(&self) -> usize {
        self.failed.len() + self.failed_source_tests.len()
    }
}

/// A filter's choice, with how many runs it keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FacetOption {
    /// The query value, e.g. `failed`.
    pub value: String,
    /// For people.
    pub label: String,
    /// How many listed runs it keeps.
    pub count: usize,
    /// Whether it is applied.
    pub selected: bool,
}

/// A filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Facet {
    /// Its query key, e.g. `outcome`.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// Its choices; the first is "all".
    pub options: Vec<FacetOption>,
    /// Why it can't filter, if it can't (e.g. the data isn't recorded).
    pub unavailable: Option<&'static str>,
}

/// Which runs to show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunFilter {
    /// `recorded`, `succeeded` or `failed`.
    pub outcome: Option<String>,
    /// A target name.
    pub target: Option<String>,
    /// `1d`, `7d` or `30d`.
    pub date: Option<String>,
    /// The run shown in the side panel.
    pub run: Option<String>,
}

/// The Runs page (`/state/runs`, `/api/state/runs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunsView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The project.
    pub project: String,
    /// The state scope.
    pub scope: String,
    /// Where the state is.
    pub state: StateStatus,
    /// The store; only on loopback.
    pub store: Option<StoreLocation>,
    /// What to do when there are no runs.
    pub empty: Option<EmptyState>,
    /// How many snapshots the scope has (at least, when `total_capped`).
    pub total: usize,
    /// Whether `total` is a lower bound.
    pub total_capped: bool,
    /// How many are listed at most.
    pub limit: usize,
    /// The filters, with counts.
    pub facets: Vec<Facet>,
    /// The runs shown, newest first.
    pub runs: Vec<RunRow>,
    /// How many runs are shown, the last run's own row included.
    pub listed: usize,
    /// How many runs there are to show without filters, the last run's row included.
    pub unfiltered: usize,
    /// Whether any filter is applied.
    pub filtered: bool,
    /// How many of the shown runs recorded a snapshot.
    pub recorded_snapshots: usize,
    /// How many of the shown runs are known to have failed.
    pub failed: usize,
    /// The last run started against this store, if kept and for this scope.
    pub last_run: Option<LastRunView>,
    /// Whether the last run is listed as a row of its own, before the recorded runs:
    /// it recorded no snapshot, its outcome is known, and the filters keep it.
    pub last_run_listed: bool,
    /// The last run, when it doesn't say which scope it ran for: shown apart, as it
    /// may belong to another target.
    pub unscoped_last_run: Option<LastRunView>,
    /// The run in the side panel: the one asked for, else the newest shown.
    pub selected: Option<String>,
    /// What the store does and doesn't record.
    pub recorded: Vec<String>,
    /// The CI runs tab: planned.
    pub ci: &'static str,
}

/// A node in a run's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TimelineRow {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// `model`, `seed`, … when planned now; `None` otherwise.
    pub kind: Option<String>,
    /// Whether this run built it; otherwise it kept an earlier build.
    pub built: bool,
    /// Among the built nodes, how many built parents it waited on in a row.
    pub lane: usize,
    /// For a kept node, the run whose build it kept.
    pub kept_from: Option<String>,
    /// How long it took; not recorded yet.
    pub duration: Option<String>,
}

/// Why a node was built, from what the snapshots record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct BuiltWhy {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// What differs from its previous recorded build.
    pub changes: Vec<Change>,
    /// The same, for people.
    pub why: String,
}

/// One run (`/state/runs/<run_id>`, `/api/state/runs/<run_id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunPageView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The state scope.
    pub scope: String,
    /// The run.
    pub run: RunRow,
    /// The run whose snapshot it replaced.
    pub compared_with: Option<RunRef>,
    /// When it started; not recorded yet.
    pub started_at: Option<Timestamp>,
    /// Every node its snapshot records: built ones last, in the order they waited on
    /// each other.
    pub timeline: Vec<TimelineRow>,
    /// Why each built node was built.
    pub built: Vec<BuiltWhy>,
    /// What failed, if this is the last run and its outcome is known.
    pub last_run: Option<LastRunView>,
    /// What the state rule meant for this run.
    pub state_rule: String,
    /// The runs before it, newest first.
    pub earlier: Vec<RunRow>,
}

/// A run, by snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunRef {
    /// The snapshot.
    pub snapshot: u64,
    /// The run's id.
    pub run_id: String,
    /// Its first eight characters.
    pub short_run_id: String,
}

// ----------------------------------------------------------------------- building

/// Node ids to names, from wherever they are known.
pub type Names = BTreeMap<String, String>;

fn name_in(names: &Names, id: &str) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_owned())
}

fn node_ref(names: &Names, id: &str) -> NodeRef {
    NodeRef {
        node: id.to_owned(),
        name: name_in(names, id),
    }
}

/// Exactness as a grade people read, as the Freshness evidence screen names them.
pub(crate) fn grade(exactness: Exactness) -> &'static str {
    match exactness {
        Exactness::Exact => "exact",
        Exactness::Semantic => "semantic",
        Exactness::Proxy => "proxy",
        Exactness::Inferred => "inferred",
        _ => "unknown",
    }
}

/// Digests are long; people only need to tell them apart (as `ods state explain`).
fn short_digest(value: &str) -> String {
    if value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{}…", &value[..12])
    } else {
        value.to_owned()
    }
}

/// `2026-09-29T00:04:10Z` → (`2026-09-29`, `00:04:10Z`).
pub(crate) fn date_and_time(at: Timestamp) -> (String, String) {
    let text = at.to_string();
    match text.split_once('T') {
        Some((date, time)) => (date.to_owned(), time.to_owned()),
        None => (text, String::new()),
    }
}

/// Codes that mean evidence is missing, rather than that something changed.
fn unknown_evidence(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::MissingDataEvidence
            | ReasonCode::CodeEvidenceIncomplete
            | ReasonCode::UnknownDependency
            | ReasonCode::RelationUnverified
    )
}

impl Dashboard {
    /// The snapshots and last run the State pages list; also what the Lineage page
    /// compares fingerprints with (#312).
    pub(crate) fn history(&self) -> Option<&super::state::History> {
        self.recorded().and_then(|r| r.history.as_deref())
    }

    /// Snapshots, newest first; from the history if there is one, else the ones Home
    /// was given don't carry nodes, so none.
    fn snapshots(&self) -> &[(u64, StateSnapshot)] {
        self.history().map_or(&[], |h| h.snapshots.as_slice())
    }

    /// The plan page as of `now`: `filter` is `build`, `reuse` or anything else for
    /// all; `node` selects the Why panel. `names` names nodes the plan doesn't.
    pub fn plan_view(
        &self,
        details: bool,
        now: Timestamp,
        filter: Option<&str>,
        node: Option<&str>,
        names: &Names,
    ) -> PlanView {
        let (state, empty) = self.state_status(details);
        let filter = match filter {
            Some("build") => "build",
            Some("reuse") => "reuse",
            _ => "all",
        };
        let mut view = PlanView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            scope: self.scope.clone(),
            environment: self.environment.clone(),
            target: self.shell("state").target.name,
            state,
            empty,
            based_on: None,
            created_at: None,
            dry_run: true,
            counts: PlanCounts {
                total: 0,
                build: 0,
                reuse: 0,
                relations_checked: 0,
            },
            filter,
            rows: Vec::new(),
            commands: Vec::new(),
            error: None,
            warnings: Vec::new(),
            selected: None,
        };
        let Some((plan, warnings)) = self.plan_at(now) else {
            return view;
        };
        // With a store but no runs yet, the plan builds everything: still shown.
        view.empty = None;
        view.warnings = warnings;
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                view.error = Some(if details {
                    error
                } else {
                    "the plan couldn't be made; see the server log".to_owned()
                });
                return view;
            }
        };
        let names = self.all_names(&plan, names);
        view.based_on = plan.based_on.map(|s| s.0);
        view.created_at = Some(plan.created_at);
        view.counts = plan_counts(&plan);
        // Without a node asked for, the first build (else the first node), as the
        // design opens on one: the panel is never empty while something is planned.
        let selected = match node {
            Some(n) => resolve(&plan, n),
            None => plan
                .with_action(PlanAction::Build)
                .next()
                .or_else(|| plan.entries.first())
                .map(|e| e.node.clone()),
        };
        view.rows = self.plan_rows(&plan, filter, selected.as_deref());
        view.commands = vec![
            hint(
                "ods state build",
                "plans again, then builds only what's marked Build, and records the run",
            ),
            hint(
                "ods state build --dry-run",
                "plans as a build would, checking that reused relations still exist",
            ),
        ];
        view.selected = selected.and_then(|id| self.why(&plan, &view.warnings, &id, &names));
        view
    }

    /// The Why panel of `node` (an id or a name only one node has) as of `now`, or
    /// `None` if it isn't planned (or there is no plan).
    pub fn why_view(&self, now: Timestamp, node: &str, names: &Names) -> Option<WhyView> {
        let (plan, warnings) = self.plan_at(now)?;
        let plan = plan.ok()?;
        let id = resolve(&plan, node)?;
        let names = self.all_names(&plan, names);
        self.why(&plan, &warnings, &id, &names)
    }

    fn all_names(&self, plan: &ExecutionPlan, names: &Names) -> Names {
        let mut all = names.clone();
        all.extend(self.names());
        all.extend(
            plan.entries
                .iter()
                .map(|e| (e.node.clone(), e.name.clone())),
        );
        all
    }

    /// Long run ids read better short, as in the runs table. The ids are gathered once
    /// per reload.
    fn shorten(&self, message: &str) -> String {
        let Some(history) = self.history() else {
            return message.to_owned();
        };
        let ids = history.long_run_ids.get_or_init(|| {
            let ids: BTreeSet<&str> = history
                .snapshots
                .iter()
                .map(|(_, s)| s.run_id.as_str())
                .filter(|id| id.len() > 8)
                .collect();
            ids.into_iter().map(str::to_owned).collect()
        });
        ids.iter()
            .filter(|id| message.contains(id.as_str()))
            .fold(message.to_owned(), |m, id| {
                m.replace(id.as_str(), &short(id))
            })
    }

    fn why(
        &self,
        plan: &ExecutionPlan,
        warnings: &[String],
        node: &str,
        names: &Names,
    ) -> Option<WhyView> {
        let explanation = ods_state::explain(plan, node)?;
        let entry = &explanation.entry;
        let builds = entry.action == PlanAction::Build;
        // As `ods state explain` words it.
        let verdict = if builds {
            format!("{} would be built", entry.name)
        } else {
            format!("{} would be reused", entry.name)
        };
        let recorded = plan.based_on.and_then(|id| {
            self.snapshots()
                .iter()
                .find(|(s, _)| *s == id.0)
                .and_then(|(s, snap)| snap.nodes.get(node).map(|n| (*s, n)))
        });
        let last_build = recorded.map(|(snapshot, n)| LastBuild {
            snapshot,
            built_in: self
                .snapshots()
                .iter()
                .find(|(_, s)| s.run_id == n.run_id)
                .map(|(id, _)| *id),
            run_id: n.run_id.clone(),
            short_run_id: short(&n.run_id),
            built_at: n.built_at,
            tested_in: n.tested.as_ref().map(|t| t.run_id.clone()),
        });
        let evidence: Vec<EvidenceView> = entry
            .evidence
            .iter()
            .map(|e| evidence_view(e, names))
            .collect();
        // Readers rebuild with it only if it builds.
        let readers = if builds {
            plan.entries
                .iter()
                .filter(|e| e.action == PlanAction::Build && e.depends_on.iter().any(|p| p == node))
                .map(|e| NodeRef {
                    node: e.node.clone(),
                    name: e.name.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(WhyView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            node: entry.node.clone(),
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            action: entry.action,
            verdict,
            based_on: plan.based_on.map(|s| s.0),
            last_build,
            fingerprint: fingerprint_view(entry, recorded.map(|(_, n)| n)),
            relation: relation_view(entry),
            sources: source_versions(&entry.evidence, names),
            parents: entry
                .evidence
                .iter()
                .filter(|e| e.kind == "parent_decision")
                .map(|e| {
                    let decision = e.value.clone().unwrap_or_default();
                    ParentView {
                        node: e.subject.clone(),
                        name: name_in(names, &e.subject),
                        builds: decision.starts_with("build"),
                        decision,
                    }
                })
                .collect(),
            readers,
            evidence,
            reasons: entry
                .reasons
                .iter()
                .map(|r| ReasonView {
                    code: r.code,
                    label: reason_label(r.code),
                    message: self.shorten(&r.message),
                })
                .collect(),
            chain: {
                let mut lines = Vec::new();
                self.chain(&explanation, 0, &mut lines);
                lines
            },
            commands: vec![hint(
                &format!("ods state explain {}", entry.name),
                "the same explanation in a terminal (--output json for this panel's data)",
            )],
            explanation,
            warnings: warnings.to_vec(),
        })
    }
}

impl Dashboard {
    fn chain(&self, e: &Explanation, depth: usize, lines: &mut Vec<ChainLine>) {
        lines.push(ChainLine {
            depth,
            node: e.entry.node.clone(),
            name: e.entry.name.clone(),
            action: e.entry.action,
            repeated: e.repeated,
            reasons: if e.repeated {
                Vec::new()
            } else {
                e.entry
                    .reasons
                    .iter()
                    .map(|r| self.shorten(&r.message))
                    .collect()
            },
            changed: if e.repeated {
                Vec::new()
            } else {
                e.entry.changed_components.clone()
            },
        });
        for cause in &e.causes {
            self.chain(cause, depth + 1, lines);
        }
    }
}

impl Dashboard {
    /// The decision table: `filter` is `all`, `build` or `reuse`.
    fn plan_rows(
        &self,
        plan: &ExecutionPlan,
        filter: &str,
        selected: Option<&str>,
    ) -> Vec<PlanRow> {
        let mut rows = plan
            .entries
            .iter()
            .filter(|e| match filter {
                "build" => e.action == PlanAction::Build,
                "reuse" => e.action == PlanAction::Reuse,
                _ => true,
            })
            .map(|e| {
                let code = e.reasons.first().map(|r| r.code);
                PlanRow {
                    node: e.node.clone(),
                    name: e.name.clone(),
                    kind: e.kind.clone(),
                    action: e.action,
                    code,
                    reason: code.map_or_else(String::new, reason_label),
                    why: self.shorten(
                        &e.reasons
                            .iter()
                            .map(|r| r.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                    unknown_evidence: code.is_some_and(unknown_evidence),
                    selected: selected == Some(e.node.as_str()),
                }
            })
            .collect::<Vec<_>>();
        // Builds first, as they are the news; plan order (by depth) within each. The
        // sort is stable.
        rows.sort_by_key(|r| r.action != PlanAction::Build);
        rows
    }
}

/// How many nodes build and are reused, and how many reused ones had their relation
/// found (only a build's dry run checks; an offline plan checks none).
fn plan_counts(plan: &ExecutionPlan) -> PlanCounts {
    let reused: Vec<&PlanEntry> = plan.with_action(PlanAction::Reuse).collect();
    PlanCounts {
        total: plan.entries.len(),
        build: plan.entries.len() - reused.len(),
        reuse: reused.len(),
        relations_checked: reused
            .iter()
            .filter(|e| {
                e.evidence.iter().any(|ev| {
                    ev.kind == "relation_exists" && ev.subject == e.node && ev.value.is_some()
                })
            })
            .count(),
    }
}

/// The planned node `spec` names: its id, or a name only one node has.
fn resolve(plan: &ExecutionPlan, spec: &str) -> Option<String> {
    if plan.entries.iter().any(|e| e.node == spec) {
        return Some(spec.to_owned());
    }
    let mut named = plan.entries.iter().filter(|e| e.name == spec);
    match (named.next(), named.next()) {
        (Some(one), None) => Some(one.node.clone()),
        _ => None,
    }
}

fn evidence_view(e: &Evidence, names: &Names) -> EvidenceView {
    EvidenceView {
        kind: e.kind.clone(),
        subject: e.subject.clone(),
        subject_name: name_in(names, &e.subject),
        value: e.value.clone(),
        exactness: e.exactness,
        grade: grade(e.exactness),
        fact: e.exactness.allows_reuse(),
    }
}

fn fingerprint_view(entry: &PlanEntry, recorded: Option<&NodeState>) -> FingerprintView {
    // Compared only when the planner looked at the node's own fingerprint against a
    // recorded one; otherwise nothing is said about its parts.
    let compared = entry.before.is_some()
        && entry
            .evidence
            .iter()
            .any(|e| e.kind == "fingerprint" && e.subject == entry.node);
    let mut components: Vec<ComponentView> = Vec::new();
    if compared {
        let mut names: BTreeSet<String> = recorded
            .map(|n| n.fingerprint.components.keys().cloned().collect())
            .unwrap_or_default();
        names.extend(entry.changed_components.iter().cloned());
        components = names
            .into_iter()
            .map(|name| ComponentView {
                changed: entry.changed_components.contains(&name),
                name,
            })
            .collect();
        // Changed parts first, as they are the news.
        components.sort_by_key(|c| !c.changed);
    }
    let changed = entry.changed_components.len();
    let summary = if !compared {
        match entry.reasons.first().map(|r| r.code) {
            Some(ReasonCode::NeverBuilt) => "no recorded build to compare with".to_owned(),
            Some(ReasonCode::TargetChanged) => {
                "not compared: the recorded state is from another target".to_owned()
            }
            Some(ReasonCode::CodeEvidenceIncomplete) => {
                "not compared: its code can't be fingerprinted completely".to_owned()
            }
            _ if entry.before.is_none() => "no recorded build to compare with".to_owned(),
            _ => "not compared".to_owned(),
        }
    } else if changed == 0 {
        "unchanged since its recorded build".to_owned()
    } else if changed == 1 {
        "differs in one part".to_owned()
    } else {
        format!("differs in {changed} parts")
    };
    FingerprintView {
        compared,
        before: entry.before.as_deref().map(short_digest),
        after: entry.after.as_deref().map(short_digest),
        components,
        summary,
    }
}

fn relation_view(entry: &PlanEntry) -> RelationView {
    let evidence = entry
        .evidence
        .iter()
        .find(|e| e.kind == "relation_exists" && e.subject == entry.node);
    let unverified = entry
        .reasons
        .iter()
        .find(|r| r.code == ReasonCode::RelationUnverified);
    match (evidence, unverified) {
        (_, Some(reason)) => RelationView {
            status: "unverified",
            value: None,
            grade: "unknown",
            note: reason.message.clone(),
        },
        (Some(e), None) => match e.value.as_deref() {
            Some("missing") => RelationView {
                status: "missing",
                value: e.value.clone(),
                grade: grade(e.exactness),
                note: if e.exactness.allows_reuse() {
                    "its table isn't in the warehouse: it builds".to_owned()
                } else {
                    format!(
                        "its table seems to be gone ({} evidence): it builds",
                        grade(e.exactness)
                    )
                },
            },
            Some(kind) => RelationView {
                status: "present",
                value: Some(kind.to_owned()),
                grade: grade(e.exactness),
                note: if e.exactness.allows_reuse() {
                    format!("found in the warehouse ({kind})")
                } else {
                    format!(
                        "reported in the warehouse ({kind}), on {} evidence only",
                        grade(e.exactness)
                    )
                },
            },
            None => RelationView {
                status: "not_checked",
                value: None,
                grade: grade(e.exactness),
                note: "not checked: this plan is made offline, so reuse assumes the relation built earlier still exists. `ods state build --dry-run` checks.".to_owned(),
            },
        },
        (None, None) if entry.action == PlanAction::Build => RelationView {
            status: "not_needed",
            value: None,
            grade: "not_applicable",
            note: "not needed: it builds".to_owned(),
        },
        (None, None) => RelationView {
            status: "not_checked",
            value: None,
            grade: "unknown",
            note: "not checked".to_owned(),
        },
    }
}

/// Source evidence, grouped by source (ADR-0022): the version read, the strategy that
/// read it, where it came from, and the strategies passed over.
fn source_versions(evidence: &[Evidence], names: &Names) -> Vec<SourceVersionView> {
    let mut by_source: BTreeMap<&str, SourceVersionView> = BTreeMap::new();
    for e in evidence {
        if !e.kind.starts_with("source_") {
            continue;
        }
        let view = by_source
            .entry(e.subject.as_str())
            .or_insert_with(|| SourceVersionView {
                source: e.subject.clone(),
                name: name_in(names, &e.subject),
                version: None,
                grade: "unknown",
                usable: false,
                strategy: None,
                origin: None,
                skipped: Vec::new(),
            });
        match e.kind.as_str() {
            "source_data_version" => {
                view.version.clone_from(&e.value);
                view.grade = grade(e.exactness);
                view.usable = e.value.is_some() && e.exactness.allows_reuse();
            }
            "source_version_strategy" => view.strategy.clone_from(&e.value),
            "source_version_origin" => view.origin.clone_from(&e.value),
            "source_version_skipped" => view.skipped.extend(e.value.clone()),
            _ => {}
        }
    }
    by_source.into_values().collect()
}

// --------------------------------------------------------------------------- runs

/// The run a snapshot records, as a row: built nodes are the ones whose last build is
/// the snapshot's run.
fn row(id: u64, snapshot: &StateSnapshot) -> RunRow {
    let built = snapshot
        .nodes
        .values()
        .filter(|n| n.run_id == snapshot.run_id)
        .count();
    RunRow {
        snapshot: id,
        replaces: snapshot.parent.map(|p| p.0),
        run_id: snapshot.run_id.clone(),
        short_run_id: short(&snapshot.run_id),
        recorded_at: snapshot.created_at,
        command: None,
        target: snapshot
            .target
            .as_ref()
            .map(|t| Target::new(t.name.clone(), t.kind.clone())),
        built,
        kept: snapshot.nodes.len() - built,
        failed: None,
        skipped: None,
        outcome: RunOutcome::Recorded,
        from_last_run: false,
        duration: None,
        triggered_by: None,
    }
}

/// How the last run relates to the listed snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tie {
    /// A snapshot records the run's id: it committed that snapshot.
    Snapshot(u64),
    /// No listed snapshot is its own: it recorded nothing, as far as can be told.
    NothingInferred,
    /// It can't be told (no run id, and snapshots were recorded since it started).
    Unknown,
}

impl Dashboard {
    /// Every listed run, newest first, with the last run's command and outcome on the
    /// snapshot that records its run id.
    fn run_rows(&self, details: bool) -> Vec<RunRow> {
        let last = self.last_run();
        let tie = self.last_run_tie();
        self.snapshots()
            .iter()
            .take(RUNS_LISTED)
            .map(|(id, snapshot)| {
                let mut row = row(*id, snapshot);
                if let Some(last) = last
                    && tie == Tie::Snapshot(*id)
                {
                    row.from_last_run = true;
                    row.command = Some(if details {
                        last.command.clone()
                    } else {
                        last.command_name.clone()
                    });
                    if let Some(outcome) = &last.outcome {
                        row.failed = Some(outcome.failures());
                        row.skipped = Some(outcome.skipped.len());
                        row.outcome = if outcome.is_clean() {
                            RunOutcome::Succeeded
                        } else {
                            RunOutcome::Failed
                        };
                    }
                }
                row
            })
            .collect()
    }

    /// The last run, only if it names this page's scope: the file is kept per state
    /// database, which several targets may share.
    fn last_run(&self) -> Option<&LastRun> {
        self.history()
            .and_then(|h| h.last_run.as_ref())
            .filter(|l| l.scope.as_deref() == Some(self.scope.as_str()))
    }

    /// The last run, when it doesn't say which scope it ran for (an older ODS, or it
    /// stopped before it built): it may belong to another target.
    fn unscoped_last_run(&self) -> Option<&LastRun> {
        self.history()
            .and_then(|h| h.last_run.as_ref())
            .filter(|l| l.scope.is_none())
    }

    /// Which snapshot, if any, the last run recorded.
    fn last_run_tie(&self) -> Tie {
        let Some(last) = self.last_run() else {
            return Tie::Unknown;
        };
        let snapshots = self.snapshots();
        if let Some(run_id) = &last.run_id {
            return snapshots
                .iter()
                .find(|(_, s)| &s.run_id == run_id)
                .map_or(Tie::NothingInferred, |(id, _)| Tie::Snapshot(*id));
        }
        // No run id: by time alone, and only when nothing was recorded since it
        // started (times are kept to the second, so the same second counts as since).
        if snapshots
            .iter()
            .any(|(_, s)| s.created_at >= last.started_at)
        {
            Tie::Unknown
        } else {
            Tie::NothingInferred
        }
    }

    fn last_run_view(&self, details: bool, names: &Names) -> Option<LastRunView> {
        let last = self.last_run()?;
        Some(self.describe_last_run(last, self.last_run_tie(), details, names))
    }

    fn describe_last_run(
        &self,
        last: &LastRun,
        tie: Tie,
        details: bool,
        names: &Names,
    ) -> LastRunView {
        let refs = |ids: &[String]| ids.iter().map(|id| node_ref(names, id)).collect::<Vec<_>>();
        let outcome = last.outcome.clone().unwrap_or_default();
        let mut next = Vec::new();
        // `retry --failed` retries failed and skipped nodes and failed source tests,
        // and only after a build: it refuses a test run, so it isn't suggested then.
        if !outcome.is_clean() {
            if let Some(command) = &last.retry_failed {
                next.push(hint(
                    command,
                    "builds only what failed or was skipped because of it, still planned; what succeeded isn't repeated",
                ));
            }
            if let Some(command) = &last.retry {
                next.push(hint(
                    command,
                    "plans again and also builds what changed since",
                ));
            }
        }
        LastRunView {
            command: if details {
                last.command.clone()
            } else {
                last.command_name.clone()
            },
            command_name: last.command_name.clone(),
            started_at: last.started_at,
            scope: last.scope.clone(),
            outcome_known: last.outcome.is_some(),
            failed: refs(&outcome.failed),
            skipped: refs(&outcome.skipped),
            failed_source_tests: refs(&outcome.failed_source_tests),
            snapshot: match tie {
                Tie::Snapshot(id) => Some(id),
                _ => None,
            },
            recorded_nothing_inferred: tie == Tie::NothingInferred,
            last_good: self.snapshots().first().map(|(id, _)| *id),
            next,
            file: details.then(|| last.file.clone()),
        }
    }

    /// The Runs page as of `now` (for the date filter).
    pub fn runs_view(
        &self,
        details: bool,
        now: Timestamp,
        filter: &RunFilter,
        names: &Names,
    ) -> RunsView {
        let (state, empty) = self.state_status(details);
        let all = self.run_rows(details);
        let mut facets = facets(&all, filter, now);
        let runs: Vec<RunRow> = all
            .iter()
            .filter(|r| keeps(filter, now, r, ""))
            .cloned()
            .collect();
        let last_run = self.last_run_view(details, names);
        // The last run, when it recorded nothing, is a run too: listed and counted.
        let unrecorded = last_run
            .as_ref()
            .filter(|l| l.recorded_nothing_inferred && l.outcome_known)
            .map(|l| {
                let outcome = if l.has_failures() {
                    "failed"
                } else {
                    "succeeded"
                };
                let date = filter
                    .date
                    .as_deref()
                    .and_then(days)
                    .is_none_or(|d| l.started_at.unix() >= now.unix() - d * 86_400);
                // It names no target, so a target filter leaves it out.
                (outcome, date && filter.target.is_none())
            });
        if let (Some((outcome, true)), Some(f)) =
            (unrecorded, facets.iter_mut().find(|f| f.key == "outcome"))
        {
            for option in &mut f.options {
                if option.value.is_empty() || option.value == outcome {
                    option.count += 1;
                }
            }
        }
        let last_run_listed = unrecorded.is_some_and(|(outcome, kept)| {
            kept && filter.outcome.as_deref().is_none_or(|o| o == outcome)
        });
        let last_failed = last_run_listed && unrecorded.is_some_and(|(o, _)| o == "failed");
        let selected = match filter.run.as_deref() {
            Some("last")
                if last_run
                    .as_ref()
                    .is_some_and(|l| l.recorded_nothing_inferred) =>
            {
                Some("last".to_owned())
            }
            Some(want) => find_row(&runs, want).map(|r| r.run_id.clone()),
            None => None,
        }
        .or_else(|| {
            // A failed run first, as the design has it; else the newest.
            if last_failed {
                return Some("last".to_owned());
            }
            runs.iter()
                .find(|r| r.outcome == RunOutcome::Failed)
                .or_else(|| runs.first())
                .map(|r| r.run_id.clone())
        });
        let (total, capped) = self
            .recorded()
            .map_or((0, false), |r| (r.snapshots, r.snapshots_capped));
        RunsView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            project: self.project.clone(),
            scope: self.scope.clone(),
            state,
            store: self.store(details),
            empty,
            total: total.max(all.len()),
            total_capped: capped,
            limit: RUNS_LISTED,
            facets,
            failed: runs
                .iter()
                .filter(|r| r.outcome == RunOutcome::Failed)
                .count()
                + usize::from(last_failed),
            listed: runs.len() + usize::from(last_run_listed),
            unfiltered: all.len() + usize::from(unrecorded.is_some()),
            filtered: filter.outcome.is_some() || filter.target.is_some() || filter.date.is_some(),
            recorded_snapshots: runs.len(),
            runs,
            last_run,
            last_run_listed,
            unscoped_last_run: self
                .unscoped_last_run()
                .map(|l| self.describe_last_run(l, Tie::Unknown, details, names)),
            selected,
            recorded: RECORDED.iter().map(|&line| line.to_owned()).collect(),
            ci: "CI runs — coming with server mode",
        }
    }

    /// The store's location, on loopback only.
    fn store(&self, details: bool) -> Option<StoreLocation> {
        details
            .then(|| match &self.state {
                StateInput::NoStore { store } | StateInput::Unreadable { store, .. } => {
                    store.clone()
                }
                StateInput::Recorded(r) => r.store.clone(),
                StateInput::ProjectUnreadable { .. } => StoreLocation::new("", ""),
            })
            .filter(|s| !s.full.is_empty())
    }

    /// One run, by its id or an unambiguous prefix of it (at least 8 characters).
    pub fn run_view(&self, details: bool, run: &str, names: &Names) -> Option<RunPageView> {
        let rows = self.run_rows(details);
        let this = find_row(&rows, run)?.clone();
        let snapshots = self.snapshots();
        let position = snapshots.iter().position(|(id, _)| *id == this.snapshot)?;
        let (_, snapshot) = &snapshots[position];
        let previous = snapshots
            .iter()
            .find(|(id, _)| Some(*id) == this.replaces)
            .map(|(_, s)| s);
        let mut names = names.clone();
        names.extend(self.names());
        // Kind and position in the plan (by depth), where the node is planned now.
        let planned: BTreeMap<String, (usize, String)> = match self.recorded().map(|r| &r.plan) {
            Some(Ok(plan)) => plan
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node.clone(), (i, e.kind.clone())))
                .collect(),
            _ => BTreeMap::new(),
        };
        let built: BTreeSet<&str> = snapshot
            .nodes
            .iter()
            .filter(|(_, n)| n.run_id == snapshot.run_id)
            .map(|(id, _)| id.as_str())
            .collect();
        let lanes = lanes(snapshot, &built);
        let mut timeline: Vec<TimelineRow> = snapshot
            .nodes
            .iter()
            .map(|(id, n)| {
                let is_built = built.contains(id.as_str());
                TimelineRow {
                    node: id.clone(),
                    name: name_in(&names, id),
                    kind: planned.get(id).map(|(_, kind)| kind.clone()),
                    built: is_built,
                    lane: lanes.get(id.as_str()).copied().unwrap_or(0),
                    kept_from: (!is_built).then(|| n.run_id.clone()),
                    duration: None,
                }
            })
            .collect();
        let at = |id: &str| planned.get(id).map_or(usize::MAX, |(i, _)| *i);
        timeline.sort_by(|a, b| {
            (a.built, a.lane, at(&a.node), &a.node).cmp(&(b.built, b.lane, at(&b.node), &b.node))
        });
        let built_why = self.built_why(&timeline, snapshot, previous, &names);
        let last_run = (self.last_run_tie() == Tie::Snapshot(this.snapshot))
            .then(|| self.last_run_view(details, &names))
            .flatten();
        let state_rule = state_rule(&this);
        Some(RunPageView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            scope: self.scope.clone(),
            compared_with: previous.map(|p| RunRef {
                snapshot: this.replaces.unwrap_or_default(),
                run_id: p.run_id.clone(),
                short_run_id: short(&p.run_id),
            }),
            started_at: None,
            timeline,
            built: built_why,
            last_run,
            state_rule,
            earlier: rows
                .iter()
                .filter(|r| r.snapshot < this.snapshot)
                .take(EARLIER_RUNS)
                .cloned()
                .collect(),
            run: this,
        })
    }
}

impl Dashboard {
    /// Why each built node of `snapshot` was built: what differs from its build in
    /// `previous`, as the snapshots record it.
    fn built_why(
        &self,
        timeline: &[TimelineRow],
        snapshot: &StateSnapshot,
        previous: Option<&StateSnapshot>,
        names: &Names,
    ) -> Vec<BuiltWhy> {
        let diff = previous.map(|p| ods_state::diff_states(p, snapshot));
        timeline
            .iter()
            .filter(|t| t.built)
            .map(|t| {
                let changes = diff
                    .as_ref()
                    .and_then(|d| d.changed.iter().find(|c| c.node == t.node))
                    .map(|c| c.changes.clone())
                    .unwrap_or_default();
                let first = previous.is_none_or(|p| !p.nodes.contains_key(&t.node));
                let why = if first {
                    "first recorded build".to_owned()
                } else if changes.is_empty() {
                    "nothing recorded changed: rebuilt for a reason snapshots don't keep (e.g. a full refresh or a missing relation)".to_owned()
                } else {
                    changes
                        .iter()
                        .map(|c| change_text(c, names))
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                BuiltWhy {
                    node: t.node.clone(),
                    name: t.name.clone(),
                    changes,
                    why: self.shorten(&why),
                }
            })
            .collect()
    }
}

/// Among the nodes `snapshot`'s run built: how many built parents each waited on in a
/// row, from the parents each build recorded.
fn lanes<'a>(snapshot: &'a StateSnapshot, built: &BTreeSet<&'a str>) -> BTreeMap<&'a str, usize> {
    let mut lanes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut pending: Vec<&str> = built.iter().copied().collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|id| {
            let parents: Vec<&str> = snapshot
                .nodes
                .get(*id)
                .into_iter()
                .flat_map(|n| n.parents.keys())
                .map(String::as_str)
                .filter(|p| built.contains(p) && p != id)
                .collect();
            let placed: Option<Vec<usize>> =
                parents.iter().map(|p| lanes.get(p).copied()).collect();
            match placed {
                Some(placed) => {
                    lanes.insert(id, placed.iter().map(|l| l + 1).max().unwrap_or(0));
                    false
                }
                None => true,
            }
        });
        if pending.len() == before {
            // A cycle can't be recorded; if one were, don't loop on it.
            for id in pending.drain(..) {
                lanes.insert(id, 0);
            }
        }
    }
    lanes
}

/// What the state rule (AGENTS rule 5) meant for `run`.
///
/// What is always true comes first; what the last run's record adds (which nodes failed,
/// or that none did) is said to come from it, and only for the run whose id it records.
fn state_rule(run: &RunRow) -> String {
    let base = match run.replaces {
        Some(prev) => format!(
            "Snapshot {} replaced snapshot {prev} with this run's successful builds; every other node keeps its last good build.",
            run.snapshot
        ),
        None => format!(
            "Snapshot {} is the first: it records this run's successful builds.",
            run.snapshot
        ),
    };
    match (run.outcome, run.from_last_run) {
        (RunOutcome::Failed, true) => format!(
            "{base} The last run's record says some nodes failed or weren't recorded: they keep their last good build."
        ),
        (RunOutcome::Succeeded, true) => {
            format!("{base} The last run's record says nothing failed.")
        }
        _ => format!("{base} Whether any node failed isn't stored for this run."),
    }
}

/// What the store does and doesn't record, for the Runs page.
const RECORDED: [&str; 4] = [
    "Each run that recorded something is a snapshot: which nodes it built, and the last good build of every other node.",
    "A snapshot doesn't record which of the other nodes were reused, left out or failed, so they read kept earlier build.",
    "Failures are only known for the last run, kept beside the store for `ods state retry`, and shown only for the target it ran for. It is tied to the snapshot that records its run id; that it recorded nothing is inferred.",
    "Durations, start times, commands of earlier runs and who ran them aren't recorded yet.",
];

/// Whether `filter` keeps `row`, ignoring the filter named `skip` (to count what each
/// of its choices would keep).
fn keeps(filter: &RunFilter, now: Timestamp, row: &RunRow, skip: &str) -> bool {
    let only = |key: &str, value: &Option<String>| {
        let mut one = RunFilter::default();
        match key {
            "outcome" => one.outcome.clone_from(value),
            "target" => one.target.clone_from(value),
            _ => one.date.clone_from(value),
        }
        one
    };
    [
        ("outcome", &filter.outcome),
        ("target", &filter.target),
        ("date", &filter.date),
    ]
    .into_iter()
    .all(|(key, value)| key == skip || matches_one(row, &only(key, value), now))
}

/// The filters, each choice with how many of `all` it keeps, with the other filters
/// applied.
fn facets(all: &[RunRow], filter: &RunFilter, now: Timestamp) -> Vec<Facet> {
    let facet = |key: &'static str, label: &'static str, choices: Vec<(String, String)>| {
        let applied = match key {
            "outcome" => filter.outcome.as_deref(),
            "target" => filter.target.as_deref(),
            _ => filter.date.as_deref(),
        };
        let pool: Vec<&RunRow> = all.iter().filter(|r| keeps(filter, now, r, key)).collect();
        let mut options = vec![FacetOption {
            value: String::new(),
            label: "All".to_owned(),
            count: pool.len(),
            selected: applied.is_none(),
        }];
        for (value, text) in choices {
            let mut one = RunFilter::default();
            match key {
                "outcome" => one.outcome = Some(value.clone()),
                "target" => one.target = Some(value.clone()),
                _ => one.date = Some(value.clone()),
            }
            options.push(FacetOption {
                count: pool.iter().filter(|r| matches_one(r, &one, now)).count(),
                selected: applied == Some(value.as_str()),
                value,
                label: text,
            });
        }
        Facet {
            key,
            label,
            options,
            unavailable: None,
        }
    };
    let targets: BTreeSet<String> = all
        .iter()
        .filter_map(|r| r.target.as_ref().map(|t| t.name.clone()))
        .collect();
    vec![
        facet(
            "outcome",
            "Outcome",
            vec![
                ("recorded".into(), "Recorded".into()),
                ("succeeded".into(), "Succeeded".into()),
                ("failed".into(), "Failed".into()),
            ],
        ),
        Facet {
            key: "command",
            label: "Command",
            options: vec![FacetOption {
                value: String::new(),
                label: "All".to_owned(),
                count: all.len(),
                selected: true,
            }],
            unavailable: Some("Snapshots don't record their command yet"),
        },
        facet(
            "target",
            "Target",
            targets.into_iter().map(|t| (t.clone(), t)).collect(),
        ),
        facet(
            "date",
            "Date",
            vec![
                ("1d".into(), "Last day".into()),
                ("7d".into(), "Last 7 days".into()),
                ("30d".into(), "Last 30 days".into()),
            ],
        ),
    ]
}

fn days(value: &str) -> Option<i64> {
    match value {
        "1d" => Some(1),
        "7d" => Some(7),
        "30d" => Some(30),
        _ => None,
    }
}

fn matches_one(row: &RunRow, filter: &RunFilter, now: Timestamp) -> bool {
    if let Some(o) = &filter.outcome {
        let outcome = match row.outcome {
            RunOutcome::Recorded => "recorded",
            RunOutcome::Succeeded => "succeeded",
            RunOutcome::Failed => "failed",
        };
        return outcome == o;
    }
    if let Some(t) = &filter.target {
        return row.target.as_ref().is_some_and(|x| &x.name == t);
    }
    if let Some(d) = filter.date.as_deref().and_then(days) {
        return row.recorded_at.unix() >= now.unix() - d * 86_400;
    }
    true
}

/// The row of run `id`, or of the only run whose id starts with it (8+ characters).
fn find_row<'a>(rows: &'a [RunRow], id: &str) -> Option<&'a RunRow> {
    if let Some(row) = rows.iter().find(|r| r.run_id == id) {
        return Some(row);
    }
    if id.len() < 8 {
        return None;
    }
    let mut matching = rows.iter().filter(|r| r.run_id.starts_with(id));
    match (matching.next(), matching.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// What differs, for people (as `ods state history <node>` words it).
fn change_text(change: &Change, names: &Names) -> String {
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
            name_in(names, source),
            before.as_deref().unwrap_or("unknown"),
            after.as_deref().unwrap_or("unknown")
        ),
        Change::Upstream { parent, .. } => {
            format!("reads {}, which was rebuilt", name_in(names, parent))
        }
        Change::Target { before, after } => format!(
            "target: {} → {}",
            before.as_deref().unwrap_or("not recorded"),
            after.as_deref().unwrap_or("not recorded")
        ),
        _ => "changed".to_owned(),
    }
}

/// `3 runs`.
pub(crate) fn count(n: usize, word: &str) -> String {
    plural(n, word)
}
