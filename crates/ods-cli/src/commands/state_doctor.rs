//! `ods state doctor`, `ods state backup` and `ods state reset` (#188, ADR-0018): check
//! the state database, keep a copy of it, or set it aside to start afresh.

use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::Timestamp;
use ods_sdk::ProviderError;
use ods_sdk::contracts::state_store::{
    ProblemKind, ScopeSummary, StateStore, StoreProblem, StoreSchema,
};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::state_plan::{block_on, store_error};
use super::state_settings::{DEFAULT_STORE, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::Context;
use crate::present::{Level, Present, Span, Tone, ViewNode};

fn state_db_arg() -> Arg {
    Arg::new("state-db")
        .long("state-db")
        .value_name("PATH")
        .default_value(DEFAULT_STORE)
        .help("SQLite state database [config: state.db]")
}

/// `ods state doctor`'s arguments.
pub(super) fn doctor_command() -> Command {
    Command::new("doctor")
        .about(
            "Check the state database, changing nothing, and say how to recover if it is damaged",
        )
        .arg(state_db_arg())
}

/// `ods state backup`'s arguments.
pub(super) fn backup_command() -> Command {
    Command::new("backup")
        .about("Write a consistent copy of the state database, even while runs use it")
        .arg(state_db_arg())
        .arg(
            Arg::new("to")
                .long("to")
                .value_name("PATH")
                .help("Where to write it; must not exist [default: next to the database]"),
        )
}

/// `ods state reset`'s arguments.
pub(super) fn reset_command() -> Command {
    Command::new("reset")
        .about("Set the state database aside (nothing is deleted), so the next run starts with no state and builds everything")
        .arg(state_db_arg())
        .arg(
            Arg::new("yes")
                .long("yes")
                .action(ArgAction::SetTrue)
                .help("Do it: every node builds on the next run"),
        )
}

fn state_db(args: &ArgMatches, config: &Loaded) -> Result<PathBuf, CliError> {
    Ok(StateSettings::resolve(args, config)?.state_db())
}

/// Copies kept beside the database: before migrations, and by `ods state backup`.
fn copies_of(db: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name()) else {
        return Vec::new();
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(".bak")
            })
        })
        .map(|p| db.with_file_name(p.file_name().unwrap_or_default()))
        .collect();
    found.sort();
    found
}

// ----------------------------------------------------------------------------- doctor

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct DoctorReport {
    pub(super) state_db: PathBuf,
    pub(super) exists: bool,
    pub(super) schema: Option<StoreSchema>,
    pub(super) scopes: Vec<ScopeSummary>,
    pub(super) problems: Vec<StoreProblem>,
    /// Copies of the database found beside it, oldest name first.
    pub(super) copies: Vec<PathBuf>,
    /// What to do, for people.
    pub(super) advice: Vec<String>,
}

impl DoctorReport {
    pub(super) fn run(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let report = Self::build(&state_db(args, ctx.config)?)?;
        if report.problems.is_empty() {
            return ctx.emit(&report);
        }
        let n = report.problems.len();
        let error = CliError::new(
            ExitStatus::Failure,
            codes::STATE_DAMAGED,
            format!(
                "the state database has {n} problem{}",
                if n == 1 { "" } else { "s" }
            ),
        )
        .with_hint("see what to do above; docs/cli.md#recovering-state");
        ctx.emit_failed(&report, error)
    }

    /// Checks the database at `db`, changing nothing. Also `ods doctor`'s state store
    /// check (#181), so the two never disagree.
    ///
    /// # Errors
    /// The database exists but can't be opened (other than being damaged).
    pub(super) fn build(db: &Path) -> Result<Self, CliError> {
        let mut report = Self {
            state_db: db.to_owned(),
            exists: db.is_file(),
            schema: None,
            scopes: Vec::new(),
            problems: Vec::new(),
            copies: copies_of(db),
            advice: Vec::new(),
        };
        if !report.exists {
            report
                .advice
                .push("there is no state yet: the first run creates it".to_owned());
            return Ok(report);
        }
        match block_on(SqliteStateStore::open_existing(db))? {
            Ok(store) => {
                let check = block_on(store.check())?.map_err(|e| store_error(&e))?;
                report.schema = check.schema;
                report.scopes = check.scopes;
                report.problems = check.problems;
            }
            Err(ProviderError::Corrupt(detail)) => {
                report
                    .problems
                    .push(StoreProblem::new(ProblemKind::Damaged, detail));
            }
            Err(e) => return Err(store_error(&e)),
        }
        report.advice = advice(&report);
        Ok(report)
    }
}

fn advice(report: &DoctorReport) -> Vec<String> {
    let db = report.state_db.display();
    let mut advice = Vec::new();
    if report
        .problems
        .iter()
        .any(|p| p.kind == ProblemKind::NewerSchema)
    {
        advice.push(
            "a newer ODS wrote this database: upgrade ODS; nothing here is wrong with it"
                .to_owned(),
        );
        return advice;
    }
    if report.problems.is_empty() {
        if let Some(schema) = report.schema.filter(|s| s.version < s.latest) {
            advice.push(format!(
                "the next `ods state` command that writes migrates it from schema version {} to {}, keeping a copy first",
                schema.version, schema.latest
            ));
        }
        return advice;
    }
    advice.push(
        "until this is fixed, `ods state` commands that read this state stop rather than guess"
            .to_owned(),
    );
    if let Some(copy) = report.copies.last() {
        advice.push(format!(
            "to go back to a copy: `ods state reset --yes --state-db {db}`, then copy `{}` to `{db}`, and check again",
            copy.display()
        ));
    }
    advice.push(format!(
        "to start afresh: `ods state reset --yes --state-db {db}` sets the database aside (nothing is deleted); the next run builds everything and records new state"
    ));
    advice
}

pub(super) fn problem_label(kind: ProblemKind) -> &'static str {
    match kind {
        ProblemKind::Damaged => "damaged",
        ProblemKind::NewerSchema => "newer schema",
        ProblemKind::UnreadableSnapshot => "unreadable snapshot",
        ProblemKind::InconsistentSnapshot => "inconsistent snapshot",
        ProblemKind::DanglingHead => "dangling head",
        ProblemKind::BrokenChain => "broken chain",
        _ => "problem",
    }
}

impl Present for DoctorReport {
    const COMMAND: &'static str = "state.doctor";

    fn view(&self) -> ViewNode {
        let status = if !self.exists {
            Span::toned("no database yet", Tone::Muted)
        } else if self.problems.is_empty() {
            Span::toned("sound", Tone::Success)
        } else {
            Span::toned(format!("{} problem(s)", self.problems.len()), Tone::Error)
        };
        let mut summary = vec![
            (
                "database".into(),
                vec![Span::plain(self.state_db.display().to_string())],
            ),
            ("status".into(), vec![status]),
        ];
        if let Some(schema) = self.schema {
            summary.push((
                "schema".into(),
                vec![Span::plain(format!(
                    "version {} (this ODS: {})",
                    schema.version, schema.latest
                ))],
            ));
        }
        if !self.copies.is_empty() {
            summary.push((
                "copies".into(),
                vec![Span::plain(
                    self.copies
                        .iter()
                        .map(|c| c.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                )],
            ));
        }
        let mut blocks = vec![
            ViewNode::Heading("State database".into()),
            ViewNode::KeyValue(summary),
        ];
        if !self.scopes.is_empty() {
            blocks.push(ViewNode::Table {
                title: None,
                columns: vec!["scope".into(), "head".into(), "snapshots".into()],
                rows: self
                    .scopes
                    .iter()
                    .map(|s| {
                        vec![
                            vec![Span::plain(s.scope.as_str())],
                            vec![Span::plain(
                                s.head.map_or_else(|| "-".to_owned(), |h| h.to_string()),
                            )],
                            vec![Span::plain(s.snapshots.to_string())],
                        ]
                    })
                    .collect(),
            });
        }
        for problem in &self.problems {
            blocks.push(ViewNode::Notice {
                level: Level::Error,
                message: vec![Span::plain(format!(
                    "{}: {}",
                    problem_label(problem.kind),
                    problem.detail
                ))],
            });
        }
        for line in &self.advice {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(line.as_str())],
            });
        }
        ViewNode::Group(blocks)
    }
}

// ----------------------------------------------------------------------------- backup

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct BackupReport {
    state_db: PathBuf,
    copy: PathBuf,
}

impl BackupReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let db = state_db(args, config)?;
        let copy = args.get_one::<String>("to").map_or_else(
            || {
                let mut name = db.as_os_str().to_owned();
                name.push(format!(".{}.bak", Timestamp::now().unix()));
                PathBuf::from(name)
            },
            PathBuf::from,
        );
        // Never migrates: the copy is the database as it is.
        let store = block_on(SqliteStateStore::open_existing(&db))?.map_err(|e| {
            store_error(&e).with_hint("`ods state doctor` says what is wrong with it")
        })?;
        block_on(store.backup(&copy))?.map_err(|e| store_error(&e))?;
        Ok(Self { state_db: db, copy })
    }
}

impl Present for BackupReport {
    const COMMAND: &'static str = "state.backup";

    fn view(&self) -> ViewNode {
        ViewNode::Paragraph(vec![
            Span::plain("copied "),
            Span::toned(self.state_db.display().to_string(), Tone::Code),
            Span::plain(" to "),
            Span::toned(self.copy.display().to_string(), Tone::Code),
        ])
    }
}

// ------------------------------------------------------------------------------ reset

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ResetReport {
    state_db: PathBuf,
    /// Where each file went; empty if there was no database.
    set_aside: Vec<PathBuf>,
}

impl ResetReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let db = state_db(args, config)?;
        if !args.get_flag("yes") {
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::STATE_INPUT,
                format!(
                    "resetting sets `{}` aside, so the next run builds every node",
                    db.display()
                ),
            )
            .with_hint("pass --yes to do it; `ods state backup` keeps a copy first"));
        }
        let set_aside = if db.exists() {
            SqliteStateStore::set_aside(&db).map_err(|e| store_error(&e))?
        } else {
            Vec::new()
        };
        Ok(Self {
            state_db: db,
            set_aside,
        })
    }
}

impl Present for ResetReport {
    const COMMAND: &'static str = "state.reset";

    fn view(&self) -> ViewNode {
        match self.set_aside.first() {
            None => ViewNode::Paragraph(vec![Span::plain(format!(
                "there is no state database at {}: nothing to reset",
                self.state_db.display()
            ))]),
            Some(main) => ViewNode::Group(vec![
                ViewNode::Paragraph(vec![
                    Span::plain("set "),
                    Span::toned(self.state_db.display().to_string(), Tone::Code),
                    Span::plain(" aside as "),
                    Span::toned(main.display().to_string(), Tone::Code),
                ]),
                ViewNode::Notice {
                    level: Level::Info,
                    message: vec![Span::plain(
                        "the next run starts with no state and builds everything",
                    )],
                },
            ]),
        }
    }
}
