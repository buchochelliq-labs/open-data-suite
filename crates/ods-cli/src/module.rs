//! Command modules and the registry that assembles the `ods` command (ADR-0004 §1, §2).

use std::io::Write;

use clap::{ArgMatches, Command};

use ods_config::Loaded;

use crate::exit::CliError;
use crate::output::OutputSettings;
use crate::present::{self, Present};

/// A group of `ods` subcommands, e.g. `state` or `erd`.
///
/// Implementations live in `ods-cli/src/commands/`: they translate arguments, call the
/// module crate's API and hand the result model to [`Context::emit`]. They never print
/// errors or exit the process; failures are returned as [`CliError`].
pub trait Module {
    /// The subcommand definition. Its name is the command word.
    fn command(&self) -> Command;

    /// Id of an argument that captures arbitrary trailing arguments, if any.
    ///
    /// Global flags found inside that tail (e.g. `ods state plan --json`) are hoisted
    /// and parsed as globals, so they work anywhere on the line (ADR-0004 §1).
    fn passthrough_arg(&self) -> Option<&'static str> {
        None
    }

    /// Runs the subcommand with its parsed arguments.
    ///
    /// # Errors
    /// Returns a [`CliError`] describing the failure and its exit status.
    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError>;
}

/// Everything a command receives besides its own arguments.
///
/// Policy (#9) will be added here so every command gets it the same way.
pub struct Context<'a> {
    /// Resolved output settings (ADR-0003 §2).
    pub output: OutputSettings,
    /// Loaded configuration and its provenance (ADR-0005).
    pub config: &'a Loaded,
    out: &'a mut dyn Write,
    root: &'a Command,
    /// Whether and how to print progress lines on stderr.
    pub progress: ProgressSettings,
}

/// Progress lines on stderr: short notes between another tool's output saying which
/// step runs and why. Off with `-q`; never on stdout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressSettings {
    /// Print them.
    pub enabled: bool,
    /// Style them with ANSI escapes.
    pub ansi: bool,
}

impl<'a> Context<'a> {
    /// Creates a context writing results to `out`.
    pub fn new(
        output: OutputSettings,
        config: &'a Loaded,
        out: &'a mut dyn Write,
        root: &'a Command,
    ) -> Self {
        Self {
            output,
            config,
            out,
            root,
            progress: ProgressSettings::default(),
        }
    }

    /// Sets how progress lines are shown.
    #[must_use]
    pub fn with_progress(mut self, progress: ProgressSettings) -> Self {
        self.progress = progress;
        self
    }

    /// Writes a command result in the active output mode.
    ///
    /// # Errors
    /// Returns a [`CliError`] if writing fails.
    pub fn emit<T: Present>(&mut self, result: &T) -> Result<(), CliError> {
        present::emit(result, &self.output, self.out).map_err(CliError::from)
    }

    /// Renders the result of a command that failed anyway (e.g. a run whose successes
    /// were recorded but some nodes failed) and returns the error to exit with. The
    /// error is reported once: inside the JSON envelope, or on stderr otherwise.
    ///
    /// # Errors
    /// Always: `error`, or the failure to write the output.
    pub fn emit_failed<T: Present>(&mut self, result: &T, error: CliError) -> Result<(), CliError> {
        present::emit_with_error(result, &error, &self.output, self.out)?;
        self.out.flush()?;
        Err(error.in_envelope())
    }

    /// Raw stdout, for output that is not a result model (e.g. completion scripts).
    pub fn raw_out(&mut self) -> &mut dyn Write {
        self.out
    }

    /// The fully assembled root command.
    pub fn root_command(&self) -> &Command {
        self.root
    }
}

/// Why a module could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Another module already uses this name.
    #[error("command `{0}` is already registered")]
    Duplicate(String),
    /// The name is reserved by clap or `ods` itself.
    #[error("command name `{0}` is reserved")]
    Reserved(String),
}

const RESERVED: &[&str] = &["help"];

/// The registered command modules, in `--help` order.
#[derive(Default)]
pub struct Registry {
    modules: Vec<Box<dyn Module>>,
}

impl Registry {
    /// A registry with no modules.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a module.
    ///
    /// # Errors
    /// Returns [`RegistryError`] if the name is taken or reserved.
    pub fn register(&mut self, module: Box<dyn Module>) -> Result<(), RegistryError> {
        let name = module.command().get_name().to_owned();
        if RESERVED.contains(&name.as_str()) {
            return Err(RegistryError::Reserved(name));
        }
        if self.get(&name).is_some() {
            return Err(RegistryError::Duplicate(name));
        }
        self.modules.push(module);
        Ok(())
    }

    /// Looks up a module by command name.
    pub fn get(&self, name: &str) -> Option<&dyn Module> {
        self.modules
            .iter()
            .find(|m| m.command().get_name() == name)
            .map(AsRef::as_ref)
    }

    /// The registered command names, in order.
    pub fn names(&self) -> Vec<String> {
        self.modules
            .iter()
            .map(|m| m.command().get_name().to_owned())
            .collect()
    }

    /// Adds every module's subcommand to `root`.
    pub fn attach(&self, root: Command) -> Command {
        self.modules
            .iter()
            .fold(root, |root, module| root.subcommand(module.command()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str);

    impl Module for Named {
        fn command(&self) -> Command {
            Command::new(self.0)
        }

        fn run(&self, _: &ArgMatches, _: &mut Context<'_>) -> Result<(), CliError> {
            Ok(())
        }
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let mut registry = Registry::new();
        registry.register(Box::new(Named("state"))).unwrap();
        assert_eq!(
            registry.register(Box::new(Named("state"))).unwrap_err(),
            RegistryError::Duplicate("state".into())
        );
    }

    #[test]
    fn reserved_names_are_rejected() {
        let mut registry = Registry::new();
        assert_eq!(
            registry.register(Box::new(Named("help"))).unwrap_err(),
            RegistryError::Reserved("help".into())
        );
    }

    #[test]
    fn registration_order_is_preserved() {
        let mut registry = Registry::new();
        for name in ["b", "a", "c"] {
            registry.register(Box::new(Named(name))).unwrap();
        }
        assert_eq!(registry.names(), ["b", "a", "c"]);
        assert!(registry.get("a").is_some());
        assert!(registry.get("z").is_none());
    }
}
