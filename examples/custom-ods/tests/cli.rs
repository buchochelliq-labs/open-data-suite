//! The custom `ods` end to end: it says it is custom, runs its plugin check with the
//! others, takes `[health.plugins.<id>]`, and refuses a misspelt one (ADR-0031 §2, §4).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

/// `custom-ods` with `args` and `--json`, in a project whose `ods.toml` is `ods_toml`:
/// its exit code and envelope.
fn ods(ods_toml: &str, args: &[&str]) -> (i32, Value) {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("ods.toml"), ods_toml).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_custom-ods"))
        .args(args)
        .arg("--json")
        .current_dir(project.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap(), json)
}

fn health_check(ods_toml: &str) -> (i32, Value) {
    let target = fixture();
    ods(
        ods_toml,
        &[
            "health",
            "check",
            "--no-record",
            "--target-dir",
            target.to_str().unwrap(),
        ],
    )
}

#[test]
fn it_says_which_plugins_it_added() {
    let (code, envelope) = ods("", &["version"]);
    assert_eq!(code, 0, "{envelope:#}");
    let plugins = envelope["result"]["plugins"].as_array().unwrap();
    let find = |contract: &str, name: &str| {
        plugins
            .iter()
            .find(|p| p["contract"] == contract && p["name"] == name)
            .unwrap_or_else(|| panic!("no {contract} for {name}: {plugins:#?}"))
    };
    let check = find("health_check", "custom.owner_tagged");
    assert_eq!(check["builtin"], false);
    assert!(check["from"].as_str().unwrap().starts_with("custom-ods "));
    // It replaced the built-in `duckdb` plugin, keeping its error patterns.
    let duckdb = find("change_provider", "duckdb");
    assert_eq!(duckdb["builtin"], false);
    let errors = find("error_catalogue", "duckdb");
    assert_eq!(errors["builtin"], false);
    assert!(errors["from"].as_str().unwrap().starts_with("custom-ods "));
    // The built-ins are still there.
    assert_eq!(find("change_provider", "databricks")["builtin"], true);
}

#[test]
fn its_check_runs_with_the_others_and_is_tuned_in_config() {
    let owner = |envelope: &Value| -> Vec<Value> {
        envelope["result"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("{envelope:#}"))
            .iter()
            .flat_map(|n| n["findings"].as_array().unwrap().clone())
            .filter(|f| f["check"] == "custom.owner_tagged")
            .collect()
    };

    // At its default severity (warn): the fixture's nodes have no owner tag.
    let (code, envelope) = health_check("");
    assert_eq!(code, 0, "{envelope:#}");
    let findings = owner(&envelope);
    assert!(!findings.is_empty(), "{envelope:#}");
    assert!(
        findings
            .iter()
            .all(|f| f["status"] == "fail" && f["source"] == "plugin")
    );

    // `[health.plugins.<id>]` raises it to error, which fails the gate (exit 5)...
    let (code, envelope) =
        health_check("[health.plugins.\"custom.owner_tagged\"]\nseverity = \"error\"\n");
    assert_eq!(code, 5, "{envelope:#}");

    // ...or turns it off.
    let (code, envelope) =
        health_check("[health.plugins.\"custom.owner_tagged\"]\nseverity = \"off\"\n");
    assert_eq!(code, 0, "{envelope:#}");
    assert!(owner(&envelope).is_empty(), "{envelope:#}");
}

#[test]
fn a_misspelt_plugin_id_is_a_configuration_error() {
    let (code, envelope) =
        health_check("[health.plugins.\"custom.owner_taged\"]\nseverity = \"error\"\n");
    assert_eq!(code, 4, "{envelope:#}");
    let message = envelope["diagnostics"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("custom.owner_tagged"), "{envelope:#}");
}
