//! `custom-ods`: the released `ods`, plus this crate's plugins.

use std::process::ExitCode;
use std::sync::Arc;

use custom_ods::{LoadBatchPlugin, OwnerTagged};

fn main() -> ExitCode {
    let ods = ods_cli::Ods::new()
        .health_check(Arc::new(OwnerTagged))
        .and_then(|ods| ods.warehouse(Arc::new(LoadBatchPlugin)));
    match ods {
        Ok(ods) => ods.run(),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
