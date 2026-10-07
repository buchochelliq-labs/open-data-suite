//! `ods plugin list` and `ods plugin show` end to end (#415, ADR-0031 §3c): what the
//! released `ods` runs with, as detected, in JSON and for people.

use std::process::Command;

use serde_json::Value;

/// `ods` with `args` in a fresh directory holding `ods_toml`: exit code, stdout and
/// stderr.
fn ods(ods_toml: &str, args: &[&str]) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ods.toml"), ods_toml).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .current_dir(dir.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", dir.path())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn json(ods_toml: &str, args: &[&str]) -> (i32, Value) {
    let mut args = args.to_vec();
    args.push("--json");
    let (code, out, err) = ods(ods_toml, &args);
    let envelope = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}\n{err}"));
    (code, envelope)
}

#[test]
fn list_names_each_plugin_and_what_it_offers() {
    let (code, envelope) = json("", &["plugin", "list"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(envelope["command"], "plugin.list");
    let plugins = envelope["result"]["plugins"].as_array().unwrap();
    let databricks = plugins.iter().find(|p| p["name"] == "databricks").unwrap();
    assert_eq!(databricks["kind"], "warehouse");
    assert_eq!(databricks["builtin"], true);
    let features: Vec<&str> = databricks["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        features,
        [
            "source_versions",
            "login_check",
            "links",
            "observed_lineage",
            "errors",
            "dialect"
        ]
    );

    let (code, plain, _) = ods("", &["plugin", "list", "--output", "plain"]);
    assert_eq!(code, 0);
    assert!(
        plain
            .lines()
            .any(|l| l.starts_with("databricks\twarehouse\t")),
        "{plain}"
    );
}

#[test]
fn show_gives_each_feature_with_its_contract_and_the_parents_configured() {
    let (code, envelope) = json("", &["plugin", "show", "databricks"]);
    assert_eq!(code, 0, "{envelope:#}");
    let plugin = &envelope["result"]["plugin"];
    assert_eq!(plugin["parents"], serde_json::json!(["spark"]));
    assert_eq!(plugin["parents_from"], "plugin");
    let errors = plugin["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "errors")
        .unwrap();
    assert_eq!(errors["contract"], "error_catalogue");
    assert!(errors["contract_version"].is_string(), "{errors:#}");
    assert_eq!(errors["detail"], "databricks catalogue 1");

    // `[warehouses.<kind>] extends` replaces the plugin's own parents, and says so.
    let (code, envelope) = json(
        "[warehouses.databricks]\nextends = [\"hive\"]\n",
        &["plugin", "show", "databricks"],
    );
    assert_eq!(code, 0, "{envelope:#}");
    let plugin = &envelope["result"]["plugin"];
    assert_eq!(plugin["parents"], serde_json::json!(["hive"]));
    assert_eq!(plugin["parents_from"], "configuration");
}

#[test]
fn show_of_an_unknown_plugin_is_a_usage_error_naming_those_there_are() {
    let (code, envelope) = json("", &["plugin", "show", "snowflake"]);
    assert_eq!(code, 2, "{envelope:#}");
    let diagnostic = &envelope["diagnostics"][0];
    assert_eq!(diagnostic["code"], "ODS-E0801");
    assert_eq!(diagnostic["message"], "no plugin `snowflake`");
    assert!(
        diagnostic["hint"]
            .as_str()
            .unwrap()
            .contains("`databricks`"),
        "{diagnostic:#}"
    );
}
