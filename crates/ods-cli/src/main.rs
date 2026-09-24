//! The `ods` command-line interface.
//!
//! This binary is the composition root (ADR-0001): it is the only place that wires
//! concrete providers into modules. Module subcommands are registered here; their
//! implementations live in the module crates. The full framework is #6.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ods",
    version,
    about = "OpenDataSuite: explainable control plane for analytics engineering"
)]
struct Cli {
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
    /// Print build and SDK version information.
    Version,
}

/// Exit code for commands that exist in the CLI surface but are not implemented yet.
///
/// Distinct from generic failure (1) and usage errors (2) so scripts can detect it.
const EXIT_NOT_IMPLEMENTED: u8 = 3;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            let sdk = ods_sdk::SDK_VERSION;
            println!(
                "ods {} (sdk {}.{})",
                env!("CARGO_PKG_VERSION"),
                sdk.major,
                sdk.minor
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "`ods {}` is not implemented yet; see docs/ROADMAP.md",
                name(&other)
            );
            ExitCode::from(EXIT_NOT_IMPLEMENTED)
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
