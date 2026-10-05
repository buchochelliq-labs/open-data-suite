//! `ods state explain-failure` (#348): one node's or test's failure in a run, explained
//! from the run's journal, as `ods state run`, `history --run` and the dashboard explain
//! it (ADR-0025). The explanations are [`explain_history`]'s, picked for the node asked
//! about: nothing here explains anything itself.

use std::path::PathBuf;

use clap::{Arg, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::failure::{ErrorExplanation, check_handle};
use ods_sdk::contracts::run_events::{CheckStatus, NodeRunStatus, RunSummary};
use serde::Serialize;

use super::failures::{CheckNames, check_line};
use super::state_plan::{Sources, Workspace, common, display_name, explain_history, read_journal};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Line, Present, Span, Tone, ViewNode};

/// `ods state explain-failure`'s arguments.
pub(super) fn explain_failure_command() -> Command {
    common(Command::new("explain-failure").about(
        "Explain why a node or test failed in a run (default: the last run): what went wrong, the evidence, and what to try",
    ))
    .arg(
        Arg::new("node")
            .value_name("NODE")
            .required(true)
            .help("A model, seed or snapshot (name or unique id), or a test by its name or handle (`check-…`)"),
    )
    .arg(
        Arg::new("run")
            .long("run")
            .value_name("RUN_ID")
            .help("A run's id, as `ods state history` lists it [default: the last run]"),
    )
}

/// How the node or test asked about ended in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    /// It failed: `failures` explains it.
    Failed,
    /// It succeeded (a test: it passed).
    Succeeded,
    /// A test found something, at warning severity: nothing failed.
    Warned,
    /// It wasn't tried, e.g. because something upstream failed.
    Skipped,
    /// It hadn't finished when the journal ends: still running, or the run stopped.
    NotFinished,
    /// How it ended isn't known: the engine didn't say, or journal lines were lost.
    Unknown,
    /// It is in the project but wasn't part of this run.
    NotInRun,
}

impl Outcome {
    fn of_node(status: NodeRunStatus) -> Self {
        match status {
            NodeRunStatus::Error => Self::Failed,
            NodeRunStatus::Success => Self::Succeeded,
            NodeRunStatus::Skipped => Self::Skipped,
            NodeRunStatus::Queued | NodeRunStatus::Running => Self::NotFinished,
            _ => Self::Unknown,
        }
    }

    fn of_check(status: CheckStatus) -> Self {
        match status {
            CheckStatus::Failed => Self::Failed,
            CheckStatus::Passed => Self::Succeeded,
            CheckStatus::Warned => Self::Warned,
            CheckStatus::Skipped => Self::Skipped,
            _ => Self::Unknown,
        }
    }

    /// What it did, for people, after the node's name.
    fn said(self, test: bool) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Succeeded if test => "passed",
            Self::Succeeded => "succeeded",
            Self::Warned => "only warned",
            Self::Skipped => "was skipped",
            Self::NotFinished => "hadn't finished when the journal ends",
            Self::Unknown => "ended in a way that isn't known",
            Self::NotInRun => "wasn't part of this run",
        }
    }
}

/// What the node asked about is, in the run.
enum Found<'a> {
    Node(&'a ods_sdk::contracts::run_events::NodeSummary),
    Check(&'a ods_sdk::contracts::run_events::CheckSummary),
    /// In the project, not in the run: its id.
    Elsewhere(String),
}

/// `ods state explain-failure`'s report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ExplainFailureReport {
    state_db: PathBuf,
    journal: PathBuf,
    run_id: Option<String>,
    /// The run's scope (project and target), if its events say.
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// The node's id, or a test's handle: never a test's id, which may hold its
    /// arguments.
    node: String,
    /// Whether it is a test.
    test: bool,
    outcome: Outcome,
    /// For a skipped node, the failed nodes (or a failed test's handle) that blocked it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocked_by: Vec<String>,
    /// Its failure, explained, and the failed tests on it: a model can build and its
    /// tests fail. Empty when neither failed.
    failures: Vec<ErrorExplanation>,
    /// Lines of the journal that couldn't be read (a newer version, or a last line cut
    /// short): then a node the journal doesn't show finishing, or at all, is `unknown`.
    #[serde(skip_serializing_if = "is_zero")]
    unreadable_lines: usize,
    /// For people: a test by what it tests.
    #[serde(skip)]
    label: Line,
    /// The state database, when it came from a flag or the environment, for the
    /// commands this one suggests.
    #[serde(skip)]
    state_db_flag: Option<String>,
}

impl ExplainFailureReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let state_db = settings.state_db();
        let asked = args.get_one::<String>("node").map_or("", String::as_str);
        // The project, if it can be read: its scope picks the last run, and it knows the
        // nodes a run didn't include.
        let ws = Workspace::load(args, &settings, Sources::Ignore).ok();
        let scope = ws.as_ref().map(|w| w.scope.to_string());
        let run_id = match args.get_one::<String>("run") {
            Some(run_id) => Some(run_id.clone()),
            None => last_run_of(&state_db, scope.as_deref()),
        };
        let (journal, read) = read_journal(&state_db, run_id.as_deref())?;
        let run = RunSummary::from_events(&read.events);
        let found = match find(&run, asked)? {
            Some(found) => found,
            None => Found::Elsewhere(
                in_project(ws.as_ref(), asked)?.ok_or_else(|| not_found(&run, asked))?,
            ),
        };
        let (node, test, outcome, blocked_by, label) = match &found {
            Found::Node(n) => (
                n.node.clone(),
                false,
                Outcome::of_node(n.stats.status),
                n.stats.blocked_by.clone(),
                vec![Span::toned(display_name(&n.node), Tone::Code)],
            ),
            Found::Check(c) => {
                let names: CheckNames = [(
                    c.check.clone(),
                    ods_state::describe_check(&c.check, &c.covers, None),
                )]
                .into();
                (
                    check_handle(&c.check),
                    true,
                    Outcome::of_check(c.status),
                    Vec::new(),
                    check_line(&c.check, &names),
                )
            }
            Found::Elsewhere(id) => (
                id.clone(),
                false,
                Outcome::NotInRun,
                Vec::new(),
                vec![Span::toned(display_name(id), Tone::Code)],
            ),
        };
        // Lines of the journal were lost: what it doesn't say isn't known (AGENTS rule 3).
        let outcome =
            if read.unreadable > 0 && matches!(outcome, Outcome::NotInRun | Outcome::NotFinished) {
                Outcome::Unknown
            } else {
                outcome
            };
        // Every failure of the run, explained: the node's own, the failed tests on it (a
        // build doesn't fail a model whose tests fail), and those that blocked it.
        let explained = explain_history(args, &settings, &run);
        let mut blocked_by = blocked_by;
        if outcome == Outcome::Skipped && !test {
            blocked_by.extend(
                explained
                    .iter()
                    .filter(|e| e.impact().is_some_and(|i| i.blocked.contains(&node)))
                    .map(|e| e.node().to_owned()),
            );
            blocked_by.sort();
            blocked_by.dedup();
        }
        let failures = explained
            .into_iter()
            .filter(|e| {
                e.node() == node || (!test && e.check().is_some_and(|c| c.covers.contains(&node)))
            })
            .collect();
        Ok(Self {
            state_db,
            journal,
            run_id: run.run_id.clone(),
            scope: run.scope.clone(),
            node,
            test,
            outcome,
            blocked_by,
            failures,
            unreadable_lines: read.unreadable,
            label,
            state_db_flag: super::failures::retry_state_db(&settings).map(str::to_owned),
        })
    }
}

/// The node or test of `run` that `asked` names: a node by id or name, a test by
/// handle, id or name; `None` if none. A name several share is a usage error listing
/// their ids (a test's handles).
fn find<'a>(run: &'a RunSummary, asked: &str) -> Result<Option<Found<'a>>, CliError> {
    let nodes: Vec<_> = run
        .nodes
        .iter()
        .filter(|n| n.node == asked || display_name(&n.node) == asked)
        .collect();
    match nodes.as_slice() {
        [node] => return Ok(Some(Found::Node(node))),
        [] => {}
        many => {
            // An id names one node; only a shared name gets here.
            let ids: Vec<&str> = many.iter().map(|n| n.node.as_str()).collect();
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::STATE_INPUT,
                format!("several nodes of this run are called `{asked}`"),
            )
            .with_hint(format!("name one by its unique id: {}", ids.join(", "))));
        }
    }
    let checks: Vec<_> = run
        .checks
        .iter()
        .filter(|c| {
            c.check == asked || check_handle(&c.check) == asked || display_name(&c.check) == asked
        })
        .collect();
    match checks.as_slice() {
        [check] => Ok(Some(Found::Check(check))),
        [] => Ok(None),
        // Never their ids: a test's may hold its arguments.
        many => Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!("several tests of this run are called `{asked}`"),
        )
        .with_hint(format!(
            "name one by its handle: {}",
            many.iter()
                .map(|c| check_handle(&c.check))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// `asked` names nothing in the run or the project.
fn not_found(run: &RunSummary, asked: &str) -> CliError {
    CliError::new(
        ExitStatus::Usage,
        codes::STATE_INPUT,
        format!(
            "`{}` isn't a node or test of run {}, nor a model, seed or snapshot of the project",
            asked.escape_debug(),
            run.run_id.as_deref().unwrap_or("?")
        ),
    )
    .with_hint("`ods state history --run <id>` lists a run's nodes and explains its failed tests, each with its handle (`check-…`)")
}

/// The id of the project's model, seed or snapshot `asked` names: one the run didn't
/// include. `None` if none, or without a project, which can't tell; a name several
/// share is a usage error listing their ids.
fn in_project(ws: Option<&Workspace>, asked: &str) -> Result<Option<String>, CliError> {
    let Some(ws) = ws else {
        return Ok(None);
    };
    let ids: Vec<&str> = ws
        .project
        .nodes
        .iter()
        .filter(|n| n.id == asked || display_name(&n.id) == asked)
        .map(|n| n.id.as_str())
        .collect();
    match ids.as_slice() {
        [] => Ok(None),
        [id] => Ok(Some((*id).to_owned())),
        many => Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!("several nodes of the project are called `{asked}`"),
        )
        .with_hint(format!("name one by its unique id: {}", many.join(", ")))),
    }
}

/// The last run of `scope` with a journal: the newest whose events name that scope, or
/// none (an older journal's). Without a scope, the newest run; `None` if there is no
/// journal, which [`read_journal`] then says.
fn last_run_of(state_db: &std::path::Path, scope: Option<&str>) -> Option<String> {
    let files = ods_sdk::run_journal::Journals::beside(state_db)
        .list()
        .ok()?;
    let Some(scope) = scope else {
        return files.into_iter().next().map(|f| f.run_id);
    };
    files
        .into_iter()
        .find(|f| {
            super::run_journal::read(&f.path)
                .ok()
                .flatten()
                .is_some_and(|j| {
                    RunSummary::from_events(&j.events)
                        .scope
                        .is_none_or(|s| s == scope)
                })
        })
        .map(|f| f.run_id)
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Present for ExplainFailureReport {
    const COMMAND: &'static str = "state.explain_failure";

    fn view(&self) -> ViewNode {
        let run = self.run_id.as_deref().unwrap_or("?");
        let mut facts = vec![("run".to_owned(), vec![Span::toned(run, Tone::Code)])];
        if let Some(scope) = &self.scope {
            facts.push((
                "scope".to_owned(),
                vec![Span::toned(scope.as_str(), Tone::Code)],
            ));
        }
        let mut blocks = vec![ViewNode::KeyValue(facts)];
        if self.unreadable_lines > 0 {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(format!(
                    "{} line(s) of the run's journal couldn't be read: written by a newer ODS, or cut short when the run stopped",
                    self.unreadable_lines
                ))],
            });
        }
        if self.outcome == Outcome::Failed {
            blocks.extend(super::failures::section(&self.failures));
            return ViewNode::Group(blocks);
        }
        let mut message = self.label.clone();
        message.push(Span::plain(format!(
            " didn't fail in this run: it {}",
            self.outcome.said(self.test)
        )));
        message.push(Span::plain(match self.failures.len() {
            0 => ".".to_owned(),
            1 => ", but a test on it failed.".to_owned(),
            n => format!(", but {n} tests on it failed."),
        }));
        blocks.push(ViewNode::Notice {
            level: if self.failures.is_empty() {
                Level::Info
            } else {
                Level::Warning
            },
            message,
        });
        blocks.extend(super::failures::section(&self.failures));
        if !self.blocked_by.is_empty() {
            let mut line = vec![Span::plain("It was skipped because ")];
            for (i, node) in self.blocked_by.iter().enumerate() {
                if i > 0 {
                    line.push(Span::plain(", "));
                }
                line.push(Span::toned(display_name(node), Tone::Code));
            }
            line.push(Span::plain(" failed; to see why: "));
            let quote = |s: &str| {
                shlex::try_quote(s).map_or_else(|_| s.to_owned(), std::borrow::Cow::into_owned)
            };
            let state_db = self
                .state_db_flag
                .as_ref()
                .map(|db| format!(" --state-db {}", quote(db)))
                .unwrap_or_default();
            line.push(Span::toned(
                format!(
                    "ods state explain-failure {} --run {}{state_db}",
                    quote(&self.blocked_by[0]),
                    quote(run)
                ),
                Tone::Code,
            ));
            blocks.push(ViewNode::Paragraph(line));
        }
        ViewNode::Group(blocks)
    }
}
