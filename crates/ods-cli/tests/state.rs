//! `ods state policies` on the dbt State fixture (#168).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture(version: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt/jaffle-ods-state/artifacts")
        .join(version)
}

fn ods(args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .arg("--json")
        .env_clear()
        .env("XDG_CONFIG_HOME", std::env::temp_dir())
        .output()
        .unwrap();
    let envelope = if out.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&out.stdout).unwrap()
    };
    (out.status.code().unwrap(), envelope)
}

#[test]
fn policies_show_configured_default_and_blocked_models_and_sources() {
    let target = fixture("dbt-2.0");
    let (code, envelope) = ods(&[
        "state",
        "policies",
        "--target-dir",
        target.to_str().unwrap(),
        "--artifacts",
        "info-schema",
    ]);
    assert_eq!(code, 0, "{envelope}");
    let result = &envelope["result"];
    assert_eq!(result["uses_dbt_state"], true);
    let model = |id: &str| {
        result["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["unique_id"] == id)
            .unwrap()
            .clone()
    };
    let rank = model("model.jaffle_ods.customer_order_rank");
    assert_eq!(rank["lag_tolerance"], "4h");
    assert_eq!(rank["lag_tolerance_secs"], 14_400);
    assert_eq!(rank["require_fresh_data_from"], "all");
    assert_eq!(rank["origin"]["kind"], "configured");
    assert_eq!(
        model("model.jaffle_ods.stg_orders")["origin"]["kind"],
        "format_default"
    );
    let orders = model("model.jaffle_ods.orders");
    assert_eq!(orders["reuse_allowed"], false);
    assert_eq!(
        orders["unapplied"][0]["setting"],
        "state.evaluate_volatile_sql"
    );
    assert_eq!(result["sources"].as_array().unwrap().len(), 3);
}

#[test]
fn one_model_by_name_and_unknown_models_are_usage_errors() {
    let target = fixture("dbt-1.10");
    let target = target.to_str().unwrap();
    let (code, envelope) = ods(&[
        "state",
        "policies",
        "--target-dir",
        target,
        "--model",
        "orders",
    ]);
    assert_eq!(code, 0);
    assert_eq!(envelope["result"]["models"].as_array().unwrap().len(), 1);
    let (code, _) = ods(&[
        "state",
        "policies",
        "--target-dir",
        target,
        "--model",
        "nope",
    ]);
    assert_eq!(code, 2);
}

#[test]
fn other_state_subcommands_are_still_planned() {
    let (code, envelope) = ods(&["state", "plan", "--select", "+orders"]);
    assert_eq!(code, 3);
    assert_eq!(envelope["diagnostics"][0]["code"], "ODS-E0003");
}
