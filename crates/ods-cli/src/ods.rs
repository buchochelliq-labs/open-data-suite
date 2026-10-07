//! `ods` as a library: the released binary, or a custom build with more plugins
//! (ADR-0031 §2).

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use ods_sdk::contracts::health_check::HealthCheck;

use crate::app::{self, Io};
use crate::commands::default_registry;
use crate::exit::ExitStatus;
use crate::plugins::{self, Origin, PluginError, Plugins, WarehousePlugin};

/// The `ods` CLI with its plugins. The released binary is `Ods::new().run()`; a custom
/// build adds health checks and warehouse plugins first:
///
/// ```no_run
/// # use std::process::ExitCode;
/// fn main() -> ExitCode {
///     let ods = ods_cli::Ods::new();
///     // let ods = ods
///     //     .health_check(ods_cli::origin!(), Arc::new(MyCheck))?
///     //     .warehouse(Arc::new(MyWarehouse))?;
///     ods.run()
/// }
/// ```
#[derive(Debug)]
pub struct Ods {
    plugins: Plugins,
}

impl Default for Ods {
    fn default() -> Self {
        Self::new()
    }
}

impl Ods {
    /// The released `ods`: its commands and built-in plugins.
    pub fn new() -> Self {
        Self {
            plugins: Plugins::builtin(),
        }
    }

    /// Adds a health check, which `ods health check` runs with the others
    /// (`[health.plugins.<id>]` tunes it). `origin` is the crate it comes from, which
    /// `ods version` and `ods doctor` name: pass [`origin!()`](crate::origin), written in
    /// that crate. (A check's `info().kind` is its kind as configured, not its crate.)
    ///
    /// # Errors
    /// Its id isn't valid, or another plugin check has it.
    pub fn health_check(
        mut self,
        origin: Origin,
        check: Arc<dyn HealthCheck>,
    ) -> Result<Self, PluginError> {
        self.plugins.add_health_check(origin, check)?;
        Ok(self)
    }

    /// Adds the providers for one warehouse.
    ///
    /// # Errors
    /// Another plugin, built in or added, serves that warehouse, or it names a SQL
    /// dialect the parser doesn't know.
    pub fn warehouse(mut self, plugin: Arc<dyn WarehousePlugin>) -> Result<Self, PluginError> {
        self.plugins.add_warehouse(plugin)?;
        Ok(self)
    }

    /// Adds the providers for one warehouse in place of those serving it.
    ///
    /// # Errors
    /// It names a SQL dialect the parser doesn't know.
    pub fn replacing_warehouse(
        mut self,
        plugin: Arc<dyn WarehousePlugin>,
    ) -> Result<Self, PluginError> {
        self.plugins.replace_warehouse(plugin)?;
        Ok(self)
    }

    /// The plugins it will run with.
    pub fn plugins(&self) -> &Plugins {
        &self.plugins
    }

    /// Runs the CLI with the process's arguments, streams and environment, and returns
    /// its exit status (ADR-0004). Call it once per process.
    pub fn run(self) -> ExitCode {
        let (stdout, mut stderr) = (io::stdout(), io::stderr());
        if let Err(e) = plugins::install(self.plugins) {
            let _ = writeln!(stderr, "error: {e}");
            return ExitCode::from(ExitStatus::Failure.code());
        }
        let registry = default_registry();
        // `vars()` would panic on a non-UTF-8 variable. Configuration only needs UTF-8
        // ones; an `ODS…` variable that is not UTF-8 is reported instead of silently
        // dropped.
        let (mut env, mut invalid_env) = (Vec::new(), Vec::new());
        for (key, value) in std::env::vars_os() {
            match (key.into_string(), value.into_string()) {
                (Ok(key), Ok(value)) => env.push((key, value)),
                (Ok(key), Err(_)) if key.starts_with("ODS") => invalid_env.push(key),
                _ => {}
            }
        }
        let status = app::run(
            &registry,
            std::env::args_os(),
            &mut Io {
                stdout_is_terminal: stdout.is_terminal(),
                stderr_is_terminal: stderr.is_terminal(),
                out: &mut stdout.lock(),
                // Locked per write, not for the whole run: log lines from other threads
                // (e.g. `ods serve` reloading) share stderr and would otherwise block
                // forever.
                err: &mut stderr,
                ods_log: std::env::var("ODS_LOG").ok(),
                no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
                dumb_terminal: crate::output::term_is_dumb(),
                cwd: std::env::current_dir().ok(),
                env,
                invalid_env,
            },
        );
        ExitCode::from(status.code())
    }
}
