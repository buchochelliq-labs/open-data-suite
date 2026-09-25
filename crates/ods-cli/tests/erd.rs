//! `ods erd generate` on the `jaffle-ods` fixture.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn artifacts(version: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt/jaffle-ods/artifacts")
        .join(version)
}

fn ods(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .env_clear()
        .env("XDG_CONFIG_HOME", std::env::temp_dir())
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn json(args: &[&str]) -> Value {
    let mut all = args.to_vec();
    all.push("--json");
    let (code, out) = ods(&all);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str::<Value>(&out).unwrap()["result"].clone()
}

fn relationships(result: &Value) -> Vec<(String, String, String)> {
    result["erd"]["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["from"].as_str().unwrap().to_owned(),
                r["to"].as_str().unwrap().to_owned(),
                r["basis"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn prints_a_mermaid_diagram_of_tested_keys_and_relationships() {
    let target = artifacts("dbt-1.10");
    let (code, out) = ods(&["erd", "generate", "--target-dir", target.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.starts_with("erDiagram\n"), "{out}");
    assert!(
        out.contains("INTEGER customer_id PK \"tested key\""),
        "{out}"
    );
    assert!(
        out.contains("orders }o--o| customers : \"customer_id\""),
        "{out}"
    );
    assert!(
        out.contains("customer_order_rank |o--o| orders"),
        "unique reference is 1:1"
    );
    assert!(
        !out.contains("stg_customers"),
        "unconnected entities are left out by default"
    );
}

#[test]
fn every_artifact_format_gives_the_same_tested_relationships() {
    let v1 = json(&[
        "erd",
        "generate",
        "--target-dir",
        artifacts("dbt-1.10").to_str().unwrap(),
    ]);
    let v2 = json(&[
        "erd",
        "generate",
        "--target-dir",
        artifacts("dbt-2.0").to_str().unwrap(),
        "--artifacts",
        "info-schema",
    ]);
    assert_eq!(v1["tested"], 3);
    assert_eq!(relationships(&v1), relationships(&v2));
}

#[test]
fn inference_is_opt_in_and_labelled() {
    let target = artifacts("dbt-1.10");
    let target = target.to_str().unwrap();
    assert_eq!(
        json(&["erd", "generate", "--target-dir", target])["inferred"],
        0
    );
    let inferred = json(&["erd", "generate", "--target-dir", target, "--infer"]);
    assert!(inferred["inferred"].as_u64().unwrap() > 0);
    assert!(
        inferred["erd"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("several possible targets")),
        "ambiguous names are reported, not guessed"
    );
}

#[test]
fn select_focuses_and_unknown_models_are_usage_errors() {
    let target = artifacts("dbt-1.10");
    let target = target.to_str().unwrap();
    let focused = json(&[
        "erd",
        "generate",
        "--target-dir",
        target,
        "--select",
        "customers",
        "--depth",
        "1",
    ]);
    let ids: Vec<&str> = focused["erd"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["customers", "orders"]);
    let (code, _) = ods(&[
        "erd",
        "generate",
        "--target-dir",
        target,
        "--select",
        "nope",
    ]);
    assert_eq!(code, 2);
    let (code, dot) = ods(&["erd", "generate", "--target-dir", target, "--format", "dot"]);
    assert_eq!(code, 0);
    assert!(dot.starts_with("digraph erd {"));
}

#[test]
fn other_erd_subcommands_are_still_planned() {
    let (code, _) = ods(&["erd", "inspect"]);
    assert_eq!(code, 3);
}
