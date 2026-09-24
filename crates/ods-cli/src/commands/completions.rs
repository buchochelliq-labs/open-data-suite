//! `ods completions <shell>` (ADR-0004 §6).

use clap::{Arg, ArgMatches, Command, value_parser};
use clap_complete::Shell;

use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};

/// Prints a shell completion script generated from the registered commands.
pub struct Completions;

impl Module for Completions {
    fn command(&self) -> Command {
        Command::new("completions")
            .about("Print a shell completion script")
            .long_about(
                "Print a shell completion script for the registered commands. The script \
                 is written to stdout as-is, whatever the output mode.\n\n\
                 Example (bash): ods completions bash > ~/.local/share/bash-completion/completions/ods",
            )
            .arg(
                Arg::new("shell")
                    .required(true)
                    .value_parser(value_parser!(Shell))
                    .help("Shell to generate completions for"),
            )
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let shell = *matches.get_one::<Shell>("shell").ok_or_else(|| {
            CliError::new(
                ExitStatus::Failure,
                codes::INTERNAL,
                "missing required shell argument",
            )
        })?;
        let mut root = ctx.root_command().clone();
        // Generate into memory first: clap_complete panics if its writer fails, and a
        // closed pipe must stay a clean exit (ADR-0004 §3).
        let mut script = Vec::new();
        clap_complete::generate(shell, &mut root, "ods", &mut script);
        ctx.raw_out().write_all(&script)?;
        Ok(())
    }
}
