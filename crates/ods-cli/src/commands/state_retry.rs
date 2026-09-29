//! `ods state retry` (#276): rerun the last `ods state` command that ran dbt, with the
//! options it was given, planned afresh.
//!
//! Every `run`, `seed`, `snapshot`, `build` and `test` that isn't a dry run keeps its
//! command line beside the state database, in `<state-db>.last-run.json`. Only what was
//! typed is kept: environment variables and configuration are read again when retrying,
//! as for any command, so nothing from them is written down (AGENTS.md rule 9).
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
/// optional `outcome` (#292); a 1.0 file still reads, without one.
const LAST_RUN_VERSION: SchemaVersion = SchemaVersion::new(1, 1);

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
}

/// The last run, as kept beside the state database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct LastRun {
    schema_version: SchemaVersion,
    /// `run`, `seed`, `snapshot`, `build` or `test`.
    pub(super) command: String,
    /// The options as typed, in the command's order, then `--` and dbt's.
    pub(super) args: Vec<String>,
    /// When it started.
    pub(super) recorded_at: Timestamp,
    /// How the run ended, once dbt built (#292, since 1.1). `None` in a file from an
    /// older ODS, or when the run stopped before dbt finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) outcome: Option<LastOutcome>,
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
    pub(super) fn shown(&self) -> String {
        let words = ["ods", "state", self.command.as_str()]
            .into_iter()
            .chain(self.args.iter().map(String::as_str));
        // Only a NUL byte can't be quoted, and a command line can't hold one.
        shlex::try_join(words.clone()).unwrap_or_else(|_| words.collect::<Vec<_>>().join(" "))
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
        Ok(last) if LAST_RUN_VERSION.can_read(last.schema_version) => Some((path, last)),
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

/// Where the last run beside `state_db` is kept.
fn path_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".last-run.json");
    PathBuf::from(name)
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
    let remembered = Remembered {
        path: path_for(&settings.state_db()),
        last: LastRun {
            schema_version: LAST_RUN_VERSION,
            command: command.get_name().to_owned(),
            args: typed(command, args),
            recorded_at: Timestamp::now(),
            outcome: None,
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
    /// Keeps how the run ended beside its command line (#292).
    pub(super) fn finish(mut self, outcome: LastOutcome) {
        self.last.outcome = Some(outcome);
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
    let mut line = vec!["state".to_owned(), last.command.clone()];
    let split = last
        .args
        .iter()
        .position(|a| a == "--")
        .unwrap_or(last.args.len());
    let options = &last.args[..split];
    line.extend(options.iter().cloned());
    // The database the retry was found in is the one it runs against, even if the run
    // took its database from configuration that now names another.
    if !options.iter().any(|a| a == "--state-db") {
        line.push("--state-db".to_owned());
        line.push(db.display().to_string());
    }
    if dry_run && !options.iter().any(|a| a == "--dry-run") {
        line.push("--dry-run".to_owned());
    }
    line.extend(last.args[split..].iter().cloned());
    Ok((last, line, failed))
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
        assert!(typed(&command(), &matches).is_empty());
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
            args: vec!["--vars".into(), "{a: 1}".into(), "-s".into(), "x".into()],
            recorded_at: Timestamp::from_unix(0),
            outcome: None,
        };
        assert_eq!(last.shown(), "ods state build --vars '{a: 1}' -s x");
        let odd = LastRun {
            args: vec![
                "--".into(),
                "--log-path".into(),
                "$(id)".into(),
                "it's".into(),
                "+orders".into(),
                String::new(),
            ],
            ..last
        };
        assert_eq!(
            odd.shown(),
            "ods state build -- --log-path '$(id)' \"it's\" +orders ''"
        );
    }
}
