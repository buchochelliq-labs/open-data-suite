//! `ods state test` (#220): test what was built but not yet tested, without building.
//!
//! 1. Prepare: compile, so tests and selection match the code (`--no-compile` skips).
//! 2. Pick the nodes ODS has a build of whose tests haven't passed since that build
//!    (all of them with `--all`), within `--select` and minus `--exclude`.
//!    With them, the sources whose data is new or unknown since their tests last
//!    passed (#232).
//! 3. Run only their tests, selected exactly.
//! 4. Record: nodes whose tests passed are marked tested; nodes whose tests failed are
//!    marked untested, so they come up again. Builds are unchanged. A source's passing
//!    tests are recorded against its data version; failing ones come up again.

use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::SnapshotId;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, NodeExecution,
};
use ods_sdk::contracts::state_store::StateStore;
use ods_state::{RecordedSources, RecordedTests, SourceCheck, TestResult};
use serde::Serialize;

use super::state_plan::{Workspace, block_on, common, display_name, select_specs, store_error};
use super::state_run::{
    LeftOut, Steps, dbt_args, dbt_options, executor, narrow, plan_source_checks, source_requests,
    source_results, source_rows,
};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, ProgressSettings};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods state test`'s own arguments.
pub(super) fn test_command() -> Command {
    dbt_options(common(Command::new("test").about(
        "Run the tests of what was built but not yet tested, with dbt, and record the results",
    )))
    .arg(
        Arg::new("all").long("all").action(ArgAction::SetTrue).help(
            "Test every node ODS has a build of, and every source, not only the untested ones",
        ),
    )
    // Whether sources have new data decides whether their tests run (#232).
    .arg(super::state_run::no_source_freshness())
}

/// How the test run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestOutcome {
    /// Nothing untested.
    NothingToTest,
    /// Every test passed.
    Passed,
    /// Some tests failed.
    Failed,
    /// No test failed, but some nodes' tests didn't all run (dbt stopped early, or
    /// they were deselected): those nodes stay untested.
    Incomplete,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct TestReport {
    state_db: PathBuf,
    scope: String,
    /// The snapshot compared with; `None` when nothing was recorded yet and only
    /// sources were tested.
    based_on: Option<SnapshotId>,
    outcome: TestOutcome,
    /// Nodes whose tests were asked to run.
    requested: usize,
    /// Built nodes in the selection that have no tests: nothing can mark them tested.
    without_checks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    left_out: Vec<LeftOut>,
    /// Every selected source with tests, and whether they run and why (#232).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_tests: Vec<SourceCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<TestRecordReport>,
    /// The dbt settings in effect, and where each came from.
    dbt: Vec<super::state_run::DbtSetting>,
    /// The target dbt builds in.
    target: ods_core::state::TargetIdentity,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    /// Each node's stats and the run's journal (#322, ADR-0024).
    #[serde(flatten)]
    observed: super::state_run::RunObserved,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct TestRecordReport {
    snapshot: SnapshotId,
    #[serde(flatten)]
    recorded: RecordedTests,
    /// Which sources' tests passed or failed (#232).
    #[serde(skip_serializing_if = "RecordedSources::is_empty")]
    source_tests: RecordedSources,
}

/// Builds in another target aren't this target's to vouch for (#227).
fn same_target(
    recorded: Option<&ods_core::state::TargetIdentity>,
    target: &ods_core::state::TargetIdentity,
) -> Result<(), CliError> {
    if recorded == Some(target) {
        return Ok(());
    }
    Err(CliError::new(
        ExitStatus::Failure,
        codes::STATE_INPUT,
        match recorded {
            Some(recorded) => format!(
                "ODS's builds here were made in target {recorded}, not {target}: none of them are this target's to test"
            ),
            None => format!(
                "ODS's builds here don't say which target they were made in, so none of them can be tested as {target}'s"
            ),
        },
    )
    .with_hint("build in this target first, e.g. `ods state build`"))
}

impl TestReport {
    /// Runs the tests and emits the report.
    pub(super) fn run(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let settings = StateSettings::resolve(args, ctx.config)?;
        let remembered = super::state_retry::remember(&test_command(), args, &settings);
        let started = std::time::SystemTime::now();
        let report = match Self::build(args, &settings, ctx.progress) {
            Ok(report) => report,
            Err(error) => {
                return super::state_run::failed_before_running::<true>(
                    ctx, &settings, started, error,
                );
            }
        };
        // How it ended, as `run` and `build` keep it: the dashboard shows the last run
        // only for its scope, tied to its snapshot by run id (#311).
        if let Some(remembered) = remembered {
            remembered.finish(
                report
                    .execution
                    .as_ref()
                    .map_or_else(Default::default, |e| {
                        super::state_retry::LastOutcome::of(e, report.record.is_some())
                    }),
                report.scope.clone(),
                report.execution.as_ref().map(|e| e.run_id.clone()),
            );
        }
        if report.outcome == TestOutcome::Incomplete {
            let error = CliError::new(
                ExitStatus::Failure,
                codes::STATE_EXECUTION,
                "some tests didn't run, so their nodes weren't marked tested",
            )
            .with_hint("see which below; run `ods state test` again once dbt can run them all");
            return ctx.emit_failed(&report, error);
        }
        if report.outcome == TestOutcome::Failed {
            let (nodes, sources) = report.record.as_ref().map_or((0, 0), |r| {
                (r.recorded.failed.len(), r.source_tests.failed.len())
            });
            let plural =
                |n: usize, noun: &str| format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
            let on = match (nodes, sources) {
                (n, 0) => plural(n, "node"),
                (0, s) => plural(s, "source"),
                (n, s) => format!("{} and {}", plural(n, "node"), plural(s, "source")),
            };
            let error = CliError::new(
                ExitStatus::Failure,
                codes::STATE_EXECUTION,
                format!("tests failed on {on}"),
            )
            .with_hint("they stay untested; fix them and run `ods state test` again");
            return ctx.emit_failed(&report, error);
        }
        ctx.emit(&report)
    }

    fn build(
        args: &ArgMatches,
        settings: &StateSettings,
        progress: ProgressSettings,
    ) -> Result<Self, CliError> {
        let compiles = !args.get_flag("no-compile");
        let measures = compiles && !args.get_flag("no-source-freshness");
        // The source measurement, the compile, the target check, and the test.
        let steps = Steps::new(progress, usize::from(measures) + usize::from(compiles) + 2);
        let executor = steps.attach(executor(args, settings));
        let mut warnings = Vec::new();
        let dbt = super::state_run::check_settings(args, settings, &executor, &mut warnings)?;
        // Sources are measured too: whether their data is new decides whether their
        // tests run (#232).
        let (_, sources) = super::state_run::prepare(args, &executor, &mut warnings)?;
        let target = super::state_run::identify(&executor)?;
        let ws = Workspace::load(args, settings, sources)?;
        let no_state = || {
            CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                "ODS has no recorded builds to test",
            )
            .with_hint(
                "build first with `ods state run`, or record a dbt run with `ods state record`",
            )
        };
        // Nodes need a recorded build to be tested; sources don't (#232), so a store
        // with nothing in it yet can still run their tests.
        let store = if ws.state_db.is_file() {
            Some(ws.open_store()?)
        } else {
            None
        };
        let latest = match &store {
            Some(store) => ws.latest(store)?,
            None => None,
        };
        if let Some(latest) = &latest {
            same_target(latest.snapshot.target.as_ref(), &target)?;
        }
        let empty = ods_core::state::StateSnapshot::new(
            None,
            ods_core::state::Timestamp::now(),
            "",
            std::collections::BTreeMap::new(),
        )
        .with_target(Some(target.clone()));
        let base = latest.as_ref().map_or(&empty, |l| &l.snapshot);

        // What to test: built by ODS, in the selection, with checks, and not tested
        // with the checks it has now unless --all. A node without checks has nothing
        // to run: it is never tested.
        let selected = ods_state::select(&ws.project, &select_specs(args))
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::LINEAGE_TARGET, e))?;
        let all = args.get_flag("all");
        let built: Vec<&ods_state::Node> = ws
            .project
            .nodes
            .iter()
            .filter(|n| selected.contains(&n.id))
            .filter(|n| base.nodes.contains_key(&n.id))
            .collect();
        let without_checks = built.iter().filter(|n| n.checks.is_none()).count();
        let candidates: Vec<(String, String, String)> = built
            .iter()
            .filter(|n| n.checks.is_some())
            .filter(|n| all || !n.is_tested(&base.nodes[&n.id]))
            .map(|n| (n.id.clone(), n.name.clone(), n.kind.clone()))
            .collect();
        let (requested, left_out) = narrow(args, &ws.project, candidates, &[], "ods state test")?;
        // Sources whose data is new or unknown since their tests passed here (#232).
        let source_tests = plan_source_checks(args, &ws.project, latest.as_ref(), all)?;
        let checked_sources = source_requests(&source_tests);
        if latest.is_none() && checked_sources.is_empty() {
            return Err(no_state());
        }
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.as_ref().map(|l| l.id),
            outcome: TestOutcome::NothingToTest,
            requested: requested.len(),
            without_checks,
            left_out,
            source_tests,
            execution: None,
            record: None,
            dbt,
            target,
            warnings,
            observed: super::state_run::RunObserved::default(),
        };
        if requested.is_empty() && checked_sources.is_empty() {
            steps.note("nothing to test, so dbt doesn't run again");
            return Ok(report);
        }

        let request = ExecutionRequest::new(requested, ExecutionMode::Test)
            .with_sources(checked_sources)
            .with_engine_args(dbt_args(args))
            .with_scope(report.scope.clone());
        let execution = report.execute(&executor, &request, &steps)?;
        report.explain_failures(&ws, settings, latest.as_ref());
        let store = match store {
            Some(store) => store,
            None => ws.open_store()?,
        };
        let previous = (latest.as_ref().map(|l| l.id), base);
        let record = record(&ws, &store, previous, &execution)?;
        report.outcome = record.outcome(&execution);
        report.execution = Some(execution);
        report.record = Some(record);
        Ok(report)
    }
}

impl TestReport {
    /// Explains each node that failed (#323).
    fn explain_failures(
        &mut self,
        ws: &Workspace,
        settings: &StateSettings,
        latest: Option<&ods_sdk::contracts::state_store::StoredSnapshot>,
    ) {
        let Some(run) = &self.observed.run_stats else {
            return;
        };
        let project_dir = super::failures::project_dir(settings);
        let evidence = super::failures::Evidence {
            files: super::failures::ProjectFiles {
                project_dir: &project_dir,
                target_dir: &ws.target_dir,
                manifest: Some(&ws.manifest),
                last_manifest: None,
            },
            plan: None,
            before: latest.map(|l| &l.snapshot),
            state_db: &self.state_db,
            retry: None,
            state_db_flag: super::failures::retry_state_db(settings),
            project_is_run: true,
            doctor: Some(super::failures::Doctor {
                config: None,
                settings,
            }),
        };
        self.observed.failures = super::failures::explain_run(run, &evidence);
    }

    /// Runs the tests, keeping the run's events in its journal (#322).
    fn execute(
        &mut self,
        executor: &ods_provider_dbt::executor::DbtExecutor,
        request: &ExecutionRequest,
        steps: &super::state_run::Steps,
    ) -> Result<ExecutionReport, CliError> {
        let (executed, observed) = super::state_run::execute_observed(
            executor,
            request,
            (&self.state_db, steps),
            &mut self.warnings,
        )?;
        self.observed = observed;
        executed.map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::STATE_EXECUTION, e.to_string())
                .with_hint("nothing was recorded")
        })
    }

    /// How it ended, for people.
    fn outcome_span(&self) -> Span {
        match self.outcome {
            TestOutcome::NothingToTest => Span::toned(
                "nothing to test: every build with tests is tested",
                Tone::Success,
            ),
            TestOutcome::Passed => Span::toned(
                match self.execution.as_ref().map_or(0, |e| e.sources.len()) {
                    0 => format!("{} tested, all passed", self.requested),
                    sources => format!(
                        "{} node{} and {sources} source{} tested, all passed",
                        self.requested,
                        if self.requested == 1 { "" } else { "s" },
                        if sources == 1 { "" } else { "s" },
                    ),
                },
                Tone::Success,
            ),
            TestOutcome::Failed => Span::toned("tests failed", Tone::Error),
            TestOutcome::Incomplete => Span::toned(
                "some tests didn't run; those nodes stay untested",
                Tone::Warning,
            ),
        }
    }
}

impl TestRecordReport {
    /// How the test run ended, given what was recorded of `execution`.
    fn outcome(&self, execution: &ExecutionReport) -> TestOutcome {
        let (recorded, sources) = (&self.recorded, &self.source_tests);
        if !recorded.failed.is_empty() || !sources.failed.is_empty() {
            TestOutcome::Failed
        } else if recorded.passed.len() == execution.nodes.len()
            && sources.passed.len() == execution.sources.len()
        {
            TestOutcome::Passed
        } else {
            TestOutcome::Incomplete
        }
    }
}

/// Step 4: records what the tests showed about the nodes' builds and the sources' data
/// (#232), and commits it.
fn record(
    ws: &Workspace,
    store: &ods_store_sqlite::SqliteStateStore,
    previous: (Option<SnapshotId>, &ods_core::state::StateSnapshot),
    execution: &ExecutionReport,
) -> Result<TestRecordReport, CliError> {
    let results = results_to_record(execution);
    let mut recorded = ods_state::record_tests(
        &ws.project,
        previous,
        &results,
        &execution.run_id,
        execution.finished_at,
    );
    let source_results: Vec<TestResult> = source_results(execution)
        .into_iter()
        .filter(|r| trusted(execution) || !r.passed)
        .collect();
    let sources_predate_run = matches!(
        (ws.sources_taken_at, execution.started_at),
        // Strictly before: timestamps are to the second.
        (Some(taken), Some(started)) if taken < started
    );
    let source_tests = ods_state::record_source_checks(
        &mut recorded.snapshot,
        &ws.project,
        &source_results,
        execution.finished_at,
        sources_predate_run,
    );
    tracing::info!(
        run = %execution.run_id,
        passed = recorded.passed.len(),
        failed = recorded.failed.len(),
        ignored = recorded.ignored.len(),
        source_tests_passed = source_tests.passed.len(),
        source_tests_failed = source_tests.failed.len(),
        "recording the tests"
    );
    let snapshot =
        block_on(store.commit(&ws.scope, &recorded.snapshot))?.map_err(|e| store_error(&e))?;
    Ok(TestRecordReport {
        snapshot,
        recorded,
        source_tests,
    })
}

impl Present for TestReport {
    const COMMAND: &'static str = "state.test";

    fn view(&self) -> ViewNode {
        let mut summary = vec![
            (
                "scope".into(),
                vec![Span::toned(self.scope.as_str(), Tone::Code)],
            ),
            (
                "compared with".into(),
                vec![Span::plain(match self.based_on {
                    Some(id) => format!("snapshot {id} in {}", self.state_db.display()),
                    None => format!("nothing recorded yet in {}", self.state_db.display()),
                })],
            ),
        ];
        summary.push((
            "dbt".into(),
            vec![Span::plain(super::state_run::settings_line(&self.dbt))],
        ));
        if let Some(command) = self.execution.as_ref().and_then(|e| e.command.as_deref()) {
            summary.push(("ran".into(), vec![Span::toned(command, Tone::Code)]));
        }
        summary.push(("outcome".into(), vec![self.outcome_span()]));
        if self.without_checks > 0 {
            summary.push((
                "no tests".into(),
                vec![Span::plain(format!(
                    "{} built node{} (never marked tested)",
                    self.without_checks,
                    if self.without_checks == 1 { "" } else { "s" }
                ))],
            ));
        }
        let mut blocks = vec![
            ViewNode::Heading("State test".into()),
            ViewNode::KeyValue(summary),
        ];
        if let Some(execution) = &self.execution {
            blocks.push(ViewNode::Table {
                title: None,
                columns: vec!["node".into(), "tests".into()],
                rows: execution
                    .nodes
                    .iter()
                    .map(|n| {
                        vec![
                            vec![Span::toned(display_name(&n.node), Tone::Code)],
                            vec![test_cell(n)],
                        ]
                    })
                    .collect(),
            });
        }
        if !self.source_tests.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("source tests".into()),
                columns: vec!["source".into(), "tests".into(), "why".into()],
                rows: source_rows(&self.source_tests, self.execution.as_ref()),
            });
        }
        blocks.extend(super::failures::section(&self.observed.failures));
        if !self.left_out.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "left out: {}",
                    self.left_out
                        .iter()
                        .map(|l| display_name(&l.node))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))],
            });
        }
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        ViewNode::Group(blocks)
    }
}

/// Whether a run's passes can be believed. A failed run is still trustworthy when a
/// test failing explains it, on a node or on a source; if dbt failed with no test
/// failing, something else went wrong and no pass counts.
fn trusted(execution: &ExecutionReport) -> bool {
    execution.succeeded || execution.nodes.iter().chain(&execution.sources).any(failed)
}

fn failed(n: &NodeExecution) -> bool {
    n.status == ExecutionStatus::Failed || !n.checks_failed.is_empty()
}

/// What a test run shows about each node. A check that didn't run leaves its node
/// untested (the executor lists it as skipped), so partial results can't mark anything
/// tested. But if dbt failed with no test failing, something else went wrong: nothing
/// is marked tested. Nodes whose tests didn't all run keep what they had.
fn results_to_record(execution: &ExecutionReport) -> Vec<TestResult> {
    let trusted = trusted(execution);
    execution
        .nodes
        .iter()
        .filter_map(|n| {
            if failed(n) {
                Some(TestResult::new(n.node.clone(), false, n.completed_at))
            } else if trusted && n.fully_checked() {
                Some(TestResult::new(n.node.clone(), true, n.completed_at))
            } else {
                None
            }
        })
        .collect()
}

/// One node's test outcome, for people.
fn test_cell(n: &NodeExecution) -> Span {
    let names = |checks: &[String]| {
        checks
            .iter()
            .map(|c| display_name(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if n.fully_checked() {
        Span::toned("passed", Tone::Success)
    } else if !n.checks_failed.is_empty() {
        Span::toned(format!("failed: {}", names(&n.checks_failed)), Tone::Error)
    } else if !n.checks_skipped.is_empty() {
        Span::toned(
            format!("didn't run: {}", names(&n.checks_skipped)),
            Tone::Warning,
        )
    } else if n.checks_passed.is_empty() {
        Span::toned("no tests ran on it", Tone::Warning)
    } else {
        Span::toned("not tested", Tone::Warning)
    }
}
