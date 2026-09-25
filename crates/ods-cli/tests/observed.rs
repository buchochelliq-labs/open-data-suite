//! `--observed`: Unity Catalog lineage checking the analyzer and filling in Python models.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn target() -> PathBuf {
    root().join("dbt/jaffle-ods/artifacts/dbt-1.10")
}

fn export() -> String {
    root()
        .join("databricks/uc-lineage/column_lineage.csv")
        .display()
        .to_string()
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("ods-cli-observed-{}-{n}", std::process::id()));
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

/// Runs `ods … --json`: (exit code, envelope).
fn ods(args: &[&str]) -> (i32, Value) {
    let home = Temp::new();
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .arg("--json")
        .current_dir(&home.0)
        .env_clear()
        .env("XDG_CONFIG_HOME", &home.0)
        .output()
        .unwrap();
    // Usage errors come from the argument parser, on stderr only.
    let envelope = if out.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
    };
    (out.status.code().unwrap(), envelope)
}

fn result(args: &[&str]) -> Value {
    let (code, envelope) = ods(args);
    assert_eq!(code, 0, "{envelope}");
    envelope["result"].clone()
}

#[test]
fn compare_reports_agreement_misses_and_unobserved_models() {
    let target = target();
    let export = export();
    let report = result(&[
        "lineage",
        "compare",
        "--target-dir",
        target.to_str().unwrap(),
        "--observed",
        &export,
    ]);
    assert_eq!(report["records"], 50);
    assert_eq!(report["skipped"], 2);
    assert_eq!(report["precision"], 1.0);
    let c = &report["comparison"];
    assert_eq!(c["missing"], 1);
    assert_eq!(
        c["matched_indirect"], 1,
        "order_seq <- order_date is a window input"
    );
    assert_eq!(c["outside_project"], 1);
    let verdict = |id: &str| {
        c["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["node"] == id)
            .unwrap()["agreement"]
            .clone()
    };
    assert_eq!(verdict("model.jaffle_ods.orders"), "agrees");
    assert_eq!(verdict("model.jaffle_ods.customers"), "misses");
    assert_eq!(verdict("model.jaffle_ods.order_events"), "not_observed");
    assert_eq!(
        verdict("model.jaffle_ods.customer_order_rank"),
        "agrees",
        "mixed-case names are normalized like the analyzer's"
    );
}

#[test]
fn compare_needs_an_export_and_a_bad_export_is_an_artifacts_error() {
    let target = target();
    let (code, _) = ods(&[
        "lineage",
        "compare",
        "--target-dir",
        target.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "--observed is required");
    let (code, envelope) = ods(&[
        "lineage",
        "columns",
        "--target-dir",
        target.to_str().unwrap(),
        "--observed",
        target.join("manifest.json").to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert_eq!(
        envelope["diagnostics"][0]["code"], "ODS-E0201",
        "{envelope}"
    );
}

/// The fixture with `customer_order_rank` turned into a Python model.
fn with_python_model() -> Temp {
    let dir = Temp::new();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(target().join("manifest.json")).unwrap()).unwrap();
    let node = &mut manifest["nodes"]["model.jaffle_ods.customer_order_rank"];
    node["language"] = "python".into();
    node["compiled_code"] = "def model(dbt, session):\n    return dbt.ref('orders')\n".into();
    fs::write(dir.0.join("manifest.json"), manifest.to_string()).unwrap();
    fs::copy(target().join("catalog.json"), dir.0.join("catalog.json")).unwrap();
    dir
}

fn impact_on_order_status(target: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "lineage",
        "impact",
        "--target-dir",
        target.to_str().unwrap(),
        "--column",
        "orders.status",
    ];
    args.extend_from_slice(extra);
    result(&args)
}

#[test]
fn python_models_take_observed_lineage_but_only_trust_makes_impact_skip_them() {
    let python = with_python_model();
    let export = export();
    let rank = "model.jaffle_ods.customer_order_rank";

    // Without observed lineage the Python model is opaque: it runs.
    let plain = impact_on_order_status(&python.0, &[]);
    assert!(plain["run"].as_array().unwrap().iter().any(|r| r == rank));

    // Observed lineage is shown, but impact stays conservative.
    let shown = impact_on_order_status(&python.0, &["--observed", &export]);
    assert!(shown["run"].as_array().unwrap().iter().any(|r| r == rank));
    assert_eq!(shown["summary"]["observed"]["stitched"][0], rank);
    assert_eq!(shown["summary"]["observed"]["trusted"], false);

    // Trusted: it only reads orders' amount, customer_id and order_date, so it's skipped.
    let trusted = impact_on_order_status(&python.0, &["--observed", &export, "--trust-observed"]);
    assert!(!trusted["run"].as_array().unwrap().iter().any(|r| r == rank));
    assert!(
        trusted["impact"]["pruned"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["node"] == rank)
    );

    let columns = result(&[
        "lineage",
        "columns",
        "--target-dir",
        python.0.to_str().unwrap(),
        "--model",
        "customer_order_rank",
        "--observed",
        &export,
    ]);
    let model = &columns["models"][0];
    assert_eq!(model["confidence"], "observed");
    assert!(
        model["columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "running_amount")
    );
}

#[test]
fn trust_observed_requires_observed() {
    let target = target();
    let (code, _) = ods(&[
        "lineage",
        "columns",
        "--target-dir",
        target.to_str().unwrap(),
        "--trust-observed",
    ]);
    assert_eq!(code, 2);
}
