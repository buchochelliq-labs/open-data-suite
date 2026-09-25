//! `ods state run` (#23, #24, ADR-0014): plan, build exactly the BUILD set through an
//! [`Executor`], and record what succeeded.
//!
//! 1. Prepare: compile, so fingerprints describe the code that would run, and measure
//!    sources (`--no-compile`, `--no-source-freshness` skip these).
//! 2. Plan against the latest state, as `ods state plan` does.
//! 3. Execute the BUILD set, and nothing else, unless `--dry-run` or there is nothing
//!    to build.
//! 4. Record: re-read the artifacts the run wrote and commit a snapshot in which only
//!    the nodes that succeeded advance. Failed and skipped nodes keep their last
//!    successful state (AGENTS.md rule 5); if nothing succeeded, nothing is committed.
//!    The commit is a compare-and-swap on the state read in step 2, so a concurrent
//!    run can't be overwritten.

use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::{ExecutionPlan, PlanAction, SnapshotId, Timestamp};
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput};
use ods_sdk::ProviderError;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, PrepareReport,
    PrepareRequest, RequestedNode,
};
use ods_sdk::contracts::state_store::{StateStore, StoredSnapshot};
use ods_state::{Outcome, Recorded, RunResult};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::state_plan::{
    Sources, Workspace, block_on, common, display_name, plan_against, select_specs, store_error,
};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::Context;
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
            Arg::new("project-dir")
                .long("project-dir")
                .value_name("DIR")
                .help("dbt's --project-dir"),
        )
        .arg(
            Arg::new("profiles-dir")
                .long("profiles-dir")
                .value_name("DIR")
                .help("dbt's --profiles-dir"),
        )
        .arg(
            Arg::new("target")
                .long("target")
                .value_name("NAME")
                .help("dbt's --target (the profile output to use)"),
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

/// `ods state run`'s own arguments.
pub(super) fn run_command() -> Command {
    dbt_options(common(
        Command::new("run").about(
            "Build only what needs building (models, seeds, snapshots) with dbt, and record what succeeded",
        ),
    ))
    .arg(
        Arg::new("dry-run")
            .long("dry-run")
            .action(ArgAction::SetTrue)
            .help("Prepare and plan, but build and record nothing"),
    )
    .arg(
        Arg::new("test")
            .long("test")
            .action(ArgAction::SetTrue)
            .help("Also run the built nodes' tests, like `dbt build`; a node whose tests fail keeps its last state"),
    )
    .arg(
        Arg::new("full-refresh")
            .long("full-refresh")
            .action(ArgAction::SetTrue)
            .help("dbt's --full-refresh for the nodes being built"),
    )
    .arg(
        Arg::new("resource-type")
            .long("resource-type")
            .value_name("TYPE")
            .value_parser(["model", "seed", "snapshot"])
            .action(ArgAction::Append)
            .help("Only build nodes of this type; the others stay to build; repeatable"),
    )
    .arg(
        Arg::new("no-source-freshness")
            .long("no-source-freshness")
            .action(ArgAction::SetTrue)
            .help("Don't run `dbt source freshness` first; use --sources or an existing sources.json"),
    )
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
    let types: Vec<&String> = args
        .try_get_many::<String>("resource-type")
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .collect();
    let mut requested = Vec::new();
    let mut left_out = Vec::new();
    for (id, name, kind) in entries {
        if excluded.contains(&id) {
            left_out.push(LeftOut {
                node: id,
                reason: "excluded with --exclude".to_owned(),
            });
        } else if !types.is_empty() && !types.iter().any(|t| **t == kind) {
            left_out.push(LeftOut {
                node: id,
                reason: format!(
                    "a {kind}, and only {} were asked for",
                    types
                        .iter()
                        .map(|t| format!("{t}s"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        } else {
            requested.push(RequestedNode::new(id, name));
        }
    }
    Ok((requested, left_out))
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
    executor
}

/// Whether this run tests what it builds (`--test`).
fn execution_tested(args: &ArgMatches) -> bool {
    args.get_flag("test")
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
    sources: Sources,
    execution: &ExecutionReport,
    latest: Option<&StoredSnapshot>,
    store: &SqliteStateStore,
) -> Result<RunRecord, CliError> {
    let built = Workspace::load(args, sources)?;
    if built.invocation_id.as_deref() != Some(execution.run_id.as_str()) {
        return Err(CliError::new(
            ExitStatus::Failure,
            codes::STATE_INPUT,
            "the manifest in the target directory isn't from this run, so ODS can't tell which code was built; nothing was recorded",
        )
        .with_hint("don't run other dbt commands against the same target directory during `ods state run`"));
    }
    let tested = execution_tested(args);
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
            if tested && n.status == ExecutionStatus::Success {
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
    #[serde(skip)]
    has_sources: bool,
}

impl RunReport {
    /// Runs the whole flow and emits the report.
    pub(super) fn run(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let (report, record_error) = Self::build(args)?;
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
    fn build(args: &ArgMatches) -> Result<(Self, Option<CliError>), CliError> {
        let target_dir = PathBuf::from(
            args.get_one::<String>("target-dir")
                .map_or("target", String::as_str),
        );
        let executor = executor(args, &target_dir);
        let mut warnings = Vec::new();

        // 1. Prepare.
        let (prepared, sources) = prepare(args, &executor, &mut warnings)?;

        // 2. Plan.
        let ws = Workspace::load(args, sources)?;
        let dry_run = args.get_flag("dry-run");
        let store = if dry_run && !ws.state_db.is_file() {
            None
        } else {
            Some(ws.open_store()?)
        };
        let latest = match &store {
            Some(store) => ws.latest(store)?,
            None => None,
        };
        let (plan, plan_warnings) =
            plan_against(&ws, latest.as_ref(), &select_specs(args), Timestamp::now())?;
        warnings.extend(plan_warnings);
        let (requested, left_out) = narrow(
            args,
            &ws.project,
            plan.with_action(PlanAction::Build)
                .map(|e| (e.node.clone(), e.name.clone(), e.kind.clone()))
                .collect(),
        )?;
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.as_ref().map(|s| s.id),
            outcome: RunOutcome::DryRun,
            build: requested.len(),
            reuse: plan.with_action(PlanAction::Reuse).count(),
            tests: execution_tested(args),
            left_out,
            prepared,
            plan,
            execution: None,
            record: None,
            warnings,
            has_sources: !ws.project.sources.is_empty(),
        };
        if dry_run {
            return Ok((report, None));
        }
        let (Some(store), false) = (store, requested.is_empty()) else {
            report.outcome = RunOutcome::NothingToBuild;
            return Ok((report, None));
        };

        // 3. Execute.
        let mode = if execution_tested(args) {
            ExecutionMode::Build
        } else {
            ExecutionMode::Run
        };
        let request = ExecutionRequest::new(requested, mode)
            .with_full_refresh(args.get_flag("full-refresh"))
            .with_engine_args(dbt_args(args));
        let execution = block_on(executor.execute(&request))?.map_err(|e| {
            execution_error(&e)
                .with_hint("nothing was recorded; the last successful state is unchanged")
        })?;
        if !execution.unrequested.is_empty() {
            report.warnings.push(format!(
                "dbt also built {}, which weren't requested (the project changed after it was compiled?); they weren't recorded and will be built next run",
                execution.unrequested.iter().map(|n| display_name(n)).collect::<Vec<_>>().join(", ")
            ));
        }

        // 4. Record.
        let recorded = record(args, sources, &execution, latest.as_ref(), &store);
        report.execution = Some(execution);
        Ok(match recorded {
            Ok(record) => {
                report.outcome = if report.execution.as_ref().is_some_and(|e| e.succeeded) {
                    RunOutcome::Succeeded
                } else {
                    RunOutcome::Failed
                };
                report.record = Some(record);
                (report, None)
            }
            Err(error) => {
                report.outcome = RunOutcome::NotRecorded;
                (report, Some(error))
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
        if let Some(command) = self.execution.as_ref().and_then(|e| e.command.as_deref()) {
            summary.push(("ran".into(), vec![Span::toned(command, Tone::Code)]));
        }
        summary.push((
            "outcome".into(),
            vec![match self.outcome {
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
