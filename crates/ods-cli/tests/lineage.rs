//! `ods lineage` end to end on the committed `jaffle-ods` dbt fixture (#74, #100).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("ods-cli-lineage-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ods(args: &[&str]) -> Output {
    let home = Temp::new();
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .current_dir(&home.0)
        .env_clear()
        .env("XDG_CONFIG_HOME", &home.0)
        .output()
        .expect("failed to spawn ods")
}

fn json(args: &[&str]) -> Value {
    let mut all = args.to_vec();
    all.push("--json");
    let out = ods(&all);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    envelope["result"].clone()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn columns_trace_every_model_column_to_its_inputs() {
    let target = fixture();
    let result = json(&[
        "lineage",
        "columns",
        "--target-dir",
        target.to_str().unwrap(),
        "--model",
        "customers",
    ]);
    assert_eq!(result["summary"]["dialect"], "duckdb");
    assert_eq!(result["summary"]["models_opaque"], 0);
    let model = &result["models"][0];
    assert_eq!(model["unique_id"], "model.jaffle_ods.customers");
    let lifetime = model["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "lifetime_value")
        .unwrap();
    assert_eq!(lifetime["inputs"][0]["column"], "orders.amount");
    assert_eq!(
        lifetime["inputs"][0]["edge"],
        serde_json::json!({"type": "direct", "subtype": "aggregation"})
    );
}

#[test]
fn impact_prunes_readers_that_do_not_use_the_changed_column() {
    let target = fixture();
    let result = json(&[
        "lineage",
        "impact",
        "--target-dir",
        target.to_str().unwrap(),
        "--column",
        "stg_payments.payment_method",
    ]);
    assert_eq!(strings(&result["run"]), ["model.jaffle_ods.orders"]);
    let pruned: Vec<&str> = result["impact"]["pruned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["node"].as_str().unwrap())
        .collect();
    assert_eq!(
        pruned,
        [
            "model.jaffle_ods.customer_order_rank",
            "model.jaffle_ods.customers",
            "model.jaffle_ods.order_events"
        ]
    );
}

#[test]
fn impact_against_a_base_build_finds_the_real_change_and_its_consumers() {
    let head = Temp::new();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), head.0.join(file)).unwrap();
    }
    let manifest_path = head.0.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let code = &mut manifest["nodes"]["model.jaffle_ods.orders"]["compiled_code"];
    let changed = code.as_str().unwrap().replace(
        "coalesce(p.total_amount, 0) as amount",
        "coalesce(p.total_amount, 0) * 1.2 as amount",
    );
    *code = Value::String(changed);
    fs::write(&manifest_path, manifest.to_string()).unwrap();

    let base = fixture();
    let result = json(&[
        "lineage",
        "impact",
        "--target-dir",
        head.0.to_str().unwrap(),
        "--base",
        base.to_str().unwrap(),
    ]);
    assert_eq!(
        strings(&result["changed_models"]),
        ["model.jaffle_ods.orders"]
    );
    assert_eq!(
        result["changes"].as_array().unwrap().len(),
        1,
        "only `amount` changed"
    );
    assert_eq!(
        strings(&result["run"]),
        [
            "model.jaffle_ods.customer_order_rank",
            "model.jaffle_ods.customers",
            "model.jaffle_ods.customers_snapshot_view",
            "model.jaffle_ods.orders"
        ]
    );
    // `order_events` doesn't read `orders` at all, so it's neither run nor pruned.
    assert!(!result.to_string().contains("order_events"));
}

#[test]
fn export_writes_openlineage_job_events_with_column_lineage() {
    let out_dir = Temp::new();
    let file = out_dir.0.join("events.ndjson");
    let target = fixture();
    let result = json(&[
        "lineage",
        "export",
        "--target-dir",
        target.to_str().unwrap(),
        "--namespace",
        "duckdb://local",
        "--event-time",
        "2026-09-25T00:00:00Z",
        "--output-file",
        file.to_str().unwrap(),
    ]);
    assert_eq!(result["events"], 8);
    assert_eq!(result["with_column_lineage"], 8);
    let events: Vec<Value> = fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let orders = events
        .iter()
        .find(|e| e["job"]["name"] == "model.jaffle_ods.orders")
        .unwrap();
    assert!(
        orders["schemaURL"]
            .as_str()
            .unwrap()
            .ends_with("#/$defs/JobEvent")
    );
    let facet = &orders["outputs"][0]["facets"]["columnLineage"];
    assert!(
        facet["_schemaURL"]
            .as_str()
            .unwrap()
            .contains("ColumnLineageDatasetFacet")
    );
    assert_eq!(
        facet["fields"]["amount"]["inputFields"][0]["name"],
        "jaffle_ods.main.stg_payments"
    );
    assert_eq!(
        facet["fields"]["amount"]["inputFields"][0]["transformations"][0]["subtype"],
        "AGGREGATION"
    );
    // Deterministic: the same inputs give byte-identical events.
    let again = out_dir.0.join("again.ndjson");
    json(&[
        "lineage",
        "export",
        "--target-dir",
        target.to_str().unwrap(),
        "--namespace",
        "duckdb://local",
        "--event-time",
        "2026-09-25T00:00:00Z",
        "--output-file",
        again.to_str().unwrap(),
    ]);
    assert_eq!(fs::read(&file).unwrap(), fs::read(&again).unwrap());
}

#[test]
fn errors_have_stable_codes_and_exit_statuses() {
    let missing = ods(&[
        "lineage",
        "columns",
        "--target-dir",
        "no/such/dir",
        "--json",
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("ODS-E0201"));

    let target = fixture();
    let unknown = ods(&[
        "lineage",
        "impact",
        "--target-dir",
        target.to_str().unwrap(),
        "--column",
        "nope.x",
        "--json",
    ]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stdout).contains("ODS-E0203"));
}
