//! `ods state plan | record | history` on the `jaffle-ods` fixture (ADR-0013).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory holding a copy of the fixture's `dbt build` artifacts (optionally
/// edited) and the state database. Removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ods-state-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build");
        for file in ["manifest.json", "run_results.json"] {
            std::fs::copy(fixture.join(file), target.join(file)).unwrap();
        }
        Self { dir }
    }

    fn target(&self) -> PathBuf {
        self.dir.join("target")
    }

    fn db(&self) -> PathBuf {
        self.dir.join(".ods").join("state.db")
    }

    fn edit(&self, file: &str, change: impl FnOnce(&mut Value)) {
        let path = self.target().join(file);
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut value);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let target = self.target();
        let db = self.db();
        let mut all: Vec<&str> = args.to_vec();
        all.extend([
            "--target-dir",
            target.to_str().unwrap(),
            "--state-db",
            db.to_str().unwrap(),
            "--json",
        ]);
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(&all)
            .env_clear()
            .env("XDG_CONFIG_HOME", &self.dir)
            .current_dir(&self.dir)
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (out.status.code().unwrap(), json)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, json) = self.ods(args);
        assert_eq!(code, 0, "{json:#}");
        json["result"].clone()
    }

    fn plan(&self, extra: &[&str]) -> Value {
        let mut args = vec!["state", "plan", "--now", "2026-09-25T12:00:00Z"];
        args.extend(extra);
        self.ok(&args)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `name → (action, first reason code)`.
fn decisions(plan: &Value) -> std::collections::BTreeMap<String, (String, String)> {
    plan["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_owned(),
                (
                    e["action"].as_str().unwrap().to_owned(),
                    e["reasons"][0]["code"].as_str().unwrap().to_owned(),
                ),
            )
        })
        .collect()
}

fn pair(action: &str, code: &str) -> (String, String) {
    (action.to_owned(), code.to_owned())
}

#[test]
fn without_state_everything_is_built_and_planning_writes_nothing() {
    let s = Scratch::new("fresh");
    let plan = s.plan(&[]);
    assert_eq!(plan["build"], 13);
    assert_eq!(plan["reuse"], 0);
    assert_eq!(plan["based_on"], Value::Null);
    assert!(
        plan["dbt_command"]
            .as_str()
            .unwrap()
            .starts_with("dbt build --select raw_customers raw_orders raw_payments stg_customers"),
        "{}",
        plan["dbt_command"]
    );
    assert!(!s.db().exists(), "plan must not create or change state");
    assert_eq!(
        plan["plan"]["schema_version"],
        json!({"major": 1, "minor": 1})
    );
}

#[test]
fn a_recorded_run_is_reused_until_something_changes() {
    let s = Scratch::new("reuse");
    let recorded = s.ok(&["state", "record"]);
    assert_eq!(recorded["snapshot"], 1);
    assert_eq!(recorded["advanced"].as_array().unwrap().len(), 13);
    assert_eq!(recorded["scope"], "jaffle_ods/default");

    let plan = s.plan(&[]);
    assert_eq!(
        (plan["build"].clone(), plan["reuse"].clone()),
        (json!(0), json!(13))
    );
    assert_eq!(plan["dbt_command"], Value::Null);
    assert_eq!(
        decisions(&plan)["customer_segments"],
        pair("reuse", "unchanged"),
        "a Python model is fingerprinted like any other (its code is in the manifest)"
    );
    let entry = &plan["plan"]["entries"][0];
    assert!(
        entry["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "relation_exists" && e["exactness"] == "none"),
        "reuse is labelled as not checked against the warehouse: {entry:#}"
    );

    // A comment and a reformat of stg_orders change nothing that is built (#209).
    s.edit("manifest.json", |m| {
        let node = &mut m["nodes"]["model.jaffle_ods.stg_orders"];
        let sql = node["compiled_code"].as_str().unwrap().to_owned();
        node["compiled_code"] = json!(format!("-- edited\n{}", sql.replace('\n', "\n  ")));
        node["checksum"]["checksum"] = json!("edited");
    });
    let plan = s.plan(&[]);
    let stg = plan["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "stg_orders")
        .unwrap();
    assert_eq!(stg["action"], "reuse", "{stg:#}");
    let why = stg["reasons"][0]["message"].as_str().unwrap();
    assert!(why.contains("only formatting changed (sql)"), "{why}");
    assert_eq!(decisions(&plan)["orders"], pair("reuse", "unchanged"));

    // Change stg_orders' compiled SQL, as `dbt compile` would after an edit.
    s.edit("manifest.json", |m| {
        let node = &mut m["nodes"]["model.jaffle_ods.stg_orders"];
        let sql = node["compiled_code"].as_str().unwrap().to_owned();
        node["compiled_code"] = json!(format!("{sql}\nwhere 1 = 1"));
    });
    let plan = s.plan(&[]);
    let got = decisions(&plan);
    assert_eq!(got["stg_orders"], pair("build", "code_changed"));
    assert_eq!(got["orders"], pair("build", "upstream_code_changed"));
    assert_eq!(got["customers"], pair("build", "upstream_code_changed"));
    // The Python model and the SQL model reading it rebuild too.
    assert_eq!(
        got["customer_segments"],
        pair("build", "upstream_code_changed")
    );
    assert_eq!(
        got["segment_summary"],
        pair("build", "upstream_code_changed")
    );
    assert_eq!(
        got["stg_customers"],
        pair("reuse", "unchanged"),
        "unrelated"
    );
    assert_eq!(
        got["raw_orders"],
        pair("reuse", "unchanged"),
        "upstream of the change"
    );
    let stg = plan["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "stg_orders")
        .unwrap();
    assert_eq!(stg["changed_components"], json!(["sql"]));

    // `--select` narrows the plan, not the decisions.
    let narrowed = s.plan(&["--select", "+orders"]);
    let names: Vec<&str> = narrowed["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "raw_orders",
            "raw_payments",
            "stg_orders",
            "stg_payments",
            "orders"
        ]
    );
    assert_eq!(
        narrowed["dbt_command"],
        "dbt build --select stg_orders orders"
    );
}

#[test]
fn a_failed_node_keeps_its_last_state_and_catches_up_next_time() {
    let s = Scratch::new("failure");
    s.ok(&["state", "record"]);
    // A second run: stg_orders rebuilt, orders failed, its children skipped.
    s.edit("manifest.json", |m| {
        m["metadata"]["invocation_id"] = json!("run-2");
    });
    s.edit("run_results.json", |r| {
        r["metadata"]["invocation_id"] = json!("run-2");
        r["metadata"]["invocation_started_at"] = json!("2026-09-25T09:59:00Z");
        r["metadata"]["generated_at"] = json!("2026-09-25T10:00:00Z");
        for result in r["results"].as_array_mut().unwrap() {
            let id = result["unique_id"].as_str().unwrap().to_owned();
            for timing in result["timing"].as_array_mut().unwrap() {
                timing["completed_at"] = json!("2026-09-25T10:00:00Z");
            }
            match id.as_str() {
                "model.jaffle_ods.orders" => result["status"] = json!("error"),
                "model.jaffle_ods.customers" | "model.jaffle_ods.customer_order_rank" => {
                    result["status"] = json!("skipped");
                }
                _ => {}
            }
        }
    });
    let recorded = s.ok(&["state", "record"]);
    assert_eq!(recorded["snapshot"], 2);
    assert_eq!(recorded["parent"], 1);
    assert_eq!(recorded["run_id"], "run-2");
    let kept = recorded["kept"].as_object().unwrap();
    assert!(
        kept["model.jaffle_ods.orders"]
            .as_str()
            .unwrap()
            .contains("failed")
    );
    assert!(kept.contains_key("model.jaffle_ods.customers"));

    let got = decisions(&s.plan(&[]));
    assert_eq!(got["stg_orders"], pair("reuse", "unchanged"));
    assert_eq!(
        got["orders"],
        pair("build", "new_upstream_data"),
        "its parents were rebuilt after its last success"
    );
    assert_eq!(got["customers"], pair("build", "new_upstream_data"));

    let history = s.ok(&["state", "history"]);
    let ids: Vec<i64> = history["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [2, 1]);
}

#[test]
fn bad_input_is_reported_with_state_error_codes() {
    let s = Scratch::new("errors");
    std::fs::remove_file(s.target().join("run_results.json")).unwrap();
    let (code, json) = s.ods(&["state", "record"]);
    assert_eq!(code, 1);
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0403", "{json:#}");
    assert!(!s.db().exists() || s.ok(&["state", "history"])["snapshots"] == json!([]));

    let (code, json) = s.ods(&["state", "plan", "--select", "nope"]);
    assert_eq!(code, 2);
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0203");

    let (code, json) = s.ods(&["state", "plan", "--environment", "a/b"]);
    assert_eq!(code, 2);
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0403");
}

#[test]
fn environments_keep_separate_state() {
    let s = Scratch::new("envs");
    s.ok(&["state", "record", "--environment", "dev"]);
    assert_eq!(s.plan(&["--environment", "dev"])["reuse"], 13);
    assert_eq!(s.plan(&["--environment", "prod"])["build"], 13);
}

#[test]
fn only_real_builds_of_this_code_are_recorded() {
    let refused = |edit: &dyn Fn(&Scratch), why: &str| {
        let s = Scratch::new("refused");
        edit(&s);
        let (code, json) = s.ods(&["state", "record"]);
        assert_eq!(code, 1, "{why}: {json:#}");
        assert_eq!(json["diagnostics"][0]["code"], "ODS-E0403", "{why}");
        assert!(!s.db().exists(), "{why}: nothing is written");
    };
    refused(
        &|s| {
            s.edit("run_results.json", |r| {
                r["args"]["which"] = json!("generate");
            });
        },
        "`dbt docs generate` reports every node as a success",
    );
    refused(
        &|s| {
            s.edit("run_results.json", |r| {
                r["args"]["which"] = json!("compile");
            });
        },
        "`dbt compile` builds nothing",
    );
    refused(
        &|s| s.edit("run_results.json", |r| r["args"]["empty"] = json!(true)),
        "`--empty` builds no rows",
    );
    refused(
        &|s| {
            s.edit("manifest.json", |m| {
                m["metadata"]["invocation_id"] = json!("a-later-compile");
            });
        },
        "the manifest was rewritten after the run",
    );
}

#[test]
fn a_run_is_recorded_once_and_in_order() {
    let s = Scratch::new("order");
    s.ok(&["state", "record"]);
    let (code, json) = s.ods(&["state", "record"]);
    assert_eq!(code, 1);
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("already recorded")
    );

    // A different run that started before the recorded one finished.
    s.edit("manifest.json", |m| {
        m["metadata"]["invocation_id"] = json!("older");
    });
    s.edit("run_results.json", |r| {
        r["metadata"]["invocation_id"] = json!("older");
        r["metadata"]["invocation_started_at"] = json!("2026-01-01T00:00:00Z");
    });
    let (code, json) = s.ods(&["state", "record"]);
    assert_eq!(code, 1);
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("before the recorded state"),
        "{json:#}"
    );
    assert_eq!(
        s.ok(&["state", "history"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn sources_json(generated_at: &str, max_loaded_at: &str) -> Value {
    json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/sources/v3.json",
            "generated_at": generated_at,
            "invocation_id": "freshness"
        },
        "results": [{
            "unique_id": "source.jaffle_ods.landing.feed",
            "status": "pass",
            "max_loaded_at": max_loaded_at,
            "snapshotted_at": generated_at
        }],
        "elapsed_time": 0.1
    })
}

#[test]
fn source_versions_count_only_when_measured_after_the_last_build() {
    let s = Scratch::new("sources");
    // A source that stg_orders reads.
    s.edit("manifest.json", |m| {
        m["sources"]["source.jaffle_ods.landing.feed"] = json!({
            "unique_id": "source.jaffle_ods.landing.feed",
            "resource_type": "source",
            "name": "feed",
            "config": {"enabled": true}
        });
        m["nodes"]["model.jaffle_ods.stg_orders"]["depends_on"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!("source.jaffle_ods.landing.feed"));
    });
    let write = |value: &Value| {
        std::fs::write(s.target().join("sources.json"), value.to_string()).unwrap();
    };
    // `dbt source freshness` just before the build.
    write(&sources_json(
        "2026-09-25T06:40:00Z",
        "2026-09-25T06:30:00+00:00",
    ));
    let recorded = s.ok(&["state", "record"]);
    assert_eq!(recorded["sources_recorded"], true);

    // Planning later with the same file: it says nothing about data since the build.
    let plan = s.plan(&[]);
    assert_eq!(
        decisions(&plan)["stg_orders"],
        pair("build", "missing_data_evidence")
    );
    assert!(
        plan["warnings"][0]
            .as_str()
            .unwrap()
            .contains("dbt source freshness"),
        "{:#}",
        plan["warnings"]
    );

    // Measured again after the build, no new data: reused.
    write(&sources_json(
        "2026-09-25T11:00:00Z",
        "2026-09-25T06:30:00+00:00",
    ));
    let plan = s.plan(&[]);
    assert_eq!(decisions(&plan)["stg_orders"], pair("reuse", "unchanged"));
    assert_eq!(
        decisions(&plan)["stg_customers"],
        pair("reuse", "unchanged")
    );

    // New data arrived.
    write(&sources_json(
        "2026-09-25T11:00:00Z",
        "2026-09-25T10:45:00+00:00",
    ));
    let got = decisions(&s.plan(&[]));
    assert_eq!(got["stg_orders"], pair("build", "new_upstream_data"));
    assert_eq!(got["orders"], pair("build", "new_upstream_data"));
    assert_eq!(got["stg_customers"], pair("reuse", "unchanged"));
}
