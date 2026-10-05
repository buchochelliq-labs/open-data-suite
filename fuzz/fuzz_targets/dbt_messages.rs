//! dbt's error output, which can hold anything a model printed, is summarised without
//! panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ods_provider_dbt::events::{error_summary, project_failure};

fuzz_target!(|text: &str| {
    let _ = error_summary(text);
    let _ = project_failure(text);
});
