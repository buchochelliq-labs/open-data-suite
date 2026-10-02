//! `ods state retry` (#276): rerun the last `ods state` command that ran dbt, with the
//! options it was given, planned afresh.
//!
//! Every `run`, `seed`, `snapshot`, `build` and `test` that isn't a dry run keeps its
//! command line beside the state database, in `<state-db>.last-run.json`. Only what was
//! typed is kept: environment variables and configuration are read again when retrying,
//! as for any command, so nothing from them is written down (AGENTS.md rule 9).
//!
//! Of what was typed, only the values of [`KEPT_VALUES`] are written down: they select
//! what runs and where. `--vars` and what follows `--` may hold secrets, so only that
//! they were given is kept (#321), never their values or a digest of them (a digest of a
//! short secret can be reversed by guessing). `ods state retry` asks for them again.
//!
//! Once dbt has built, the file also keeps how the run ended (#292): the nodes that
//! failed, those skipped because of a failure, and the sources whose tests failed.
//! `retry --failed` builds only those, still planned (see [`ods_state::split_retry`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use ods_sdk::contracts::executor::{ExecutionReport, ExecutionStatus};
use serde::{Deserialize, Serialize};

use super::state_settings::{DEFAULT_STORE, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};

/// The version of the last-run file this build writes and reads. 1.1 added the
/// optional `outcome` (#292); 1.2 the optional `scope` and `run_id` (#311), so the
/// dashboard can show the run only for its own state and tie it to its snapshot; 1.3
/// `withheld`, and `args` without the values it names (#321). Older files still read,
/// without them; their withheld values are dropped as they are read.
const LAST_RUN_VERSION: SchemaVersion = SchemaVersion::new(1, 3);

/// The version before which `args` could hold values that are now withheld.
const WITHHOLDING_SINCE: SchemaVersion = SchemaVersion::new(1, 3);

/// Options whose values are kept for retry: they choose what runs, where, and with
/// which program and profile, and name no secret. Every option that takes a value is
/// in this list or [`WITHHELD_VALUES`] (a test checks it).
const KEPT_VALUES: [&str; 15] = [
    "select",
    "exclude",
    "resource-type",
    "exclude-resource-type",
    "target",
    "environment",
    "dbt-output",
    "state-db",
    "target-dir",
    "project-dir",
    "profiles-dir",
    "dbt-profile",
    "dbt",
    "artifacts",
    "sources",
];

/// Options whose values may hold secrets: only that they were given is kept, and
/// `ods state retry` takes them again (#321). What follows `--` is withheld too, written
/// as [`PASSTHROUGH`].
const WITHHELD_VALUES: [&str; 1] = ["vars"];

/// How `withheld` names the arguments after `--`.
const PASSTHROUGH: &str = "--";

/// Options whose values are shown by the dashboard: they select what runs and where,
/// and carry nothing secret. Every other option's value (e.g. `--vars`, which may hold
/// credentials) and everything after `--` is redacted (AGENTS.md rule 9).
const SHOWN_VALUES: [&str; 7] = [
    "select",
    "exclude",
    "resource-type",
    "exclude-resource-type",
    "target",
    "environment",
    "dbt-output",
];

/// What a redacted value reads as.
const REDACTED: &str = "<redacted>";

/// What a withheld value reads as on a command line shown in the terminal.
const NOT_KEPT: &str = "<not kept>";

/// What a withheld value to give again reads as in a suggested retry.
const VALUE_AGAIN: &str = "<value>";

/// What withheld arguments after `--` to give again read as in a suggested retry.
const ARGUMENTS_AGAIN: &str = "<dbt arguments>";

/// `ods state retry`'s arguments.
pub(super) fn retry_command() -> Command {
    Command::new("retry")
        .about("Run the last `ods state` command that ran dbt again, with its options, planned afresh: what failed builds again, what succeeded is reused")
        .arg(
            Arg::new("state-db")
                .long("state-db")
                .value_name("PATH")
                .default_value(DEFAULT_STORE)
                .help("SQLite state database whose last run to retry [config: state.db]"),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .action(ArgAction::SetTrue)
                .help("Prepare and plan the retry, but build and record nothing"),
        )
        .arg(
            Arg::new("failed")
                .long("failed")
                .action(ArgAction::SetTrue)
                .help("Build only what failed, or was skipped because of a failure, in the last run, and test only the sources whose tests failed; nothing that changed since. Still planned: what the plan now reuses is reused"),
        )
        .arg(
            Arg::new("vars")
                .long("vars")
                .value_name("YAML")
                .help("dbt's --vars, if the last run had them: their value isn't kept, as it may hold a secret, so give it again"),
        )
        .arg(
            Arg::new("dbt-args")
                .value_name("DBT_ARGS")
                .num_args(0..)
                .last(true)
                .help("After `--`: the options the last run passed to dbt, if it had any; they aren't kept, as they may hold a secret, so give them again"),
        )
}

/// The last run, as kept beside the state database. Its `Debug` redacts the options,
/// as [`LastRun::redacted`] does.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct LastRun {
    schema_version: SchemaVersion,
    /// `run`, `seed`, `snapshot`, `build` or `test`.
    pub(super) command: String,
    /// The options as typed, in the command's order, without the values of the options
    /// in `withheld` and without what followed `--`.
    pub(super) args: Vec<String>,
    /// The options given whose values aren't kept, by long name (`vars`), and `--` if
    /// arguments followed it (#321, since 1.3). A retry must be given them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) withheld: Vec<String>,
    /// When it started.
    pub(super) recorded_at: Timestamp,
    /// How the run ended, once dbt built (#292, since 1.1). `None` in a file from an
    /// older ODS, or when the run stopped before dbt finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) outcome: Option<LastOutcome>,
    /// The state scope it ran for, e.g. `shop/dev`, once it built (#311, since 1.2):
    /// the file is kept per database, not per scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) scope: Option<String>,
    /// The run's id, as a snapshot it commits records it, once it built (#311, since
    /// 1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_id: Option<String>,
}

impl std::fmt::Debug for LastRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LastRun")
            .field("schema_version", &self.schema_version)
            .field("command", &self.command)
            .field("args", &redact(&self.args))
            .field("withheld", &self.withheld)
            .field("recorded_at", &self.recorded_at)
            .field("outcome", &self.outcome)
            .field("scope", &self.scope)
            .field("run_id", &self.run_id)
            .finish()
    }
}

/// What failed in the last run (#292): what `retry --failed` builds again. Sorted, so
/// the same outcome writes the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct LastOutcome {
    /// Nodes that failed, whose tests failed, or that dbt ran but ODS couldn't record.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) failed: BTreeSet<String>,
    /// Nodes skipped because of a failure: a parent's or a source test's, or, in a
    /// retry, held back because a parent they read wasn't built.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) skipped: BTreeSet<String>,
    /// Sources whose tests failed (#232).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) failed_source_tests: BTreeSet<String>,
}

impl LastOutcome {
    /// What `execution` shows failed. `recorded` is whether the run was recorded: if
    /// it wasn't, nothing it did counts, so its successes must build again too.
    pub(super) fn of(execution: &ExecutionReport, recorded: bool) -> Self {
        let mut outcome = Self::default();
        for node in &execution.nodes {
            match node.status {
                ExecutionStatus::Success if recorded && node.checks_failed.is_empty() => {}
                ExecutionStatus::Skipped => {
                    outcome.skipped.insert(node.node.clone());
                }
                _ => {
                    outcome.failed.insert(node.node.clone());
                }
            }
        }
        for source in &execution.sources {
            if !recorded
                || source.status == ExecutionStatus::Failed
                || !source.checks_failed.is_empty()
            {
                outcome.failed_source_tests.insert(source.node.clone());
            }
        }
        outcome
    }

    /// Whether nothing failed.
    pub(super) fn is_empty(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty() && self.failed_source_tests.is_empty()
    }

    /// The nodes to build again: failed and skipped.
    pub(super) fn nodes(&self) -> BTreeSet<String> {
        self.failed.union(&self.skipped).cloned().collect()
    }
}

/// A last run as read, against what 1.3 withholds (#321).
#[derive(Debug)]
enum Scrub {
    /// Written at 1.3 or later: nothing to scrub.
    Current,
    /// Older, and read without its withheld values.
    Scrubbed(Box<LastRun>),
    /// Older, with a command, option or word this build doesn't know.
    Unreadable,
}

/// A retry of only what failed (#292): the run retried, and what failed in it.
#[derive(Debug, Clone)]
pub(super) struct RetryFailed {
    /// The command line retried, to show.
    pub(super) shown: String,
    /// What failed in it.
    pub(super) outcome: LastOutcome,
}

impl LastRun {
    /// The command line to show: `ods state build -s +orders`.
    ///
    /// Quoted as a POSIX shell reads it back, so `$(…)`, `$HOME` or `*` stay literal.
    /// Withheld values read `<not kept>`.
    pub(super) fn shown(&self) -> String {
        let words: Vec<String> = ["ods", "state", self.command.as_str()]
            .into_iter()
            .map(str::to_owned)
            .chain(self.args.iter().cloned())
            .chain(self.withheld_words(NOT_KEPT, NOT_KEPT))
            .collect();
        // Only a NUL byte can't be quoted, and a command line can't hold one.
        shlex::try_join(words.iter().map(String::as_str)).unwrap_or_else(|_| words.join(" "))
    }

    /// The withheld options as words, each value reading `value`, then `--` and `tail`.
    fn withheld_words<'a>(
        &'a self,
        value: &'a str,
        tail: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        let options = self
            .withheld
            .iter()
            .filter(|w| *w != PASSTHROUGH)
            .flat_map(move |name| [format!("--{name}"), value.to_owned()]);
        let rest = self
            .withheld
            .iter()
            .any(|w| w == PASSTHROUGH)
            .then(|| [PASSTHROUGH.to_owned(), tail.to_owned()])
            .into_iter()
            .flatten();
        options.chain(rest)
    }

    /// `ods state retry`, with `--failed` if `failed`, and what the last run withheld
    /// to give again, as placeholders: what the dashboard offers to copy (#321).
    pub(super) fn retry_line(&self, failed: bool) -> String {
        let words: Vec<String> = ["ods", "state", "retry"]
            .into_iter()
            .chain(failed.then_some("--failed"))
            .map(str::to_owned)
            .chain(self.withheld_words(VALUE_AGAIN, ARGUMENTS_AGAIN))
            .collect();
        shlex::try_join(words.iter().map(String::as_str)).unwrap_or_else(|_| words.join(" "))
    }

    /// The same run, without what a file older than 1.3 kept of the values now withheld:
    /// read through `command`, the command it ran (#321).
    fn scrubbed(&self, command: Option<&Command>) -> Scrub {
        if self.schema_version >= WITHHOLDING_SINCE {
            return Scrub::Current;
        }
        // A command, option or word this build doesn't know: what it held can't be
        // told apart, so none of it is kept, nor retried with a wider selection.
        match command.and_then(|command| split(command, &self.args)) {
            Some((args, withheld)) => Scrub::Scrubbed(Box::new(Self {
                schema_version: LAST_RUN_VERSION,
                args,
                withheld,
                ..self.clone()
            })),
            None => Scrub::Unreadable,
        }
    }
}

/// The last run kept beside `state_db`, and where, for the dashboard (#311): `None`
/// when there is none, or it can't be read (logged: the dashboard shows the rest).
pub(super) fn peek(state_db: &Path) -> Option<(PathBuf, LastRun)> {
    let path = path_for(state_db);
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), "can't read the last run: {e}");
            return None;
        }
    };
    match serde_json::from_slice::<LastRun>(&text) {
        Ok(last) if LAST_RUN_VERSION.can_read(last.schema_version) => {
            match last.scrubbed(command_named(&last.command).as_ref()) {
                Scrub::Current => Some((path, last)),
                Scrub::Scrubbed(scrubbed) => Some((path, *scrubbed)),
                Scrub::Unreadable => {
                    tracing::warn!(path = %path.display(), "the last run was kept by an older ODS with options this one doesn't know");
                    None
                }
            }
        }
        Ok(_) => {
            tracing::warn!(path = %path.display(), "the last run was kept by a newer ODS");
            None
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "can't read the last run: {e}");
            None
        }
    }
}

impl LastRun {
    /// The command line to show outside the terminal (the dashboard): option names,
    /// and only the values of [`SHOWN_VALUES`]; every other value and everything after
    /// `--` reads `<redacted>`, as they may carry secrets (`--vars`, AGENTS.md rule 9).
    pub(super) fn redacted(&self) -> String {
        let words: Vec<String> = ["ods".to_owned(), "state".to_owned(), self.command.clone()]
            .into_iter()
            .chain(redact(&self.args))
            .chain(self.withheld_words(REDACTED, REDACTED))
            .collect();
        shlex::try_join(words.iter().map(String::as_str)).unwrap_or_else(|_| words.join(" "))
    }
}

/// `args` with option names kept, the values of [`SHOWN_VALUES`] kept, and every other
/// value and everything after `--` replaced by `<redacted>`.
fn redact(args: &[String]) -> Vec<String> {
    let mut words = Vec::new();
    let mut option = "";
    for arg in args {
        if arg == "--" {
            words.push("--".to_owned());
            words.push(REDACTED.to_owned());
            break;
        }
        if let Some(name) = arg.strip_prefix("--") {
            option = name;
            words.push(arg.clone());
        } else if SHOWN_VALUES.contains(&option) {
            words.push(arg.clone());
        } else {
            words.push(REDACTED.to_owned());
        }
    }
    words
}

/// Where the last run beside `state_db` is kept.
fn path_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".last-run.json");
    PathBuf::from(name)
}

/// The command `name` (`build`, `test`, …), to read a kept command line with.
fn command_named(name: &str) -> Option<Command> {
    if name == "test" {
        return Some(super::state_test::test_command());
    }
    super::state_run::Kind::named(name).map(super::state_run::build_command)
}

/// `args`, a command line [`typed`] for `command`, split into what is kept and the names
/// of what is withheld: the values of options not in [`KEPT_VALUES`] (`vars`), and what
/// follows `--`. `None` when it holds an option `command` doesn't know, or a word with
/// no option before it: what is a value can't be told then.
fn split(command: &Command, args: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let mut kept = Vec::new();
    let mut withheld: Vec<String> = Vec::new();
    let mut words = args.iter().peekable();
    while let Some(word) = words.next() {
        if word == PASSTHROUGH {
            if words.peek().is_some() {
                withheld.push(PASSTHROUGH.to_owned());
            }
            break;
        }
        let name = word.strip_prefix("--")?;
        let arg = command
            .get_arguments()
            .find(|a| a.get_long() == Some(name))?;
        let takes_value = !matches!(
            arg.get_action(),
            ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
        );
        if !takes_value {
            kept.push(word.clone());
        } else if KEPT_VALUES.contains(&name) {
            kept.push(word.clone());
            kept.push(words.next()?.clone());
        } else {
            if !withheld.iter().any(|w| w == name) {
                withheld.push(name.to_owned());
            }
            words.next();
        }
    }
    Some((kept, withheld))
}

/// The options typed for `command`, as `args` parsed them: flags and values set on the
/// command line, in the command's order, then `--` and what followed it. Defaults and
/// values from the environment are left out.
fn typed(command: &Command, args: &ArgMatches) -> Vec<String> {
    let mut out = Vec::new();
    let mut tail = Vec::new();
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if args.value_source(id) != Some(ValueSource::CommandLine) {
            continue;
        }
        let values: Vec<String> = args
            .get_raw(id)
            .into_iter()
            .flatten()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        let Some(long) = arg.get_long() else {
            // The only positional is what follows `--`.
            tail = values;
            continue;
        };
        if matches!(arg.get_action(), ArgAction::SetTrue) {
            out.push(format!("--{long}"));
        } else {
            for value in values {
                out.push(format!("--{long}"));
                out.push(value);
            }
        }
    }
    if !tail.is_empty() {
        out.push("--".to_owned());
        out.extend(tail);
    }
    out
}

/// Keeps `args`, parsed by `command`, as the last run for `settings`' state database,
/// before it runs, so a run that stops early can still be retried. Dry runs aren't
/// kept. A failure to write it is a warning: the run itself goes on.
///
/// Returns the run kept, to add how it ended once that is known.
pub(super) fn remember(
    command: &Command,
    args: &ArgMatches,
    settings: &StateSettings,
) -> Option<Remembered> {
    if args.try_get_one::<bool>("dry-run").ok().flatten() == Some(&true) {
        return None;
    }
    // `typed` writes only options `command` has, so it always splits.
    let Some((kept, withheld)) = split(command, &typed(command, args)) else {
        tracing::warn!("can't keep this run for `ods state retry`: its options can't be read");
        return None;
    };
    let remembered = Remembered {
        path: path_for(&settings.state_db()),
        last: LastRun {
            schema_version: LAST_RUN_VERSION,
            command: command.get_name().to_owned(),
            args: kept,
            withheld,
            recorded_at: Timestamp::now(),
            outcome: None,
            scope: None,
            run_id: None,
        },
    };
    remembered.keep();
    Some(remembered)
}

/// A run kept for retry, whose outcome isn't known yet.
#[derive(Debug)]
pub(super) struct Remembered {
    path: PathBuf,
    last: LastRun,
}

impl Remembered {
    /// Keeps how the run ended beside its command line (#292), with the scope it ran
    /// for and its id, if it got one (#311).
    pub(super) fn finish(mut self, outcome: LastOutcome, scope: String, run_id: Option<String>) {
        self.last.outcome = Some(outcome);
        self.last.scope = Some(scope);
        self.last.run_id = run_id;
        self.keep();
    }

    fn keep(&self) {
        if let Err(e) = write(&self.path, &self.last) {
            tracing::warn!(path = %self.path.display(), "can't keep this run for `ods state retry`: {e}");
        }
    }
}

fn write(path: &Path, last: &LastRun) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(last).map_err(std::io::Error::other)?;
    // Written whole to a temporary file beside it, then renamed: a reader never sees
    // half of it, and a failed write leaves nothing behind.
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut partial = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut partial, &json)?;
    partial.persist(path).map(drop).map_err(|e| e.error)
}

/// The last run for the state database `args` and `config` name, the command line to
/// run again (`state <command> <args…>`, with `--dry-run` if the retry has it) and,
/// with `--failed`, what failed in it.
///
/// # Errors
/// There is no last run, it can't be read, or, with `--failed`, nothing in it is
/// recorded as failed.
pub(super) fn last_run(
    args: &ArgMatches,
    config: &ods_config::Loaded,
) -> Result<(LastRun, Vec<String>, Option<RetryFailed>), CliError> {
    let db = StateSettings::resolve(args, config)?.state_db();
    let dry_run = args.get_flag("dry-run");
    let path = path_for(&db);
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                format!("there is no run to retry for `{}`", db.display()),
            )
            .with_hint("`ods state run`, `seed`, `snapshot`, `build` and `test` keep their command line for retry; dry runs don't"));
        }
        Err(e) => {
            return Err(CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                format!("can't read `{}`: {e}", path.display()),
            ));
        }
    };
    let unreadable = |why: String| {
        CliError::new(
            ExitStatus::Failure,
            codes::STATE_INPUT,
            format!("the last run in `{}` can't be read: {why}", path.display()),
        )
        .with_hint("run the command again yourself; retry will then use it")
    };
    let last: LastRun = serde_json::from_slice(&text).map_err(|e| unreadable(e.to_string()))?;
    if !LAST_RUN_VERSION.can_read(last.schema_version) {
        return Err(unreadable(format!(
            "it was written by a newer ODS (version {}.{})",
            last.schema_version.major, last.schema_version.minor
        )));
    }
    // A file from before 1.3 may hold `--vars` or dbt's arguments: they are dropped, and
    // the file rewritten without them (#321).
    // A dry run rewrites it too: a secret kept isn't a change worth keeping.
    let last = match last.scrubbed(command_named(&last.command).as_ref()) {
        Scrub::Current => last,
        Scrub::Scrubbed(scrubbed) => {
            if let Err(e) = write(&path, &scrubbed) {
                tracing::warn!(path = %path.display(), "can't rewrite the last run without its withheld values: {e}");
            }
            *scrubbed
        }
        Scrub::Unreadable => {
            // It may hold a secret that can't be told apart: it goes.
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), "can't remove the last run: {e}");
            }
            return Err(unreadable(
                "it was kept by an older ODS with options this one doesn't know, so it was removed"
                    .to_owned(),
            ));
        }
    };
    if dry_run && last.command == "test" {
        return Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!("the last run, `{}`, has no dry run", last.shown()),
        )
        .with_hint("`ods state test` only runs tests; retry without --dry-run to run them again"));
    }
    let failed = if args.get_flag("failed") {
        Some(failures(&last)?)
    } else {
        None
    };
    let given = given_again(args, &last)?;
    let mut line = vec!["state".to_owned(), last.command.clone()];
    line.extend(last.args.iter().cloned());
    for (name, value) in &given.options {
        line.push(format!("--{name}"));
        line.push(value.clone());
    }
    // The database the retry was found in is the one it runs against, even if the run
    // took its database from configuration that now names another.
    if !last.args.iter().any(|a| a == "--state-db") {
        line.push("--state-db".to_owned());
        line.push(db.display().to_string());
    }
    if dry_run && !last.args.iter().any(|a| a == "--dry-run") {
        line.push("--dry-run".to_owned());
    }
    if !given.passthrough.is_empty() {
        line.push(PASSTHROUGH.to_owned());
        line.extend(given.passthrough);
    }
    Ok((last, line, failed))
}

/// `ods state retry`, with the options this retry was given on the command line
/// (`--state-db`, `--dry-run`, `--failed`), then `missing`: the retry to run instead.
fn retry_again(args: &ArgMatches, missing: &[String]) -> String {
    let mut words = vec!["ods state retry".to_owned()];
    if args.value_source("state-db") == Some(ValueSource::CommandLine)
        && let Some(db) = args.get_one::<String>("state-db")
    {
        words.push("--state-db".to_owned());
        words.push(shlex::try_quote(db).map_or_else(|_| db.clone(), std::borrow::Cow::into_owned));
    }
    for flag in ["dry-run", "failed"] {
        if args.get_flag(flag) {
            words.push(format!("--{flag}"));
        }
    }
    words.extend(missing.iter().cloned());
    words.join(" ")
}

/// What the retry was given again of what the last run withheld.
struct GivenAgain {
    /// Withheld options and their values, in the order the last run had them.
    options: Vec<(String, String)>,
    /// What follows `--`.
    passthrough: Vec<String>,
}

/// The withheld values of `last`, as given again to the retry in `args`: each one the
/// last run had must be given, and none it didn't have (#321). Values never come from
/// the file: they aren't in it.
fn given_again(args: &ArgMatches, last: &LastRun) -> Result<GivenAgain, CliError> {
    let had = |name: &str| last.withheld.iter().any(|w| w == name);
    let passthrough: Vec<String> = args
        .get_many::<String>("dbt-args")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let mut options = Vec::new();
    let mut missing = Vec::new();
    for name in &last.withheld {
        if name == PASSTHROUGH {
            if passthrough.is_empty() {
                missing.push(format!("{PASSTHROUGH} <dbt arguments>"));
            }
            continue;
        }
        match WITHHELD_VALUES
            .contains(&name.as_str())
            .then(|| args.get_one::<String>(name))
            .flatten()
        {
            Some(value) => options.push((name.clone(), value.clone())),
            None => missing.push(format!("--{name} <value>")),
        }
    }
    if !missing.is_empty() {
        return Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!(
                "the last run, `{}`, had values that aren't kept, as they may hold a secret: {}",
                last.shown(),
                missing.join(", ")
            ),
        )
        .with_hint(format!(
            "give them again, as the last run had them: `{}`",
            retry_again(args, &missing)
        )));
    }
    let mut extra: Vec<String> = WITHHELD_VALUES
        .iter()
        .filter(|name| !had(name) && args.get_one::<String>(name).is_some())
        .map(|name| format!("--{name}"))
        .collect();
    if !passthrough.is_empty() && !had(PASSTHROUGH) {
        extra.push(format!("{PASSTHROUGH} <dbt arguments>"));
    }
    if !extra.is_empty() {
        return Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!(
                "the last run, `{}`, didn't have {}: retry runs it as it was",
                last.shown(),
                extra.join(" or ")
            ),
        )
        .with_hint("run the command you want yourself; retry will then use it"));
    }
    Ok(GivenAgain {
        options,
        passthrough,
    })
}

/// What `retry --failed` builds again: what failed in `last`, if anything did.
fn failures(last: &LastRun) -> Result<RetryFailed, CliError> {
    let nothing = |why: String| {
        CliError::new(ExitStatus::Failure, codes::STATE_INPUT, why)
            .with_hint("`ods state retry` without --failed runs it again, planned afresh")
    };
    if last.command == "test" {
        return Err(CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!(
                "the last run, `{}`, only ran tests: --failed retries builds",
                last.shown()
            ),
        )
        .with_hint("`ods state test` already runs only the tests that haven't passed; `ods state retry` runs it again"));
    }
    match &last.outcome {
        None => Err(nothing(format!(
            "nothing is recorded to retry from the last run, `{}`: it was kept by an older ODS, or stopped before dbt finished",
            last.shown()
        ))),
        Some(outcome) if outcome.is_empty() => Err(nothing(format!(
            "the last run, `{}`, succeeded: nothing failed, so there is nothing to retry",
            last.shown()
        ))),
        Some(outcome) => Ok(RetryFailed {
            shown: last.shown(),
            outcome: outcome.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Command {
        Command::new("build")
            .arg(
                Arg::new("select")
                    .long("select")
                    .short('s')
                    .action(ArgAction::Append),
            )
            .arg(
                Arg::new("full-refresh")
                    .long("full-refresh")
                    .action(ArgAction::SetTrue),
            )
            .arg(Arg::new("dbt").long("dbt").default_value("dbt"))
            .arg(
                Arg::new("target")
                    .long("target")
                    .env("ODS_TEST_RETRY_TARGET"),
            )
            .arg(Arg::new("dbt-args").num_args(0..).last(true))
    }

    #[test]
    fn keeps_only_what_was_typed() {
        let matches = command()
            .try_get_matches_from([
                "build",
                "-s",
                "+orders",
                "--full-refresh",
                "--select",
                "b c",
                "--",
                "--threads",
                "8",
            ])
            .unwrap();
        assert_eq!(
            typed(&command(), &matches),
            [
                "--select",
                "+orders",
                "--select",
                "b c",
                "--full-refresh",
                "--",
                "--threads",
                "8"
            ]
        );
        // Defaults aren't kept, so the retry uses what is in effect then.
        let matches = command().try_get_matches_from(["build"]).unwrap();
        assert!(
            typed(&command(), &matches).is_empty(),
            "{:?}",
            typed(&command(), &matches)
        );
    }

    #[test]
    fn the_dashboard_sees_no_option_values_but_the_selection() {
        let last = LastRun {
            schema_version: LAST_RUN_VERSION,
            command: "build".into(),
            args: [
                "--select",
                "+orders",
                "--vars",
                r#"{"password":"x"}"#,
                "--full-refresh",
                "--",
                "--some-dbt-flag",
                "secret",
            ]
            .map(str::to_owned)
            .to_vec(),
            recorded_at: Timestamp::parse("2026-09-29T00:00:00Z").unwrap(),
            withheld: Vec::new(),
            outcome: None,
            scope: None,
            run_id: None,
        };
        let shown = last.redacted();
        assert_eq!(
            shown,
            "ods state build --select +orders --vars '<redacted>' --full-refresh -- '<redacted>'"
        );
        // As kept since 1.3: the values aren't in it, and it reads the same.
        let (args, withheld) = split(&command_named("build").unwrap(), &last.args).unwrap();
        let kept = LastRun {
            args,
            withheld,
            ..last.clone()
        };
        assert_eq!(
            kept.redacted(),
            "ods state build --select +orders --full-refresh --vars '<redacted>' -- '<redacted>'"
        );
        for text in [shown, format!("{last:?}")] {
            assert!(
                !text.contains("password") && !text.contains("secret"),
                "{text}"
            );
            assert!(!text.contains("some-dbt-flag"), "{text}");
        }
    }

    #[test]
    fn peek_reads_only_a_readable_file_it_understands() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        assert!(peek(&db).is_none(), "missing");
        let path = path_for(&db);
        std::fs::write(&path, "{").unwrap();
        assert!(peek(&db).is_none(), "malformed");
        std::fs::write(
            &path,
            r#"{"schema_version":{"major":2,"minor":0},"command":"build","args":[],"recorded_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(peek(&db).is_none(), "written by a newer ODS");
        // 1.1, before the scope and run id: still read, without them.
        std::fs::write(
            &path,
            r#"{"schema_version":{"major":1,"minor":1},"command":"build","args":[],"recorded_at":"2026-01-01T00:00:00Z","outcome":{"failed":["model.a"]}}"#,
        )
        .unwrap();
        let (_, last) = peek(&db).unwrap();
        assert_eq!(last.scope, None);
        assert_eq!(last.run_id, None);
        assert_eq!(last.outcome.unwrap().failed.len(), 1);
    }

    /// ADR-0019 §2: a file written at 1.0, before the outcome, still reads (#292).
    #[test]
    fn reads_a_last_run_without_an_outcome() {
        let old = r#"{
            "schema_version": {"major": 1, "minor": 0},
            "command": "build",
            "args": ["-s", "+orders"],
            "recorded_at": "2026-01-01T00:00:00Z"
        }"#;
        let last: LastRun = serde_json::from_str(old).unwrap();
        assert!(LAST_RUN_VERSION.can_read(last.schema_version));
        assert_eq!(last.outcome, None);
        let error = failures(&last).unwrap_err();
        assert!(
            error.to_string().contains("nothing is recorded to retry"),
            "{error}"
        );
        // An outcome with nothing failed isn't written as a list of empty lists.
        let done = LastRun {
            outcome: Some(LastOutcome::default()),
            ..last
        };
        let json = serde_json::to_value(&done).unwrap();
        assert_eq!(json["outcome"], serde_json::json!({}));
        assert!(
            failures(&done)
                .unwrap_err()
                .to_string()
                .contains("succeeded")
        );
    }

    #[test]
    fn shows_a_command_line_to_copy() {
        let last = LastRun {
            schema_version: LAST_RUN_VERSION,
            command: "build".into(),
            args: vec!["-s".into(), "x".into()],
            withheld: vec!["vars".into()],
            recorded_at: Timestamp::from_unix(0),
            outcome: None,
            scope: None,
            run_id: None,
        };
        assert_eq!(last.shown(), "ods state build -s x --vars '<not kept>'");
        let odd = LastRun {
            args: vec![
                "--select".into(),
                "$(id)".into(),
                "--exclude".into(),
                "it's".into(),
                "--select".into(),
                String::new(),
            ],
            withheld: vec![PASSTHROUGH.into()],
            ..last
        };
        assert_eq!(
            odd.shown(),
            "ods state build --select '$(id)' --exclude \"it's\" --select '' -- '<not kept>'"
        );
    }

    /// #321: every option that takes a value, of every command that keeps its line, is
    /// either kept or withheld; a new one must be put in one list or the other.
    #[test]
    fn every_value_option_is_kept_or_withheld() {
        let commands = ["run", "seed", "snapshot", "build", "test"]
            .map(|name| command_named(name).unwrap_or_else(|| panic!("{name}")));
        for command in &commands {
            for arg in command.get_arguments() {
                let Some(long) = arg.get_long() else { continue };
                let flag = matches!(
                    arg.get_action(),
                    ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
                );
                assert!(
                    flag || KEPT_VALUES.contains(&long) || WITHHELD_VALUES.contains(&long),
                    "`{} --{long}` is in neither KEPT_VALUES nor WITHHELD_VALUES",
                    command.get_name()
                );
            }
        }
        // And retry takes each withheld option again.
        let retry = retry_command();
        for name in WITHHELD_VALUES {
            assert!(
                retry.get_arguments().any(|a| a.get_long() == Some(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn splits_off_what_may_be_secret() {
        let build = command_named("build").unwrap();
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let line = words(&[
            "--select",
            "+orders",
            "--vars",
            "--looks-like-an-option",
            "--full-refresh",
            "--target",
            "dev",
            "--",
            "--log-path",
            "s3cr3t",
        ]);
        let (kept, withheld) = split(&build, &line).unwrap();
        assert_eq!(
            kept,
            ["--select", "+orders", "--full-refresh", "--target", "dev"]
        );
        assert_eq!(withheld, ["vars", "--"]);
        // `--` with nothing after it withholds nothing.
        let (_, withheld) = split(&build, &words(&["--"])).unwrap();
        assert!(withheld.is_empty(), "{withheld:?}");
        // What can't be told apart isn't split at all: an option this build doesn't
        // know, a word with no option before it, a kept option without its value.
        for line in [&["--gone", "--tok3n"][..], &["-s", "x"], &["--select"]] {
            assert_eq!(split(&build, &words(line)), None, "{line:?}");
        }
    }

    #[test]
    fn the_dashboard_offers_a_retry_with_what_to_give_again() {
        let last = LastRun {
            schema_version: LAST_RUN_VERSION,
            command: "build".into(),
            args: vec!["--select".into(), "x".into()],
            withheld: vec!["vars".into(), PASSTHROUGH.into()],
            recorded_at: Timestamp::from_unix(0),
            outcome: None,
            scope: None,
            run_id: None,
        };
        assert_eq!(
            last.retry_line(true),
            "ods state retry --failed --vars '<value>' -- '<dbt arguments>'"
        );
        let plain = LastRun {
            withheld: Vec::new(),
            ..last
        };
        assert_eq!(plain.retry_line(false), "ods state retry");
    }

    #[test]
    fn a_file_older_than_1_3_is_scrubbed_as_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        std::fs::write(
            path_for(&db),
            r#"{"schema_version":{"major":1,"minor":2},"command":"build","args":["--select","x","--vars","{\"password\":\"hunter2\"}","--","--log-path","s3cr3t"],"recorded_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let (_, last) = peek(&db).unwrap();
        let text = format!(
            "{} {last:?} {}",
            last.shown(),
            serde_json::to_string(&last).unwrap()
        );
        assert!(
            !text.contains("hunter2") && !text.contains("s3cr3t"),
            "{text}"
        );
        assert_eq!(last.withheld, ["vars", "--"]);
        assert_eq!(last.args, ["--select", "x"]);
        assert_eq!(last.schema_version, LAST_RUN_VERSION);
        // A command, or an option, this build doesn't know: none of it can be kept.
        let old = LastRun {
            schema_version: SchemaVersion::new(1, 2),
            args: vec!["--vars".into(), "hunter2".into()],
            ..last.clone()
        };
        assert!(matches!(old.scrubbed(None), Scrub::Unreadable));
        let gone = LastRun {
            args: vec!["--gone".into(), "hunter2".into()],
            ..old
        };
        assert!(matches!(
            gone.scrubbed(command_named("build").as_ref()),
            Scrub::Unreadable
        ));
        // A 1.3 file is left as it is.
        assert!(matches!(
            last.scrubbed(command_named("build").as_ref()),
            Scrub::Current
        ));
    }
}
