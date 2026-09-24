//! The `ods` command-line interface.
//!
//! This binary is the composition root (ADR-0001): it is the only place that wires
//! concrete providers into modules, and it owns presentation (ADR-0003). Module
//! subcommands are registered here; their implementations live in the module crates.
//! The full framework is #6.

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ods_cli::{output, present, version};

#[derive(Debug, Parser)]
#[command(
    name = "ods",
    version,
    about = "OpenDataSuite: explainable control plane for analytics engineering"
)]
struct Cli {
    #[command(flatten)]
    output: output::OutputArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Plan and run only what needs to run, with explanations (M1).
    State,
    /// Generate and inspect entity-relationship models (M3).
    Erd,
    /// Inspect real downstream usage of assets and columns (M3).
    Usage,
    /// Change-impact analysis and selective CI (M4).
    Ci,
    /// Run the ODS language server (M5).
    Lsp,
    /// Analytics-engineering agent and skills (M6).
    Agent,
    /// Print build and compatibility version information.
    Version,
}

/// Exit code for commands that exist in the CLI surface but are not implemented yet.
///
/// Distinct from generic failure (1) and usage errors (2) so scripts can detect it.
const EXIT_NOT_IMPLEMENTED: u8 = 3;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let settings = cli.output.resolve(stdout.is_terminal());
    let mut out = stdout.lock();

    let result = match cli.command {
        Command::Version => present::emit(&version::VersionInfo::current(), &settings, &mut out),
        other => {
            eprintln!(
                "`ods {}` is not implemented yet; see docs/ROADMAP.md",
                name(&other)
            );
            return ExitCode::from(EXIT_NOT_IMPLEMENTED);
        }
    };

    match result.and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        // A closed pipe (e.g. `ods version | head -1`) is not an error for the user.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: failed to write output: {err}");
            ExitCode::FAILURE
        }
    }
}

fn name(command: &Command) -> &'static str {
    match command {
        Command::State => "state",
        Command::Erd => "erd",
        Command::Usage => "usage",
        Command::Ci => "ci",
        Command::Lsp => "lsp",
        Command::Agent => "agent",
        Command::Version => "version",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
