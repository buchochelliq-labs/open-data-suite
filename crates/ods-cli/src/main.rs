//! The `ods` command-line interface.
//!
//! Everything happens in [`ods_cli::Ods`]; this binary is the released build, with the
//! built-in plugins only (ADR-0031).

use std::process::ExitCode;

fn main() -> ExitCode {
    ods_cli::Ods::new().run()
}
