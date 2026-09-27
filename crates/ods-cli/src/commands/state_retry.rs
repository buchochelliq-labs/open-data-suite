//! `ods state retry` (#276): rerun the last `ods state` command that ran dbt, with the
//! options it was given, planned afresh.
//!
//! Every `run`, `seed`, `snapshot`, `build` and `test` that isn't a dry run keeps its
//! command line beside the state database, in `<state-db>.last-run.json`. Only what was
//! typed is kept: environment variables and configuration are read again when retrying,
//! as for any command, so nothing from them is written down (AGENTS.md rule 9).

use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::{Deserialize, Serialize};

use super::state_settings::{DEFAULT_STORE, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};

/// The version of the last-run file this build writes and reads.
const LAST_RUN_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

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
    recorded_at: Timestamp,
}

impl LastRun {
    /// The command line to show: `ods state build -s +orders`.
    pub(super) fn shown(&self) -> String {
        let mut words = vec!["ods".to_owned(), "state".to_owned(), self.command.clone()];
        words.extend(self.args.iter().map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains(['\'', '"']) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        }));
        words.join(" ")
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

/// Keeps `args`, parsed by `command`, as the last run for `settings`' state database.
/// Dry runs aren't kept. A failure to write it is a warning: the run itself goes on.
pub(super) fn remember(command: &Command, args: &ArgMatches, settings: &StateSettings) {
    if args.try_get_one::<bool>("dry-run").ok().flatten() == Some(&true) {
        return;
    }
    let last = LastRun {
        schema_version: LAST_RUN_VERSION,
        command: command.get_name().to_owned(),
        args: typed(command, args),
        recorded_at: Timestamp::now(),
    };
    let path = path_for(&settings.state_db());
    if let Err(e) = write(&path, &last) {
        tracing::warn!(path = %path.display(), "can't keep this run for `ods state retry`: {e}");
    }
}

fn write(path: &Path, last: &LastRun) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(last).map_err(std::io::Error::other)?;
    // Written whole, then renamed: a reader never sees half of it.
    let mut partial = path.as_os_str().to_owned();
    partial.push(format!(".{}.partial", std::process::id()));
    let partial = PathBuf::from(partial);
    std::fs::write(&partial, json)?;
    std::fs::rename(&partial, path)
}

/// The last run for the state database `args` and `config` name, and the command line
/// to run again: `state <command> <args…>`, with `--dry-run` if the retry has it.
///
/// # Errors
/// There is no last run, or it can't be read.
pub(super) fn last_run(
    args: &ArgMatches,
    config: &ods_config::Loaded,
) -> Result<(LastRun, Vec<String>), CliError> {
    let db = StateSettings::resolve(args, config)?.state_db();
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
    let mut line = vec!["state".to_owned(), last.command.clone()];
    let split = last
        .args
        .iter()
        .position(|a| a == "--")
        .unwrap_or(last.args.len());
    line.extend(last.args[..split].iter().cloned());
    if args.get_flag("dry-run") && !last.args[..split].iter().any(|a| a == "--dry-run") {
        line.push("--dry-run".to_owned());
    }
    line.extend(last.args[split..].iter().cloned());
    Ok((last, line))
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

    #[test]
    fn shows_a_command_line_to_copy() {
        let last = LastRun {
            schema_version: LAST_RUN_VERSION,
            command: "build".into(),
            args: vec!["--vars".into(), "{a: 1}".into(), "-s".into(), "x".into()],
            recorded_at: Timestamp::from_unix(0),
        };
        assert_eq!(last.shown(), "ods state build --vars '{a: 1}' -s x");
    }
}
