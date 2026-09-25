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
    ExecutionMode, ExecutionReport, ExecutionRequest, Executor, NodeExecution, PrepareRequest,
};
use ods_sdk::contracts::state_store::StateStore;
use ods_state::{RecordedTests, TestResult};
use serde::Serialize;

use super::state_plan::{
    Sources, Workspace, block_on, common, display_name, select_specs, store_error,
};
use super::state_run::{LeftOut, dbt_args, dbt_options, executor, narrow};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::Context;
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct TestReport {
    state_db: PathBuf,
    scope: String,
    based_on: SnapshotId,
    outcome: TestOutcome,
    /// Nodes whose tests ran.
    tested: usize,
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
        let report = Self::build(args)?;
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

    fn build(args: &ArgMatches) -> Result<Self, CliError> {
        let target_dir = PathBuf::from(
            args.get_one::<String>("target-dir")
                .map_or("target", String::as_str),
        );
        let executor = executor(args, &target_dir);
        if !args.get_flag("no-compile") {
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

        // What to test: built by ODS, in the selection, untested unless --all.
        let selected = ods_state::select(&ws.project, &select_specs(args))
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::LINEAGE_TARGET, e))?;
        let all = args.get_flag("all");
        let candidates: Vec<(String, String, String)> = ws
            .project
            .nodes
            .iter()
            .filter(|n| selected.contains(&n.id))
            .filter(|n| {
                latest
                    .snapshot
                    .nodes
                    .get(&n.id)
                    .is_some_and(|s| all || !s.is_tested())
            })
            .map(|n| (n.id.clone(), n.name.clone(), n.kind.clone()))
            .collect();
        let (requested, left_out) = narrow(args, &ws.project, candidates)?;
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            based_on: latest.id,
            outcome: TestOutcome::NothingToTest,
            tested: requested.len(),
            left_out,
            execution: None,
            record: None,
        };
        if requested.is_empty() {
            return Ok(report);
        }

        let request =
            ExecutionRequest::new(requested, ExecutionMode::Test).with_engine_args(dbt_args(args));
        let execution = block_on(executor.execute(&request))?.map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::STATE_EXECUTION, e.to_string())
                .with_hint("nothing was recorded")
        })?;
        let results: Vec<TestResult> = execution
            .nodes
            .iter()
            .map(|n| TestResult::new(n.node.clone(), n.fully_checked(), n.completed_at))
            .collect();
        let recorded = ods_state::record_tests(
            (latest.id, &latest.snapshot),
            &results,
            &execution.run_id,
            execution.finished_at,
        );
        let snapshot =
            block_on(store.commit(&ws.scope, &recorded.snapshot))?.map_err(|e| store_error(&e))?;
        report.outcome = if recorded.failed.is_empty() {
            TestOutcome::Passed
        } else {
            TestOutcome::Failed
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
                TestOutcome::NothingToTest => {
                    Span::toned("nothing to test: every build is tested", Tone::Success)
                }
                TestOutcome::Passed => {
                    Span::toned(format!("{} tested, all passed", self.tested), Tone::Success)
                }
                TestOutcome::Failed => Span::toned("tests failed or didn't run", Tone::Error),
            }],
        ));
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
    } else {
        Span::toned("not tested", Tone::Warning)
    }
}
