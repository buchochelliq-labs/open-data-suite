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

    /// Sets a node's config key in the fake dbt's manifest, as editing its model would.
    fn set_config(&self, node: &str, key: &str, value: impl Into<Value>) {
        let path = self.dir.join("base/manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["nodes"][node]["config"][key] = value.into();
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

    /// Like [`ods_with_stderr`](Self::ods_with_stderr), without `--target-dir`, so
    /// ODS finds the target directory itself (#227).
    fn ods_bare(&self, args: &[&str]) -> (i32, Value, String) {
        let db = self.db();
        let mut all: Vec<&str> = args.to_vec();
        all.extend(["--state-db", db.to_str().unwrap(), "--json"]);
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(&all)
            .env_clear()
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

    /// What the fake dbt saw on each call: its arguments and the `DBT_*` names in its
    /// environment.
    fn seen(&self) -> Vec<(Vec<String>, Vec<String>)> {
        std::fs::read_to_string(self.dir.join("seen"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let v: Value = serde_json::from_str(line).unwrap();
                let strings = |key: &str| -> Vec<String> {
                    v[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s.as_str().unwrap().to_owned())
                        .collect()
                };
                (strings("argv"), strings("env"))
            })
            .collect()
    }

    /// `ods state build` without tests (what `ods state run` did before #229), or with
    /// them when `extra` holds `--test`.
    fn run(&self, extra: &[&str]) -> (i32, Value) {
        let tests = extra.contains(&"--test");
        let mut rest: Vec<&str> = extra.iter().copied().filter(|a| *a != "--test").collect();
        if !tests {
            let split = rest.iter().position(|a| *a == "--").unwrap_or(rest.len());
            rest.splice(split..split, ["--exclude-resource-type", "test"]);
        }
        self.command("build", &rest)
    }

    /// `ods state <command>` against the fake dbt.
    fn command(&self, command: &str, extra: &[&str]) -> (i32, Value) {
        let dbt = fixture("fake-dbt/dbt");
        let mut args = vec![
            "state",
            command,
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
    let (code, json, stderr) = project.ods_with_stderr(&[
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{json:#}");
    // ODS says which dbt command runs, and why, just before dbt's own output (#220).
    let expected = [
        "ods ▸ 1/3 dbt source freshness: how new each source's data is",
        "fake dbt: source freshness",
        "ods ▸ 2/3 dbt compile: the code as it is now, for the plan",
        "fake dbt: compile",
        "ods ▸ plan: 13 to build, 0 to reuse",
        // What builds, by reason; long lists are cut short.
        "ods ▸   not built by ODS yet: raw_customers, ",
        " and 5 more",
        "ods ▸ 3/3 dbt build: 13 nodes, without tests",
        "fake dbt: build",
    ];
    let mut rest = stderr.as_str();
    for line in expected {
        let at = rest
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} missing or out of order in stderr:\n{stderr}"));
        rest = &rest[at + line.len()..];
    }
    let (_, _, again) = project.ods_with_stderr(&[
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert!(
        again.contains("ods ▸ nothing to build, so dbt doesn't run again"),
        "{again}"
    );
    // With state, what would be reused is checked first, in one dbt call (#230).
    assert!(
        again.contains(
            "ods ▸ 3/4 dbt show: are the tables of 13 nodes ODS would reuse still there?"
        ),
        "{again}"
    );
    let (_, _, quiet) = project.ods_with_stderr(&[
        "-q",
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert!(!quiet.contains("ods ▸"), "-q hides progress: {quiet}");

    // Logs: none by default; -v says what ODS runs and records, -vv why, per node.
    assert!(!stderr.contains("running dbt"), "{stderr}");
    project.change_code("model.jaffle_ods.orders");
    let (_, _, info) = project.ods_with_stderr(&[
        "-v",
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert!(info.contains("ods ▸   code changed: orders"), "{info}");
    for text in ["running dbt", "dbt finished", "recording the run"] {
        assert!(info.contains(text), "{text} missing at -v:\n{info}");
    }
    assert!(!info.contains("planned"), "{info}");
    project.change_code("model.jaffle_ods.orders");
    let (_, _, debug) = project.ods_with_stderr(&[
        "--log-level",
        "debug",
        "state",
        "run",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    for text in ["planned", "dbt result", "dbt settings"] {
        assert!(debug.contains(text), "{text} missing at debug:\n{debug}");
    }
    // Library logs (e.g. the state store's SQL) wait for trace.
    assert!(!debug.contains("db.statement"), "{debug}");
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

/// #229: each `ods state` command runs the dbt command it's named after, and builds
/// only its own resource type.
#[test]
fn each_command_runs_its_dbt_namesake() {
    let project = Project::new("commands");
    let command = |json: &Value| json["execution"]["command"].as_str().unwrap().to_owned();
    let dbt = fixture("fake-dbt/dbt").display().to_string();

    // compile: freshness and compile, then the plan; nothing is built or recorded.
    let (code, compiled) = project.command("compile", &[]);
    assert_eq!(code, 0, "{compiled:#}");
    assert_eq!(compiled["result"]["outcome"], "compiled");
    assert_eq!(compiled["result"]["build"], 13);
    assert!(compiled["result"].get("execution").is_none());
    assert!(project.history().is_empty());

    // run: models only, with `dbt run`; the seeds they read are left out, and it says so.
    let (code, ran) = project.command("run", &[]);
    assert_eq!(code, 0, "{ran:#}");
    let ran = &ran["result"];
    assert!(
        command(ran).starts_with(&format!("{dbt} run --select ")),
        "{}",
        command(ran)
    );
    assert!(
        !command(ran).contains("--exclude-resource-type"),
        "{}",
        command(ran)
    );
    assert_eq!(ran["execution"]["nodes"].as_array().unwrap().len(), 10);
    assert_eq!(
        names(&ran["left_out"]),
        ["raw_customers", "raw_orders", "raw_payments"]
    );
    assert!(
        ran["left_out"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("use `ods state seed`"),
        "{ran:#}"
    );
    let warnings = ran["warnings"].to_string();
    assert!(
        warnings.contains("`raw_orders` (seed) needs building") && warnings.contains("stg_orders"),
        "{warnings}"
    );

    // seed: seeds only, with `dbt seed`.
    let seeded = project.command("seed", &[]).1;
    let seeded = &seeded["result"];
    assert!(
        command(seeded).starts_with(&format!("{dbt} seed --select ")),
        "{}",
        command(seeded)
    );
    assert_eq!(
        names(&seeded["execution"]["nodes"]),
        ["raw_customers", "raw_orders", "raw_payments"]
    );

    // build: everything left, with its tests, like `dbt build`.
    let (code, built) = project.command("build", &[]);
    assert_eq!(code, 0, "{built:#}");
    let built = &built["result"];
    assert_eq!(built["tests"], true);
    assert!(
        command(built).starts_with(&format!("{dbt} build --select ")),
        "{}",
        command(built)
    );
    assert!(
        !command(built).contains("--exclude-resource-type"),
        "{}",
        command(built)
    );
    assert!(
        built["execution"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["checks_passed"].as_array().is_some_and(|c| !c.is_empty())),
        "tests ran: {built:#}"
    );
    // `--test` is gone: `build` tests, `--exclude-resource-type test` doesn't.
    let status = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--test"])
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(2), "an unknown flag is a usage error");
}

/// #229: `--full-refresh` rebuilds the selected incremental models and seeds even if
/// unchanged, as dbt's does, and their readers with them; tables, views and nodes
/// with `full_refresh: false` follow the plan.
#[test]
fn full_refresh_rebuilds_what_it_changes() {
    let project = Project::new("full-refresh");
    project.set_config("model.jaffle_ods.orders", "materialized", "incremental");
    project.set_config("model.jaffle_ods.customers", "materialized", "incremental");
    project.set_config("model.jaffle_ods.customers", "full_refresh", false);
    project.run_ok(&[]);

    let result = project.run_ok(&["--full-refresh", "-s", "raw_orders+"]);
    let why = |name: &str| {
        result["plan"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| {
                (
                    e["action"].clone(),
                    e["reasons"][0]["message"].as_str().unwrap().to_owned(),
                )
            })
            .unwrap()
    };
    assert_eq!(
        why("raw_orders").1,
        "full refresh requested: rebuilt from scratch"
    );
    assert_eq!(
        why("orders").1,
        "full refresh requested: rebuilt from scratch"
    );
    // Its readers see new data, and say where from.
    assert!(
        why("stg_orders").1.contains("raw_orders (full refresh)"),
        "{:?}",
        why("stg_orders")
    );
    // An incremental model that opts out is only built for its new upstream data.
    assert!(
        !why("customers").1.contains("full refresh requested"),
        "{:?}",
        why("customers")
    );
    let command = result["execution"]["command"].as_str().unwrap();
    assert!(command.contains("--full-refresh"), "{command}");
    // Without it, nothing needs building.
    assert_eq!(project.run_ok(&[])["outcome"], "nothing_to_build");
}

/// #229: `--vars` goes to every dbt command ODS runs, so the plan and the build see
/// the same values; with `--no-compile`, artifacts compiled with other vars are named.
#[test]
fn vars_reach_every_dbt_command() {
    let project = Project::new("vars");
    let dbt = fixture("fake-dbt/dbt");
    let (code, json, stderr) = project.ods_with_stderr(&[
        "-v",
        "state",
        "build",
        "--vars",
        r#"{"region": "eu"}"#,
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{json:#}");
    let commands: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("running dbt"))
        .collect();
    assert_eq!(commands.len(), 3, "{stderr}");
    for line in commands {
        assert!(
            line.contains(r#"--vars {"region": "eu"}"#)
                || line.contains(r#"--vars '{"region": "eu"}'"#),
            "{line}"
        );
    }
    // Compiled with `region: eu`; planning from those artifacts with other vars is flagged.
    let (_, other) = project.command("run", &["--no-compile", "--vars", r#"{"region": "us"}"#]);
    assert!(
        other["result"]["warnings"]
            .to_string()
            .contains("compiled with vars"),
        "{other:#}"
    );
    let (_, same) = project.command("run", &["--no-compile", "--vars", r#"{"region": "eu"}"#]);
    assert!(
        !same["result"]["warnings"]
            .to_string()
            .contains("compiled with vars"),
        "{same:#}"
    );
    let (_, none) = project.command("run", &["--no-compile"]);
    assert!(
        none["result"]["warnings"]
            .to_string()
            .contains("compiled with vars"),
        "{none:#}"
    );
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

fn entry<'v>(result: &'v Value, name: &str) -> &'v Value {
    result["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("{name} isn't planned: {result:#}"))
}

/// #230: a node whose table was dropped isn't reused, however unchanged it is; one dbt
/// call checks every node that would be.
#[test]
fn a_dropped_relation_is_rebuilt_with_its_reason() {
    let project = Project::new("dropped");
    let dropped = project.dir.join("dropped");
    let calls = project.dir.join("calls");
    let project = project
        .with("FAKE_DBT_DROPPED", dropped.to_str().unwrap())
        .with("FAKE_DBT_CALLS", calls.to_str().unwrap());
    project.run_ok(&[]);
    let calls_made = || std::fs::read_to_string(&calls).unwrap_or_default();
    // Nothing to reuse on a first run, so nothing to check.
    assert!(!calls_made().contains("show"), "{}", calls_made());

    std::fs::write(&dropped, "model.jaffle_ods.stg_payments\n").unwrap();
    std::fs::remove_file(&calls).unwrap();
    let planned = project.run_ok(&["--dry-run"]);
    assert_eq!(calls_made().matches("show").count(), 1, "{}", calls_made());
    let payments = entry(&planned, "stg_payments");
    assert_eq!(payments["action"], "build");
    assert_eq!(payments["reasons"][0]["code"], "relation_missing");
    assert_eq!(
        payments["reasons"][0]["message"],
        "its table isn't in the warehouse"
    );
    let evidence = payments["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "relation_exists")
        .unwrap();
    assert_eq!(evidence["value"], "missing");
    assert_eq!(evidence["exactness"], "exact");
    // What still exists is reused, and says it was checked.
    let raw = entry(&planned, "raw_orders");
    assert_eq!(raw["action"], "reuse");
    assert!(
        raw["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "relation_exists" && e["exactness"] == "exact"),
        "{raw:#}"
    );
    // Its readers see new data, as after any rebuild.
    assert_eq!(
        entry(&planned, "orders")["reasons"][0]["code"],
        "new_upstream_data"
    );

    let built = project.run_ok(&[]);
    let nodes = names(&built["execution"]["nodes"]);
    assert!(nodes.contains(&"stg_payments".to_owned()), "{nodes:?}");
    assert!(!nodes.contains(&"stg_orders".to_owned()), "{nodes:?}");
    // Built again, it is back: nothing more to do.
    assert_eq!(project.run_ok(&[])["outcome"], "nothing_to_build");
}

/// A full refresh depends on the selection: a reader of a node it would rebuild is
/// still reused when that node isn't selected, so it is checked like any other.
#[test]
fn a_full_refresh_selection_still_checks_what_it_reuses() {
    let project = Project::new("dropped-full-refresh");
    let dropped = project.dir.join("dropped");
    let project = project.with("FAKE_DBT_DROPPED", dropped.to_str().unwrap());
    project.set_config("model.jaffle_ods.orders", "materialized", "incremental");
    project.run_ok(&[]);
    std::fs::write(&dropped, "model.jaffle_ods.customer_order_rank\n").unwrap();
    let planned = project.run_ok(&["--dry-run", "--full-refresh", "-s", "customer_order_rank"]);
    let rank = entry(&planned, "customer_order_rank");
    assert_eq!(rank["action"], "build", "{rank:#}");
    assert_eq!(rank["reasons"][0]["code"], "relation_missing", "{rank:#}");
}

/// If the warehouse can't be asked, nothing is reused on trust: every candidate is
/// built, and the run says why.
#[test]
fn when_the_relation_check_fails_candidates_are_built() {
    let project = Project::new("check-fails");
    project.run_ok(&[]);
    let project = project.with("FAKE_DBT_SHOW_FAIL", "1");
    let (code, json, stderr) = project.ods_with_stderr(&[
        "state",
        "build",
        "--dry-run",
        "--dbt",
        fixture("fake-dbt/dbt").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["build"], 13);
    assert_eq!(
        entry(result, "raw_orders")["reasons"][0]["code"],
        "relation_unverified"
    );
    let warnings = result["warnings"].to_string();
    assert!(
        warnings.contains("couldn't check that the tables"),
        "{warnings}"
    );
    assert!(
        stderr.contains("couldn't check the warehouse: "),
        "the plan groups them: {stderr}"
    );
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

/// A copy of the demo project for real dbt, with a folder named like a model (#211):
/// `fqn:…segment_summary` alone would also select the model in it.
fn real_project() -> Project {
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
    let nested = project.dir.join("models/marts/segment_summary");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("unrelated.sql"),
        "select count(*) as n from {{ ref('raw_orders') }}\n",
    )
    .unwrap();
    project
}

/// The same flow against real dbt and `DuckDB`, on a copy of the demo project. Set
/// `ODS_TEST_DBT` to a dbt executable with `dbt-duckdb` installed.
#[test]
fn real_dbt() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let project = real_project();
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
    let advanced = |json: &Value| {
        json["result"]["record"]["advanced"]
            .as_array()
            .unwrap()
            .len()
    };
    let ran = |json: &Value| {
        json["result"]["execution"]["command"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // Each command runs its dbt namesake (#229): seeds first, as with dbt on a fresh
    // database, then the models that read them.
    let (code, seeded) = dbt_cmd("seed", &[]);
    assert_eq!(code, 0, "{seeded:#}");
    assert_eq!(advanced(&seeded), 3);
    assert!(ran(&seeded).contains(" seed --select "), "{}", ran(&seeded));
    let (code, first) = real(&[]);
    assert_eq!(code, 0, "{first:#}");
    assert_eq!(advanced(&first), 11, "10 models and the extra one");
    assert!(ran(&first).contains(" run --select "), "{}", ran(&first));
    let (code, again) = dbt_cmd("build", &["--exclude-resource-type", "test"]);
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
    assert_eq!(project.history().len(), 4, "seed, run, test, run");

    std::fs::write(&model, &sql).unwrap();
}

/// `ods state <command>` against real dbt in `project`.
fn real_cmd(project: &Project, dbt: &str, command: &str, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "state",
        command,
        "--dbt",
        dbt,
        "--profiles-dir",
        ".",
        "--dbt-output",
        "capture",
    ];
    args.extend(extra);
    project.ods(&args)
}

/// What `build` tests stays tested (#229), now that the checks' fingerprints include
/// the tests' compiled SQL, against real dbt and `DuckDB`.
#[test]
fn real_dbt_build_then_test() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let dbt = dbt.to_str().unwrap().to_owned();
    let project = real_project();
    for command in ["seed", "run", "test"] {
        let (code, json) = real_cmd(&project, &dbt, command, &[]);
        assert_eq!(code, 0, "{command}: {json:#}");
    }
    // A view with tests: DuckDB can't replace a table that a view reads, which a full
    // refresh of `raw_orders+` would, with plain dbt too.
    let staging = project.dir.join("models/staging/stg_orders.sql");
    let staged = std::fs::read_to_string(&staging).unwrap();
    std::fs::write(&staging, format!("{}\nwhere 1 = 1\n", staged.trim_end())).unwrap();
    let (code, rebuilt) = real_cmd(&project, &dbt, "build", &["-s", "stg_orders"]);
    assert_eq!(code, 0, "{rebuilt:#}");
    assert_eq!(
        names(&rebuilt["result"]["execution"]["nodes"]),
        ["stg_orders"]
    );
    assert_eq!(rebuilt["result"]["tests"], true);
    let (code, tested) = real_cmd(&project, &dbt, "test", &[]);
    assert_eq!(code, 0, "{tested:#}");
    assert_eq!(tested["result"]["outcome"], "nothing_to_test", "{tested:#}");
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

/// #230 against real dbt and `DuckDB`: a table and a view dropped behind ODS's back
/// are rebuilt, with the reason, and nothing that is still there. Both are leaves:
/// `DuckDB` can't replace a table a view reads, with or without ODS.
#[test]
fn real_dbt_rebuilds_a_dropped_table() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let dbt = dbt.to_str().unwrap().to_owned();
    let project = real_project();
    // Only this copy of the project has it; written before the first run, so it is
    // part of the code every run sees.
    std::fs::write(
        project.dir.join("macros/ods_test_drop.sql"),
        "{% macro ods_test_drop(kind, name) %}\
         {% do run_query('drop ' ~ kind ~ ' ' ~ name) %}\
         {% endmacro %}\n",
    )
    .unwrap();
    for command in ["seed", "run"] {
        let (code, json) = real_cmd(&project, &dbt, command, &[]);
        assert_eq!(code, 0, "{command}: {json:#}");
    }
    let dropped = ["customers_snapshot_view", "segment_summary"];
    for (kind, name) in [("view", dropped[0]), ("table", dropped[1])] {
        let out = Command::new(&dbt)
            .args([
                "run-operation",
                "ods_test_drop",
                "--args",
                &format!("{{kind: {kind}, name: {name}}}"),
                "--profiles-dir",
                ".",
            ])
            .env("DBT_SEND_ANONYMOUS_USAGE_STATS", "false")
            .current_dir(&project.dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let (code, json) = real_cmd(
        &project,
        &dbt,
        "build",
        &["--exclude-resource-type", "test"],
    );
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    for name in dropped {
        assert_eq!(
            entry(result, name)["reasons"][0]["code"],
            "relation_missing",
            "{name}"
        );
    }
    assert_eq!(names(&result["execution"]["nodes"]), dropped);
    let (code, again) = real_cmd(&project, &dbt, "run", &[]);
    assert_eq!(code, 0, "{again:#}");
    assert_eq!(again["result"]["outcome"], "nothing_to_build", "{again:#}");
}

/// The value and source of a dbt setting in a report.
fn setting(result: &Value, name: &str) -> Option<(String, String)> {
    result["dbt"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .map(|s| {
            (
                s["value"].as_str().unwrap().to_owned(),
                s["source"].as_str().unwrap().to_owned(),
            )
        })
}

/// #227: dbt's own variables for the settings ODS has options for are read as those
/// options' defaults, and reach dbt as flags only.
#[test]
fn dbt_variables_act_like_the_options_ods_has_for_them() {
    let project = Project::new("owned-env");
    let seen = project.dir.join("seen");
    let project = project
        .with("FAKE_DBT_SEEN", seen.to_str().unwrap())
        .with("DBT_TARGET", "prod")
        .with("DBT_PROFILE", "warehouse")
        .with("DBT_FULL_REFRESH", "true");
    let result = project.run_ok(&[]);
    assert_eq!(
        setting(&result, "target"),
        Some(("prod".into(), "DBT_TARGET".into()))
    );
    assert_eq!(
        setting(&result, "profile"),
        Some(("warehouse".into(), "DBT_PROFILE".into()))
    );
    assert_eq!(
        setting(&result, "full_refresh"),
        Some(("true".into(), "DBT_FULL_REFRESH".into()))
    );
    for (argv, env) in project.seen() {
        let joined = argv.join(" ");
        assert!(joined.contains("--target prod"), "{joined}");
        assert!(joined.contains("--profile warehouse"), "{joined}");
        for owned in ["DBT_TARGET", "DBT_PROFILE", "DBT_FULL_REFRESH"] {
            assert!(
                !env.iter().any(|n| n == owned),
                "{owned} reached dbt: {env:?}"
            );
        }
    }
    let command = result["execution"]["command"].as_str().unwrap();
    assert!(command.contains("--full-refresh"), "{command}");

    // An explicit option beats the variable, as in dbt.
    let result = project.run_ok(&["--dry-run", "--target", "dev", "--dbt-profile", "other"]);
    assert_eq!(
        setting(&result, "target"),
        Some(("dev".into(), "flag".into()))
    );
    assert_eq!(
        setting(&result, "profile"),
        Some(("other".into(), "flag".into()))
    );
}

/// #227: with `DBT_PROJECT_DIR`, ODS reads the artifacts where dbt writes them, the
/// project's `target`, in every command.
#[test]
fn the_project_dir_moves_the_target_dir() {
    let project = Project::new("project-dir").with("DBT_PROJECT_DIR", "proj");
    let dbt = fixture("fake-dbt/dbt");
    let build = [
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt",
        dbt.to_str().unwrap(),
        "--dbt-output",
        "capture",
    ];
    let (code, json, _) = project.ods_bare(&build);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(
        setting(result, "target_dir"),
        Some(("proj/target".into(), "project_dir".into()))
    );
    assert!(project.dir.join("proj/target/manifest.json").is_file());
    // Planning finds the same artifacts, and the state recorded from them.
    let (code, plan, _) = project.ods_bare(&["state", "plan"]);
    assert_eq!(code, 0, "{plan:#}");
    assert!(
        plan["result"]["plan"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["action"] == "reuse"),
        "{plan:#}"
    );
}

/// #227: dbt settings that change what is built are beaten by a flag where dbt has
/// one, with a warning, and refused before dbt runs where it hasn't.
#[test]
fn dbt_variables_that_change_the_build_are_overridden_or_refused() {
    let project = Project::new("env-policy");
    let seen = project.dir.join("seen");
    let project = project
        .with("FAKE_DBT_SEEN", seen.to_str().unwrap())
        .with("DBT_DEFER", "true");
    let result = project.run_ok(&[]);
    let warnings = result["warnings"].to_string();
    assert!(
        warnings.contains("DBT_DEFER is set: ODS passes --no-defer"),
        "{warnings}"
    );
    assert!(
        project
            .seen()
            .iter()
            .all(|(argv, _)| argv.iter().any(|a| a == "--no-defer")),
        "every dbt call gets it"
    );

    // Slim-CI settings only matter for deferral, which --no-defer turns off.
    let slim = project.with("DBT_STATE", "prod-artifacts");
    let result = slim.run_ok(&["--dry-run"]);
    assert!(
        result["warnings"].to_string().contains("DBT_STATE is set"),
        "{result:#}"
    );

    let calls = slim.seen().len();
    let refused = slim.with("DBT_SAMPLE", "3 days");
    let (code, json) = refused.run(&[]);
    assert_eq!(code, 2, "{json:#}");
    let message = json["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("unset DBT_SAMPLE for ODS runs"),
        "{message}"
    );
    assert_eq!(refused.seen().len(), calls, "dbt didn't run");
}

/// #227: the target directory is where dbt writes, however it is given, in every
/// State command.
#[test]
fn the_target_dir_is_found_as_dbt_finds_it() {
    let dbt = fixture("fake-dbt/dbt");
    let dry = |project: &Project, extra: &[&str]| {
        let mut args = vec![
            "state",
            "build",
            "--dry-run",
            "--dbt",
            dbt.to_str().unwrap(),
            "--dbt-output",
            "capture",
        ];
        args.extend(extra);
        let (code, json, _) = project.ods_bare(&args);
        assert_eq!(code, 0, "{json:#}");
        setting(&json["result"], "target_dir").unwrap()
    };
    // A relative DBT_TARGET_PATH is read against the project, as dbt reads it.
    let both = Project::new("target-path")
        .with("DBT_PROJECT_DIR", "proj")
        .with("DBT_TARGET_PATH", "out");
    assert_eq!(
        dry(&both, &[]),
        ("proj/out".into(), "DBT_TARGET_PATH".into())
    );
    assert!(both.dir.join("proj/out/manifest.json").is_file());
    // An explicit --target-dir wins, relative to where ODS runs.
    assert_eq!(
        dry(&both, &["--target-dir", "mine"]),
        ("mine".into(), "flag".into())
    );
    // An absolute one is used as it is.
    let absolute = both.dir.join("abs");
    let abs = Project::new("target-abs").with("DBT_TARGET_PATH", absolute.to_str().unwrap());
    assert_eq!(
        dry(&abs, &[]),
        (absolute.display().to_string(), "DBT_TARGET_PATH".into())
    );
    // `policies` reads the same place.
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "policies", "--json"])
        .env_clear()
        .envs(both.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&both.dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// #227: `--help` names the dbt variables options default to, never their values.
#[test]
fn help_hides_the_values_of_dbt_variables() {
    let project = Project::new("help")
        .with("DBT_TARGET", "prod-secret-name")
        .with("DBT_FULL_REFRESH", "true");
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--help"])
        .env_clear()
        .envs(project.env.iter().map(|(k, v)| (k, v)))
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("DBT_TARGET"), "{help}");
    assert!(!help.contains("prod-secret-name"), "{help}");
    assert!(!help.contains("DBT_FULL_REFRESH=true"), "{help}");
}
