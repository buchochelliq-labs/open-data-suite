//! `ods state run` (#23, #24, ADR-0014): plan, build exactly the BUILD set through an
//! [`Executor`], and record what succeeded.
//!
//! 1. Prepare: compile, so fingerprints describe the code that would run, and measure
//!    sources (`--no-compile`, `--no-source-freshness` skip these).
//! 2. Plan against the latest state, as `ods state plan` does, after checking that
//!    the nodes it would reuse are still in the warehouse (#230): one query for all of
//!    them, and a node that isn't there, or can't be shown to be, is built.
//! 3. Execute the BUILD set, and nothing else, unless `--dry-run` or there is nothing
//!    to build.
//! 4. Record: re-read the artifacts the run wrote and commit a snapshot in which only
//!    the nodes that succeeded advance. Failed and skipped nodes keep their last
//!    successful state (AGENTS.md rule 5); if nothing succeeded, nothing is committed.
//!    The commit is a compare-and-swap on the state read in step 2, so a concurrent
//!    run can't be overwritten.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::{
    ExecutionPlan, PlanAction, ReasonCode, RunAction, RunEntry, RunEntryOutcome, RunNode,
    SnapshotId, TargetIdentity, Timestamp,
};
use ods_core::{Capability, Strategy, choose};
use ods_provider_dbt::RunResults;
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput, DbtStep};
use ods_sdk::ProviderError;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, PrepareReport,
    PrepareRequest, RequestedNode,
};
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence};
use ods_sdk::contracts::run_events::{
    CollectedEvents, RunEvent, RunEventKind, RunEventSink, RunSummary,
};
use ods_sdk::contracts::state_store::{StateScope, StateStore, StoredSnapshot};
use ods_state::{
    Outcome, Recorded, RecordedSources, RelationFact, RunResult, SourceCheck, SourceCheckAction,
    TestResult, VersionReading,
};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::run_journal::JournalSink;
use super::state_plan::{
    Sources, Workspace, block_on, common, display_name, plan_against, select_specs, store_error,
};
use super::state_retry::{LastOutcome, RetryFailed};
use super::state_settings::{Origin, Setting, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, ProgressSettings};
use crate::present::{Level, Line, Present, Span, Tone, ViewNode};

/// Options shared by the commands that run dbt (`run`, `test`).
pub(super) fn dbt_options(command: Command) -> Command {
    let command = command
        .arg(
            Arg::new("select")
                .long("select")
                .short('s')
                .value_name("SPEC")
                .action(ArgAction::Append)
                .help("Only consider these nodes: `name`, `+name` (with ancestors), `name+` (with descendants); repeatable"),
        )
        .arg(
            Arg::new("exclude")
                .long("exclude")
                .value_name("SPEC")
                .action(ArgAction::Append)
                .help("Leave these nodes out (same syntax as --select); they keep their last state; repeatable"),
        )
        .arg(
            Arg::new("no-compile")
                .long("no-compile")
                .action(ArgAction::SetTrue)
                .help("Use the artifacts already in the target directory instead of running `dbt compile` first. Sources aren't measured either: only --sources is read"),
        );
    dbt_invocation_options(command).arg(
        Arg::new("dbt-args")
            .value_name("DBT_ARGS")
            .num_args(0..)
            .last(true)
            .help("After `--`: options passed to dbt as they are, e.g. `-- --threads 8`. Selection and artifact options are refused"),
    )
}

/// How to invoke dbt: the program, its profile, vars and where its output goes. Every
/// command that calls dbt takes these.
pub(super) fn dbt_invocation_options(command: Command) -> Command {
    command
        .arg(
            Arg::new("dbt")
                .long("dbt")
                .value_name("PROGRAM")
                .default_value("dbt")
                .help("The dbt executable"),
        )
        .arg(
            Arg::new("profiles-dir")
                .long("profiles-dir")
                .value_name("DIR")
                .env("DBT_PROFILES_DIR")
                .hide_env_values(true)
                .help("dbt's --profiles-dir"),
        )
        .arg(
            Arg::new("dbt-profile")
                .long("dbt-profile")
                .value_name("NAME")
                .env("DBT_PROFILE")
                .hide_env_values(true)
                .help("dbt's --profile: the profile in profiles.yml to use instead of the project's (ODS's own --profile picks its configuration)"),
        )

        .arg(
            Arg::new("vars")
                .long("vars")
                .value_name("YAML")
                .help("dbt's --vars, e.g. '{region: eu}'; passed to every dbt command ODS runs, so the plan and the build see the same values"),
        )
        .arg(
            Arg::new("dbt-output")
                .long("dbt-output")
                .value_name("WHERE")
                .value_parser(["stderr", "capture"])
                .default_value("stderr")
                .help("Where dbt's own output goes: shown on stderr, or captured and shown only on failure"),
        )
}

/// The `ods state` commands that build (or only prepare), each named after the dbt
/// command it runs, so dbt users find what they expect (#229, ADR-0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `dbt source freshness` + `dbt compile`, then the plan. Builds nothing.
    Compile,
    /// `dbt run`: models.
    Run,
    /// `dbt seed`: seeds.
    Seed,
    /// `dbt snapshot`: snapshots.
    Snapshot,
    /// `dbt build`: models, seeds and snapshots, with their tests.
    Build,
}

impl Kind {
    /// Every kind, in `--help` order.
    pub(super) const ALL: [Self; 5] = [
        Self::Compile,
        Self::Run,
        Self::Seed,
        Self::Snapshot,
        Self::Build,
    ];

    /// The kind named `name`, if any.
    pub(super) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The subcommand, and the dbt command it matches.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Run => "run",
            Self::Seed => "seed",
            Self::Snapshot => "snapshot",
            Self::Build => "build",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Self::Compile => {
                "Measure sources and compile with dbt, then show what would be built and why; builds nothing"
            }
            Self::Run => {
                "Build the models that need building (`dbt run`), and record what succeeded"
            }
            Self::Seed => {
                "Load the seeds that need loading (`dbt seed`), and record what succeeded"
            }
            Self::Snapshot => {
                "Take the snapshots that need taking (`dbt snapshot`), and record what succeeded"
            }
            Self::Build => {
                "Build and test what needs building: models, seeds and snapshots (`dbt build`), and record what succeeded"
            }
        }
    }

    /// The resource types it builds; `None` for `compile`, which builds nothing.
    fn types(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Compile => None,
            Self::Run => Some(&["model"]),
            Self::Seed => Some(&["seed"]),
            Self::Snapshot => Some(&["snapshot"]),
            Self::Build => Some(&["model", "seed", "snapshot"]),
        }
    }

    /// The `ods state` command that builds nodes of `kind`, to suggest.
    fn command_for(kind: &str) -> &'static str {
        match kind {
            "seed" => "ods state seed",
            "snapshot" => "ods state snapshot",
            _ => "ods state run",
        }
    }
}

/// `--no-source-freshness`, for the commands that measure sources.
pub(super) fn no_source_freshness() -> Arg {
    Arg::new("no-source-freshness")
        .long("no-source-freshness")
        .action(ArgAction::SetTrue)
        .help("Don't run `dbt source freshness` first; use --sources or an existing sources.json")
}

/// An `ods state` command that builds, with its own arguments.
pub(super) fn build_command(kind: Kind) -> Command {
    let mut command = dbt_options(common(Command::new(kind.name()).about(kind.about())))
        .arg(no_source_freshness());
    if kind == Kind::Compile {
        return command;
    }
    command = command.arg(
        Arg::new("dry-run")
            .long("dry-run")
            .action(ArgAction::SetTrue)
            .help("Prepare and plan, but build and record nothing"),
    );
    // `dbt snapshot` has no --full-refresh: a snapshot's history is the point.
    if kind != Kind::Snapshot {
        command = command.arg(
            Arg::new("full-refresh")
                .long("full-refresh")
                .action(ArgAction::SetTrue)
                .env("DBT_FULL_REFRESH")
                .hide_env_values(true)
                .value_parser(clap::builder::BoolishValueParser::new())
                .help("Rebuild the selected incremental models and seeds from scratch, even if unchanged, as dbt's --full-refresh does"),
        );
    }
    if kind == Kind::Build {
        command = command
            .arg(
                Arg::new("resource-type")
                    .long("resource-type")
                    .value_name("TYPE")
                    .value_parser(["model", "seed", "snapshot"])
                    .action(ArgAction::Append)
                    .help("Only build nodes of this type; the others stay to build; repeatable"),
            )
            .arg(
                Arg::new("exclude-resource-type")
                    .long("exclude-resource-type")
                    .value_name("TYPE")
                    .value_parser(["model", "seed", "snapshot", "test"])
                    .action(ArgAction::Append)
                    .help("Leave this type out: `test` builds without tests (unit tests too); a node type stays to build; repeatable"),
            );
    }
    command
}

/// Nodes the plan would build that this run leaves out, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct LeftOut {
    pub(super) node: String,
    pub(super) reason: String,
}

/// Splits the plan's BUILD set into what this run builds and what `--exclude` and
/// `--resource-type` leave out.
pub(super) fn narrow(
    args: &ArgMatches,
    project: &ods_state::Project,
    entries: Vec<(String, String, String)>,
    types: &[String],
    command: &str,
) -> Result<(Vec<RequestedNode>, Vec<LeftOut>), CliError> {
    let exclude_specs: Vec<String> = args
        .get_many::<String>("exclude")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let excluded = if exclude_specs.is_empty() {
        std::collections::BTreeSet::new()
    } else {
        ods_state::select(project, &exclude_specs)
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::LINEAGE_TARGET, e))?
    };
    let mut requested = Vec::new();
    let mut left_out = Vec::new();
    for (id, name, kind) in entries {
        if excluded.contains(&id) {
            left_out.push(LeftOut {
                node: id,
                reason: "excluded with --exclude".to_owned(),
            });
        } else if !types.is_empty() && !types.contains(&kind) {
            left_out.push(LeftOut {
                node: id,
                reason: format!(
                    "a {kind}: `{command}` builds {} only; use `{}` or `ods state build`",
                    types
                        .iter()
                        .map(|t| format!("{t}s"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    Kind::command_for(&kind)
                ),
            });
        } else {
            requested.push(RequestedNode::new(id, name));
        }
    }
    Ok((requested, left_out))
}

/// A dbt setting in effect for a run, and where it came from (#227).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct DbtSetting {
    name: &'static str,
    value: String,
    /// `flag`, the `DBT_*` variable it was read from, the configuration that set it
    /// (e.g. `project config`), the setting it follows (e.g. `project_dir`), or
    /// `default`.
    source: String,
}

/// The dbt settings ODS passes to dbt, and where each came from: ODS's option, the
/// dbt variable it reads as that option's default, ODS's configuration (#214), or a
/// default.
pub(super) fn dbt_settings(args: &ArgMatches, settings: &StateSettings) -> Vec<DbtSetting> {
    let from = |name, setting: &Setting| DbtSetting {
        name,
        value: setting.value.clone(),
        source: setting.origin.label(),
    };
    let mut out = Vec::new();
    if settings.program.origin != Origin::Default {
        out.push(from("program", &settings.program));
    }
    for (name, setting) in [
        ("target", &settings.target),
        ("profile", &settings.profile),
        ("profiles_dir", &settings.profiles_dir),
        ("project_dir", &settings.project_dir),
    ] {
        if let Some(setting) = setting {
            out.push(from(name, setting));
        }
    }
    // Given, but never shown: vars can hold credentials (rule 9, #321).
    if args.try_get_one::<String>("vars").ok().flatten().is_some() {
        out.push(DbtSetting {
            name: "vars",
            value: "[value removed]".to_owned(),
            source: "flag".to_owned(),
        });
    }
    out.push(from("target_dir", &settings.target_dir));
    if full_refresh(args) {
        out.push(DbtSetting {
            name: "full_refresh",
            value: "true".to_owned(),
            source: match args.value_source("full-refresh") {
                Some(clap::parser::ValueSource::EnvVariable) => "DBT_FULL_REFRESH".to_owned(),
                _ => "flag".to_owned(),
            },
        });
    }
    out
}

/// The settings on one line, for people: `target prod (DBT_TARGET), …`.
pub(super) fn settings_line(settings: &[DbtSetting]) -> String {
    settings
        .iter()
        .map(|s| {
            if s.source == "flag" {
                format!("{} {}", s.name, s.value)
            } else {
                format!("{} {} ({})", s.name, s.value, s.source)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Before any dbt call: returns the dbt settings in effect, and logs them; refuses dbt
/// settings in the environment that change what is built or recorded and that no
/// flag beats; and warns about the ones ODS overrides (#227), and about artifacts
/// compiled with other vars.
pub(super) fn check_settings(
    args: &ArgMatches,
    resolved: &StateSettings,
    executor: &DbtExecutor,
    warnings: &mut Vec<String>,
) -> Result<Vec<DbtSetting>, CliError> {
    let settings = dbt_settings(args, resolved);
    // Not the vars: the log may go further than the report.
    let logged: Vec<DbtSetting> = settings
        .iter()
        .filter(|s| s.name != "vars")
        .cloned()
        .collect();
    tracing::info!(settings = %settings_line(&logged), "dbt settings");
    executor.refuse_env().map_err(|e| {
        CliError::new(ExitStatus::Usage, codes::STATE_INPUT, e.to_string()).with_hint(
            "these dbt settings change what dbt builds in ways ODS can't record; see `docs/cli.md`",
        )
    })?;
    for warning in executor
        .env_warnings()
        .into_iter()
        .chain(vars_mismatch(args, &resolved.target_dir()))
    {
        // The report shows it; the log only when asked for.
        tracing::debug!("{warning}");
        warnings.push(warning);
    }
    Ok(settings)
}

/// Options after `--`, for dbt.
pub(super) fn dbt_args(args: &ArgMatches) -> Vec<String> {
    args.get_many::<String>("dbt-args")
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

pub(super) fn executor(args: &ArgMatches, settings: &StateSettings) -> DbtExecutor {
    let capture = args.get_one::<String>("dbt-output").map(String::as_str) == Some("capture");
    let mut executor = DbtExecutor::new(&settings.program.value, settings.target_dir())
        .output(if capture {
            DbtOutput::Capture
        } else {
            DbtOutput::Stderr
        })
        // dbt's lines, read from its structured log, rendered here (#322, rule 7).
        .on_output(|line| {
            let text = crate::present::engine_line(line.time.as_deref(), &line.text);
            // Best effort: showing dbt's output must never fail the run.
            let _ = writeln!(std::io::stderr(), "{text}");
        });
    if let Some(dir) = &settings.project_dir {
        executor = executor.project_dir(&dir.value);
    }
    if let Some(dir) = &settings.profiles_dir {
        executor = executor.profiles_dir(&dir.value);
    }
    if let Some(target) = &settings.target {
        executor = executor.target(&target.value);
    }
    if let Some(profile) = &settings.profile {
        executor = executor.profile(&profile.value);
    }
    if let Some(vars) = args.get_one::<String>("vars") {
        executor = executor.vars(vars);
    }
    executor
}

/// Progress lines on stderr between dbt's own output (#220): which dbt command runs,
/// and why. dbt starts each command with the same banner, so without them the steps
/// look alike. They go to the process's stderr, where dbt writes, so they interleave
/// with it in order.
#[derive(Debug, Clone)]
pub(super) struct Steps {
    settings: ProgressSettings,
    done: Arc<AtomicUsize>,
    total: usize,
}

impl Steps {
    /// Numbered out of `total`: the dbt commands this run will make if it builds.
    pub(super) fn new(settings: ProgressSettings, total: usize) -> Self {
        Self {
            settings,
            done: Arc::default(),
            total,
        }
    }

    /// A note that isn't a dbt command, e.g. the plan.
    pub(super) fn note(&self, text: &str) {
        self.line(None, text);
    }

    /// The dbt command about to run.
    pub(super) fn step(&self, step: DbtStep) {
        let n = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        let text = match step {
            DbtStep::SourceFreshness => {
                "dbt source freshness: how new each source's data is".to_owned()
            }
            DbtStep::Compile => "dbt compile: the code as it is now, for the plan".to_owned(),
            DbtStep::Build {
                command,
                nodes,
                tests,
                sources,
            } => format!(
                "dbt {command}: {}{}{}",
                count(nodes, "node"),
                match (command, tests) {
                    (_, true) => " and their tests",
                    ("build", false) => ", without tests",
                    _ => "",
                },
                source_step(sources),
            ),
            DbtStep::Test { nodes, sources } => format!(
                "dbt test: the tests of {}{}",
                count(nodes, "node"),
                source_step(sources)
            ),
            DbtStep::Identify => "dbt compile --inline: which target dbt builds in".to_owned(),
            DbtStep::RelationCheck { nodes } => format!(
                "dbt show: are the tables of {} ODS would reuse still there?",
                count(nodes, "node")
            ),
            // The only probe ODS runs reads table versions (ADR-0022).
            DbtStep::RelationProbe { sources } => format!(
                "dbt show: reading table versions for {}",
                count(sources, "source")
            ),
            _ => "dbt".to_owned(),
        };
        self.line(Some(n), &text);
    }

    fn line(&self, n: Option<usize>, text: &str) {
        if !self.settings.enabled {
            return;
        }
        let number = n.map_or_else(String::new, |n| format!("{n}/{} ", self.total.max(n)));
        let line = if self.settings.ansi {
            format!("\x1b[1;36mods ▸\x1b[0m \x1b[1m{number}\x1b[0m{text}\n")
        } else {
            format!("ods ▸ {number}{text}\n")
        };
        // Best effort: progress must never fail the command.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    /// Makes `executor` announce each dbt command here.
    pub(super) fn attach(&self, executor: DbtExecutor) -> DbtExecutor {
        let steps = self.clone();
        executor.on_step(move |step| steps.step(step))
    }
}

/// Where a run's events go (#322): its journal, a copy kept for the report, and a
/// progress line per finished node on stderr.
pub(super) struct Observed<'s> {
    journal: JournalSink,
    collected: CollectedEvents,
    steps: &'s Steps,
    mode: ExecutionMode,
}

impl<'s> Observed<'s> {
    /// For a run in `mode` recorded beside `state_db`, with progress through `steps`.
    pub(super) fn new(state_db: &Path, steps: &'s Steps, mode: ExecutionMode) -> Self {
        Self {
            journal: JournalSink::new(state_db),
            collected: CollectedEvents::new(),
            steps,
            mode,
        }
    }

    /// The run as its events tell it (if it started), its journal (if written), and
    /// why the journal couldn't be written (if it couldn't).
    pub(super) fn finish(self) -> (Option<RunSummary>, Option<PathBuf>, Option<String>) {
        let events = self.collected.events();
        let run = (!events.is_empty()).then(|| RunSummary::from_events(&events));
        (run, self.journal.path(), self.journal.warning())
    }
}

impl RunEventSink for Observed<'_> {
    fn emit(&self, event: RunEvent) {
        // Whatever the executor filled in, only redacted text is kept or shown (rule 9).
        let event = event.sanitized();
        if let RunEventKind::NodeFinished { node, stats } = &event.kind {
            let mut line = format!(
                "{} {}",
                display_name(node),
                super::run_stats::status(stats, Some(self.mode)).text
            );
            if stats.took_ms().is_some() {
                let _ = write!(line, " in {}", super::run_stats::took(stats));
            }
            if let Some(rows) = stats.rows_affected {
                let _ = write!(line, ", {rows} rows");
            }
            let detail = super::run_stats::detail(stats);
            if !detail.is_empty() {
                let _ = write!(line, " ({detail})");
            }
            self.steps.note(&line);
        }
        self.journal.emit(event.clone());
        self.collected.emit(event);
    }
}

/// `, and the tests of 2 sources`, when there are any (#232).
fn source_step(sources: usize) -> String {
    if sources == 0 {
        String::new()
    } else {
        format!(", and the tests of {}", count(sources, "source"))
    }
}

/// Names listed per reason before the rest are counted.
const NAMES_PER_REASON: usize = 8;

/// The plan: a summary line, then what builds grouped by its main reason. Reused nodes
/// are only counted: in a large project they are most of it (`-vv` names them).
fn plan_notes(report: &RunReport) -> Vec<String> {
    let mut summary = format!("plan: {} to build, {} to reuse", report.build, report.reuse);
    if !report.left_out.is_empty() {
        let _ = write!(summary, ", {} left out", report.left_out.len());
    }
    if let Some(retry) = &report.retry {
        let _ = write!(
            summary,
            ", {} changed since and not retried",
            retry.changed_since.len()
        );
    }
    // In plan order, so groups and names read upstream first.
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for entry in report.plan.with_action(PlanAction::Build) {
        // Left out, or not retried.
        if !report.requested.contains(&entry.node) {
            continue;
        }
        let why = entry
            .reasons
            .first()
            .map_or("other", |r| reason_label(r.code));
        match groups.iter_mut().find(|(label, _)| *label == why) {
            Some((_, names)) => names.push(&entry.name),
            None => groups.push((why, vec![&entry.name])),
        }
    }
    let mut notes = vec![summary];
    let testing: Vec<String> = report
        .source_tests
        .iter()
        .filter(|c| c.action == SourceCheckAction::Test)
        .map(|c| {
            let why = c
                .reasons
                .first()
                .map_or("other", |r| source_reason_label(r.code));
            format!("{} ({why})", c.name)
        })
        .collect();
    if !testing.is_empty() {
        let mut line = format!(
            "  source tests to run: {}",
            testing[..testing.len().min(NAMES_PER_REASON)].join(", ")
        );
        if testing.len() > NAMES_PER_REASON {
            let _ = write!(line, " and {} more", testing.len() - NAMES_PER_REASON);
        }
        notes.push(line);
    }
    for (why, names) in groups {
        let mut line = format!(
            "  {why}: {}",
            names[..names.len().min(NAMES_PER_REASON)].join(", ")
        );
        if names.len() > NAMES_PER_REASON {
            let _ = write!(line, " and {} more", names.len() - NAMES_PER_REASON);
        }
        notes.push(line);
    }
    notes
}

/// A few words for why a node builds.
fn reason_label(code: ReasonCode) -> &'static str {
    match code {
        ReasonCode::NeverBuilt => "not built by ODS yet",
        ReasonCode::CodeChanged => "code changed",
        ReasonCode::UpstreamCodeChanged => "upstream code changed",
        ReasonCode::NewUpstreamData => "new upstream data",
        ReasonCode::MissingDataEvidence => "source data version unknown",
        ReasonCode::CodeEvidenceIncomplete => "code can't be fully fingerprinted",
        ReasonCode::UnknownDependency => "depends on something ODS can't see",
        ReasonCode::PolicyBlocksReuse => "its policy never reuses it",
        ReasonCode::FullRefreshRequested => "full refresh requested",
        ReasonCode::UpstreamFullRefresh => "upstream full refresh",
        ReasonCode::RelationMissing => "not in the warehouse",
        ReasonCode::RelationUnverified => "couldn't check the warehouse",
        ReasonCode::TargetChanged => "target changed",
        _ => "other",
    }
}

/// A few words for why a source's tests run (#232).
fn source_reason_label(code: ReasonCode) -> &'static str {
    match code {
        ReasonCode::NotTested => "not tested by ODS yet",
        ReasonCode::ChecksChanged => "its tests changed",
        ReasonCode::NewUpstreamData => "new data",
        ReasonCode::MissingDataEvidence => "data version unknown",
        ReasonCode::Unchanged => "asked for",
        _ => "other",
    }
}

/// Warnings for nodes this run leaves unbuilt although nodes it builds read them, e.g.
/// a changed seed under `ods state run`: dbt builds the readers against the seed's
/// current table, and says nothing (#229).
fn unbuilt_parents(
    project: &ods_state::Project,
    plan: &ExecutionPlan,
    requested: &[RequestedNode],
    left_out: &[LeftOut],
    kind: Kind,
) -> Vec<String> {
    let requested: BTreeSet<&str> = requested.iter().map(|n| n.id.as_str()).collect();
    left_out
        .iter()
        .filter_map(|l| {
            let readers: Vec<&str> = project
                .nodes
                .iter()
                .filter(|n| requested.contains(n.id.as_str()) && n.parents.contains(&l.node))
                .map(|n| n.name.as_str())
                .collect();
            if readers.is_empty() {
                return None;
            }
            let entry = plan.entries.iter().find(|e| e.node == l.node)?;
            let why = entry.reasons.first().map_or("it needs building", |r| r.message.as_str());
            Some(format!(
                "`{}` ({}) needs building ({why}), but `ods state {}` leaves it out: {} will read its current table. Run `{}` or `ods state build` too",
                entry.name,
                entry.kind,
                kind.name(),
                readers.join(", "),
                Kind::command_for(&entry.kind),
            ))
        })
        .collect()
}

/// With `--no-compile`, ODS plans from artifacts some earlier dbt command wrote. dbt
/// records the vars it ran with in `run_results.json`: if they differ from `--vars`,
/// the plan describes other code than the build will run (#229).
fn vars_mismatch(args: &ArgMatches, target_dir: &Path) -> Option<String> {
    if !args.get_flag("no-compile") {
        return None;
    }
    let compiled = RunResults::read(&target_dir.join("run_results.json"))
        .ok()?
        .vars
        .filter(|v| v.as_object().is_none_or(|o| !o.is_empty()));
    let given = args.get_one::<String>("vars");
    let differs = match (&compiled, given) {
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => true,
        // YAML that isn't JSON (`{a: 1}`) can't be compared here without a YAML parser.
        (Some(compiled), Some(given)) => {
            serde_json::from_str::<serde_json::Value>(given).is_ok_and(|g| &g != compiled)
        }
    };
    // Which vars, but not their values: they can hold credentials (rule 9, #321).
    let shown = |given: bool| if given { "other vars" } else { "none" };
    differs.then(|| {
        format!(
            "the artifacts in {} were compiled with vars ({}), but this run has {}: the plan may not match what dbt builds. Drop --no-compile, or pass the same --vars",
            target_dir.display(),
            if compiled.is_some() { "some" } else { "none" },
            shown(given.is_some()),
        )
    })
}

/// How to plan: a full refresh forces what it rebuilds, and a change of target says
/// why nodes build.
fn plan_options(args: &ArgMatches, target_changed: bool) -> ods_state::PlanOptions {
    let mut options = ods_state::PlanOptions::default();
    if full_refresh(args) {
        options = options.full_refresh();
    }
    if target_changed {
        options = options.target_changed();
    }
    options
}

/// Which target dbt builds in (#227): asked of dbt, so it is the target every dbt
/// command here uses.
pub(super) fn identify(executor: &DbtExecutor) -> Result<TargetIdentity, CliError> {
    let target = block_on(executor.identify())?.map_err(|e| {
        execution_error(&e)
            .with_hint(
                "ODS keeps state per target, so it needs dbt to render the profile; check `dbt debug`",
            )
            .before_running()
    })?;
    tracing::info!(target = %target, "dbt target");
    Ok(target)
}

/// The target a run builds in. A run that records needs it; one that doesn't (a dry
/// run, `compile`) plans as if in an unknown target, reusing nothing.
fn target_for(
    executor: &DbtExecutor,
    dry_run: bool,
    warnings: &mut Vec<String>,
) -> Result<Option<TargetIdentity>, CliError> {
    match identify(executor) {
        Ok(target) => Ok(Some(target)),
        Err(e) if dry_run => {
            warnings.push(format!("{e}; planned as if nothing were built yet"));
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The recorded state to plan against in `target`: `latest` if it was built there;
/// otherwise none of its builds count (#227), though the next snapshot still follows
/// it. Returns whether the target changed, and says so in `warnings`.
pub(super) fn in_target(
    latest: Option<StoredSnapshot>,
    target: Option<&TargetIdentity>,
    warnings: &mut Vec<String>,
) -> (Option<StoredSnapshot>, bool) {
    let Some(latest) = latest else {
        return (None, false);
    };
    if target.is_some() && latest.snapshot.target.as_ref() == target {
        return (Some(latest), false);
    }
    warnings.push(match (&latest.snapshot.target, target) {
        (Some(before), Some(target)) => format!(
            "the recorded state was built in target {before}, not {target}: nothing in it is reused"
        ),
        (None, _) => "the recorded state doesn't say which target it was built in (it predates ODS recording targets, or was recorded with `ods state record`): nothing in it is reused".to_owned(),
        (Some(before), None) => format!(
            "the recorded state was built in target {before}, and which target dbt builds in now is unknown: nothing in it is reused"
        ),
    });
    let mut empty = latest.snapshot.clone();
    empty.nodes.clear();
    // Source checks passed in another target's warehouse vouch for nothing here.
    empty.sources.clear();
    (Some(StoredSnapshot::new(latest.id, empty)), true)
}

/// The state store and its latest snapshot. A dry run with no database yet has
/// neither: it plans as a first run, and creates nothing.
fn open_state(
    ws: &Workspace,
    dry_run: bool,
) -> Result<(Option<SqliteStateStore>, Option<StoredSnapshot>), CliError> {
    if dry_run && !ws.state_db.is_file() {
        return Ok((None, None));
    }
    let store = ws.open_store()?;
    let latest = ws.latest(&store)?;
    Ok((Some(store), latest))
}

/// How many dbt commands a run will make if it builds.
fn step_count(args: &ArgMatches, settings: &StateSettings, builds: bool) -> usize {
    let compiles = !args.get_flag("no-compile");
    let measures = compiles && !args.get_flag("no-source-freshness");
    // Whether there is state, and so reuse to check: a guess made before anything
    // runs, for the count only.
    let checks = settings.state_db().is_file();
    // The target check always runs.
    1 + [measures, compiles, checks, builds]
        .into_iter()
        .map(usize::from)
        .sum::<usize>()
}

/// What a relation check is for, so its warnings say what the answer changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckFor {
    /// Reusing nodes (#230): unchecked nodes are built.
    Reuse,
    /// Pointing deferred references at this target (#296): unchecked nodes point
    /// upstream.
    Export,
}

impl CheckFor {
    fn unsupported(self) -> &'static str {
        match self {
            CheckFor::Reuse => {
                "the executor can't check that reused tables are still in the warehouse: reuse trusts the last recorded build"
            }
            CheckFor::Export => {
                "the executor can't check that tables built here are still in the warehouse: every node points upstream (relation_unverified)"
            }
        }
    }

    fn failed(self, error: &ProviderError) -> String {
        match self {
            CheckFor::Reuse => format!(
                "couldn't check that the tables of the nodes ODS would reuse are still in the warehouse, so they are built: {error}"
            ),
            CheckFor::Export => format!(
                "couldn't check that the tables built here are still in the warehouse, so they point upstream: {error}"
            ),
        }
    }
}

/// Asks `inspector` whether the relations of `requested` exist, in one query, and
/// returns what it found per node. `None` when it can't check at all (it lacks the
/// `relation_existence` capability): the caller must then treat every relation as
/// unchecked. A failed check, or a node it didn't answer for, is `Unverified`, never
/// present (AGENTS.md rule 3). Nothing is asked when nothing is requested.
pub(super) fn relation_facts<I: RelationInspector + ?Sized>(
    inspector: &I,
    requested: &[RequestedNode],
    purpose: CheckFor,
    warnings: &mut Vec<String>,
) -> Result<Option<BTreeMap<String, RelationFact>>, CliError> {
    let strategies = [
        Strategy::new("warehouse_check", [Capability::RelationExistence], true),
        Strategy::fallback("trust_recorded_build", false),
    ];
    let choice = choose(&inspector.info().capabilities, &strategies)
        .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))?;
    if !choice.chosen.value {
        warnings.push(purpose.unsupported().to_owned());
        return Ok(None);
    }
    let mut facts: BTreeMap<String, RelationFact> = BTreeMap::new();
    if requested.is_empty() {
        return Ok(Some(facts));
    }
    match block_on(inspector.inspect(requested))? {
        Ok(report) => {
            for (id, presence) in report.nodes {
                let fact = match presence {
                    RelationPresence::Present { kind } => RelationFact::Present(kind),
                    RelationPresence::Missing => RelationFact::Missing,
                    RelationPresence::Unknown(why) => RelationFact::Unverified(why),
                    _ => RelationFact::Unverified("an answer ODS doesn't understand".to_owned()),
                };
                // Two answers for one node: trust neither.
                if facts.insert(id.clone(), fact).is_some() {
                    facts.insert(
                        id,
                        RelationFact::Unverified("the check answered twice".to_owned()),
                    );
                }
            }
        }
        Err(e) => warnings.push(purpose.failed(&e)),
    }
    for node in requested {
        facts.entry(node.id.clone()).or_insert_with(|| {
            RelationFact::Unverified("the relation check didn't report on it".to_owned())
        });
    }
    // Only what was asked about.
    let asked: BTreeSet<&str> = requested.iter().map(|n| n.id.as_str()).collect();
    facts.retain(|id, _| asked.contains(id.as_str()));
    Ok(Some(facts))
}

/// Checks that the nodes the plan would reuse are still in the warehouse, and records
/// the answers on them, so the plan builds the ones that aren't (#230). One query for
/// all of them; none when there is nothing to reuse. If the check fails, they are all
/// built: missing evidence never means reuse (AGENTS.md rule 3). Returns `options`
/// for the plan, saying whether relations were checked, so it builds any node that
/// wasn't.
fn check_relations<I: RelationInspector + ?Sized>(
    project: &mut ods_state::Project,
    latest: Option<&StoredSnapshot>,
    inspector: &I,
    now: Timestamp,
    options: ods_state::PlanOptions,
    warnings: &mut Vec<String>,
) -> Result<ods_state::PlanOptions, CliError> {
    // Asking about nothing makes no query: it only says whether a check is possible.
    // If it isn't, reuse says in its evidence that nothing checked the relation.
    if relation_facts(inspector, &[], CheckFor::Reuse, warnings)?.is_none() {
        return Ok(options);
    }
    let candidates =
        ods_state::reuse_candidates(project, latest.map(|s| (s.id, &s.snapshot)), now, options)
            .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))?;
    if candidates.is_empty() {
        return Ok(options);
    }
    let names: BTreeMap<&str, &str> = project
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();
    let requested: Vec<RequestedNode> = candidates
        .iter()
        .map(|id| RequestedNode::new(id.clone(), names.get(id.as_str()).copied().unwrap_or(id)))
        .collect();
    let Some(mut facts) = relation_facts(inspector, &requested, CheckFor::Reuse, warnings)? else {
        return Ok(options);
    };
    let candidates: BTreeSet<String> = candidates.into_iter().collect();
    for node in &mut project.nodes {
        if candidates.contains(&node.id) {
            node.relation = facts.remove(&node.id).unwrap_or_else(|| {
                RelationFact::Unverified("the relation check didn't report on it".to_owned())
            });
        }
    }
    Ok(options.relations_checked())
}

/// Each node's and source's checks digest.
fn checks_by_node(project: &ods_state::Project) -> BTreeMap<String, Option<String>> {
    project
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.checks.clone()))
        .chain(
            project
                .sources
                .iter()
                .map(|s| (s.id.clone(), s.checks.clone())),
        )
        .collect()
}

/// Which sources' checks run with this command (#232): those in the selection whose
/// data is new or unknown since they last passed in this target, or all of them with
/// `all`. Every source with checks in the selection is listed, with why it runs or not.
pub(super) fn plan_source_checks(
    args: &ArgMatches,
    project: &ods_state::Project,
    latest: Option<&StoredSnapshot>,
    all: bool,
) -> Result<Vec<SourceCheck>, CliError> {
    let scope = ods_state::select_sources(project, &select_specs(args))
        .map_err(|e| CliError::new(ExitStatus::Usage, codes::LINEAGE_TARGET, e))?;
    let checks = ods_state::source_checks(project, latest.map(|s| &s.snapshot), &scope, all);
    for check in &checks {
        tracing::debug!(
            source = %check.name,
            action = ?check.action,
            why = check.reasons.first().map_or("", |r| r.message.as_str()),
            "source tests"
        );
    }
    Ok(checks)
}

/// A build's source checks: like `dbt build`, a build with tests runs the sources'
/// tests too (#232); one without runs none.
fn build_source_checks(
    tested: bool,
    args: &ArgMatches,
    project: &ods_state::Project,
    latest: Option<&StoredSnapshot>,
) -> Result<Vec<SourceCheck>, CliError> {
    if tested {
        plan_source_checks(args, project, latest, false)
    } else {
        Ok(Vec::new())
    }
}

/// The sources whose checks run.
pub(super) fn source_requests(checks: &[SourceCheck]) -> Vec<RequestedNode> {
    checks
        .iter()
        .filter(|c| c.action == SourceCheckAction::Test)
        .map(|c| RequestedNode::new(c.source.clone(), c.name.clone()))
        .collect()
}

/// What an execution shows about each requested source's checks: passed only when
/// they all ran and passed, failed when one failed; a source whose checks didn't all
/// run keeps what it had.
pub(super) fn source_results(execution: &ExecutionReport) -> Vec<TestResult> {
    execution
        .sources
        .iter()
        .filter_map(|s| {
            if s.status == ExecutionStatus::Failed || !s.checks_failed.is_empty() {
                Some(TestResult::new(s.node.clone(), false, s.completed_at))
            } else if s.fully_checked() {
                Some(TestResult::new(s.node.clone(), true, s.completed_at))
            } else {
                None
            }
        })
        .collect()
}

/// The source checks' rows for people: what ran and how it ended, or, if nothing ran,
/// what would run. A failed check is named as `names` describes it (#323).
pub(super) fn source_rows(
    checks: &[SourceCheck],
    execution: Option<&ExecutionReport>,
    names: &super::failures::CheckNames,
) -> Vec<Vec<Vec<Span>>> {
    checks
        .iter()
        .map(|c| {
            let ran = execution.and_then(|e| e.sources.iter().find(|s| s.node == c.source));
            let result = match (c.action, ran) {
                (SourceCheckAction::Skip, _) => vec![Span::toned("skipped", Tone::Success)],
                (_, None) => vec![Span::toned("to test", Tone::Warning)],
                (_, Some(s)) if s.fully_checked() => vec![Span::toned("passed", Tone::Success)],
                (_, Some(s)) if !s.checks_failed.is_empty() => {
                    let mut line = vec![Span::toned("failed: ", Tone::Error)];
                    line.extend(super::failures::checks_line(&s.checks_failed, names));
                    line
                }
                (_, Some(s)) => vec![Span::toned(
                    s.message.clone().unwrap_or_else(|| "didn't run".to_owned()),
                    Tone::Warning,
                )],
            };
            vec![
                vec![Span::toned(c.name.as_str(), Tone::Code)],
                result,
                vec![Span::plain(
                    c.reasons
                        .iter()
                        .map(|r| r.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                )],
            ]
        })
        .collect()
}

/// Whether `--full-refresh` was given (not every command has it).
fn full_refresh(args: &ArgMatches) -> bool {
    args.try_get_one::<bool>("full-refresh").ok().flatten() == Some(&true)
}

/// Values of a repeatable option, if the command has it.
fn values(args: &ArgMatches, id: &str) -> Vec<String> {
    args.try_get_many::<String>(id)
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

/// The resource types this run builds: the command's, narrowed by `--resource-type`
/// and `--exclude-resource-type`.
fn build_types(kind: Kind, args: &ArgMatches) -> Vec<String> {
    let asked = values(args, "resource-type");
    let excluded = values(args, "exclude-resource-type");
    kind.types()
        .unwrap_or_default()
        .iter()
        .filter(|t| asked.is_empty() || asked.iter().any(|a| a == *t))
        .filter(|t| !excluded.iter().any(|e| e == *t))
        .map(|t| (*t).to_owned())
        .collect()
}

/// Whether this run tests what it builds: `build` does, like `dbt build`, unless
/// `--exclude-resource-type test`.
fn execution_tested(kind: Kind, args: &ArgMatches) -> bool {
    kind == Kind::Build
        && !values(args, "exclude-resource-type")
            .iter()
            .any(|e| e == "test")
}

/// `1 node`, `2 nodes`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Step 1: refresh the artifacts and measure sources, unless told not to. Returns what
/// the executor reported and whether to read `sources.json`.
pub(super) fn prepare(
    args: &ArgMatches,
    executor: &DbtExecutor,
    warnings: &mut Vec<String>,
) -> Result<(Option<PrepareReport>, Sources), CliError> {
    if args.get_flag("no-compile") {
        // Measuring would rewrite the manifest (dbt does that on every command), so
        // nothing is measured. A `sources.json` left in the target directory could
        // predate new data, so only an explicit `--sources` is trusted.
        let sources = if args.contains_id("sources")
            && args.value_source("sources") == Some(clap::parser::ValueSource::CommandLine)
        {
            Sources::AsGiven
        } else {
            Sources::Ignore
        };
        return Ok((None, sources));
    }
    let measure = !args.get_flag("no-source-freshness");
    let request = if measure {
        PrepareRequest::new().measuring_sources()
    } else {
        PrepareRequest::new()
    };
    let report = block_on(executor.prepare(&request))?.map_err(|e| {
        execution_error(&e)
            .with_hint(
                "fix the project so `dbt compile` succeeds, or plan from existing artifacts with --no-compile",
            )
            .before_running()
    })?;
    warnings.extend(report.warnings.iter().cloned());
    // A measurement that was attempted and failed leaves an out-of-date file behind.
    let sources = if measure && !report.sources_measured {
        Sources::Ignore
    } else {
        Sources::AsGiven
    };
    Ok((Some(report), sources))
}

/// Step 4: record the run from the artifacts it wrote, which describe the code it
/// built. Only successes advance; if none did, nothing is committed.
fn record(
    (args, settings): (&ArgMatches, &StateSettings),
    tested: bool,
    (sources, table_versions): (Sources, Option<&VersionReading>),
    execution: &ExecutionReport,
    (latest, target): (Option<&StoredSnapshot>, Option<&TargetIdentity>),
    store: &SqliteStateStore,
    (planned_checks, timings): (&BTreeMap<String, Option<String>>, Option<&RunSummary>),
) -> Result<RunRecord, CliError> {
    let mut built = Workspace::load(args, settings, sources)?;
    // Each node's checks as planned. The manifest dbt writes while building keeps the
    // compiled SQL only of the tests that invocation ran, so digests computed from it
    // depend on what ran; the plan's (from `dbt compile`, just before this build, so
    // the same code) match what the next plan computes (#229).
    for node in &mut built.project.nodes {
        if let Some(checks) = planned_checks.get(&node.id) {
            node.checks.clone_from(checks);
        }
    }
    for source in &mut built.project.sources {
        if let Some(checks) = planned_checks.get(&source.id) {
            source.checks.clone_from(checks);
        }
    }
    // The table versions read before the build, not after it (ADR-0022 §3).
    if let Some(reading) = table_versions {
        built.add_reading(reading.clone());
    }
    if built.invocation_id.as_deref() != Some(execution.run_id.as_str()) {
        return Err(CliError::new(
            ExitStatus::Failure,
            codes::STATE_INPUT,
            "the manifest in the target directory isn't from this run, so ODS can't tell which code was built; nothing was recorded",
        )
        .with_hint("don't run other dbt commands against the same target directory during `ods state run`"));
    }
    let results: Vec<RunResult> = execution
        .nodes
        .iter()
        .map(|n| {
            let result = RunResult::new(
                n.node.clone(),
                match n.status {
                    // Built but not validated: keep the last validated state, so the
                    // node and its checks run again.
                    ExecutionStatus::Success if !n.checks_failed.is_empty() => Outcome::Failed,
                    ExecutionStatus::Success => Outcome::Success,
                    ExecutionStatus::Skipped => Outcome::Skipped,
                    _ => Outcome::Failed,
                },
                n.completed_at,
            )
            // How long dbt took to build it, from the run's events (ADR-0029).
            .timed(
                timings
                    .and_then(|run| run.get(&n.node))
                    .and_then(|s| s.stats.took_ms()),
            );
            // Built with its tests, and they passed.
            if tested && n.fully_checked() {
                result.tested()
            } else {
                result
            }
        })
        .collect();
    let sources_recorded = keep_versions_read_before(&mut built.project, execution.started_at);
    let mut recorded = ods_state::record(
        &built.project,
        latest.map(|s| (s.id, &s.snapshot)),
        &results,
        &execution.run_id,
        execution.finished_at,
        // Each source's version was dropped above unless it predates the run.
        true,
    );
    recorded.snapshot.target = target.cloned();
    // Source tests (#232): recorded against the data measured before the run.
    let source_tests = ods_state::record_source_checks(
        &mut recorded.snapshot,
        &built.project,
        &source_results(execution),
        execution.finished_at,
        true,
    );
    tracing::info!(
        run = %execution.run_id,
        advanced = recorded.advanced.len(),
        kept = recorded.kept.len(),
        source_tests_passed = source_tests.passed.len(),
        source_tests_failed = source_tests.failed.len(),
        "recording the run"
    );
    for (node, why) in &recorded.kept {
        tracing::debug!(node = %node, why = %why, "kept its last state");
    }
    let sources_changed = latest.map_or_else(BTreeMap::new, |s| s.snapshot.sources.clone())
        != recorded.snapshot.sources;
    let snapshot = if recorded.advanced.is_empty() && !sources_changed {
        None
    } else {
        Some(
            block_on(store.commit(&built.scope, &recorded.snapshot))?.map_err(|e| {
                let error = store_error(&e);
                if matches!(e, ProviderError::Conflict(_)) {
                    error.with_hint(
                        "another run recorded state while this one ran; this run's results weren't recorded, so the next run builds them again",
                    )
                } else {
                    error
                }
            })?,
        )
    };
    Ok(RunRecord {
        snapshot,
        sources_recorded,
        recorded,
        source_tests,
    })
}

/// Drops the source versions not read strictly before the run `started` (timestamps
/// are to the second): one read later could show data the run didn't see. Returns
/// whether any version is left to record.
fn keep_versions_read_before(project: &mut ods_state::Project, started: Option<Timestamp>) -> bool {
    let mut kept = false;
    for source in &mut project.sources {
        if matches!((source.observed_at, started), (Some(at), Some(started)) if at < started) {
            kept |= source.version.is_some();
        } else {
            source.version = None;
            source.observed_at = None;
        }
    }
    kept
}

fn execution_error(error: &ProviderError) -> CliError {
    CliError::new(
        ExitStatus::Failure,
        codes::STATE_EXECUTION,
        error.to_string(),
    )
}

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RunOutcome {
    /// `ods state compile`: prepared and planned only.
    Compiled,
    /// `--dry-run`: planned only.
    DryRun,
    /// Everything could be reused; nothing ran.
    NothingToBuild,
    /// Every node built and every check passed.
    Succeeded,
    /// Some nodes or checks failed; the successes were recorded.
    Failed,
    /// dbt ran, but nothing could be recorded (e.g. another run recorded first).
    NotRecorded,
}

/// What was committed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RunRecord {
    /// The new snapshot, or `None` if nothing succeeded and nothing was committed.
    snapshot: Option<SnapshotId>,
    /// Whether source versions were recorded (only if measured before the run).
    sources_recorded: bool,
    #[serde(flatten)]
    recorded: Recorded,
    /// Which sources' tests passed or failed (#232).
    #[serde(skip_serializing_if = "RecordedSources::is_empty")]
    source_tests: RecordedSources,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RunReport {
    state_db: PathBuf,
    scope: String,
    based_on: Option<SnapshotId>,
    outcome: RunOutcome,
    build: usize,
    reuse: usize,
    /// The build time reusing saved, estimated from each reused node's last measured
    /// build (#210, ADR-0029).
    savings: ods_state::Savings,
    /// The nodes reused that this command would otherwise have built, in plan order.
    #[serde(skip)]
    reuse_scope: Vec<String>,
    /// Whether the built nodes' tests ran too (`--test`).
    tests: bool,
    /// Nodes to build that this run left out (`--exclude`, `--resource-type`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    left_out: Vec<LeftOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared: Option<PrepareReport>,
    plan: ExecutionPlan,
    /// Every selected source with tests, and whether they run and why (#232).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_tests: Vec<SourceCheck>,
    /// Checks by their handles, never their ids (#323).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::failures::serialize_execution"
    )]
    execution: Option<ExecutionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<RunRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    /// The dbt settings in effect, and where each came from.
    dbt: Vec<DbtSetting>,
    /// The target dbt builds in, if dbt could say.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<TargetIdentity>,
    #[serde(skip)]
    has_sources: bool,
    /// Whether the warehouse was asked if the nodes to reuse are still there (#230).
    /// Each reused node's evidence says so already.
    #[serde(skip)]
    relations_checked: bool,
    /// Each node's checks digest when planned, for recording.
    #[serde(skip)]
    planned_checks: BTreeMap<String, Option<String>>,
    /// The nodes this run builds.
    #[serde(skip)]
    requested: BTreeSet<String>,
    /// With `ods state retry --failed` (#292): what is retried, and what isn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    retry: Option<RetryReport>,
    #[serde(flatten)]
    observed: RunObserved,
}

/// What a run's events said (#322, ADR-0024).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RunObserved {
    /// Each node's stats, and the totals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) run_stats: Option<RunSummary>,
    /// The run's journal, beside the state database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) journal: Option<PathBuf>,
    /// Each failed node, explained (#323, ADR-0025).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) failures: Vec<ods_core::failure::ErrorExplanation>,
    /// The checks the execution lists as failed or skipped, described for the terminal
    /// (#323); never serialized.
    #[serde(skip)]
    pub(super) check_names: super::failures::CheckNames,
}

/// Runs `request` through `executor`, keeping its events in the journal beside
/// `state_db` and showing each node's result through `steps` as it finishes. A journal
/// that couldn't be written adds a warning.
pub(super) fn execute_observed(
    executor: &DbtExecutor,
    request: &ExecutionRequest,
    (state_db, steps): (&Path, &Steps),
    warnings: &mut Vec<String>,
) -> Result<(Result<ExecutionReport, ProviderError>, RunObserved), CliError> {
    let observed = Observed::new(state_db, steps, request.mode);
    let executed = block_on(executor.execute_with_events(request, &observed))?;
    let (run_stats, journal, warning) = observed.finish();
    warnings.extend(warning);
    Ok((
        executed,
        RunObserved {
            run_stats,
            journal,
            failures: Vec::new(),
            check_names: super::failures::CheckNames::new(),
        },
    ))
}

/// A node and why, e.g. why a retry leaves it as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct NodeWhy {
    node: String,
    reason: String,
}

/// `ods state retry --failed` (#292): the run retried, and how the plan splits for it.
/// Every list is in plan order.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RetryReport {
    /// The command line retried.
    of: String,
    /// Nodes that failed or were skipped in it, and are built again.
    retried: Vec<String>,
    /// Nodes that failed or were skipped in it that the plan now reuses, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reused: Vec<NodeWhy>,
    /// Nodes that failed or were skipped in it that aren't built, because a parent
    /// they read needs building and isn't: they'd run on stale input.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    held_back: Vec<NodeWhy>,
    /// Nodes the plan builds that weren't in the failed run's failures: they changed
    /// since, and aren't built. The reason is the plan's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_since: Vec<NodeWhy>,
    /// Nodes that failed or were skipped in it that the plan doesn't have, e.g.
    /// removed from the project.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_planned: Vec<String>,
    /// Sources whose tests failed in it and run again (#232).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_tests: Vec<String>,
    /// Sources whose tests would run now but didn't fail in it: they aren't run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_tests_changed_since: Vec<String>,
    /// The sources whose tests failed in it.
    #[serde(skip)]
    failed_sources: BTreeSet<String>,
}

/// Plans every node of the project, not just the selection, so a retry can see which
/// unselected parents need building. A full refresh applies only to what the command
/// selected, so it is left out here.
fn plan_upstream(
    ws: &Workspace,
    latest: Option<&StoredSnapshot>,
    now: Timestamp,
    mut options: ods_state::PlanOptions,
) -> Result<ExecutionPlan, CliError> {
    options.full_refresh = false;
    let all: BTreeSet<String> = ws.project.nodes.iter().map(|n| n.id.clone()).collect();
    ods_state::plan_with(
        &ws.project,
        latest.map(|s| (s.id, &s.snapshot)),
        &all,
        now,
        options,
    )
    .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))
}

impl RetryReport {
    /// Narrows `requested`, what the command would build of `plan`, to what failed in
    /// the run `failed` retries, through [`ods_state::split_retry`]. Without `failed`,
    /// `requested` is unchanged.
    ///
    /// `upstream` plans every node of the project, so a parent the command didn't
    /// select still holds back a child that would read it stale. It is only called
    /// when retrying.
    fn narrow(
        failed: Option<&RetryFailed>,
        plan: &ExecutionPlan,
        requested: Vec<RequestedNode>,
        upstream: impl FnOnce() -> Result<ExecutionPlan, CliError>,
    ) -> Result<(Vec<RequestedNode>, Option<Self>), CliError> {
        let Some(failed) = failed else {
            return Ok((requested, None));
        };
        let buildable: BTreeSet<String> = requested.iter().map(|n| n.id.clone()).collect();
        let upstream = upstream()?;
        let actions: BTreeMap<String, PlanAction> = upstream
            .entries
            .iter()
            .map(|e| (e.node.clone(), e.action))
            .collect();
        let split = ods_state::split_retry(plan, &failed.outcome.nodes(), &buildable, &actions);
        // The command's own entries win; unselected parents are named from `upstream`.
        let entries: BTreeMap<&str, &ods_core::state::PlanEntry> = upstream
            .entries
            .iter()
            .chain(&plan.entries)
            .map(|e| (e.node.as_str(), e))
            .collect();
        let why = |id: &str| {
            entries
                .get(id)
                .and_then(|e| e.reasons.first())
                .map_or_else(String::new, |r| r.message.clone())
        };
        let noted = |ids: &[String]| {
            ids.iter()
                .map(|id| NodeWhy {
                    node: id.clone(),
                    reason: why(id),
                })
                .collect()
        };
        let held_back = split
            .held_back
            .iter()
            .map(|h| NodeWhy {
                node: h.node.clone(),
                reason: format!(
                    "reads `{}`, which needs building ({}) but isn't built in this retry: it would run on stale input",
                    entries.get(h.parent.as_str()).map_or(h.parent.as_str(), |e| e.name.as_str()),
                    why(&h.parent)
                ),
            })
            .collect();
        let retried: BTreeSet<&str> = split.retried.iter().map(String::as_str).collect();
        let requested = requested
            .into_iter()
            .filter(|n| retried.contains(n.id.as_str()))
            .collect();
        let report = Self {
            of: failed.shown.clone(),
            reused: noted(&split.reused),
            held_back,
            changed_since: noted(&split.changed_since),
            not_planned: split.not_planned.clone(),
            retried: split.retried,
            source_tests: Vec::new(),
            source_tests_changed_since: Vec::new(),
            failed_sources: failed.outcome.failed_source_tests.clone(),
        };
        Ok((requested, Some(report)))
    }

    /// Keeps in `checks` only the sources whose tests failed in the run retried; the
    /// others that would be tested are listed as changed since.
    fn with_sources(mut self, checks: &mut Vec<SourceCheck>) -> Self {
        let (retried, others): (Vec<SourceCheck>, Vec<SourceCheck>) = std::mem::take(checks)
            .into_iter()
            .partition(|c| self.failed_sources.contains(&c.source));
        self.source_tests = retried
            .iter()
            .filter(|c| c.action == SourceCheckAction::Test)
            .map(|c| c.source.clone())
            .collect();
        self.source_tests_changed_since = others
            .iter()
            .filter(|c| c.action == SourceCheckAction::Test)
            .map(|c| c.source.clone())
            .collect();
        *checks = retried;
        self
    }
}

impl RunReport {
    /// Runs the whole flow and emits the report.
    pub(super) fn run(
        kind: Kind,
        args: &ArgMatches,
        ctx: &mut Context<'_>,
    ) -> Result<(), CliError> {
        Self::run_with(kind, args, ctx, None)
    }

    /// Runs the whole flow and emits the report; with `retry`, builds only what failed
    /// in the run it retries (#292).
    pub(super) fn run_with(
        kind: Kind,
        args: &ArgMatches,
        ctx: &mut Context<'_>,
        retry: Option<&RetryFailed>,
    ) -> Result<(), CliError> {
        let settings = StateSettings::resolve(args, ctx.config)?;
        let remembered = if kind == Kind::Compile {
            None
        } else {
            super::state_retry::remember(&build_command(kind), args, &settings)
        };
        let started = std::time::SystemTime::now();
        let (report, record_error) = match Self::build(kind, args, &settings, ctx.progress, retry) {
            Ok(built) => built,
            Err(error) => return failed_before_running::<false>(ctx, &settings, started, error),
        };
        if let Some(remembered) = remembered {
            remembered.finish(
                report.last_outcome(),
                report.scope.clone(),
                report.execution.as_ref().map(|e| e.run_id.clone()),
            );
        }
        if let Some(error) = record_error {
            return ctx.emit_failed(&report, error);
        }
        match report.outcome {
            RunOutcome::Failed => {
                let execution = report.execution.as_ref();
                let failed = execution.map_or(0, |e| {
                    e.nodes
                        .iter()
                        .filter(|n| {
                            n.status != ExecutionStatus::Success || !n.checks_failed.is_empty()
                        })
                        .count()
                });
                let checks = execution.map_or(0, |e| e.checks_failed.len());
                let error = CliError::new(
                    ExitStatus::Failure,
                    codes::STATE_EXECUTION,
                    format!(
                        "the run didn't fully succeed ({} failed, were skipped or failed their tests; {} failed)",
                        count(failed, "node"),
                        count(checks, "check")
                    ),
                )
                .with_hint(
                    "nodes that built and passed their tests were recorded; the others keep their last successful state and are built (and tested) next run",
                );
                ctx.emit_failed(&report, error)
            }
            _ => ctx.emit(&report),
        }
    }

    /// What failed, for `ods state retry --failed` (#292): what dbt reports, and, in a
    /// retry, what it held back. A run that built nothing failed nothing.
    fn last_outcome(&self) -> LastOutcome {
        let mut outcome = self
            .execution
            .as_ref()
            .map_or_else(LastOutcome::default, |e| {
                LastOutcome::of(e, self.record.is_some())
            });
        if let Some(retry) = &self.retry {
            outcome
                .skipped
                .extend(retry.held_back.iter().map(|h| h.node.clone()));
        }
        outcome
    }

    /// Runs the flow. A failure to record after dbt ran comes back with the report, so
    /// the caller still sees what dbt did.
    #[allow(
        clippy::too_many_lines,
        reason = "one flow, in order: prepare, plan, narrow, report"
    )]
    fn build(
        kind: Kind,
        args: &ArgMatches,
        settings: &StateSettings,
        progress: ProgressSettings,
        retry: Option<&RetryFailed>,
    ) -> Result<(Self, Option<CliError>), CliError> {
        let builds = kind != Kind::Compile;
        let steps = Steps::new(progress, step_count(args, settings, builds));
        let executor = steps.attach(executor(args, settings));
        let mut warnings = Vec::new();
        let dbt = check_settings(args, settings, &executor, &mut warnings)?;

        // 1. Prepare: the code as it is, and the target dbt builds in.
        let (prepared, sources) = prepare(args, &executor, &mut warnings)?;
        let dry_run = !builds || args.get_flag("dry-run");
        let target = target_for(&executor, dry_run, &mut warnings)?;

        // 2. Plan, with the sources' table versions where the warehouse has them.
        let mut ws = Workspace::load(args, settings, sources)?;
        let tested = execution_tested(kind, args);
        // The store first: a problem with it stops the run before the warehouse is asked.
        let (store, latest) = open_state(&ws, dry_run)?;
        let table_versions =
            super::state_versions::read_table_versions(&mut ws, &executor, &mut warnings)?;
        let (latest, target_changed) = in_target(latest, target.as_ref(), &mut warnings);
        // One clock for the check and the plan, so they agree on what is due.
        let now = Timestamp::now();
        let options = check_relations(
            &mut ws.project,
            latest.as_ref(),
            &executor,
            now,
            plan_options(args, target_changed),
            &mut warnings,
        )?;
        let (plan, plan_warnings) =
            plan_against(&ws, latest.as_ref(), &select_specs(args), now, options)?;
        warnings.extend(plan_warnings);
        // Empty for `compile`, which builds nothing.
        let types = build_types(kind, args);
        let (requested, left_out) = narrow(
            args,
            &ws.project,
            plan.with_action(PlanAction::Build)
                .map(|e| (e.node.clone(), e.name.clone(), e.kind.clone()))
                .collect(),
            &types,
            &format!("ods state {}", kind.name()),
        )?;
        let every_node = || plan_upstream(&ws, latest.as_ref(), now, options);
        let (requested, retry) = RetryReport::narrow(retry, &plan, requested, every_node)?;
        if builds {
            warnings.extend(unbuilt_parents(
                &ws.project,
                &plan,
                &requested,
                &left_out,
                kind,
            ));
        }
        let mut source_tests = build_source_checks(tested, args, &ws.project, latest.as_ref())?;
        let retry = retry.map(|r| r.with_sources(&mut source_tests));
        // What this command reuses that it would otherwise have built: of its kinds of
        // node, or, retrying, of what failed (ADR-0029). Reusing a model saves `ods state
        // seed` nothing.
        let reuse_scope: Vec<String> = match &retry {
            Some(retry) => retry.reused.iter().map(|n| n.node.clone()).collect(),
            None => plan
                .with_action(PlanAction::Reuse)
                .filter(|e| types.contains(&e.kind))
                .map(|e| e.node.clone())
                .collect(),
        };
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.as_ref().map(|s| s.id),
            outcome: RunOutcome::DryRun,
            build: requested.len(),
            reuse: plan.with_action(PlanAction::Reuse).count(),
            savings: ods_state::savings(
                reuse_scope.iter().map(String::as_str),
                requested.len(),
                latest.as_ref().map(|s| &s.snapshot),
            ),
            reuse_scope,
            tests: tested,
            left_out,
            prepared,
            plan,
            source_tests,
            execution: None,
            record: None,
            warnings,
            dbt,
            target,
            has_sources: !ws.project.sources.is_empty(),
            relations_checked: options.relations_checked,
            planned_checks: checks_by_node(&ws.project),
            requested: requested.iter().map(|n| n.id.clone()).collect(),
            retry,
            observed: RunObserved::default(),
        };
        for note in plan_notes(&report) {
            steps.note(&note);
        }
        if !builds {
            steps.note("compiled: nothing is built or recorded");
            report.outcome = RunOutcome::Compiled;
            return Ok((report, None));
        }
        if dry_run {
            steps.note("dry run: nothing is built or recorded");
            return Ok((report, None));
        }
        let checked_sources = source_requests(&report.source_tests);
        let nothing = requested.is_empty() && checked_sources.is_empty();
        let store = match store {
            Some(store) if !nothing => store,
            store => {
                steps.note("nothing to build, so dbt doesn't run again");
                report.outcome = RunOutcome::NothingToBuild;
                if let Some(store) = &store {
                    // No executor, so no run id: ODS's own, to the millisecond (ADR-0029).
                    let now = Timestamp::now();
                    let run_id =
                        format!("ods-{}", ods_core::state::TimestampMs::now().unix_millis());
                    report.write_ledger(
                        store,
                        &ws.scope,
                        &run_id,
                        (Some(now), now),
                        latest.as_ref(),
                    );
                }
                return Ok((report, None));
            }
        };

        report.execute(
            (args, settings),
            (&executor, &steps),
            (requested, checked_sources),
            (sources, table_versions.as_ref()),
            (latest.as_ref(), &ws),
            &store,
        )
    }

    /// Explains each node that failed (#323).
    fn explain_failures(
        &mut self,
        ws: &Workspace,
        settings: &StateSettings,
        before: Option<&StoredSnapshot>,
    ) {
        let project_dir = super::failures::project_dir(settings);
        let files = super::failures::ProjectFiles {
            project_dir: &project_dir,
            target_dir: &ws.target_dir,
            manifest: Some(&ws.manifest),
            last_manifest: None,
        };
        if let Some(execution) = &self.execution {
            self.observed.check_names = super::failures::describe_checks(execution, &files);
        }
        let Some(run) = &self.observed.run_stats else {
            return;
        };
        let evidence = super::failures::Evidence {
            files,
            plan: Some(&self.plan),
            before: before.map(|s| &s.snapshot),
            state_db: &self.state_db,
            retry: Some(ods_state::Retry::new(super::failures::retry_state_db(
                settings,
            ))),
            state_db_flag: super::failures::retry_state_db(settings),
            project_is_run: true,
            doctor: Some(super::failures::Doctor {
                config: None,
                settings,
            }),
        };
        self.observed.failures = super::failures::explain_run(run, &evidence);
    }

    /// Steps 3 and 4: build the requested nodes with dbt, then record the run. A
    /// failure to record comes back with the report, so the caller still sees what dbt
    /// did.
    fn execute(
        mut self,
        (args, settings): (&ArgMatches, &StateSettings),
        (executor, steps): (&DbtExecutor, &Steps),
        (requested, checked_sources): (Vec<RequestedNode>, Vec<RequestedNode>),
        sources: (Sources, Option<&VersionReading>),
        (latest, ws): (Option<&StoredSnapshot>, &Workspace),
        store: &SqliteStateStore,
    ) -> Result<(Self, Option<CliError>), CliError> {
        // 3. Execute.
        let mode = if self.tests {
            ExecutionMode::Build
        } else {
            ExecutionMode::Run
        };
        let request = ExecutionRequest::new(requested, mode)
            .with_sources(checked_sources)
            .with_full_refresh(full_refresh(args))
            .with_engine_args(dbt_args(args))
            .with_scope(self.scope.clone());
        let (executed, observed) = execute_observed(
            executor,
            &request,
            (&self.state_db, steps),
            &mut self.warnings,
        )?;
        self.observed = observed;
        // dbt started: what it did is in the run's journal, not "nothing ran".
        let journal_hint = self
            .observed
            .run_stats
            .as_ref()
            .and_then(|r| r.run_id.as_deref())
            .map_or_else(String::new, |id| {
                format!("; `ods state history --run {id}` shows and explains what it did")
            });
        let execution = executed.map_err(|e| {
            execution_error(&e).with_hint(format!(
                "nothing was recorded; the last successful state is unchanged{journal_hint}"
            ))
        })?;
        if !execution.unrequested.is_empty() {
            self.warnings.push(format!(
                "dbt also built {}, which weren't requested (the project changed after it was compiled?); they weren't recorded and will be built next run",
                execution.unrequested.iter().map(|n| display_name(n)).collect::<Vec<_>>().join(", ")
            ));
        }

        // 4. Record.
        let recorded = record(
            (args, settings),
            self.tests,
            sources,
            &execution,
            (latest, self.target.as_ref()),
            store,
            (&self.planned_checks, self.observed.run_stats.as_ref()),
        );
        self.execution = Some(execution);
        self.explain_failures(ws, settings, latest);
        Ok(match recorded {
            Ok(record) => {
                self.outcome = if self.execution.as_ref().is_some_and(|e| e.succeeded) {
                    RunOutcome::Succeeded
                } else {
                    RunOutcome::Failed
                };
                self.record = Some(record);
                (self, None)
            }
            Err(error) => {
                self.outcome = RunOutcome::NotRecorded;
                (self, Some(error))
            }
        })
        .map(|(mut report, error)| {
            if let Some(execution) = report.execution.clone() {
                report.write_ledger(
                    store,
                    &ws.scope,
                    &execution.run_id,
                    (execution.started_at, execution.finished_at),
                    latest,
                );
            }
            (report, error)
        })
    }

    /// Appends this run to the store's run ledger (ADR-0029): what it reused, with
    /// each build's last measured time, and what it built, failed or skipped. Evidence,
    /// not state: if it can't be written, the run says so and carries on.
    fn write_ledger(
        &mut self,
        store: &SqliteStateStore,
        scope: &StateScope,
        run_id: &str,
        (started_at, finished_at): (Option<Timestamp>, Timestamp),
        before: Option<&StoredSnapshot>,
    ) {
        let outcome = match self.outcome {
            RunOutcome::NothingToBuild => RunEntryOutcome::NothingToBuild,
            RunOutcome::Succeeded => RunEntryOutcome::Succeeded,
            RunOutcome::Failed => RunEntryOutcome::Failed,
            RunOutcome::NotRecorded => RunEntryOutcome::NotRecorded,
            RunOutcome::Compiled | RunOutcome::DryRun => return,
        };
        let mut nodes = BTreeMap::new();
        for node in &self.reuse_scope {
            let last = before.and_then(|b| b.snapshot.nodes.get(node));
            nodes.insert(
                node.clone(),
                RunNode::new(RunAction::Reused).timed(
                    last.and_then(|n| n.build_ms),
                    last.map_or("", |n| n.run_id.as_str()),
                ),
            );
        }
        for node in self.execution.iter().flat_map(|e| &e.nodes) {
            let action = match node.status {
                ExecutionStatus::Success => RunAction::Built,
                ExecutionStatus::Skipped => RunAction::Skipped,
                _ => RunAction::Failed,
            };
            let took = (action == RunAction::Built)
                .then(|| {
                    self.observed
                        .run_stats
                        .as_ref()
                        .and_then(|r| r.get(&node.node))
                        .and_then(|n| n.stats.took_ms())
                })
                .flatten();
            nodes.insert(node.node.clone(), RunNode::new(action).timed(took, run_id));
        }
        let entry = RunEntry::new(run_id, finished_at, outcome, nodes)
            .started(started_at)
            .snapshots(
                before.map(|b| b.id),
                self.record.as_ref().and_then(|r| r.snapshot),
            );
        let written = block_on(store.record_run(scope, &entry))
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        if let Err(why) = written {
            let state = match self.outcome {
                RunOutcome::Succeeded | RunOutcome::Failed => "; the state was recorded as usual",
                _ => "; the state is unchanged",
            };
            self.warnings.push(format!(
                "this run couldn't be added to the run ledger, so `ods state savings` won't count it: {why}{state}"
            ));
        }
    }
}

impl RunReport {
    /// What reuse saved, for people: `~4m 12s of build time (estimate: 9 of 13 nodes
    /// reused)`. Only for a run that went ahead, and reused something.
    fn saved_line(&self) -> Option<Line> {
        let saved = &self.savings;
        if saved.reused == 0 || matches!(self.outcome, RunOutcome::Compiled | RunOutcome::DryRun) {
            return None;
        }
        let of = format!(
            "{} of {} nodes reused",
            saved.reused,
            saved.reused + saved.built
        );
        let untimed = match saved.untimed {
            0 => String::new(),
            n if n == saved.reused => String::new(),
            n => format!("; {n} without a build time, not counted, so at least this"),
        };
        Some(if saved.timed == 0 {
            vec![Span::toned(
                format!("unknown: no reused node has a build time yet ({of})"),
                Tone::Muted,
            )]
        } else {
            vec![
                Span::toned(
                    format!(
                        "~{} of build time",
                        super::run_stats::duration(saved.avoided_ms)
                    ),
                    Tone::Success,
                ),
                Span::toned(format!(" (estimate, serial: {of}{untimed})"), Tone::Muted),
            ]
        })
    }

    fn summary(&self) -> ViewNode {
        let mut summary = vec![
            (
                "scope".into(),
                vec![Span::toned(self.scope.as_str(), Tone::Code)],
            ),
            (
                "compared with".into(),
                vec![Span::plain(self.based_on.map_or_else(
                    || "no recorded state: everything is built".to_owned(),
                    |id| format!("snapshot {id} in {}", self.state_db.display()),
                ))],
            ),
            (
                "plan".into(),
                vec![Span::plain(format!(
                    "{} to build{}, {} to reuse{}",
                    self.build,
                    if self.tests { " and test" } else { "" },
                    self.reuse,
                    if self.left_out.is_empty() {
                        String::new()
                    } else {
                        format!(", {} left out", self.left_out.len())
                    }
                ))],
            ),
        ];
        if let Some(saved) = self.saved_line() {
            summary.push(("saved".into(), saved));
        }
        if let Some(retry) = &self.retry {
            summary.push((
                "retrying".into(),
                vec![
                    Span::plain("what failed in "),
                    Span::toned(retry.of.as_str(), Tone::Code),
                    Span::plain(format!(
                        ": {} retried, {} reused, {} held back",
                        retry.retried.len(),
                        retry.reused.len(),
                        retry.held_back.len()
                    )),
                ],
            ));
        }
        summary.push(("dbt".into(), vec![Span::plain(settings_line(&self.dbt))]));
        summary.push((
            "target".into(),
            vec![Span::plain(
                self.target
                    .as_ref()
                    .map_or_else(|| "unknown".to_owned(), ToString::to_string),
            )],
        ));
        if let Some(command) = self.execution.as_ref().and_then(|e| e.command.as_deref()) {
            summary.push(("ran".into(), vec![Span::toned(command, Tone::Code)]));
        }
        summary.push((
            "outcome".into(),
            vec![match self.outcome {
                RunOutcome::Compiled => Span::plain("compiled: nothing built or recorded"),
                RunOutcome::DryRun => Span::plain("dry run: nothing built or recorded"),
                RunOutcome::NothingToBuild => {
                    Span::toned("nothing to build: everything is reused", Tone::Success)
                }
                RunOutcome::Succeeded => Span::toned("succeeded", Tone::Success),
                RunOutcome::Failed => Span::toned("failed", Tone::Error),
                RunOutcome::NotRecorded => {
                    Span::toned("dbt ran, but nothing was recorded", Tone::Error)
                }
            }],
        ));
        if let Some(run) = &self.observed.run_stats {
            summary.extend(super::run_stats::totals(run));
        }
        if let Some(journal) = &self.observed.journal {
            summary.push((
                "journal".into(),
                vec![Span::toned(journal.display().to_string(), Tone::Code)],
            ));
        }
        if let Some(record) = &self.record {
            summary.push((
                "recorded".into(),
                vec![Span::plain(match record.snapshot {
                    Some(id) => format!(
                        "snapshot {id}: {} advanced",
                        count(record.recorded.advanced.len(), "node")
                    ),
                    None => "nothing: no node succeeded".to_owned(),
                })],
            ));
        }
        ViewNode::KeyValue(summary)
    }

    /// What ran and how it ended, or, if nothing ran, the plan.
    fn table(&self) -> ViewNode {
        match &self.execution {
            Some(execution) => ViewNode::Table {
                title: None,
                columns: vec![
                    "node".into(),
                    "result".into(),
                    "took".into(),
                    "rows".into(),
                    "why it ran".into(),
                ],
                rows: execution
                    .nodes
                    .iter()
                    .map(|n| {
                        let why = self
                            .plan
                            .entries
                            .iter()
                            .find(|e| e.node == n.node)
                            .and_then(|e| e.reasons.first())
                            .map_or("", |r| r.message.as_str());
                        let stats = self
                            .observed
                            .run_stats
                            .as_ref()
                            .and_then(|r| r.get(&n.node))
                            .map(|s| &s.stats);
                        let detail = stats.map(super::run_stats::detail).unwrap_or_default();
                        let result = match n.status {
                            ExecutionStatus::Success if !n.checks_failed.is_empty() => {
                                Span::toned("built, tests failed", Tone::Error)
                            }
                            ExecutionStatus::Success => Span::toned("success", Tone::Success),
                            ExecutionStatus::Skipped if detail.is_empty() => {
                                Span::toned("skipped", Tone::Warning)
                            }
                            ExecutionStatus::Skipped => {
                                Span::toned(format!("skipped: {detail}"), Tone::Warning)
                            }
                            // dbt's word, or the redacted summary of why it failed.
                            _ if detail.is_empty() => Span::toned(
                                n.message.clone().unwrap_or_else(|| "failed".to_owned()),
                                Tone::Error,
                            ),
                            _ => Span::toned(format!("failed: {detail}"), Tone::Error),
                        };
                        let missing = || super::run_stats::MISSING.to_owned();
                        vec![
                            vec![Span::toned(display_name(&n.node), Tone::Code)],
                            vec![result],
                            vec![Span::plain(
                                stats.map_or_else(missing, super::run_stats::took),
                            )],
                            vec![Span::plain(
                                stats.map_or_else(missing, super::run_stats::rows),
                            )],
                            vec![Span::plain(why)],
                        ]
                    })
                    .collect(),
            },
            None => ViewNode::Table {
                title: None,
                columns: vec!["node".into(), "action".into(), "why".into()],
                rows: self
                    .plan
                    .entries
                    .iter()
                    .map(|e| {
                        vec![
                            vec![Span::toned(e.name.as_str(), Tone::Code)],
                            vec![match e.action {
                                PlanAction::Build
                                    if self.retry.is_some()
                                        && !self.requested.contains(&e.node) =>
                                {
                                    Span::toned("not retried", Tone::Muted)
                                }
                                PlanAction::Build => Span::toned("build", Tone::Warning),
                                _ => Span::toned("reuse", Tone::Success),
                            }],
                            vec![Span::plain(
                                e.reasons
                                    .iter()
                                    .map(|r| r.message.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )],
                        ]
                    })
                    .collect(),
            },
        }
    }

    /// What a retry of failures (#292) reuses, holds back, and leaves unbuilt.
    fn retry_notices(retry: &RetryReport) -> Vec<ViewNode> {
        let listed = |nodes: &[NodeWhy]| {
            nodes
                .iter()
                .map(|n| format!("{} ({})", display_name(&n.node), n.reason))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut blocks = Vec::new();
        if !retry.reused.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "failed last time, reused now: {}",
                    listed(&retry.reused)
                ))],
            });
        }
        if !retry.held_back.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(format!(
                    "not retried, still to build: {}",
                    listed(&retry.held_back)
                ))],
            });
        }
        if !retry.changed_since.is_empty() || !retry.source_tests_changed_since.is_empty() {
            let mut names = listed(&retry.changed_since);
            if !retry.source_tests_changed_since.is_empty() {
                if !names.is_empty() {
                    names.push_str(", ");
                }
                let _ = write!(
                    names,
                    "the tests of {}",
                    retry
                        .source_tests_changed_since
                        .iter()
                        .map(|s| display_name(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "changed since, not retried: {names}; `ods state retry` without --failed builds them"
                ))],
            });
        }
        if !retry.not_planned.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "failed last time, not in the plan now: {}",
                    retry
                        .not_planned
                        .iter()
                        .map(|n| display_name(n))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))],
            });
        }
        blocks
    }

    fn notices(&self) -> Vec<ViewNode> {
        let mut blocks = self
            .retry
            .as_ref()
            .map_or_else(Vec::new, Self::retry_notices);
        if !self.left_out.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "left out, still to build: {}",
                    self.left_out
                        .iter()
                        .map(|l| format!("{} ({})", display_name(&l.node), l.reason))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))],
            });
        }
        if let Some(execution) = &self.execution
            && !execution.checks_failed.is_empty()
        {
            let mut message = vec![Span::plain("failed checks: ")];
            message.extend(super::failures::checks_line(
                &execution.checks_failed,
                &self.observed.check_names,
            ));
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message,
            });
        }
        if let Some(record) = &self.record {
            if !record.source_tests.failed.is_empty() {
                blocks.push(ViewNode::Notice {
                    level: Level::Warning,
                    message: vec![Span::plain(format!(
                        "source tests failed on {}: as with `dbt build`, the nodes reading {} weren't built, and the tests run again next time",
                        record
                            .source_tests
                            .failed
                            .iter()
                            .map(|s| display_name(s))
                            .collect::<Vec<_>>()
                            .join(", "),
                        if record.source_tests.failed.len() == 1 {
                            "it"
                        } else {
                            "them"
                        }
                    ))],
                });
            }
            if !record.recorded.kept.is_empty() {
                blocks.push(ViewNode::Notice {
                    level: Level::Info,
                    message: vec![Span::plain(format!(
                        "{} kept their last successful state and will be built next run",
                        count(record.recorded.kept.len(), "node")
                    ))],
                });
            }
            if self.has_sources && !record.sources_recorded {
                blocks.push(ViewNode::Notice {
                    level: Level::Info,
                    message: vec![Span::plain(
                        "source versions weren't recorded (none measured before the run), so nodes reading sources will be built next time",
                    )],
                });
            }
        }
        blocks.extend(trusted_reuse_notice(self.reuse, self.relations_checked));
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        blocks
    }
}

/// Says that reuse was taken on trust, if it was: with relations checked, what is
/// reused was found in the warehouse, and its evidence says so, so there is nothing to
/// add.
fn trusted_reuse_notice(reuse: usize, relations_checked: bool) -> Option<ViewNode> {
    (reuse > 0 && !relations_checked).then(|| ViewNode::Notice {
        level: Level::Info,
        message: vec![Span::plain(
            "reuse assumes each relation built earlier still exists: this run didn't check the warehouse",
        )],
    })
}

impl Present for RunReport {
    const COMMAND: &'static str = "state.run";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("State run".into()),
            self.summary(),
            self.table(),
        ];
        if !self.source_tests.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("source tests".into()),
                columns: vec!["source".into(), "tests".into(), "why".into()],
                rows: source_rows(
                    &self.source_tests,
                    self.execution.as_ref(),
                    &self.observed.check_names,
                ),
            });
        }
        blocks.extend(super::failures::section(&self.observed.failures));
        blocks.extend(self.notices());
        ViewNode::Group(blocks)
    }
}

/// A command that failed before any node ran, with dbt's error explained (#323).
/// `TEST`: it was `ods state test`, whose JSON command is `state.test`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct FailedBeforeRunning<const TEST: bool> {
    /// Always `failed_before_running`.
    outcome: &'static str,
    /// dbt's error, explained.
    failures: Vec<ods_core::failure::ErrorExplanation>,
}

impl<const TEST: bool> Present for FailedBeforeRunning<TEST> {
    const COMMAND: &'static str = if TEST { "state.test" } else { "state.run" };

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading(if TEST { "State test" } else { "State run" }.into()),
            ViewNode::KeyValue(vec![(
                "outcome".into(),
                vec![Span::toned(
                    "failed before any node ran: nothing was built or recorded",
                    Tone::Error,
                )],
            )]),
        ];
        blocks.extend(super::failures::section(&self.failures));
        ViewNode::Group(blocks)
    }
}

/// Returns `error`, first showing an explanation of dbt's own error in it when it has
/// one, only for an error from before anything ran (e.g. `dbt compile` failed in the
/// prepare step): an error once dbt started building is never shown as "nothing was
/// built". `TEST`: the command was `ods state test`.
pub(super) fn failed_before_running<const TEST: bool>(
    ctx: &mut Context<'_>,
    settings: &StateSettings,
    started: std::time::SystemTime,
    error: CliError,
) -> Result<(), CliError> {
    if !error.is_before_running() {
        return Err(error);
    }
    let project_dir = super::failures::project_dir(settings);
    let target_dir = settings.target_dir();
    // Only a manifest this command wrote describes the code that failed.
    let manifest_path = target_dir.join("manifest.json");
    let fresh = std::fs::metadata(&manifest_path)
        .and_then(|m| m.modified())
        .is_ok_and(|at| at >= started);
    let read = || ods_provider_dbt::Manifest::read(&manifest_path).ok();
    let manifest = fresh.then(read).flatten();
    // An older one names the project's nodes, for did-you-mean only.
    let last_manifest = if fresh { None } else { read() };
    let evidence = super::failures::Evidence {
        files: super::failures::ProjectFiles {
            project_dir: &project_dir,
            target_dir: &target_dir,
            manifest: manifest.as_ref(),
            last_manifest: last_manifest.as_ref(),
        },
        plan: None,
        before: None,
        state_db: Path::new(""),
        retry: None,
        state_db_flag: None,
        project_is_run: true,
        doctor: Some(super::failures::Doctor {
            config: Some(ctx.config),
            settings,
        }),
    };
    match super::failures::explain_prepare(&error.message, &evidence) {
        Some(explanation) => ctx.emit_failed(
            &FailedBeforeRunning::<TEST> {
                outcome: "failed_before_running",
                failures: vec![explanation],
            },
            error,
        ),
        None => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_provider_fake::{FakeClock, FakeExecutor};
    use ods_sdk::contracts::relations::RelationReport;
    use ods_sdk::{Provider, ProviderInfo};

    fn requested(ids: &[&str]) -> Vec<RequestedNode> {
        ids.iter().map(|id| RequestedNode::new(*id, *id)).collect()
    }

    /// An inspector that can't check relations at all.
    struct Blind;

    impl Provider for Blind {
        fn info(&self) -> ProviderInfo {
            ProviderInfo::new("blind", "blind", "0", ods_core::CapabilitySet::new())
        }
    }

    #[async_trait::async_trait]
    impl RelationInspector for Blind {
        async fn inspect(&self, _: &[RequestedNode]) -> Result<RelationReport, ProviderError> {
            panic!("never asked: it can't check");
        }
    }

    #[test]
    fn relation_facts_maps_each_answer() {
        let executor = FakeExecutor::new(FakeClock::default(), ["a", "b"]);
        executor.drop_relation("b");
        let mut warnings = Vec::new();
        let facts = relation_facts(
            &executor,
            &requested(&["a", "b", "c"]),
            CheckFor::Export,
            &mut warnings,
        )
        .unwrap()
        .unwrap();
        assert_eq!(facts["a"], RelationFact::Present(Some("table".into())));
        assert_eq!(facts["b"], RelationFact::Missing);
        assert_eq!(facts["c"], RelationFact::Unverified("unknown node".into()));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_failed_check_leaves_every_node_unverified() {
        let executor = FakeExecutor::new(FakeClock::default(), ["a"]).failing_inspection();
        let mut warnings = Vec::new();
        let facts = relation_facts(
            &executor,
            &requested(&["a"]),
            CheckFor::Export,
            &mut warnings,
        )
        .unwrap()
        .unwrap();
        assert!(
            matches!(facts["a"], RelationFact::Unverified(_)),
            "{facts:?}"
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("point upstream"), "{warnings:?}");
        // Asking about nothing asks nothing: no failure to report.
        let mut warnings = Vec::new();
        let facts = relation_facts(&executor, &[], CheckFor::Reuse, &mut warnings).unwrap();
        assert_eq!(facts, Some(BTreeMap::new()));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn without_the_capability_nothing_is_checked() {
        let mut warnings = Vec::new();
        let facts =
            relation_facts(&Blind, &requested(&["a"]), CheckFor::Reuse, &mut warnings).unwrap();
        assert_eq!(facts, None);
        assert!(warnings[0].contains("reuse trusts"), "{warnings:?}");
    }

    mod versions_read_before {
        use ods_core::state::{DataVersion, Exactness, Timestamp};
        use ods_core::{Capability, CapabilitySet};
        use ods_state::{Project, Source, VersionAnswer, VersionReading};

        use super::super::keep_versions_read_before;

        const ID: &str = "source.p.raw.orders";

        fn reading(capability: Capability, at: i64, value: &str, origin: &str) -> VersionReading {
            VersionReading::new(
                CapabilitySet::from([capability]),
                Some(Timestamp::from_unix(at)),
                [(
                    ID.to_owned(),
                    VersionAnswer::Version(DataVersion::new(value, Exactness::Exact, origin)),
                )]
                .into(),
            )
        }

        fn project(readings: &[VersionReading]) -> Project {
            let mut sources = vec![Source::new(ID, "raw.orders", None)];
            ods_state::choose_source_versions(&mut sources, readings);
            Project::new(Vec::new(), sources)
        }

        fn table(at: i64) -> VersionReading {
            reading(Capability::RelationVersions, at, "t/1", "delta_history")
        }

        #[test]
        fn only_versions_read_strictly_before_the_start_are_kept() {
            // Read the second before: kept.
            let mut before = project(&[table(99)]);
            assert!(keep_versions_read_before(
                &mut before,
                Some(Timestamp::from_unix(100))
            ));
            assert!(before.sources[0].version.is_some());
            // The same second as the start: timestamps are to the second, so it may
            // have been read after the run started. Dropped.
            let mut same = project(&[table(100)]);
            assert!(!keep_versions_read_before(
                &mut same,
                Some(Timestamp::from_unix(100))
            ));
            assert_eq!(same.sources[0].version, None);
            assert_eq!(same.sources[0].observed_at, None);
            // After the start: dropped.
            let mut after = project(&[table(101)]);
            assert!(!keep_versions_read_before(
                &mut after,
                Some(Timestamp::from_unix(100))
            ));
            assert_eq!(after.sources[0].version, None);
            // No start time: nothing can be shown to predate the run.
            let mut unknown = project(&[table(99)]);
            assert!(!keep_versions_read_before(&mut unknown, None));
            assert_eq!(unknown.sources[0].version, None);
        }

        #[test]
        fn a_late_table_version_is_dropped_without_falling_back_to_max_loaded_at() {
            // The planner chose the table version, read too late; `max_loaded_at` was
            // read in time. The version the plan used is the one recorded or none: it
            // isn't swapped for another that the plan didn't rest on. So nothing is
            // recorded, and the readers build once more next run (conservative).
            let freshness = reading(
                Capability::SourceFreshness,
                50,
                "2026-01-01T00:00:00Z",
                "max_loaded_at",
            );
            let mut late = project(&[freshness, table(100)]);
            assert!(!keep_versions_read_before(
                &mut late,
                Some(Timestamp::from_unix(100))
            ));
            assert_eq!(late.sources[0].version, None);
        }
    }

    #[test]
    fn only_reuse_on_trust_gets_a_notice() {
        assert!(trusted_reuse_notice(0, false).is_none());
        assert!(trusted_reuse_notice(3, true).is_none());
        let Some(ViewNode::Notice { level, message }) = trusted_reuse_notice(3, false) else {
            panic!("reuse without a check has a notice");
        };
        assert_eq!(level, Level::Info);
        assert!(
            format!("{message:?}").contains("didn't check the warehouse"),
            "{message:?}"
        );
    }
}
