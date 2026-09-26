//! `ods state test` (#220): test what was built but not yet tested, without building.
//!
//! 1. Prepare: compile, so tests and selection match the code (`--no-compile` skips).
//! 2. Pick the nodes ODS has a build of whose tests haven't passed since that build
//!    (all of them with `--all`), within `--select` and minus `--exclude`.
//! 3. Run only their tests, selected exactly.
//! 4. Record: nodes whose tests passed are marked tested; nodes whose tests failed are
//!    marked untested, so they come up again. Builds are unchanged.

use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::SnapshotId;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, NodeExecution,
    PrepareRequest,
};
use ods_sdk::contracts::state_store::StateStore;
use ods_state::{RecordedTests, TestResult};
use serde::Serialize;

use super::state_plan::{
    Sources, Workspace, block_on, common, display_name, select_specs, store_error,
};
use super::state_run::{LeftOut, Steps, dbt_args, dbt_options, executor, narrow};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, ProgressSettings};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods state test`'s own arguments.
pub(super) fn test_command() -> Command {
    dbt_options(common(Command::new("test").about(
        "Run the tests of what was built but not yet tested, with dbt, and record the results",
    )))
    .arg(
        Arg::new("all")
            .long("all")
            .action(ArgAction::SetTrue)
            .help("Test every node ODS has a build of, not only the untested ones"),
    )
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
    based_on: SnapshotId,
    outcome: TestOutcome,
    /// Nodes whose tests were asked to run.
    requested: usize,
    /// Built nodes in the selection that have no tests: nothing can mark them tested.
    without_checks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    left_out: Vec<LeftOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<ExecutionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<TestRecordReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct TestRecordReport {
    snapshot: SnapshotId,
    #[serde(flatten)]
    recorded: RecordedTests,
}

impl TestReport {
    /// Runs the tests and emits the report.
    pub(super) fn run(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let report = Self::build(args, ctx.progress)?;
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
            let failed = report
                .record
                .as_ref()
                .map_or(0, |r| r.recorded.failed.len());
            let error = CliError::new(
                ExitStatus::Failure,
                codes::STATE_EXECUTION,
                format!(
                    "tests failed on {} node{}",
                    failed,
                    if failed == 1 { "" } else { "s" }
                ),
            )
            .with_hint("they stay untested; fix them and run `ods state test` again");
            return ctx.emit_failed(&report, error);
        }
        ctx.emit(&report)
    }

    fn build(args: &ArgMatches, progress: ProgressSettings) -> Result<Self, CliError> {
        let target_dir = PathBuf::from(
            args.get_one::<String>("target-dir")
                .map_or("target", String::as_str),
        );
        let compiles = !args.get_flag("no-compile");
        let steps = Steps::new(progress, usize::from(compiles) + 1);
        let executor = steps.attach(executor(args, &target_dir));
        if compiles {
            block_on(executor.prepare(&PrepareRequest::new()))?.map_err(|e| {
                CliError::new(ExitStatus::Failure, codes::STATE_EXECUTION, e.to_string())
                    .with_hint("fix the project so `dbt compile` succeeds, or use --no-compile")
            })?;
        }
        let ws = Workspace::load(args, Sources::AsGiven)?;
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
        if !ws.state_db.is_file() {
            return Err(no_state());
        }
        let store = ws.open_store()?;
        let latest = ws.latest(&store)?.ok_or_else(no_state)?;

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
            .filter(|n| latest.snapshot.nodes.contains_key(&n.id))
            .collect();
        let without_checks = built.iter().filter(|n| n.checks.is_none()).count();
        let candidates: Vec<(String, String, String)> = built
            .iter()
            .filter(|n| n.checks.is_some())
            .filter(|n| all || !n.is_tested(&latest.snapshot.nodes[&n.id]))
            .map(|n| (n.id.clone(), n.name.clone(), n.kind.clone()))
            .collect();
        let (requested, left_out) = narrow(args, &ws.project, candidates, &[], "ods state test")?;
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.id,
            outcome: TestOutcome::NothingToTest,
            requested: requested.len(),
            without_checks,
            left_out,
            execution: None,
            record: None,
        };
        if requested.is_empty() {
            steps.note("nothing to test, so dbt doesn't run again");
            return Ok(report);
        }

        let request =
            ExecutionRequest::new(requested, ExecutionMode::Test).with_engine_args(dbt_args(args));
        let execution = block_on(executor.execute(&request))?.map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::STATE_EXECUTION, e.to_string())
                .with_hint("nothing was recorded")
        })?;
        let results = results_to_record(&execution);
        let recorded = ods_state::record_tests(
            &ws.project,
            (latest.id, &latest.snapshot),
            &results,
            &execution.run_id,
            execution.finished_at,
        );
        tracing::info!(
            run = %execution.run_id,
            passed = recorded.passed.len(),
            failed = recorded.failed.len(),
            ignored = recorded.ignored.len(),
            "recording the tests"
        );
        let snapshot =
            block_on(store.commit(&ws.scope, &recorded.snapshot))?.map_err(|e| store_error(&e))?;
        report.outcome = if !recorded.failed.is_empty() {
            TestOutcome::Failed
        } else if recorded.passed.len() == execution.nodes.len() {
            TestOutcome::Passed
        } else {
            TestOutcome::Incomplete
        };
        report.execution = Some(execution);
        report.record = Some(TestRecordReport { snapshot, recorded });
        Ok(report)
    }
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
                vec![Span::plain(format!(
                    "snapshot {} in {}",
                    self.based_on,
                    self.state_db.display()
                ))],
            ),
        ];
        if let Some(command) = self.execution.as_ref().and_then(|e| e.command.as_deref()) {
            summary.push(("ran".into(), vec![Span::toned(command, Tone::Code)]));
        }
        summary.push((
            "outcome".into(),
            vec![match self.outcome {
                TestOutcome::NothingToTest => Span::toned(
                    "nothing to test: every build with tests is tested",
                    Tone::Success,
                ),
                TestOutcome::Passed => Span::toned(
                    format!("{} tested, all passed", self.requested),
                    Tone::Success,
                ),
                TestOutcome::Failed => Span::toned("tests failed", Tone::Error),
                TestOutcome::Incomplete => Span::toned(
                    "some tests didn't run; those nodes stay untested",
                    Tone::Warning,
                ),
            }],
        ));
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
        ViewNode::Group(blocks)
    }
}

/// What a test run shows about each node. A check that didn't run leaves its node
/// untested (the executor lists it as skipped), so partial results can't mark anything
/// tested. But if dbt failed with no test failing, something else went wrong: nothing
/// is marked tested. Nodes whose tests didn't all run keep what they had.
fn results_to_record(execution: &ExecutionReport) -> Vec<TestResult> {
    let failed =
        |n: &NodeExecution| n.status == ExecutionStatus::Failed || !n.checks_failed.is_empty();
    let trusted = execution.succeeded || execution.nodes.iter().any(failed);
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
