//! `ods version`.

use clap::{ArgMatches, Command};

use crate::exit::CliError;
use crate::module::{Context, Module};
use crate::version::VersionInfo;

/// Prints build and compatibility version information.
pub struct Version;

impl Module for Version {
    fn command(&self) -> Command {
        Command::new("version").about("Print build and compatibility version information")
    }

    fn run(&self, _: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        ctx.emit(&VersionInfo::current())
    }
}
