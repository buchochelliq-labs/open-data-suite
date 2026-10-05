//! A `run_results.json` that isn't what dbt wrote is an error, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ods_provider_dbt::RunResults;

fuzz_target!(|data: &[u8]| {
    // `RunResults` reads a file, as ODS does; one per process, rewritten each run.
    let path =
        std::env::temp_dir().join(format!("ods-fuzz-run-results-{}.json", std::process::id()));
    if std::fs::write(&path, data).is_ok() {
        let _ = RunResults::read(&path);
    }
});
