//! Assembles the `ods` command and runs one invocation (ADR-0004).
//!
//! [`run`] takes explicit streams and environment so the whole CLI is testable in-process;
//! `main.rs` only supplies the real ones.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{ArgMatches, Args, Command, FromArgMatches};

use crate::exit::{CliError, ExitStatus};
use crate::logging::{self, LogArgs};
use crate::module::{Context, ProgressSettings, Registry};
use crate::output::{ColorChoice, Mode, OutputArgs, OutputSettings};
use crate::present;
use ods_config::Inputs;
use tracing::level_filters::LevelFilter;

/// Flags accepted by every command.
#[derive(Debug, Clone, Args)]
struct GlobalArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[command(flatten)]
    log: LogArgs,
    /// Configuration profile to use [env: `ODS_PROFILE`]
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
}

/// Where an invocation reads its environment and writes its output.
// The flags are independent observations about the process (two TTY checks, two
// environment checks), not an encoded state machine, so the lint does not apply.
#[allow(clippy::struct_excessive_bools)]
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
    /// Whether `TERM` names a terminal without styles (`dumb`, `unknown`).
    pub dumb_terminal: bool,
    /// Working directory, where configuration discovery starts. `None` loads no files.
    pub cwd: Option<PathBuf>,
    /// Process environment, for configuration (`ODS__*`, `ODS_PROFILE`, config dirs).
    pub env: Vec<(String, String)>,
    /// Names of `ODS…` variables whose value is not valid UTF-8. Reported as a
    /// configuration error rather than silently ignored.
    pub invalid_env: Vec<String>,
}

/// The root `ods` command with global flags and every registered module.
pub fn root_command(registry: &Registry) -> Command {
    let root = Command::new("ods")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true)
        .after_long_help(exit_status_help());
    // `about` is set after flattening the flag structs: clap copies a flattened
    // struct's doc comment into `about`, which would replace the headline.
    registry
        .attach(GlobalArgs::augment_args(root))
        .about("OpenDataSuite: explainable control plane for analytics engineering")
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
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let root = root_command(registry);
    let mut matches = match parse(&root, &args, io) {
        Ok(matches) => matches,
        Err(status) => return status,
    };
    // Global flags captured by a passthrough argument are moved in front of it and the
    // line is parsed once more, so `ods state explain --json` behaves like
    // `ods state --json plan` (ADR-0004 §1).
    if let Some(hoisted) = hoist_passthrough_globals(registry, &matches, &args) {
        matches = match parse(&root, &hoisted, io) {
            Ok(matches) => matches,
            Err(status) => return status,
        };
    }

    let globals = match GlobalArgs::from_arg_matches(&matches) {
        Ok(globals) => globals,
        Err(err) => {
            let _ = io.err.write_all(err.render().to_string().as_bytes());
            return ExitStatus::Usage;
        }
    };
    let Some((name, sub_matches)) = matches.subcommand() else {
        // `subcommand_required` makes clap reject this before we get here.
        return ExitStatus::Usage;
    };

    // Configuration (ADR-0005). Errors are reported using the flags alone.
    if let Some(var) = io.invalid_env.first() {
        let settings = globals.output.resolve(io.stdout_is_terminal);
        let err = CliError::new(
            ExitStatus::Config,
            "ODS-E0102",
            format!("environment variable {var} is not valid UTF-8"),
        );
        return report(&err, name, &settings, io.out, io.err);
    }
    let mut inputs = match &io.cwd {
        Some(cwd) => Inputs::discover(cwd, &io.env),
        None => Inputs {
            env: io.env.clone(),
            ..Inputs::default()
        },
    };
    inputs.profile_flag.clone_from(&globals.profile);
    inputs.flags = globals.output.flag_values();
    let loaded = match ods_config::load(&inputs) {
        Ok(loaded) => loaded,
        Err(err) => {
            let settings = globals.output.resolve(io.stdout_is_terminal);
            let err = CliError::new(ExitStatus::Config, err.code(), err.to_string())
                .with_hint("fix the value named above; see docs/cli.md#configuration");
            return report(&err, name, &settings, io.out, io.err);
        }
    };
    let settings = OutputSettings::resolve(&loaded.config.output, io.stdout_is_terminal);

    let configured_level = loaded.config.log.level.map(logging::from_config);
    let level = match globals.log.level(io.ods_log.as_deref(), configured_level) {
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
    let log_ansi = match settings.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io.stderr_is_terminal && !io.no_color && !io.dumb_terminal,
    };
    logging::init(level, log_ansi);
    tracing::debug!(command = name, mode = ?settings.mode, "dispatching");

    let Some(module) = registry.get(name) else {
        return ExitStatus::Usage;
    };
    // Progress is information: `-q` (errors only) or `ODS_LOG=off` turns it off.
    let progress = ProgressSettings {
        enabled: level >= LevelFilter::WARN,
        ansi: log_ansi,
    };
    let mut ctx = Context::new(settings, &loaded, io.out, &root).with_progress(progress);
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

/// Parses `args`, handling clap's own exits: requested help/version is a success on
/// stdout; anything else is a usage error on stderr (ADR-0004 §4: output settings are
/// not known yet).
fn parse(root: &Command, args: &[OsString], io: &mut Io<'_>) -> Result<ArgMatches, ExitStatus> {
    root.clone().try_get_matches_from(args).map_err(|err| {
        let text = err.render().to_string();
        if matches!(
            err.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        ) {
            let _ = io.out.write_all(text.as_bytes());
            ExitStatus::Success
        } else {
            let _ = io.err.write_all(text.as_bytes());
            ExitStatus::Usage
        }
    })
}

/// If the invoked module captured a trailing tail that contains global flags, returns
/// the argument list with those flags moved in front of the tail.
fn hoist_passthrough_globals(
    registry: &Registry,
    matches: &ArgMatches,
    args: &[OsString],
) -> Option<Vec<OsString>> {
    let (name, sub_matches) = matches.subcommand()?;
    let id = registry.get(name)?.passthrough_arg()?;
    let tail: Vec<OsString> = sub_matches.get_raw(id)?.map(OsStr::to_os_string).collect();
    // The captured tail is the verbatim end of the line; anything else means clap
    // rewrote it (e.g. a `--` separator), and we leave the line alone.
    let prefix_len = args.len().checked_sub(tail.len())?;
    if args[prefix_len..] != tail[..] {
        return None;
    }
    let (hoisted, rest) = split_global_flags(&tail, &GlobalFlags::from_definitions());
    if hoisted.is_empty() {
        return None;
    }
    Some(
        args[..prefix_len]
            .iter()
            .cloned()
            .chain(hoisted)
            .chain(rest)
            .collect(),
    )
}

/// Spellings of the global flags, read from their clap definitions so the two cannot
/// drift apart. The `bool` is whether the flag takes a value.
struct GlobalFlags {
    longs: Vec<(String, bool)>,
    shorts: Vec<(char, bool)>,
}

impl GlobalFlags {
    fn from_definitions() -> Self {
        // `-h`/`--help` are clap built-ins, not part of `GlobalArgs`.
        let mut flags = Self {
            longs: vec![("help".into(), false)],
            shorts: vec![('h', false)],
        };
        let command = GlobalArgs::augment_args(Command::new("ods"));
        for arg in command.get_arguments() {
            let takes_value = arg.get_action().takes_values();
            if let Some(long) = arg.get_long() {
                flags.longs.push((long.to_owned(), takes_value));
            }
            if let Some(short) = arg.get_short() {
                flags.shorts.push((short, takes_value));
            }
        }
        flags
    }

    fn long(&self, name: &str) -> Option<bool> {
        self.longs
            .iter()
            .find(|(long, _)| long == name)
            .map(|&(_, takes)| takes)
    }

    fn short(&self, name: char) -> Option<bool> {
        self.shorts
            .iter()
            .find(|&&(short, _)| short == name)
            .map(|&(_, takes)| takes)
    }
}

/// Splits `tail` into global-flag tokens (with their values) and everything else.
/// Stops at `--`, after which every token is literal.
fn split_global_flags(tail: &[OsString], flags: &GlobalFlags) -> (Vec<OsString>, Vec<OsString>) {
    let (mut hoisted, mut rest) = (Vec::new(), Vec::new());
    let mut tokens = tail.iter();
    while let Some(token) = tokens.next() {
        let Some(text) = token.to_str() else {
            rest.push(token.clone());
            continue;
        };
        if text == "--" {
            rest.push(token.clone());
            rest.extend(tokens.cloned());
            break;
        }
        let takes_value = if let Some(long) = text.strip_prefix("--") {
            match long.split_once('=') {
                Some((name, _)) => flags.long(name).map(|_| false),
                None => flags.long(long),
            }
        } else if let Some(cluster) = text.strip_prefix('-').filter(|c| !c.is_empty()) {
            let mut chars = cluster.chars();
            match chars
                .next()
                .and_then(|first| flags.short(first).map(|t| (first, t)))
            {
                // `-o plain` takes the next token; `-oplain` carries its value.
                Some((first, true)) => Some(cluster.len() == first.len_utf8()),
                // A cluster of value-less global shorts, e.g. `-vv` or `-vq`.
                Some((_, false)) if chars.all(|c| flags.short(c) == Some(false)) => Some(false),
                _ => None,
            }
        } else {
            None
        };
        match takes_value {
            Some(needs_next) => {
                hoisted.push(token.clone());
                if needs_next {
                    hoisted.extend(tokens.next().cloned());
                }
            }
            None => rest.push(token.clone()),
        }
    }
    (hoisted, rest)
}

/// Reports a command failure once, in the active output mode (ADR-0004 §4).
///
/// In JSON mode the failure envelope goes to stdout, except when stdout itself is what
/// failed: then, as when writing the envelope fails, the error goes to stderr.
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
    // The command put the error in its envelope, next to its result.
    if settings.mode == Mode::Json && err.is_in_envelope() {
        return err.status;
    }
    if settings.mode == Mode::Json
        && !err.is_output_failure()
        && present::emit_failure(out, command, err)
            .and_then(|()| out.flush())
            .is_ok()
    {
        return err.status;
    }
    // Ignored: there is nowhere left to report it, and the exit status still carries
    // the outcome.
    let _ = writeln!(stderr, "{err}");
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
                dumb_terminal: false,
                cwd: None,
                env: Vec::new(),
                invalid_env: Vec::new(),
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
        let o = invoke(&["state", "explain", "--select", "+orders"]);
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
            help.out.contains("[planned: M3 ERD & Usage (v0.3.0)]"),
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

    #[test]
    fn global_flags_after_planned_arguments_still_apply() {
        let o = invoke(&["state", "explain", "--select", "+orders", "--json"]);
        assert_eq!(o.status, ExitStatus::NotImplemented);
        let value: serde_json::Value = serde_json::from_str(&o.out).expect("one JSON document");
        assert_eq!(value["diagnostics"][0]["code"], "ODS-E0003");
        assert!(o.err.is_empty());

        let bad = invoke(&["state", "explain", "-o", "yaml"]);
        assert_eq!(
            bad.status,
            ExitStatus::Usage,
            "hoisted flags are still validated"
        );

        let help = invoke(&["state", "explain", "--help"]);
        assert_eq!(help.status, ExitStatus::Success);
        assert!(help.out.contains("Usage: ods state"), "{}", help.out);

        let literal = invoke(&["state", "explain", "--", "--json"]);
        assert_eq!(literal.status, ExitStatus::NotImplemented);
        assert!(literal.out.is_empty(), "flags after `--` are literal");
    }

    #[test]
    fn root_help_starts_with_the_product_description() {
        let help = invoke(&["--help"]);
        assert!(help.out.starts_with("OpenDataSuite:"), "{}", help.out);
    }

    #[test]
    fn invalid_ods_log_in_json_mode_is_reported_in_the_envelope() {
        let o = invoke_with(&["--json", "version"], Some("loud"));
        assert_eq!(o.status, ExitStatus::Config);
        let value: serde_json::Value = serde_json::from_str(&o.out).expect("one JSON document");
        assert_eq!(value["diagnostics"][0]["code"], "ODS-E0004");
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_write_failures_go_to_stderr_in_every_mode() {
        for mode in ["json", "plain"] {
            let mut err = Vec::new();
            let status = run(
                &default_registry(),
                ["ods", "version", "-o", mode],
                &mut Io {
                    out: &mut FailingWriter,
                    err: &mut err,
                    stdout_is_terminal: false,
                    stderr_is_terminal: false,
                    ods_log: None,
                    no_color: false,
                    dumb_terminal: false,
                    cwd: None,
                    env: Vec::new(),
                    invalid_env: Vec::new(),
                },
            );
            assert_eq!(status, ExitStatus::Failure, "{mode}");
            let err = String::from_utf8(err).unwrap();
            assert!(err.contains("error[ODS-E0001]"), "{mode}: {err}");
        }
    }

    fn split(tokens: &[&str]) -> (Vec<String>, Vec<String>) {
        let tail: Vec<OsString> = tokens.iter().map(OsString::from).collect();
        let (hoisted, rest) = split_global_flags(&tail, &GlobalFlags::from_definitions());
        let strings = |v: Vec<OsString>| v.into_iter().map(|s| s.into_string().unwrap()).collect();
        (strings(hoisted), strings(rest))
    }

    #[test]
    fn global_flags_are_split_from_passthrough_tails() {
        assert_eq!(
            split(&[
                "plan",
                "--select",
                "+x",
                "--json",
                "-o",
                "plain",
                "-vv",
                "--width=80"
            ]),
            (
                vec![
                    "--json".into(),
                    "-o".into(),
                    "plain".into(),
                    "-vv".into(),
                    "--width=80".into()
                ],
                vec!["plan".into(), "--select".into(), "+x".into()],
            )
        );
        assert_eq!(
            split(&["-oplain", "-x", "-vz"]),
            (vec!["-oplain".into()], vec!["-x".into(), "-vz".into()])
        );
        assert_eq!(
            split(&["a", "--", "--json"]),
            (vec![], vec!["a".into(), "--".into(), "--json".into()])
        );
    }
}
