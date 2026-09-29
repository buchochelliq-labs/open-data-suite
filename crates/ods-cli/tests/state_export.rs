//! `ods state export --dbt-state` end to end (#296, ADR-0020), with the fake dbt in
//! `fixtures/dbt/fake-dbt` standing in for dbt (a Python script, so Unix only). The
//! same scenario runs against real dbt in `tests/dbt_favor_state.rs`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt")
        .join(path)
}

/// A scratch project: the base artifacts the fake dbt serves, its target directory,
/// the state database, and `prod/`, an upstream state directory.
struct Project {
    dir: PathBuf,
    env: Vec<(String, String)>,
    _guard: tempfile::TempDir,
}

const NOW: &str = "2026-09-29T12:00:00Z";

impl Project {
    /// A project in which `orders` failed to build, and everything else was
    /// recorded; `prod/` holds the same project's manifest, built in schema `prod`.
    fn built() -> Self {
        let guard = tempfile::Builder::new()
            .prefix("ods-state-export-")
            .tempdir()
            .unwrap();
        let dir = guard.path().to_owned();
        std::fs::create_dir_all(dir.join("base")).unwrap();
        std::fs::copy(
            fixture("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
            dir.join("base/manifest.json"),
        )
        .unwrap();
        let project = Self {
            env: vec![(
                "FAKE_DBT_BASE".to_owned(),
                dir.join("base").display().to_string(),
            )],
            dir,
            _guard: guard,
        };
        let failing = project.with("FAKE_DBT_FAIL", "orders");
        let (code, json) = failing.ods(&[
            "state",
            "build",
            "--exclude-resource-type",
            "test",
            "--dbt",
            fixture("fake-dbt/dbt").to_str().unwrap(),
            "--dbt-output",
            "capture",
        ]);
        assert_eq!(code, 1, "orders fails: {json:#}");
        let mut project = failing;
        project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");
        project.write_prod(|_| {});
        project
    }

    fn with(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    fn base(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join("base/manifest.json")).unwrap())
            .unwrap()
    }

    /// `prod/manifest.json`: the base manifest with every relation in schema `prod`,
    /// as prod's deployment would have written it, then edited by `edit`.
    fn write_prod(&self, edit: impl FnOnce(&mut Value)) {
        let mut manifest = self.base();
        for node in manifest["nodes"].as_object_mut().unwrap().values_mut() {
            if node["schema"] == "main" {
                node["schema"] = Value::from("prod");
            }
            if let Some(relation) = node["relation_name"].as_str() {
                node["relation_name"] = Value::from(relation.replace(r#""main""#, r#""prod""#));
            }
        }
        edit(&mut manifest);
        std::fs::create_dir_all(self.dir.join("prod")).unwrap();
        std::fs::write(
            self.dir.join("prod/manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn prod(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join("prod/manifest.json")).unwrap())
            .unwrap()
    }

    fn exported(&self, file: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join("export").join(file)).unwrap()).unwrap()
    }

    fn command(&self, args: &[&str], output: &[&str]) -> std::process::Output {
        let target = self.dir.join("target");
        let db = self.dir.join(".ods/state.db");
        Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .args([
                "--target-dir",
                target.to_str().unwrap(),
                "--state-db",
                db.to_str().unwrap(),
            ])
            .args(output)
            .env_clear()
            // The fake dbt is `#!/usr/bin/env python3`.
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &self.dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.dir)
            .output()
            .unwrap()
    }

    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let out = self.command(args, &["--json"]);
        let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap(), json)
    }

    /// `ods state export` against the fake dbt, into `export/` from `prod/`.
    fn export_args<'a>(dbt: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec![
            "state",
            "export",
            "--dbt-state",
            "export",
            "--upstream",
            "prod",
            "--dbt",
            dbt,
            "--dbt-output",
            "capture",
            "--now",
            NOW,
        ];
        args.extend(extra);
        args
    }

    fn export(&self, extra: &[&str]) -> (i32, Value) {
        let dbt = fixture("fake-dbt/dbt");
        let dbt = dbt.to_str().unwrap();
        self.ods(&Self::export_args(dbt, extra))
    }

    fn export_ok(&self, extra: &[&str]) -> Value {
        let (code, json) = self.export(extra);
        assert_eq!(code, 0, "{json:#}");
        json["result"].clone()
    }

    fn export_plain(&self) -> String {
        let dbt = fixture("fake-dbt/dbt");
        let out = self.command(
            &Self::export_args(dbt.to_str().unwrap(), &[]),
            &["-o", "plain"],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

/// Each node's reason, by id.
fn reasons(result: &Value) -> std::collections::BTreeMap<String, String> {
    result["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["node"].as_str().unwrap().to_owned(),
                n["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Every path at which `a` and `b` differ, including keys only one has.
fn diff(before: &Value, after: &Value, at: &str, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in old {
                match new.get(key) {
                    Some(changed) => diff(value, changed, &format!("{at}/{key}"), out),
                    None => out.push(format!("{at}/{key} removed")),
                }
            }
            for key in new.keys().filter(|k| !old.contains_key(*k)) {
                out.push(format!("{at}/{key} added"));
            }
        }
        (Value::Array(old), Value::Array(new)) if old.len() == new.len() => {
            for (i, (value, changed)) in old.iter().zip(new).enumerate() {
                diff(value, changed, &format!("{at}/{i}"), out);
            }
        }
        _ if before != after => out.push(at.to_owned()),
        _ => {}
    }
}

/// Replaces what differs between runs (run ids, build times) with placeholders.
fn redact(text: &str, result: &Value) -> String {
    let mut text = text.to_owned();
    for node in result["nodes"].as_array().unwrap() {
        for (key, placeholder) in [("run_id", "<run-id>"), ("built_at", "<built-at>")] {
            if let Some(value) = node[key].as_str() {
                text = text.replace(value, placeholder);
            }
        }
    }
    text.replace(result["manifest_sha256"].as_str().unwrap(), "<sha256>")
}

#[test]
fn nodes_built_here_point_at_this_target_and_the_rest_upstream() {
    let project = Project::built();
    let result = project.export_ok(&[]);
    let reasons = reasons(&result);
    assert_eq!(reasons["model.jaffle_ods.order_events"], "built_here");
    assert_eq!(reasons["seed.jaffle_ods.raw_orders"], "built_here");
    // It failed, and what reads it was skipped: nothing recorded here.
    assert_eq!(reasons["model.jaffle_ods.orders"], "not_built_here");
    let test = reasons.keys().find(|id| id.starts_with("test.")).unwrap();
    assert_eq!(reasons[test], "not_deferrable");
    assert_eq!(result["relations_checked"], true);
    assert_eq!(
        result["written"],
        serde_json::json!(["ods-export.json", "manifest.json"])
    );

    // Only the four relation fields of the nodes built here changed, to this target's.
    let (prod, written) = (project.prod(), project.exported("manifest.json"));
    let mut changed = Vec::new();
    diff(&prod, &written, "", &mut changed);
    let base = project.base();
    let mut expected: Vec<String> = Vec::new();
    for (id, _) in reasons.iter().filter(|(_, r)| *r == "built_here") {
        for f in ["database", "schema", "alias", "relation_name"] {
            if prod["nodes"][id][f] != base["nodes"][id][f] {
                expected.push(format!("/nodes/{id}/{f}"));
            }
        }
    }
    expected.sort();
    changed.sort();
    assert!(!expected.is_empty());
    assert_eq!(changed, expected);
    assert_eq!(
        written["nodes"]["model.jaffle_ods.order_events"]["relation_name"],
        project.base()["nodes"]["model.jaffle_ods.order_events"]["relation_name"]
    );
    assert_eq!(
        written["nodes"]["model.jaffle_ods.orders"]["schema"],
        "prod"
    );
    // No ODS key, anywhere in dbt's file.
    let raw = std::fs::read_to_string(project.dir.join("export/manifest.json")).unwrap();
    assert!(!raw.contains("ods-export") && !raw.contains("built_here"));

    // The record describes the manifest written.
    let record = project.exported("ods-export.json");
    assert_eq!(
        record["schema_version"],
        serde_json::json!({"major": 1, "minor": 0})
    );
    let bytes = std::fs::read(project.dir.join("export/manifest.json")).unwrap();
    assert_eq!(
        record["manifest_sha256"],
        ods_core::state::sha256_hex(&bytes)
    );
    assert_eq!(record["manifest_sha256"], result["manifest_sha256"]);
    assert_eq!(record["nodes"], result["nodes"]);
    assert_eq!(record["target"]["name"], "dev");
    assert_eq!(
        record["upstream_invocation_id"],
        project.prod()["metadata"]["invocation_id"]
    );
    // The upstream is never written to.
    assert_eq!(
        std::fs::read_dir(project.dir.join("prod")).unwrap().count(),
        1
    );

    insta::assert_snapshot!(
        "export_json",
        redact(&serde_json::to_string_pretty(&result).unwrap(), &result)
    );
    insta::assert_snapshot!("export_plain", redact(&project.export_plain(), &result));
}

#[test]
fn unchecked_or_missing_relations_point_upstream() {
    let project = Project::built();
    let result = project.export_ok(&["--no-check-relations"]);
    assert_eq!(result["relations_checked"], false);
    assert_eq!(
        reasons(&result)["model.jaffle_ods.order_events"],
        "relation_unverified"
    );
    assert!(
        !reasons(&result).values().any(|r| r == "built_here"),
        "{result:#}"
    );

    let dropped = project.dir.join("dropped");
    std::fs::write(&dropped, "model.jaffle_ods.order_events\n").unwrap();
    let project = project.with("FAKE_DBT_DROPPED", dropped.to_str().unwrap());
    let result = project.export_ok(&[]);
    assert_eq!(
        reasons(&result)["model.jaffle_ods.order_events"],
        "relation_missing"
    );
    assert_eq!(
        reasons(&result)["model.jaffle_ods.stg_orders"],
        "built_here"
    );

    // A relation the check found somewhere other than manifest.json says can't be
    // the one the export writes.
    let project = project.with("FAKE_DBT_SHOW_MOVED", "model.jaffle_ods.stg_orders");
    let result = project.export_ok(&[]);
    let why = result["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "model.jaffle_ods.stg_orders")
        .unwrap();
    assert_eq!(why["reason"], "relation_unverified", "{result:#}");
    assert!(
        why["message"].as_str().unwrap().contains("recompile"),
        "{why:#}"
    );

    // A check that fails leaves them all pointing upstream, and says so.
    let failing = project.with("FAKE_DBT_SHOW_FAIL", "1");
    let result = failing.export_ok(&[]);
    assert!(!reasons(&result).values().any(|r| r == "built_here"));
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .contains("point upstream"),
        "{result:#}"
    );
}

#[test]
fn another_target_points_everything_upstream() {
    let project = Project::built();
    let result = project.export_ok(&["--target", "other"]);
    // Another target keeps its own state: none is recorded there.
    assert!(!reasons(&result).values().any(|r| r == "built_here"));
    let project = project.with("FAKE_DBT_TARGET_HOST", "elsewhere");
    let result = project.export_ok(&[]);
    assert_eq!(
        reasons(&result)["model.jaffle_ods.order_events"],
        "target_changed"
    );
}

#[test]
fn changed_code_points_upstream() {
    let project = Project::built();
    // The next compile of `customers` differs from what was built.
    let path = project.dir.join("target/manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let node = &mut manifest["nodes"]["model.jaffle_ods.order_events"];
    node["compiled_code"] = Value::from(format!("{}\n;", node["compiled_code"].as_str().unwrap()));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let result = project.export_ok(&[]);
    let customers = result["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "model.jaffle_ods.order_events")
        .unwrap();
    assert_eq!(customers["reason"], "code_changed_since_build");
    assert_eq!(customers["changed_components"], serde_json::json!(["sql"]));
}

fn refused(project: &Project, extra: &[&str], status: i32, code: &str, says: &str) {
    let (exit, json) = project.export(extra);
    assert_eq!(exit, status, "{json:#}");
    let error = &json["diagnostics"][0];
    assert_eq!(error["code"], code, "{json:#}");
    assert!(
        error["message"].as_str().unwrap().contains(says),
        "{says}: {json:#}"
    );
    assert!(
        !project.dir.join("export/manifest.json").exists(),
        "nothing is written"
    );
}

#[test]
fn bad_inputs_are_refused_before_anything_is_written() {
    let project = Project::built();
    // Relative spellings of the same directories count.
    let (exit, json) = project.ods(&[
        "state",
        "export",
        "--dbt-state",
        "./prod/",
        "--upstream",
        "prod",
        "--dbt",
        fixture("fake-dbt/dbt").to_str().unwrap(),
    ]);
    assert_eq!(exit, 2, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0403");
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("upstream")
    );
    let target = project.dir.join("target");
    let (exit, json) = project.ods(&[
        "state",
        "export",
        "--dbt-state",
        target.to_str().unwrap(),
        "--upstream",
        "prod",
    ]);
    assert_eq!(exit, 2, "{json:#}");
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("target directory")
    );

    refused(
        &project,
        &["--artifacts", "info-schema"],
        2,
        "ODS-E0403",
        "manifest",
    );

    project.write_prod(|m| m["metadata"]["project_name"] = Value::from("shop"));
    refused(&project, &[], 2, "ODS-E0403", "project `shop`");
    project.write_prod(|m| m["metadata"]["project_id"] = Value::from("0000"));
    refused(&project, &[], 2, "ODS-E0403", "id 0000");
    project.write_prod(|m| {
        m["metadata"]["dbt_schema_version"] =
            Value::from("https://schemas.getdbt.com/dbt/manifest/v11.json");
    });
    refused(&project, &[], 2, "ODS-E0403", "v11");
    std::fs::remove_file(project.dir.join("prod/manifest.json")).unwrap();
    refused(&project, &[], 2, "ODS-E0403", "manifest.json");

    // A manifest without a project id is compared by name, with a warning.
    project.write_prod(|m| {
        m["metadata"].as_object_mut().unwrap().remove("project_id");
    });
    let result = project.export_ok(&[]);
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .contains("project id"),
        "{result:#}"
    );
}

#[test]
fn a_target_ods_cannot_identify_is_an_error() {
    let project = Project::built().with("FAKE_DBT_TARGET_FAIL", "1");
    refused(&project, &[], 1, "ODS-E0404", "target");
}

#[test]
fn a_second_export_to_the_same_directory_waits_then_fails() {
    let project = Project::built();
    std::fs::create_dir_all(project.dir.join("export")).unwrap();
    let lock = std::fs::File::create(project.dir.join("export/.ods-export.lock")).unwrap();
    lock.try_lock().unwrap();
    refused(
        &project,
        &["--lock-wait", "0"],
        1,
        "ODS-E0406",
        "another export",
    );
    assert!(!project.dir.join("export/ods-export.json").exists());
    drop(lock);
    project.export_ok(&["--lock-wait", "0"]);
}

#[test]
fn a_failed_rename_names_what_was_replaced_and_a_rerun_repairs_it() {
    let project = Project::built();
    // `manifest.json` can't be replaced: it's a directory that isn't empty.
    std::fs::create_dir_all(project.dir.join("export/manifest.json/x")).unwrap();
    let (exit, json) = project.export(&[]);
    assert_eq!(exit, 1, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0406");
    let message = json["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("can't replace manifest.json")
            && message.contains("already replaced: ods-export.json"),
        "the record is replaced first, the manifest last: {message}"
    );
    assert!(project.dir.join("export/ods-export.json").is_file());
    // No temporary file is left behind.
    let names: Vec<String> = std::fs::read_dir(project.dir.join("export"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(".ods-export-")),
        "{names:?}"
    );

    std::fs::remove_dir_all(project.dir.join("export/manifest.json")).unwrap();
    project.export_ok(&[]);
    assert!(project.dir.join("export/manifest.json").is_file());
}

#[test]
fn nothing_recorded_points_everything_upstream() {
    let project = Project::built();
    std::fs::remove_dir_all(project.dir.join(".ods")).unwrap();
    let result = project.export_ok(&[]);
    assert_eq!(result["based_on"], Value::Null);
    assert!(!reasons(&result).values().any(|r| r == "built_here"));
    // The manifest written is the upstream's, as it was.
    assert_eq!(project.exported("manifest.json"), project.prod());
    assert!(
        !project.dir.join(".ods/state.db").exists(),
        "an export never creates state"
    );
}
