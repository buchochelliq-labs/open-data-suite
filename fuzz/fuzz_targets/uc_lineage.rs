//! A Unity Catalog lineage export that isn't what Databricks wrote is an error, never a
//! panic, in either format.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ods_provider_databricks::{ExportFormat, UcColumnLineage};
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;

fuzz_target!(|input: (bool, &[u8])| {
    let (csv, data) = input;
    let (format, ext) = if csv {
        (ExportFormat::Csv, "csv")
    } else {
        (ExportFormat::Json, "json")
    };
    // The reader takes a file, as ODS does; one per process, rewritten each run.
    let path = std::env::temp_dir().join(format!("ods-fuzz-uc-{}.{ext}", std::process::id()));
    if std::fs::write(&path, data).is_ok() {
        let _ = UcColumnLineage::new(&path, format).observed_lineage();
    }
});
