//! `ods state run` end to end, with the fake dbt in `fixtures/dbt/fake-dbt` standing in
//! for dbt (a Python script, so Unix only; no warehouse or network). The same flow runs
//! against real dbt when `ODS_TEST_DBT` names a dbt executable with `dbt-duckdb`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt")
        .join(path)
}

/// A scratch project directory: the base artifacts the fake dbt serves (editable, to
/// simulate code changes), its target directory, and the state database.
struct Project {
    dir: PathBuf,
    env: Vec<(String, String)>,
}

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ods-state-run-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("base")).unwrap();
        std::fs::copy(
            fixture("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
            dir.join("base/manifest.json"),
        )
        .unwrap();
        Self {
            env: vec![(
                "FAKE_DBT_BASE".to_owned(),
                dir.join("base").display().to_string(),
            )],
            dir,
        }
    }

    fn with(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Changes a node's code in what the fake dbt compiles.
    fn change_code(&self, node: &str) {
        let path = self.dir.join("base/manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &mut manifest["nodes"][node];
        let code = entry["compiled_code"].as_str().unwrap().to_owned();
        // A statement terminator: a real change, however formatting is treated.
        entry["compiled_code"] = Value::String(format!("{code}\n;"));
        entry["checksum"]["checksum"] = Value::String(format!("changed-{}", code.len()));
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }

    /// Edits a data test's definition, as changing its arguments in YAML would.
    fn change_test(&self, test: &str) {
        let path = self.dir.join("base/manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &mut manifest["nodes"][test]["test_metadata"]["kwargs"];
        entry["where"] = Value::String("order_id > 0".to_owned());
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }

    fn db(&self) -> PathBuf {
        self.dir.join(".ods/state.db")
    }

    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let (code, json, _) = self.ods_with_stderr(args);
        (code, json)
    }

    /// Like [`ods`](Self::ods), and also returns what ODS wrote to stderr.
    fn ods_with_stderr(&self, args: &[&str]) -> (i32, Value, String) {
        let target = self.dir.join("target");
        let db = self.db();
        // ODS's own options go before any `--`: what follows it is for dbt.
        let split = args.iter().position(|a| *a == "--").unwrap_or(args.len());
        let mut all: Vec<&str> = args[..split].to_vec();
        all.extend([
            "--target-dir",
            target.to_str().unwrap(),
            "--state-db",
            db.to_str().unwrap(),
            "--json",
        ]);
        all.extend(&args[split..]);
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(&all)
            .env_clear()
            // The fake dbt is `#!/usr/bin/env python3`.
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &self.dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.dir)
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (
            out.status.code().unwrap(),
            json,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run(&self, extra: &[&str]) -> (i32, Value) {
        let dbt = fixture("fake-dbt/dbt");
        let mut args = vec![
            "state",
            "run",
            "--dbt",
            dbt.to_str().unwrap(),
            "--dbt-output",
            "capture",
        ];
        args.extend(extra);
        self.ods(&args)
    }

    fn test(&self, extra: &[&str]) -> (i32, Value) {
        let dbt = fixture("fake-dbt/dbt");
        let mut args = vec![
            "state",
            "test",
            "--dbt",
            dbt.to_str().unwrap(),
            "--dbt-output",
            "capture",
        ];
        args.extend(extra);
        self.ods(&args)
    }

    fn test_ok(&self, extra: &[&str]) -> Value {
        let (code, json) = self.test(extra);
        assert_eq!(code, 0, "{json:#}");
        json["result"].clone()
    }

    fn run_ok(&self, extra: &[&str]) -> Value {
        let (code, json) = self.run(extra);
        assert_eq!(code, 0, "{json:#}");
        json["result"].clone()
    }

    fn history(&self) -> Vec<Value> {
        let (code, json) = self.ods(&["state", "history"]);
        assert_eq!(code, 0, "{json:#}");
        json["result"]["snapshots"].as_array().unwrap().clone()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn names(nodes: &Value) -> Vec<String> {
    let mut names: Vec<String> = nodes
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let id = n.as_str().or_else(|| n["node"].as_str()).unwrap();
            id.rsplit('.').next().unwrap().to_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn builds_everything_once_then_only_what_changed() {
    let project = Project::new("happy");
    let first = project.run_ok(&[]);
    assert_eq!(first["outcome"], "succeeded");
    assert_eq!(first["build"], 13);
    assert_eq!(first["record"]["snapshot"], 1);
    assert_eq!(first["record"]["advanced"].as_array().unwrap().len(), 13);
    assert!(
        first["execution"]["command"].as_str().unwrap().contains(
            "build --select fqn:jaffle_ods,resource_type:model fqn:jaffle_ods,resource_type:seed"
        ),
        "everything is requested, so whole folders are selected: {first:#}"
    );

    // Nothing changed: nothing runs and nothing is recorded.
    let again = project.run_ok(&[]);
    assert_eq!(again["outcome"], "nothing_to_build");
    assert_eq!(again["reuse"], 13);
    assert!(again.get("execution").is_none());
    assert_eq!(project.history().len(), 1);

    // One model changes: it and its descendants build, nothing else.
    project.change_code("model.jaffle_ods.customer_segments");
    let changed = project.run_ok(&[]);
    assert_eq!(changed["outcome"], "succeeded");
    assert_eq!(
        names(&changed["execution"]["nodes"]),
        ["customer_segments", "segment_summary"]
    );
    assert_eq!(changed["record"]["snapshot"], 2);
    assert_eq!(project.history().len(), 2);
}

#[test]
fn a_failed_node_keeps_its_last_state_and_the_successes_advance() {
    let project = Project::new("failure");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.stg_orders");
    let project = project.with("FAKE_DBT_FAIL", "orders");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0404");
    let result = &json["result"];
    assert_eq!(result["outcome"], "failed");
    assert_eq!(result["execution"]["succeeded"], false);
    // stg_orders and the siblings that don't read `orders` advanced.
    let advanced = names(&result["record"]["advanced"]);
    assert!(advanced.contains(&"stg_orders".to_owned()), "{advanced:?}");
    assert!(!advanced.contains(&"orders".to_owned()), "{advanced:?}");
    let kept: Vec<&str> = result["record"]["kept"]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.rsplit('.').next().unwrap())
        .collect();
    assert!(kept.contains(&"orders"), "{kept:?}");
    assert!(kept.contains(&"customer_order_rank"), "{kept:?}");

    // Next time, the failed node and what it skipped build; stg_orders is reused.
    let (_, plan) = project.ods(&["state", "plan"]);
    let plan = &plan["result"]["plan"]["entries"];
    let action = |name: &str| {
        plan.as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap()["action"]
            .clone()
    };
    assert_eq!(action("orders"), "build");
    assert_eq!(action("customer_order_rank"), "build");
    assert_eq!(action("stg_orders"), "reuse");
}

#[test]
fn nothing_is_committed_when_nothing_succeeds() {
    let project = Project::new("all-fail");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.segment_summary");
    let project = project.with("FAKE_DBT_FAIL", "segment_summary");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert!(json["result"]["record"]["snapshot"].is_null(), "{json:#}");
    assert_eq!(project.history().len(), 1);
}

#[test]
fn a_node_whose_tests_fail_keeps_its_last_state() {
    let project = Project::new("tests").with("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    let (code, json) = project.run(&["--test"]);
    assert_eq!(code, 1, "{json:#}");
    let result = &json["result"];
    assert_eq!(
        result["execution"]["checks_failed"][0],
        "test.jaffle_ods.unique_orders_order_id.fed79b3a6e"
    );
    // `orders` was built, but not validated: it doesn't advance, so it (and its test)
    // runs again next time.
    let advanced = names(&result["record"]["advanced"]);
    assert_eq!(advanced.len(), 12, "{advanced:?}");
    assert!(!advanced.contains(&"orders".to_owned()), "{advanced:?}");
    assert!(
        result["record"]["kept"]
            .as_object()
            .unwrap()
            .contains_key("model.jaffle_ods.orders")
    );
    let again = project.run(&["--test"]).1;
    assert!(names(&again["result"]["execution"]["nodes"]).contains(&"orders".to_owned()));

    // A tested build that is rebuilt and fails its tests is no longer tested: the
    // warehouse holds the new build, so `ods state test` picks it up.
    let project = project.with("FAKE_DBT_FAIL_TEST", "");
    project.run_ok(&["--test"]);
    assert_eq!(project.test_ok(&[])["outcome"], "nothing_to_test");
    project.change_code("model.jaffle_ods.orders");
    let project = project.with("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    assert_eq!(project.run(&["--test"]).0, 1);
    let (_, tested) = project.test(&[]);
    assert!(
        names(&tested["result"]["execution"]["nodes"]).contains(&"orders".to_owned()),
        "{tested:#}"
    );
}

/// By default dbt's own output streams to stderr while it runs, as with dbt itself;
/// stdout keeps only ODS's report.
#[test]
fn dbt_output_streams_to_stderr() {
    let project = Project::new("dbt-output");
    let dbt = fixture("fake-dbt/dbt");
    let (code, json, stderr) =
        project.ods_with_stderr(&["state", "run", "--dbt", dbt.to_str().unwrap()]);
    assert_eq!(code, 0, "{json:#}");
    for line in ["fake dbt: compile", "fake dbt: build"] {
        assert!(
            stderr.contains(line),
            "{line} missing from stderr:\n{stderr}"
        );
    }
    let (_, _, captured) = project.ods_with_stderr(&[
        "state",
        "run",
        "--dbt",
        dbt.to_str().unwrap(),
        "--dbt-output",
        "capture",
    ]);
    assert!(!captured.contains("fake dbt:"), "{captured}");
}

/// A test dbt skipped (e.g. after `--fail-fast` stopped) tested nothing: the node is
/// built but stays untested, and a test run doesn't mark it tested either.
#[test]
fn a_skipped_test_leaves_its_node_untested() {
    let project = Project::new("skipped-test").with("FAKE_DBT_SKIP_TEST", "unique_orders_order_id");
    let result = project.run_ok(&["--test"]);
    assert_eq!(result["record"]["advanced"].as_array().unwrap().len(), 13);
    let orders = result["execution"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "model.jaffle_ods.orders")
        .unwrap();
    assert_eq!(
        orders["checks_skipped"][0],
        "test.jaffle_ods.unique_orders_order_id.fed79b3a6e"
    );

    // Not a failure, but not a pass either: orders stays untested.
    let (code, tested) = project.test(&["--all"]);
    assert_eq!(code, 1, "{tested:#}");
    assert_eq!(tested["result"]["outcome"], "incomplete");
    assert_eq!(tested["result"]["record"]["failed"], serde_json::json!([]));
    assert!(!names(&tested["result"]["record"]["passed"]).contains(&"orders".to_owned()));

    let project = project.with("FAKE_DBT_SKIP_TEST", "");
    let again = project.test_ok(&[]);
    assert_eq!(again["requested"], 1, "only orders was left untested");
    assert_eq!(names(&again["record"]["passed"]), ["orders"]);
}

/// A test run in which no test ran vouches for nothing, and a changed test makes its
/// nodes untested again (#220 review).
#[test]
fn only_tests_that_ran_mark_a_build_tested_and_changed_tests_run_again() {
    let project = Project::new("vouch").with("FAKE_DBT_NO_TESTS", "1");
    project.run_ok(&[]);
    let (code, json) = project.test(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["result"]["outcome"], "incomplete");
    assert_eq!(json["result"]["record"]["passed"], serde_json::json!([]));

    let project = project.with("FAKE_DBT_NO_TESTS", "");
    assert_eq!(project.test_ok(&[])["requested"], 5);
    assert_eq!(project.test_ok(&[])["outcome"], "nothing_to_test");
    // Only the nodes that test reads are tested again: stg_orders is left alone.
    project.change_test("test.jaffle_ods.unique_orders_order_id.fed79b3a6e");
    let again = project.test_ok(&[]);
    assert_eq!(names(&again["record"]["passed"]), ["orders"]);
}

/// #220: a plain run builds without tests; `--test` builds and tests.
#[test]
fn a_plain_run_builds_without_tests() {
    let project = Project::new("no-tests").with("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    let result = project.run_ok(&[]);
    let command = result["execution"]["command"].as_str().unwrap();
    assert!(
        command.contains("--exclude-resource-type test --exclude-resource-type unit_test"),
        "{command}"
    );
    assert_eq!(result["tests"], false);
    assert_eq!(result["execution"]["checks_failed"], serde_json::json!([]));
    assert_eq!(result["record"]["advanced"].as_array().unwrap().len(), 13);

    // What was built without its tests is untested: `ods state test` runs them.
    let (code, tested) = project.test(&[]);
    assert_eq!(code, 1, "the failing test fails: {tested:#}");
    let tested = &tested["result"];
    // Only the 5 nodes with tests: the other 8 have nothing that could vouch for them.
    assert_eq!(tested["requested"], 5);
    assert_eq!(tested["without_checks"], 8);
    assert!(
        tested["execution"]["command"]
            .as_str()
            .unwrap()
            .starts_with(&format!(
                "{} test --select",
                fixture("fake-dbt/dbt").display()
            ))
    );
    assert_eq!(names(&tested["record"]["failed"]), ["orders"]);
    assert_eq!(tested["record"]["passed"].as_array().unwrap().len(), 4);

    // Once fixed, only the node still untested is tested again.
    let project = project.with("FAKE_DBT_FAIL_TEST", "");
    let again = project.test_ok(&[]);
    assert_eq!(again["requested"], 1);
    assert_eq!(names(&again["record"]["passed"]), ["orders"]);
    let nothing = project.test_ok(&[]);
    assert_eq!(nothing["outcome"], "nothing_to_test");
    // --all tests everything again, and a build with --test marks what it builds tested.
    assert_eq!(project.test_ok(&["--all"])["requested"], 5);
    project.change_code("model.jaffle_ods.orders");
    project.run_ok(&["--test"]);
    assert_eq!(project.test_ok(&[])["outcome"], "nothing_to_test");
}

#[test]
fn exclude_and_resource_type_leave_nodes_to_build() {
    let project = Project::new("narrow");
    let result = project.run_ok(&["--resource-type", "seed"]);
    assert_eq!(
        names(&result["execution"]["nodes"]),
        ["raw_customers", "raw_orders", "raw_payments"]
    );
    assert_eq!(result["left_out"].as_array().unwrap().len(), 10);
    // The models are still to build; excluding one leaves it (and it only) out.
    let result = project.run_ok(&["--exclude", "segment_summary"]);
    assert_eq!(result["execution"]["nodes"].as_array().unwrap().len(), 9);
    assert_eq!(names(&result["left_out"]), ["segment_summary"]);
    let last = project.run_ok(&[]);
    assert_eq!(names(&last["execution"]["nodes"]), ["segment_summary"]);
}

#[test]
fn full_refresh_and_dbt_args_are_passed_through_and_selection_args_refused() {
    let project = Project::new("passthrough");
    let result = project.run_ok(&["--full-refresh", "--", "--threads", "8"]);
    let command = result["execution"]["command"].as_str().unwrap();
    assert!(command.contains("--full-refresh"), "{command}");
    assert!(command.ends_with("--threads 8"), "{command}");
    project.change_code("model.jaffle_ods.orders");
    let (code, json) = project.run(&["--", "--select", "orders"]);
    assert_eq!(code, 1, "{json:#}");
    let message = json["diagnostics"][0]["message"].as_str().unwrap();
    assert!(message.contains("--select"), "{message}");
}

#[test]
fn a_run_that_cant_be_recorded_still_reports_what_dbt_did() {
    let project = Project::new("unrecorded").with("FAKE_DBT_KEEP_MANIFEST", "1");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0403");
    assert_eq!(json["result"]["outcome"], "not_recorded");
    assert_eq!(
        json["result"]["execution"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        13
    );
    assert!(json["result"].get("record").is_none());
    assert!(project.history().is_empty());
}

#[test]
fn a_dry_run_changes_nothing() {
    let project = Project::new("dry");
    let result = project.run_ok(&["--dry-run"]);
    assert_eq!(result["outcome"], "dry_run");
    assert_eq!(result["build"], 13);
    assert!(result.get("execution").is_none());
    assert!(!project.db().exists());
}

#[test]
fn select_limits_the_run() {
    let project = Project::new("select");
    let result = project.run_ok(&["--select", "+stg_orders"]);
    assert_eq!(
        names(&result["execution"]["nodes"]),
        ["raw_orders", "stg_orders"]
    );
}

#[test]
fn when_dbt_cannot_run_nothing_is_recorded() {
    let project = Project::new("broken").with("FAKE_DBT_EXIT", "2");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0404");
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("compile"),
        "{json:#}"
    );
    assert!(!project.db().exists() || project.history().is_empty());

    // With artifacts already there, planning works and the build itself fails.
    let target = project.dir.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        project.dir.join("base/manifest.json"),
        target.join("manifest.json"),
    )
    .unwrap();
    let (code, json) = project.run(&["--no-compile"]);
    assert_eq!(code, 1, "{json:#}");
    assert!(
        json["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("wrote no run results"),
        "{json:#}"
    );
    assert!(project.history().is_empty());
}

/// The same flow against real dbt and `DuckDB`, on a copy of the demo project. Set
/// `ODS_TEST_DBT` to a dbt executable with `dbt-duckdb` installed.
#[test]
fn real_dbt() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let project = Project::new("real").with("DBT_SEND_ANONYMOUS_USAGE_STATS", "false");
    let root = fixture("jaffle-ods");
    for entry in [
        "dbt_project.yml",
        "profiles.yml",
        "macros",
        "models",
        "seeds",
    ] {
        copy(&root.join(entry), &project.dir.join(entry));
    }
    // A folder named like a model (#211): `fqn:…segment_summary` alone would also
    // select the model in it.
    let nested = project.dir.join("models/marts/segment_summary");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("unrelated.sql"),
        "select count(*) as n from {{ ref('raw_orders') }}\n",
    )
    .unwrap();
    let dbt = dbt.to_str().unwrap().to_owned();
    let dbt_cmd = |command: &'static str, extra: &[&str]| {
        let mut args = vec![
            "state",
            command,
            "--dbt",
            &dbt,
            "--profiles-dir",
            ".",
            "--dbt-output",
            "capture",
        ];
        args.extend(extra);
        project.ods(&args)
    };
    let real = |extra: &[&str]| dbt_cmd("run", extra);
    let (code, first) = real(&[]);
    assert_eq!(code, 0, "{first:#}");
    assert_eq!(
        first["result"]["record"]["advanced"]
            .as_array()
            .unwrap()
            .len(),
        14
    );
    let (code, again) = real(&[]);
    assert_eq!(code, 0, "{again:#}");
    assert_eq!(again["result"]["outcome"], "nothing_to_build");

    // The run built without tests (#220); `ods state test` runs them with `dbt test`.
    let test = |extra: &[&str]| dbt_cmd("test", extra);
    let (code, tested) = test(&[]);
    assert_eq!(code, 0, "{tested:#}");
    assert_eq!(
        tested["result"]["requested"], 5,
        "the nodes with tests: {tested:#}"
    );
    let (code, tested) = test(&[]);
    assert_eq!(code, 0, "{tested:#}");
    assert_eq!(tested["result"]["outcome"], "nothing_to_test");

    let model = project.dir.join("models/marts/segment_summary.sql");
    let sql = std::fs::read_to_string(&model).unwrap();
    // A comment and reindenting: nothing to build (#209).
    std::fs::write(
        &model,
        format!("-- reformatted\n{}", sql.replace('\n', "\n    ")),
    )
    .unwrap();
    let (code, cosmetic) = real(&[]);
    assert_eq!(code, 0, "{cosmetic:#}");
    assert_eq!(
        cosmetic["result"]["outcome"], "nothing_to_build",
        "{cosmetic:#}"
    );

    std::fs::write(&model, sql.replace("count(*)", "count(segment)")).unwrap();
    let (code, changed) = real(&[]);
    assert_eq!(code, 0, "{changed:#}");
    assert_eq!(
        names(&changed["result"]["execution"]["nodes"]),
        ["segment_summary"]
    );
    // dbt was asked for exactly that model, and built nothing else.
    let command = changed["result"]["execution"]["command"].as_str().unwrap();
    assert!(
        command.contains(
            "path:models/marts/segment_summary.sql,fqn:jaffle_ods.marts.segment_summary,resource_type:model"
        ),
        "{command}"
    );
    assert_eq!(
        changed["result"]["execution"]["unrequested"],
        serde_json::json!([]),
        "{changed:#}"
    );

    std::fs::write(&model, "select nope from {{ ref('customer_segments') }}\n").unwrap();
    let (code, broken) = real(&[]);
    assert_eq!(code, 1, "{broken:#}");
    assert!(broken["result"]["record"]["snapshot"].is_null());
    assert_eq!(project.history().len(), 3, "run, test, run");
}

fn copy(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        std::fs::copy(from, to).unwrap();
    }
}
