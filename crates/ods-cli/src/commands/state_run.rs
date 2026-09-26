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
use ods_core::state::{ExecutionPlan, PlanAction, ReasonCode, SnapshotId, Timestamp};
use ods_core::{Capability, Strategy, choose};
use ods_provider_dbt::RunResults;
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput, DbtStep};
use ods_sdk::ProviderError;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, PrepareReport,
    PrepareRequest, RequestedNode,
};
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence};
use ods_sdk::contracts::state_store::{StateStore, StoredSnapshot};
use ods_state::{Outcome, Recorded, RelationFact, RunResult};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::state_plan::{
    Sources, Workspace, block_on, common, display_name, plan_against, select_specs, store_error,
};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, ProgressSettings};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// Options shared by the commands that run dbt (`run`, `test`).
pub(super) fn dbt_options(command: Command) -> Command {
    command
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
        )
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
            Arg::new("target")
                .long("target")
                .value_name("NAME")
                .env("DBT_TARGET")
                .hide_env_values(true)
                .help("dbt's --target (the profile output to use)"),
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
        .arg(
            Arg::new("dbt-args")
                .value_name("DBT_ARGS")
                .num_args(0..)
                .last(true)
                .help("After `--`: options passed to dbt as they are, e.g. `-- --threads 8`. Selection and artifact options are refused"),
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

/// An `ods state` command that builds, with its own arguments.
pub(super) fn build_command(kind: Kind) -> Command {
    let mut command = dbt_options(common(Command::new(kind.name()).about(kind.about()))).arg(
        Arg::new("no-source-freshness")
            .long("no-source-freshness")
            .action(ArgAction::SetTrue)
            .help(
                "Don't run `dbt source freshness` first; use --sources or an existing sources.json",
            ),
    );
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
    /// `flag`, the `DBT_*` variable it was read from, or `default`.
    source: String,
}

/// The dbt settings ODS passes to dbt, and where each came from: ODS's option, the
/// dbt variable it reads as that option's default, or a default.
pub(super) fn dbt_settings(args: &ArgMatches, target_dir: &Path) -> Vec<DbtSetting> {
    let source = |id: &str, env: &str| match args.value_source(id) {
        Some(clap::parser::ValueSource::EnvVariable) => env.to_owned(),
        Some(clap::parser::ValueSource::CommandLine) => "flag".to_owned(),
        _ => "default".to_owned(),
    };
    let mut settings = Vec::new();
    for (name, id, env) in [
        ("target", "target", "DBT_TARGET"),
        ("profile", "dbt-profile", "DBT_PROFILE"),
        ("profiles_dir", "profiles-dir", "DBT_PROFILES_DIR"),
        ("project_dir", "project-dir", "DBT_PROJECT_DIR"),
        ("vars", "vars", ""),
    ] {
        if let Some(value) = args.try_get_one::<String>(id).ok().flatten() {
            settings.push(DbtSetting {
                name,
                value: value.clone(),
                source: source(id, env),
            });
        }
    }
    settings.push(DbtSetting {
        name: "target_dir",
        value: target_dir.display().to_string(),
        source: match args.value_source("target-dir") {
            Some(_) => source("target-dir", "DBT_TARGET_PATH"),
            None if args.value_source("project-dir").is_some() => "project_dir".to_owned(),
            None => "default".to_owned(),
        },
    });
    if full_refresh(args) {
        settings.push(DbtSetting {
            name: "full_refresh",
            value: "true".to_owned(),
            source: source("full-refresh", "DBT_FULL_REFRESH"),
        });
    }
    settings
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
    executor: &DbtExecutor,
    target_dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<DbtSetting>, CliError> {
    let settings = dbt_settings(args, target_dir);
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
        .chain(vars_mismatch(args, target_dir))
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

pub(super) fn executor(args: &ArgMatches, target_dir: &PathBuf) -> DbtExecutor {
    let mut executor = DbtExecutor::new(
        args.get_one::<String>("dbt").map_or("dbt", String::as_str),
        target_dir,
    )
    .output(
        if args.get_one::<String>("dbt-output").map(String::as_str) == Some("capture") {
            DbtOutput::Capture
        } else {
            DbtOutput::Stderr
        },
    );
    if let Some(dir) = args.get_one::<String>("project-dir") {
        executor = executor.project_dir(dir);
    }
    if let Some(dir) = args.get_one::<String>("profiles-dir") {
        executor = executor.profiles_dir(dir);
    }
    if let Some(target) = args.get_one::<String>("target") {
        executor = executor.target(target);
    }
    if let Some(profile) = args.get_one::<String>("dbt-profile") {
        executor = executor.profile(profile);
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
            } => format!(
                "dbt {command}: {}{}",
                count(nodes, "node"),
                match (command, tests) {
                    (_, true) => " and their tests",
                    ("build", false) => ", without tests",
                    _ => "",
                }
            ),
            DbtStep::Test { nodes } => format!("dbt test: the tests of {}", count(nodes, "node")),
            DbtStep::RelationCheck { nodes } => format!(
                "dbt show: are the tables of {} ODS would reuse still there?",
                count(nodes, "node")
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

/// Names listed per reason before the rest are counted.
const NAMES_PER_REASON: usize = 8;

/// The plan: a summary line, then what builds grouped by its main reason. Reused nodes
/// are only counted: in a large project they are most of it (`-vv` names them).
fn plan_notes(report: &RunReport) -> Vec<String> {
    let mut summary = format!("plan: {} to build, {} to reuse", report.build, report.reuse);
    if !report.left_out.is_empty() {
        let _ = write!(summary, ", {} left out", report.left_out.len());
    }
    let left_out: BTreeSet<&str> = report.left_out.iter().map(|l| l.node.as_str()).collect();
    // In plan order, so groups and names read upstream first.
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for entry in report.plan.with_action(PlanAction::Build) {
        if left_out.contains(entry.node.as_str()) {
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
        ReasonCode::RelationMissing => "not in the warehouse",
        ReasonCode::RelationUnverified => "couldn't check the warehouse",
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
    differs.then(|| {
        format!(
            "the artifacts in {} were compiled with vars {}, but this run has {}: the plan may not match what dbt builds. Drop --no-compile, or pass the same --vars",
            target_dir.display(),
            compiled.map_or_else(|| "none".to_owned(), |v| v.to_string()),
            given.map_or_else(|| "none".to_owned(), |v| format!("`{v}`")),
        )
    })
}

/// How to plan: a full refresh forces what it rebuilds.
fn plan_options(args: &ArgMatches) -> ods_state::PlanOptions {
    let options = ods_state::PlanOptions::default();
    if full_refresh(args) {
        options.full_refresh()
    } else {
        options
    }
}

/// How many dbt commands a run will make if it builds.
fn step_count(args: &ArgMatches, builds: bool) -> usize {
    let compiles = !args.get_flag("no-compile");
    let measures = compiles && !args.get_flag("no-source-freshness");
    // Whether there is state, and so reuse to check: a guess made before anything
    // runs, for the count only.
    let checks = args
        .get_one::<String>("state-db")
        .is_some_and(|db| Path::new(db).is_file());
    [measures, compiles, checks, builds]
        .into_iter()
        .map(usize::from)
        .sum()
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
    let strategies = [
        Strategy::new("warehouse_check", [Capability::RelationExistence], true),
        Strategy::fallback("trust_recorded_build", false),
    ];
    let choice = choose(&inspector.info().capabilities, &strategies)
        .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))?;
    if !choice.chosen.value {
        // Reuse then says in its evidence that nothing checked the relation.
        warnings.push(
            "the executor can't check that reused tables are still in the warehouse: reuse trusts the last recorded build"
                .to_owned(),
        );
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
    let mut facts: BTreeMap<String, RelationFact> = BTreeMap::new();
    match block_on(inspector.inspect(&requested))? {
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
        Err(e) => {
            warnings.push(format!(
                "couldn't check that the tables of the nodes ODS would reuse are still in the warehouse, so they are built: {e}"
            ));
        }
    }
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

/// Each node's checks digest.
fn checks_by_node(project: &ods_state::Project) -> BTreeMap<String, Option<String>> {
    project
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.checks.clone()))
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
fn prepare(
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
        execution_error(&e).with_hint(
            "fix the project so `dbt compile` succeeds, or plan from existing artifacts with --no-compile",
        )
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
    args: &ArgMatches,
    tested: bool,
    sources: Sources,
    execution: &ExecutionReport,
    latest: Option<&StoredSnapshot>,
    store: &SqliteStateStore,
    planned_checks: &BTreeMap<String, Option<String>>,
) -> Result<RunRecord, CliError> {
    let mut built = Workspace::load(args, sources)?;
    // Each node's checks as planned. The manifest dbt writes while building keeps the
    // compiled SQL only of the tests that invocation ran, so digests computed from it
    // depend on what ran; the plan's (from `dbt compile`, just before this build, so
    // the same code) match what the next plan computes (#229).
    for node in &mut built.project.nodes {
        if let Some(checks) = planned_checks.get(&node.id) {
            node.checks.clone_from(checks);
        }
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
            );
            // Built with its tests, and they passed.
            if tested && n.fully_checked() {
                result.tested()
            } else {
                result
            }
        })
        .collect();
    let sources_recorded = matches!(
        (built.sources_taken_at, execution.started_at),
        // Strictly before: timestamps are to the second.
        (Some(taken), Some(started)) if taken < started
    );
    let recorded = ods_state::record(
        &built.project,
        latest.map(|s| (s.id, &s.snapshot)),
        &results,
        &execution.run_id,
        execution.finished_at,
        sources_recorded,
    );
    tracing::info!(
        run = %execution.run_id,
        advanced = recorded.advanced.len(),
        kept = recorded.kept.len(),
        "recording the run"
    );
    for (node, why) in &recorded.kept {
        tracing::debug!(node = %node, why = %why, "kept its last state");
    }
    let snapshot = if recorded.advanced.is_empty() {
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
    })
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
    /// Whether the built nodes' tests ran too (`--test`).
    tests: bool,
    /// Nodes to build that this run left out (`--exclude`, `--resource-type`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    left_out: Vec<LeftOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared: Option<PrepareReport>,
    plan: ExecutionPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<RunRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    /// The dbt settings in effect, and where each came from.
    dbt: Vec<DbtSetting>,
    #[serde(skip)]
    has_sources: bool,
    /// Each node's checks digest when planned, for recording.
    #[serde(skip)]
    planned_checks: BTreeMap<String, Option<String>>,
}

impl RunReport {
    /// Runs the whole flow and emits the report.
    pub(super) fn run(
        kind: Kind,
        args: &ArgMatches,
        ctx: &mut Context<'_>,
    ) -> Result<(), CliError> {
        let (report, record_error) = Self::build(kind, args, ctx.progress)?;
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

    /// Runs the flow. A failure to record after dbt ran comes back with the report, so
    /// the caller still sees what dbt did.
    fn build(
        kind: Kind,
        args: &ArgMatches,
        progress: ProgressSettings,
    ) -> Result<(Self, Option<CliError>), CliError> {
        let target_dir = super::state_plan::target_dir(args);
        let builds = kind != Kind::Compile;
        let steps = Steps::new(progress, step_count(args, builds));
        let executor = steps.attach(executor(args, &target_dir));
        let mut warnings = Vec::new();
        let dbt = check_settings(args, &executor, &target_dir, &mut warnings)?;

        // 1. Prepare.
        let (prepared, sources) = prepare(args, &executor, &mut warnings)?;

        // 2. Plan.
        let mut ws = Workspace::load(args, sources)?;
        let dry_run = !builds || args.get_flag("dry-run");
        let tested = execution_tested(kind, args);
        let store = if dry_run && !ws.state_db.is_file() {
            None
        } else {
            Some(ws.open_store()?)
        };
        let latest = match &store {
            Some(store) => ws.latest(store)?,
            None => None,
        };
        // One clock for the check and the plan, so they agree on what is due.
        let now = Timestamp::now();
        let options = check_relations(
            &mut ws.project,
            latest.as_ref(),
            &executor,
            now,
            plan_options(args),
            &mut warnings,
        )?;
        let (plan, plan_warnings) =
            plan_against(&ws, latest.as_ref(), &select_specs(args), now, options)?;
        warnings.extend(plan_warnings);
        let types = if builds {
            build_types(kind, args)
        } else {
            Vec::new()
        };
        let (requested, left_out) = narrow(
            args,
            &ws.project,
            plan.with_action(PlanAction::Build)
                .map(|e| (e.node.clone(), e.name.clone(), e.kind.clone()))
                .collect(),
            &types,
            &format!("ods state {}", kind.name()),
        )?;
        if builds {
            warnings.extend(unbuilt_parents(
                &ws.project,
                &plan,
                &requested,
                &left_out,
                kind,
            ));
        }
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.as_ref().map(|s| s.id),
            outcome: RunOutcome::DryRun,
            build: requested.len(),
            reuse: plan.with_action(PlanAction::Reuse).count(),
            tests: tested,
            left_out,
            prepared,
            plan,
            execution: None,
            record: None,
            warnings,
            dbt,
            has_sources: !ws.project.sources.is_empty(),
            planned_checks: checks_by_node(&ws.project),
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
        let (Some(store), false) = (store, requested.is_empty()) else {
            steps.note("nothing to build, so dbt doesn't run again");
            report.outcome = RunOutcome::NothingToBuild;
            return Ok((report, None));
        };

        report.execute(args, &executor, requested, sources, latest.as_ref(), &store)
    }

    /// Steps 3 and 4: build the requested nodes with dbt, then record the run. A
    /// failure to record comes back with the report, so the caller still sees what dbt
    /// did.
    fn execute(
        mut self,
        args: &ArgMatches,
        executor: &DbtExecutor,
        requested: Vec<RequestedNode>,
        sources: Sources,
        latest: Option<&StoredSnapshot>,
        store: &SqliteStateStore,
    ) -> Result<(Self, Option<CliError>), CliError> {
        // 3. Execute.
        let mode = if self.tests {
            ExecutionMode::Build
        } else {
            ExecutionMode::Run
        };
        let request = ExecutionRequest::new(requested, mode)
            .with_full_refresh(full_refresh(args))
            .with_engine_args(dbt_args(args));
        let execution = block_on(executor.execute(&request))?.map_err(|e| {
            execution_error(&e)
                .with_hint("nothing was recorded; the last successful state is unchanged")
        })?;
        if !execution.unrequested.is_empty() {
            self.warnings.push(format!(
                "dbt also built {}, which weren't requested (the project changed after it was compiled?); they weren't recorded and will be built next run",
                execution.unrequested.iter().map(|n| display_name(n)).collect::<Vec<_>>().join(", ")
            ));
        }

        // 4. Record.
        let recorded = record(
            args,
            self.tests,
            sources,
            &execution,
            latest,
            store,
            &self.planned_checks,
        );
        self.execution = Some(execution);
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
    }
}

impl RunReport {
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
        summary.push(("dbt".into(), vec![Span::plain(settings_line(&self.dbt))]));
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
                columns: vec!["node".into(), "result".into(), "why it ran".into()],
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
                        vec![
                            vec![Span::toned(display_name(&n.node), Tone::Code)],
                            vec![match n.status {
                                ExecutionStatus::Success if !n.checks_failed.is_empty() => {
                                    Span::toned("built, tests failed", Tone::Error)
                                }
                                ExecutionStatus::Success => Span::toned("success", Tone::Success),
                                ExecutionStatus::Skipped => Span::toned("skipped", Tone::Warning),
                                _ => Span::toned(
                                    n.message.clone().unwrap_or_else(|| "failed".to_owned()),
                                    Tone::Error,
                                ),
                            }],
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

    fn notices(&self) -> Vec<ViewNode> {
        let mut blocks = Vec::new();
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
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(format!(
                    "failed checks: {}",
                    execution
                        .checks_failed
                        .iter()
                        .map(|c| display_name(c))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))],
            });
        }
        if let Some(record) = &self.record {
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
        if self.reuse > 0 {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "reuse assumes each relation built earlier still exists; ODS doesn't check the warehouse yet",
                )],
            });
        }
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        blocks
    }
}

impl Present for RunReport {
    const COMMAND: &'static str = "state.run";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("State run".into()),
            self.summary(),
            self.table(),
        ];
        blocks.extend(self.notices());
        ViewNode::Group(blocks)
    }
}
