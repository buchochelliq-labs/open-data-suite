//! Assembles the `ods` command and runs one invocation (ADR-0004).
//!
//! [`run`] takes explicit streams and environment so the whole CLI is testable in-process;
//! `main.rs` only supplies the real ones.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;

use clap::error::ErrorKind;
use clap::{Args, Command, FromArgMatches};

use crate::exit::{CliError, ExitStatus};
use crate::logging::{self, LogArgs};
use crate::module::{Context, Registry};
use crate::output::{ColorChoice, Mode, OutputArgs, OutputSettings};
use crate::present;

/// Flags accepted by every command.
#[derive(Debug, Clone, Args)]
struct GlobalArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[command(flatten)]
    log: LogArgs,
}

/// Where an invocation reads its environment and writes its output.
pub struct Io<'a> {
    /// Results (and the JSON envelope, including failures).
    pub out: &'a mut dyn Write,
    /// Human-readable errors. Logs go to the process's stderr directly.
    pub err: &'a mut dyn Write,
    /// Whether stdout is a terminal.
    pub stdout_is_terminal: bool,
    /// Whether stderr is a terminal (controls log colour).
    pub stderr_is_terminal: bool,
    /// Value of `ODS_LOG`, if set.
    pub ods_log: Option<String>,
    /// Whether `NO_COLOR` is set to a non-empty value.
    pub no_color: bool,
}

/// The root `ods` command with global flags and every registered module.
pub fn root_command(registry: &Registry) -> Command {
    let root = Command::new("ods")
        .version(env!("CARGO_PKG_VERSION"))
        .about("OpenDataSuite: explainable control plane for analytics engineering")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .after_long_help(exit_status_help());
    registry.attach(GlobalArgs::augment_args(root))
}

fn exit_status_help() -> String {
    let mut help = String::from("Exit status:\n");
    for status in ExitStatus::ALL {
        let _ = writeln!(help, "  {}  {}", status.code(), status.name());
    }
    help.push_str("\nSee docs/cli.md for details, error codes and environment variables.");
    help
}

/// Runs one invocation and returns its exit status.
pub fn run<I, T>(registry: &Registry, args: I, io: &mut Io<'_>) -> ExitStatus
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let root = root_command(registry);
    let matches = match root.clone().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(err) => {
            let text = err.render().to_string();
            // Requested help/version is a success on stdout; anything else is a usage
            // error on stderr (ADR-0004 §4: output settings are not known yet).
            return if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = io.out.write_all(text.as_bytes());
                ExitStatus::Success
            } else {
                let _ = io.err.write_all(text.as_bytes());
                ExitStatus::Usage
            };
        }
    };

    let globals = match GlobalArgs::from_arg_matches(&matches) {
        Ok(globals) => globals,
        Err(err) => {
            let _ = io.err.write_all(err.render().to_string().as_bytes());
            return ExitStatus::Usage;
        }
    };
    let settings = globals.output.resolve(io.stdout_is_terminal);
    let Some((name, sub_matches)) = matches.subcommand() else {
        // `subcommand_required` makes clap reject this before we get here.
        return ExitStatus::Usage;
    };

    let level = match globals.log.level(io.ods_log.as_deref()) {
        Ok(level) => level,
        Err(value) => {
            let err = CliError::new(
                ExitStatus::Config,
                crate::exit::codes::INVALID_LOG_LEVEL,
                format!("invalid ODS_LOG value `{value}`"),
            )
            .with_hint("use one of off, error, warn, info, debug, trace");
            return report(&err, name, &settings, io.out, io.err);
        }
    };
    let log_ansi = match globals.output.color() {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io.stderr_is_terminal && !io.no_color,
    };
    logging::init(level, log_ansi);
    tracing::debug!(command = name, mode = ?settings.mode, "dispatching");

    let Some(module) = registry.get(name) else {
        return ExitStatus::Usage;
    };
    let mut ctx = Context::new(settings, io.out, &root);
    let result = module.run(sub_matches, &mut ctx).and_then(|()| {
        io.out.flush()?;
        Ok(())
    });
    match result {
        Ok(()) => ExitStatus::Success,
        Err(err) if err.is_broken_pipe() => ExitStatus::Success,
        Err(err) => report(&err, name, &settings, io.out, io.err),
    }
}

/// Reports a command failure once, in the active output mode (ADR-0004 §4).
fn report(
    err: &CliError,
    command: &str,
    settings: &OutputSettings,
    out: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitStatus {
    tracing::debug!(
        code = err.code,
        status = err.status.code(),
        "command failed"
    );
    // Write failures here are ignored: there is nowhere left to report them, and the
    // exit status still carries the outcome.
    if settings.mode == Mode::Json && !err.is_broken_pipe() {
        let _ = present::emit_failure(out, command, err).and_then(|()| out.flush());
    } else {
        let _ = writeln!(stderr, "{err}");
    }
    err.status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::default_registry;

    struct Outcome {
        status: ExitStatus,
        out: String,
        err: String,
    }

    fn invoke(args: &[&str]) -> Outcome {
        invoke_with(args, None)
    }

    fn invoke_with(args: &[&str], ods_log: Option<&str>) -> Outcome {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = run(
            &default_registry(),
            std::iter::once("ods").chain(args.iter().copied()),
            &mut Io {
                out: &mut out,
                err: &mut err,
                stdout_is_terminal: false,
                stderr_is_terminal: false,
                ods_log: ods_log.map(str::to_owned),
                no_color: false,
            },
        );
        Outcome {
            status,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    #[test]
    fn root_command_is_valid() {
        root_command(&default_registry()).debug_assert();
    }

    #[test]
    fn planned_command_reports_not_implemented_on_stderr() {
        let o = invoke(&["state", "plan", "--select", "+orders"]);
        assert_eq!(o.status, ExitStatus::NotImplemented);
        assert!(o.out.is_empty(), "stdout must stay clean: {}", o.out);
        insta::assert_snapshot!(o.err, @r"
        error[ODS-E0003]: `ods state` is not implemented yet
          hint: planned for M1 State MVP (v0.1.0); see docs/ROADMAP.md
        ");
    }

    #[test]
    fn planned_command_in_json_mode_emits_a_failure_envelope() {
        let o = invoke(&["--json", "state"]);
        assert_eq!(o.status, ExitStatus::NotImplemented);
        assert!(
            o.err.is_empty(),
            "JSON mode reports failures in the envelope: {}",
            o.err
        );
        insta::assert_snapshot!(o.out.replace(env!("CARGO_PKG_VERSION"), "[ods-version]"));
    }

    #[test]
    fn missing_subcommand_is_a_usage_error_with_help() {
        let o = invoke(&[]);
        assert_eq!(o.status, ExitStatus::Usage);
        assert!(o.err.contains("Usage: ods"), "{}", o.err);
    }

    #[test]
    fn unknown_command_and_bad_flags_are_usage_errors() {
        assert_eq!(invoke(&["nope"]).status, ExitStatus::Usage);
        assert_eq!(
            invoke(&["version", "--output", "yaml"]).status,
            ExitStatus::Usage
        );
        assert_eq!(invoke(&["-v", "-q", "version"]).status, ExitStatus::Usage);
    }

    #[test]
    fn help_and_version_flags_succeed_on_stdout() {
        let help = invoke(&["--help"]);
        assert_eq!(help.status, ExitStatus::Success);
        assert!(help.out.contains("Exit status:"), "{}", help.out);
        assert!(
            help.out.contains("[planned: M1 State MVP (v0.1.0)]"),
            "{}",
            help.out
        );
        let version = invoke(&["--version"]);
        assert_eq!(version.status, ExitStatus::Success);
        assert_eq!(
            version.out.trim(),
            format!("ods {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn invalid_ods_log_is_a_config_error() {
        let o = invoke_with(&["version"], Some("loud"));
        assert_eq!(o.status, ExitStatus::Config);
        assert!(o.err.contains("ODS-E0004"), "{}", o.err);
        assert!(o.out.is_empty());
    }

    #[test]
    fn completions_are_generated_from_the_registry() {
        let o = invoke(&["completions", "bash"]);
        assert_eq!(o.status, ExitStatus::Success);
        for word in ["state", "completions", "--output"] {
            assert!(o.out.contains(word), "completion script lacks {word}");
        }
    }

    #[test]
    fn version_runs_through_the_registry() {
        let o = invoke(&["version", "-o", "plain"]);
        assert_eq!(o.status, ExitStatus::Success);
        assert!(o.out.starts_with("ods: "), "{}", o.out);
    }
}
