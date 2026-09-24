//! Commands that are on the roadmap but not implemented yet (ADR-0004 §1).

use clap::{Arg, ArgMatches, Command};

use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};

/// Id of the catch-all argument that captures a planned command's arguments.
const PASSTHROUGH: &str = "args";

/// A placeholder that shows in `--help` and exits with status 3.
pub struct Planned {
    name: &'static str,
    about: &'static str,
    milestone: &'static str,
}

impl Planned {
    /// Creates a placeholder for `name`, planned for `milestone`.
    pub const fn new(name: &'static str, about: &'static str, milestone: &'static str) -> Self {
        Self {
            name,
            about,
            milestone,
        }
    }
}

impl Module for Planned {
    fn command(&self) -> Command {
        Command::new(self.name)
            .about(format!("{} [planned: {}]", self.about, self.milestone))
            // Capture arguments that do not exist yet, so `ods state plan --select x`
            // reports "not implemented" rather than a usage error. Global flags inside
            // the captured tail are hoisted and re-parsed by the framework (ADR-0004 §1).
            .arg(
                Arg::new(PASSTHROUGH)
                    .num_args(0..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .hide(true),
            )
    }

    fn passthrough_arg(&self) -> Option<&'static str> {
        Some(PASSTHROUGH)
    }

    fn run(&self, _: &ArgMatches, _: &mut Context<'_>) -> Result<(), CliError> {
        Err(CliError::new(
            ExitStatus::NotImplemented,
            codes::NOT_IMPLEMENTED,
            format!("`ods {}` is not implemented yet", self.name),
        )
        .with_hint(format!(
            "planned for {}; see docs/ROADMAP.md",
            self.milestone
        )))
    }
}
