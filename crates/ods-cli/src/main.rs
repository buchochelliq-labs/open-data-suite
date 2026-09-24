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
        },
    );
    ExitCode::from(status.code())
}
