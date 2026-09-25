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
        out.contains("orders }o--o{ stg_customers : \"customer_id (joined)\""),
        "a join in the project's SQL is a relationship, with unknown cardinality when \
         neither side is a tested key: {out}"
    );
    assert!(
        !out.contains("raw_customers"),
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
    assert_eq!(v1["tested"], 2);
    assert_eq!(
        v1["declared"], 1,
        "customer_order_rank's foreign key constraint"
    );
    assert_eq!(relationships(&v1), relationships(&v2));
}

#[test]
fn primary_and_foreign_key_constraints_are_declared_keys_and_relationships() {
    let result = json(&[
        "erd",
        "generate",
        "--target-dir",
        artifacts("dbt-1.10").to_str().unwrap(),
    ]);
    // A model-level composite primary key.
    let rank = primary_key(&result, "customer_order_rank");
    assert_eq!(
        rank["columns"],
        serde_json::json!(["customer_id", "order_seq"])
    );
    assert_eq!(rank["basis"], "declared");
    // A column-level primary key: declared wins over the unique + not_null tests.
    let orders = primary_key(&result, "orders");
    assert_eq!(orders["basis"], "declared");
    // A foreign key written as `expression: "main.orders (order_id)"`, merged with the
    // relationships test on the same columns.
    let fk = result["erd"]["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["from"] == "model.jaffle_ods.customer_order_rank")
        .unwrap();
    assert_eq!(fk["to"], "model.jaffle_ods.orders");
    assert_eq!(fk["basis"], "declared");
    let evidence = fk["evidence"].to_string();
    assert!(
        evidence.contains("constraint foreign_key") && evidence.contains("relationships_"),
        "{evidence}"
    );
    assert_eq!(result["erd"]["diagnostics"], serde_json::json!([]));
}

#[test]
fn foreign_keys_in_the_to_syntax_and_unresolvable_ones() {
    let dir = patched("fk-to", |m| {
        let rank = &mut m["nodes"]["model.jaffle_ods.customer_order_rank"]["constraints"];
        rank[1]["expression"] = Value::Null;
        rank[1]["to"] = "ref('orders')".into();
        rank[1]["to_columns"] = serde_json::json!(["order_id"]);
        let orders = &mut m["nodes"]["model.jaffle_ods.orders"];
        orders["constraints"] = serde_json::json!([{
            "type": "foreign_key", "columns": ["customer_id"],
            "expression": "somewhere_else.customers (customer_id)"
        }]);
    });
    let result = json(&["erd", "generate", "--target-dir", dir.to_str().unwrap()]);
    let declared: Vec<_> = relationships(&result)
        .into_iter()
        .filter(|r| r.2 == "declared")
        .collect();
    assert_eq!(
        declared,
        [(
            "model.jaffle_ods.customer_order_rank".to_owned(),
            "model.jaffle_ods.orders".to_owned(),
            "declared".to_owned()
        )]
    );
    assert!(
        result["erd"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("somewhere_else.customers")),
        "a table ODS doesn't know is reported, not guessed: {}",
        result["erd"]["diagnostics"]
    );
    std::fs::remove_dir_all(dir).ok();
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

/// A copy of the dbt 1.10 fixture with `edit` applied to its manifest.
fn patched(name: &str, edit: impl FnOnce(&mut Value)) -> PathBuf {
    let from = artifacts("dbt-1.10");
    let dir = std::env::temp_dir().join(format!("ods-erd-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for file in ["manifest.json", "catalog.json"] {
        std::fs::copy(from.join(file), dir.join(file)).unwrap();
    }
    let path = dir.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    dir
}

fn primary_key(result: &Value, entity: &str) -> Value {
    result["erd"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == entity)
        .unwrap_or_else(|| panic!("no {entity}"))["primary_key"]
        .clone()
}

#[test]
fn composite_keys_come_from_unique_key_config_and_column_combinations() {
    let dir = patched("composite", |m| {
        m["nodes"]["model.jaffle_ods.customer_order_rank"]["config"]["unique_key"] =
            serde_json::json!(["customer_id", "order_seq"]);
        // No `not_null` on either column: a unique combination is still the grain.
        let mut test = m["nodes"]["test.jaffle_ods.unique_stg_orders_order_id.e3b841c71a"].clone();
        test["unique_id"] = "test.jaffle_ods.combo_stg_customers.1".into();
        test["name"] = "dbt_utils_unique_combination_of_columns_stg_customers".into();
        test["column_name"] = Value::Null;
        test["attached_node"] = "model.jaffle_ods.stg_customers".into();
        test["depends_on"]["nodes"] = serde_json::json!(["model.jaffle_ods.stg_customers"]);
        test["refs"] =
            serde_json::json!([{"name": "stg_customers", "package": null, "version": null}]);
        test["test_metadata"] = serde_json::json!({
            "name": "unique_combination_of_columns",
            "namespace": "dbt_utils",
            "kwargs": {
                "combination_of_columns": ["customer_id", "signup_date"],
                "model": "{{ get_where_subquery(ref('stg_customers')) }}"
            }
        });
        m["nodes"]["test.jaffle_ods.combo_stg_customers.1"] = test;
    });
    let result = json(&[
        "erd",
        "generate",
        "--target-dir",
        dir.to_str().unwrap(),
        "--all",
        "--format",
        "json",
    ]);
    let config = primary_key(&result, "customer_order_rank");
    assert_eq!(
        config["columns"],
        serde_json::json!(["customer_id", "order_seq"])
    );
    assert_eq!(config["basis"], "declared");
    let combination = primary_key(&result, "stg_customers");
    assert_eq!(
        combination["columns"],
        serde_json::json!(["customer_id", "signup_date"])
    );
    assert_eq!(combination["basis"], "tested");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn filtered_tests_say_nothing_about_keys() {
    let dir = patched("where", |m| {
        m["nodes"]["test.jaffle_ods.unique_stg_orders_order_id.e3b841c71a"]["config"]["where"] =
            "status = 'completed'".into();
    });
    let result = json(&[
        "erd",
        "generate",
        "--target-dir",
        dir.to_str().unwrap(),
        "--all",
        "--format",
        "json",
    ]);
    assert!(primary_key(&result, "stg_orders").is_null());
    assert!(
        result["erd"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("where")),
        "{}",
        result["erd"]["diagnostics"]
    );
    std::fs::remove_dir_all(dir).ok();
}
