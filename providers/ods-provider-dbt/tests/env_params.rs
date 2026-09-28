//! ODS's table of dbt settings (#227) matches what the installed dbt reads, so a new
//! dbt setting is noticed. Runs when `ODS_TEST_DBT` names a dbt executable.

use std::path::Path;
use std::process::Command;

use ods_provider_dbt::settings::{DBT_ENV, OUTSIDE_PARAMS};

/// Settings only newer dbt versions read, with the first (major, minor) that does: on
/// an older dbt they aren't stale. CI runs this against each pinned dbt minor (#233).
const SINCE: [(&str, (u32, u32)); 6] = [
    ("DBT_ENGINE_HINTS_ENABLED", (1, 12)),
    ("DBT_ENGINE_MAXIMUM_SEED_SIZE_MIB", (1, 12)),
    ("DBT_ENGINE_SNOWFLAKE_PROJECTS_OTEL", (1, 12)),
    ("DBT_ENGINE_SQLPARSE", (1, 11)),
    ("DBT_ENGINE_USE_V2_PARSER", (1, 12)),
    ("DBT_ENGINE_V2_PARSER", (1, 12)),
];

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
            "import dbt.cli.params as p, dbt.version as v; \
             print(v.__version__); print(open(p.__file__).read())",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let (version, source) = stdout.split_once('\n').unwrap();
    let mut parts = version.trim().split('.').map(|p| p.parse::<u32>().unwrap());
    let version = (parts.next().unwrap(), parts.next().unwrap());
    let newer = |n: &str| SINCE.iter().any(|(s, since)| *s == n && version < *since);
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
        .filter(|n| !read.contains(n) && !OUTSIDE_PARAMS.contains(n) && !newer(n))
        .collect();
    assert!(stale.is_empty(), "no longer read by dbt: {stale:?}");
}
