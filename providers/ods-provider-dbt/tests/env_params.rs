//! ODS's table of dbt settings (#227) matches what the installed dbt reads, so a new
//! dbt setting is noticed. Runs when `ODS_TEST_DBT` names a dbt executable.

use std::path::Path;
use std::process::Command;

use ods_provider_dbt::settings::{DBT_ENV, OUTSIDE_PARAMS};

#[test]
fn every_setting_the_installed_dbt_reads_is_classified() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to check against an installed dbt");
        return;
    };
    // The Python dbt runs on, from its script's `#!` line.
    let script = std::fs::read_to_string(Path::new(&dbt)).unwrap();
    let python = script
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("#!"))
        .map(str::trim)
        .expect("dbt is a Python script");
    let out = Command::new(python)
        .args([
            "-c",
            "import dbt.cli.params as p; print(open(p.__file__).read())",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let source = String::from_utf8(out.stdout).unwrap();
    let mut read: Vec<&str> = source
        .split("envvar=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    read.sort_unstable();
    read.dedup();
    assert!(read.len() > 40, "{read:?}");
    let known: Vec<&str> = DBT_ENV.iter().map(|(name, _)| *name).collect();
    let unclassified: Vec<&&str> = read.iter().filter(|n| !known.contains(n)).collect();
    assert!(
        unclassified.is_empty(),
        "dbt reads settings ODS doesn't classify: {unclassified:?}"
    );
    let stale: Vec<&&str> = known
        .iter()
        .filter(|n| !read.contains(n) && !OUTSIDE_PARAMS.contains(n))
        .collect();
    assert!(stale.is_empty(), "no longer read by dbt: {stale:?}");
}
