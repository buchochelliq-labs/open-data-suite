//! The `ods` command-line interface.
//!
//! Everything happens in [`ods_cli::app::run`]; this binary only supplies the process's
//! real arguments, streams and environment (ADR-0004).

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use ods_cli::app::{self, Io};
use ods_cli::commands::default_registry;

fn main() -> ExitCode {
    let registry = default_registry();
    let (stdout, stderr) = (io::stdout(), io::stderr());
    // `vars()` would panic on a non-UTF-8 variable. Configuration only needs UTF-8 ones;
    // an `ODS…` variable that is not UTF-8 is reported instead of silently dropped.
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
            err: &mut stderr.lock(),
            ods_log: std::env::var("ODS_LOG").ok(),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            dumb_terminal: ods_cli::output::term_is_dumb(),
            cwd: std::env::current_dir().ok(),
            env,
            invalid_env,
        },
    );
    ExitCode::from(status.code())
}
