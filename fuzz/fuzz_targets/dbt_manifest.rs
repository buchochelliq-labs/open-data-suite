//! A `manifest.json` that isn't what dbt wrote is an error, never a panic.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use ods_provider_dbt::Manifest;

fuzz_target!(|json: &str| {
    let _ = Manifest::parse(Path::new("manifest.json"), json);
});
