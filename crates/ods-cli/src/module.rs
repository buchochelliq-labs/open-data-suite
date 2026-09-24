//! Command modules and the registry that assembles the `ods` command (ADR-0004 §1, §2).

use std::fmt;
use std::io::Write;

use clap::{ArgMatches, Command};

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

    /// Runs the subcommand with its parsed arguments.
    ///
    /// # Errors
    /// Returns a [`CliError`] describing the failure and its exit status.
    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError>;
}

/// Everything a command receives besides its own arguments.
///
/// Configuration (#7) and policy (#9) will be added here so every command gets them the
/// same way.
pub struct Context<'a> {
    /// Resolved output settings (ADR-0003 §2).
    pub output: OutputSettings,
    out: &'a mut dyn Write,
    root: &'a Command,
}

impl<'a> Context<'a> {
    /// Creates a context writing results to `out`.
    pub fn new(output: OutputSettings, out: &'a mut dyn Write, root: &'a Command) -> Self {
        Self { output, out, root }
    }

    /// Writes a command result in the active output mode.
    ///
    /// # Errors
    /// Returns a [`CliError`] if writing fails.
    pub fn emit<T: Present>(&mut self, result: &T) -> Result<(), CliError> {
        present::emit(result, &self.output, self.out).map_err(CliError::from)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Another module already uses this name.
    Duplicate(String),
    /// The name is reserved by clap or `ods` itself.
    Reserved(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Duplicate(name) => write!(f, "command `{name}` is already registered"),
            RegistryError::Reserved(name) => write!(f, "command name `{name}` is reserved"),
        }
    }
}

impl std::error::Error for RegistryError {}

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
