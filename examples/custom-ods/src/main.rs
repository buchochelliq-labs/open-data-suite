//! `custom-ods`: the released `ods`, plus this crate's plugins.

use std::process::ExitCode;
use std::sync::Arc;

use custom_ods::{LoadBatchPlugin, OwnerTagged};

fn main() -> ExitCode {
    let ods = ods_cli::Ods::new()
        .health_check(ods_cli::origin!(), Arc::new(OwnerTagged))
        // `duckdb` has a built-in plugin: this one takes its place, and keeps what it
        // offered (its error patterns and dialect) beside its own source versions.
        .and_then(|ods| ods.replacing_warehouse(Arc::new(LoadBatchPlugin)));
    match ods {
        Ok(ods) => ods.run(),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
