//! `ods state run` end to end, with the fake dbt in `fixtures/dbt/fake-dbt` standing in
//! for dbt (a Python script, so Unix only; no warehouse or network). The same flow runs
//! against real dbt when `ODS_TEST_DBT` names a dbt executable with `dbt-duckdb`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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
    _guard: tempfile::TempDir,
}

impl Project {
    fn new(name: &str) -> Self {
        let guard = tempfile::Builder::new()
            .prefix(&format!("ods-state-run-{name}-"))
            .tempdir()
            .unwrap();
        let dir = guard.path().to_owned();
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
            _guard: guard,
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

    /// `ods <args> -o plain` against the fake dbt, as a person sees it: its stdout.
    fn ods_plain(&self, args: &[&str]) -> String {
        let target = self.dir.join("target");
        let db = self.db();
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .args([
                "--target-dir",
                target.to_str().unwrap(),
                "--state-db",
                db.to_str().unwrap(),
                "-o",
                "plain",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &self.dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }

    /// Like [`ods_with_stderr`](Self::ods_with_stderr), without `--target-dir`, so
    /// ODS finds the target directory itself (#227).
    fn ods_bare(&self, args: &[&str]) -> (i32, Value, String) {
        let db = self.db();
        let mut all: Vec<&str> = args.to_vec();
        all.extend(["--state-db", db.to_str().unwrap()]);
        self.ods_in(&self.dir, &all)
    }

    /// `ods <args> --json` run in `cwd`, with nothing else added: settings come from
    /// the environment and configuration (#214).
    fn ods_in(&self, cwd: &Path, args: &[&str]) -> (i32, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .arg("--json")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &self.dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
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
        "ods ▸ 1/4 dbt source freshness: how new each source's data is",
        "fake dbt: source freshness",
        "ods ▸ 2/4 dbt compile: the code as it is now, for the plan",
        "fake dbt: compile",
        // Its answer is read, not shown.
        "ods ▸ 3/4 dbt compile --inline: which target dbt builds in",
        "ods ▸ plan: 13 to build, 0 to reuse",
        // What builds, by reason; long lists are cut short.
        "ods ▸   not built by ODS yet: raw_customers, ",
        " and 5 more",
        "ods ▸ 4/4 dbt build: 13 nodes, without tests",
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
            "ods ▸ 4/5 dbt show: are the tables of 13 nodes ODS would reuse still there?"
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
    // Its readers are rebuilt too, and say where from.
    assert_eq!(
        why("stg_orders").1,
        "upstream full refresh: raw_orders will be rebuilt from scratch"
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
    const VARS: &str = r#"{"region": "eu", "token": "VARS_SENTINEL_eu"}"#;
    let project = Project::new("vars");
    let seen = project.dir.join("seen");
    let project = project.with("FAKE_DBT_SEEN", seen.to_str().unwrap());
    let dbt = fixture("fake-dbt/dbt");
    let (code, json, stderr) = project.ods_with_stderr(&[
        "-v",
        "state",
        "build",
        "--vars",
        VARS,
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{json:#}");
    // dbt got them on every call: freshness, compile, the target check and the build.
    let calls = project.seen();
    assert_eq!(calls.len(), 4, "{calls:?}");
    for (argv, _) in &calls {
        let at = argv.iter().position(|a| a == "--vars").unwrap();
        assert_eq!(argv[at + 1], VARS, "{argv:?}");
    }
    // Nothing ODS prints or logs shows them (rule 9, #321): not the command lines it
    // logs, not the report's `dbt` settings or `execution.command`.
    let commands: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("running dbt"))
        .collect();
    assert_eq!(commands.len(), 4, "{stderr}");
    for line in commands {
        assert!(
            line.contains("--vars '[value removed]'") || line.contains("--vars [value removed]"),
            "{line}"
        );
    }
    assert!(!stderr.contains("VARS_SENTINEL"), "{stderr}");
    assert!(!json.to_string().contains("VARS_SENTINEL"), "{json:#}");
    assert!(
        json["result"]["execution"]["command"]
            .as_str()
            .unwrap()
            .contains("--vars '[value removed]'"),
        "{json:#}"
    );
    // Compiled with `region: eu`; planning from those artifacts with other vars is flagged.
    let (_, other) = project.command("run", &["--no-compile", "--vars", r#"{"region": "us"}"#]);
    assert!(
        other["result"]["warnings"]
            .to_string()
            .contains("compiled with vars"),
        "{other:#}"
    );
    let (_, same) = project.command("run", &["--no-compile", "--vars", VARS]);
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

/// Reuse checked in the warehouse isn't reuse on trust: a run that checked says
/// nothing more, while `ods state plan`, which runs no dbt, says it didn't check.
#[test]
fn only_unchecked_reuse_says_the_warehouse_wasnt_checked() {
    let project = Project::new("reuse-notice");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.orders");
    let dbt = fixture("fake-dbt/dbt");
    let dbt = dbt.to_str().unwrap();
    for command in ["build", "run"] {
        let shown = project.ods_plain(&["state", command, "--dry-run", "--dbt", dbt]);
        assert!(shown.contains("to reuse"), "{shown}");
        assert!(!shown.contains("0 to reuse"), "{shown}");
        assert!(!shown.contains("check the warehouse"), "{command}: {shown}");
    }
    let planned = project.ods_plain(&["state", "plan"]);
    assert!(
        planned.contains("doesn't check the warehouse")
            && planned.contains("ods state build --dry-run"),
        "{planned}"
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

    // With artifacts already there, the target check fails: dbt can't say where it
    // would build, so nothing is planned against any state.
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
            .contains("which target it builds in"),
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

/// #322: every node of a real dbt run has stats from dbt's structured log, as it ran,
/// with rows only where the adapter reported them: none are made up.
fn assert_live_stats(json: &Value, rows: bool) {
    let stats = &json["result"]["run_stats"];
    assert_eq!(stats["live"], true, "{stats:#}");
    for node in stats["nodes"].as_array().unwrap() {
        let s = &node["stats"];
        assert_eq!(s["status"], "success", "{node:#}");
        assert!(
            s["duration_ms"].is_u64() && s["thread"].is_string(),
            "{node:#}"
        );
        assert_eq!(s["rows_affected"].is_u64(), rows, "{node:#}");
    }
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
    // database, then the models that read them. This one keeps the default
    // `--dbt-output`: dbt's own output streams to stderr (#220).
    let (code, seeded, stderr) =
        project.ods_with_stderr(&["state", "seed", "--dbt", &dbt, "--profiles-dir", "."]);
    assert_eq!(code, 0, "{seeded:#}");
    assert!(stderr.contains("Running with dbt="), "{stderr}");
    assert_eq!(advanced(&seeded), 3);
    assert!(ran(&seeded).contains(" seed --select "), "{}", ran(&seeded));
    let (code, first) = real(&[]);
    assert_eq!(code, 0, "{first:#}");
    assert_eq!(advanced(&first), 11, "10 models and the extra one");
    assert!(ran(&first).contains(" run --select "), "{}", ran(&first));
    // #322: seeds report the rows they wrote; DuckDB reports none for models.
    assert_live_stats(&seeded, true);
    assert_live_stats(&first, false);
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

/// A source on the seeded `raw_orders` table, with a passing and a failing test, read
/// by `from_source`, which `from_from_source` reads (#232).
const SOURCES_YML: &str = "version: 2
sources:
  - name: raw
    schema: main
    loaded_at_field: \"cast(order_date as timestamp)\"
    freshness:
      warn_after: {count: 36500, period: day}
    tables:
      - name: raw_orders
        columns:
          - name: id
            data_tests: [not_null]
          - name: status
            data_tests:
              - accepted_values:
                  arguments:
                    values: [VALUES]
";

/// `real_project()` with [`SOURCES_YML`], whose `accepted_values` test accepts `values`.
fn real_project_with_a_source(values: &str) -> Project {
    let project = real_project();
    std::fs::write(
        project.dir.join("models/sources.yml"),
        SOURCES_YML.replace("VALUES", values),
    )
    .unwrap();
    std::fs::write(
        project.dir.join("models/marts/from_source.sql"),
        "select count(*) as n from {{ source('raw', 'raw_orders') }}\n",
    )
    .unwrap();
    std::fs::write(
        project.dir.join("models/marts/from_from_source.sql"),
        "select n from {{ ref('from_source') }}\n",
    )
    .unwrap();
    project
}

/// The nodes (not tests) a real `dbt build` of `project` skipped, by name.
fn dbt_build_skips(project: &Project, dbt: &str) -> Vec<String> {
    // The profile's database file lives there.
    std::fs::create_dir_all(project.dir.join("target")).unwrap();
    let out = Command::new(dbt)
        .args([
            "build",
            "--profiles-dir",
            ".",
            "--target-path",
            "target-dbt",
        ])
        .env("DBT_SEND_ANONYMOUS_USAGE_STATS", "false")
        .current_dir(&project.dir)
        .output()
        .unwrap();
    let output = || {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    assert!(
        !out.status.success(),
        "the source test should fail dbt too: {}",
        output()
    );
    let results = std::fs::read(project.dir.join("target-dbt/run_results.json"))
        .unwrap_or_else(|e| panic!("{e}: {}", output()));
    let results: Value = serde_json::from_slice(&results).unwrap();
    let mut skipped: Vec<String> = results["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["status"] == "skipped" && !r["unique_id"].as_str().unwrap().starts_with("test.")
        })
        .map(|r| {
            r["unique_id"]
                .as_str()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    skipped.sort();
    skipped
}

/// #232 against real dbt and `DuckDB`: a failing source test fails `ods state build`,
/// and the models downstream of the source are skipped, exactly as a plain `dbt build`
/// of the same project skips them. Once fixed, the tests pass and are recorded against
/// the source's `max_loaded_at`, so the next build skips them.
#[test]
fn real_dbt_a_failing_source_test_skips_downstream_models() {
    let Some(dbt) = std::env::var_os("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let dbt = dbt.to_str().unwrap().to_owned();
    // What dbt itself does, on its own copy (and database).
    let plain = real_project_with_a_source("'nope'");
    let dbt_skipped = dbt_build_skips(&plain, &dbt);
    assert_eq!(dbt_skipped, ["from_from_source", "from_source"]);

    let project = real_project_with_a_source("'nope'");
    let (code, json) = real_cmd(&project, &dbt, "seed", &[]);
    assert_eq!(code, 0, "{json:#}");
    let (code, json) = real_cmd(&project, &dbt, "build", &[]);
    assert_eq!(code, 1, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["outcome"], "failed", "{result:#}");
    assert_eq!(
        source_tests(result)["raw.raw_orders"],
        decided("test", "not_tested")
    );
    let failed = result["execution"]["checks_failed"].as_array().unwrap();
    assert!(
        failed.iter().any(|c| c
            .as_str()
            .unwrap()
            .contains("source_accepted_values_raw_raw_orders_status")),
        "{result:#}"
    );
    let mut ods_skipped: Vec<String> = result["execution"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["status"] == "skipped")
        .map(|n| {
            n["node"]
                .as_str()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    ods_skipped.sort();
    assert_eq!(ods_skipped, dbt_skipped, "{result:#}");
    assert_eq!(
        result["record"]["source_tests"]["failed"],
        serde_json::json!(["source.jaffle_ods.raw.raw_orders"])
    );
    let advanced = names(&result["record"]["advanced"]);
    assert!(
        !advanced.contains(&"from_source".to_owned()),
        "{advanced:?}"
    );
    assert!(advanced.contains(&"orders".to_owned()), "{advanced:?}");

    // Fixed: the tests run again (they changed) and pass, and the models build.
    std::fs::write(
        project.dir.join("models/sources.yml"),
        SOURCES_YML.replace(
            "VALUES",
            "'completed', 'returned', 'placed', 'shipped', 'return_pending'",
        ),
    )
    .unwrap();
    let (code, json) = real_cmd(&project, &dbt, "build", &[]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(
        result["record"]["source_tests"]["passed"],
        serde_json::json!(["source.jaffle_ods.raw.raw_orders"]),
        "{result:#}"
    );
    assert_eq!(
        names(&result["execution"]["nodes"]),
        ["from_from_source", "from_source"]
    );
    // Same data: the tests are skipped, and nothing is left to build.
    let (code, json) = real_cmd(&project, &dbt, "build", &[]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(
        source_tests(result)["raw.raw_orders"],
        decided("skip", "unchanged"),
        "{result:#}"
    );
    assert_eq!(result["outcome"], "nothing_to_build", "{result:#}");
    // `ods state test` agrees: nothing to test.
    let (code, json) = real_cmd(&project, &dbt, "test", &[]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["outcome"], "nothing_to_test", "{json:#}");
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

fn reasons(result: &Value) -> Vec<String> {
    let mut codes: Vec<String> = result["plan"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["reasons"][0]["code"].as_str().unwrap().to_owned())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

/// #227: state is kept per dbt target, and a build is only reused in the target it
/// went to.
#[test]
fn state_is_kept_per_target() {
    let project = Project::new("targets");
    // `--target` names the environment, so each target has its own state.
    let prod = project.run_ok(&["--target", "prod"]);
    assert_eq!(prod["scope"], "jaffle_ods/prod");
    assert_eq!(prod["target"]["name"], "prod");
    assert_eq!(prod["target"]["location"], "fake-host");
    let dev = project.run_ok(&[]);
    assert_eq!(dev["scope"], "jaffle_ods/default");
    assert_eq!(dev["build"], 13, "prod's builds don't count in dev");
    assert_eq!(
        project.run_ok(&["--target", "prod"])["outcome"],
        "nothing_to_build"
    );
    // An explicit environment wins.
    let shared = project.run_ok(&["--target", "prod", "--environment", "shared"]);
    assert_eq!(shared["scope"], "jaffle_ods/shared");

    // The same name on another host is another target: nothing is reused, and the
    // run says why.
    let moved = project.with("FAKE_DBT_TARGET_HOST", "other-host");
    let result = moved.run_ok(&["--target", "prod"]);
    assert_eq!(result["build"], 13);
    assert_eq!(reasons(&result), ["target_changed"]);
    assert!(
        result["warnings"]
            .to_string()
            .contains("was built in target prod (jaffle_ods, fake, fake-host, jaffle_ods)"),
        "{result:#}"
    );
    // Recorded there, it is reused there.
    assert_eq!(
        moved.run_ok(&["--target", "prod"])["outcome"],
        "nothing_to_build"
    );
    // And a test run back on the first host doesn't vouch for the other's builds.
    let back = moved.with("FAKE_DBT_TARGET_HOST", "fake-host");
    let (code, json) = back.test(&["--target", "prod", "--no-compile"]);
    assert_eq!(code, 1, "{json:#}");
    let message = json["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("none of them are this target's to test"),
        "{message}"
    );

    // `plan` doesn't run dbt: it names the recorded target, and plans state recorded
    // under another name as `run` would.
    let (code, plan) = back.ods(&["state", "plan", "--target", "prod"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["result"]["recorded_target"]["name"], "prod");
    assert_eq!(plan["result"]["reuse"], 13);
    let (code, plan) = back.ods(&[
        "state",
        "plan",
        "--target",
        "prod",
        "--environment",
        "default",
    ]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(
        plan["result"]["build"], 13,
        "default holds dev's builds: {plan:#}"
    );
    assert!(
        plan["result"]["warnings"].to_string().contains("not prod"),
        "{plan:#}"
    );
}

/// #227: state recorded before targets were, or by `ods state record` alone, doesn't
/// say where it was built: nothing in it is reused, once.
#[test]
fn state_without_a_target_is_rebuilt_once() {
    let project = Project::new("untargeted");
    let target = project.dir.join("target");
    // A dbt build ODS didn't run, recorded afterwards.
    let out = Command::new(fixture("fake-dbt/dbt"))
        .args([
            "build",
            "--select",
            "fqn:jaffle_ods",
            "--exclude-resource-type",
            "test",
            "--target-path",
            target.to_str().unwrap(),
        ])
        .envs(project.env.iter().map(|(k, v)| (k, v)))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, json) = project.ods(&["state", "record"]);
    assert_eq!(code, 0, "{json:#}");

    let result = project.run_ok(&[]);
    assert_eq!(result["build"], 13);
    assert_eq!(reasons(&result), ["target_changed"]);
    assert!(
        result["warnings"]
            .to_string()
            .contains("doesn't say which target it was built in"),
        "{result:#}"
    );
    assert_eq!(project.run_ok(&[])["outcome"], "nothing_to_build");

    // Recording another dbt build after it doesn't vouch for its target either: the
    // last one ODS saw isn't evidence of where this one went.
    project.change_code("model.jaffle_ods.orders");
    let out = Command::new(fixture("fake-dbt/dbt"))
        .args([
            "build",
            "--select",
            "fqn:jaffle_ods",
            "--exclude-resource-type",
            "test",
            "--target-path",
            target.to_str().unwrap(),
        ])
        .envs(project.env.iter().map(|(k, v)| (k, v)))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, json) = project.ods(&["state", "record"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(reasons(&project.run_ok(&[])), ["target_changed"]);
}

/// #227: `DBT_TARGET` names the environment, as `--target` does.
#[test]
fn dbt_target_names_the_environment() {
    let project = Project::new("target-env").with("DBT_TARGET", "prod");
    assert_eq!(project.run_ok(&[])["scope"], "jaffle_ods/prod");
    let (code, plan) = project.ods(&["state", "plan"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["result"]["scope"], "jaffle_ods/prod");
    assert_eq!(plan["result"]["reuse"], 13);
}

/// #227: if dbt can't say which target it builds in, a dry run still plans, reusing
/// nothing; a run that would record stops.
#[test]
fn a_failed_target_check_only_stops_what_records() {
    let project = Project::new("target-fails");
    project.run_ok(&[]);
    let failing = project.with("FAKE_DBT_TARGET_FAIL", "1");
    let planned = failing.run_ok(&["--dry-run"]);
    assert_eq!(planned["build"], 13);
    assert!(planned.get("target").is_none(), "{planned:#}");
    assert!(
        planned["warnings"]
            .to_string()
            .contains("which target it builds in"),
        "{planned:#}"
    );
    let (code, json) = failing.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(failing.history().len(), 1, "nothing recorded");
}

/// Writes `ods.toml` in the project, configuring the fake dbt, and returns a
/// subdirectory to run ODS in: configured paths are read against `ods.toml`'s
/// directory, not where ODS runs (#214).
fn configure(project: &Project, extra: &str) -> PathBuf {
    let dbt = fixture("fake-dbt/dbt");
    std::fs::write(
        project.dir.join("ods.toml"),
        format!(
            "[state]\ndb = \"state/ods.db\"\n\n[providers.dbt]\nkind = \"dbt\"\n\n\
             [providers.dbt.settings]\nprogram = \"{}\"\nproject_dir = \"proj\"\n\
             target = \"prod\"\n{extra}",
            dbt.display()
        ),
    )
    .unwrap();
    let sub = project.dir.join("models");
    std::fs::create_dir_all(&sub).unwrap();
    sub
}

/// #214: a configured project runs with no options at all.
#[test]
fn ods_toml_configures_state_commands() {
    let project = Project::new("configured");
    let sub = configure(&project, "");
    let build = [
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt-output",
        "capture",
    ];
    let (code, json, _) = project.ods_in(&sub, &build);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    // As ODS finds `ods.toml`: through the working directory, with symlinks resolved
    // (macOS's temporary directory is under one).
    let dir = project.dir.canonicalize().unwrap();
    let dir = dir.display();
    assert_eq!(result["scope"], "jaffle_ods/prod");
    assert_eq!(
        setting(result, "program").map(|s| s.1),
        Some("project config".into())
    );
    assert_eq!(
        setting(result, "target"),
        Some(("prod".into(), "project config".into()))
    );
    assert_eq!(
        setting(result, "project_dir"),
        Some((format!("{dir}/proj"), "project config".into()))
    );
    assert_eq!(
        setting(result, "target_dir"),
        Some((format!("{dir}/proj/target"), "project_dir".into()))
    );
    assert!(project.dir.join("proj/target/manifest.json").is_file());
    assert!(project.dir.join("state/ods.db").is_file());

    // `plan` and `history` read the same settings.
    let (code, plan, _) = project.ods_in(&sub, &["state", "plan"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["result"]["scope"], "jaffle_ods/prod");
    assert_eq!(plan["result"]["reuse"], 13, "{plan:#}");
    let (code, history, _) = project.ods_in(&sub, &["state", "history"]);
    assert_eq!(code, 0, "{history:#}");
    assert_eq!(history["result"]["snapshots"].as_array().unwrap().len(), 1);
}

/// #214: a flag beats dbt's variable, which beats `ODS__…` variables, which beat the
/// file; `ods config explain` says where a value came from.
#[test]
fn flags_beat_the_environment_which_beats_ods_toml() {
    let project = Project::new("config-precedence");
    let sub = configure(&project, "");
    let text = std::fs::read_to_string(project.dir.join("ods.toml")).unwrap();
    std::fs::write(
        project.dir.join("ods.toml"),
        text.replace("[state]\n", "[state]\nenvironment = \"from-file\"\n"),
    )
    .unwrap();
    let plan = |project: &Project, args: &[&str]| {
        let mut all = vec!["state", "plan"];
        all.extend(args);
        let (code, json, _) = project.ods_in(&sub, &all);
        assert_eq!(code, 0, "{json:#}");
        json["result"]["scope"].as_str().unwrap().to_owned()
    };
    // The file's environment beats the one the target gives.
    let (code, json, _) = project.ods_in(&sub, &["state", "compile", "--dbt-output", "capture"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(plan(&project, &[]), "jaffle_ods/from-file");
    let project = project.with("ODS__STATE__ENVIRONMENT", "from-env");
    assert_eq!(plan(&project, &[]), "jaffle_ods/from-env");
    assert_eq!(
        plan(&project, &["--environment", "from-flag"]),
        "jaffle_ods/from-flag"
    );
    let (code, explain, _) = project.ods_in(&sub, &["config", "explain", "state.environment"]);
    assert_eq!(code, 0, "{explain:#}");
    let text = explain.to_string();
    assert!(text.contains("ODS__STATE__ENVIRONMENT"), "{text}");
    assert!(text.contains("from-file"), "{text}");

    // dbt's settings: the flag beats `DBT_TARGET`, which beats the file.
    let build = |project: &Project, args: &[&str]| {
        let mut all = vec![
            "state",
            "build",
            "--dry-run",
            "--exclude-resource-type",
            "test",
            "--dbt-output",
            "capture",
        ];
        all.extend(args);
        let (code, json, _) = project.ods_in(&sub, &all);
        assert_eq!(code, 0, "{json:#}");
        setting(&json["result"], "target").unwrap()
    };
    assert_eq!(
        build(&project, &[]),
        ("prod".into(), "project config".into())
    );
    let project = project.with("DBT_TARGET", "dev");
    assert_eq!(build(&project, &[]), ("dev".into(), "DBT_TARGET".into()));
    assert_eq!(
        build(&project, &["--target", "qa"]),
        ("qa".into(), "flag".into())
    );
}

/// #214: dbt's settings are checked like the rest of the configuration.
#[test]
fn unknown_dbt_settings_are_configuration_errors() {
    let project = Project::new("config-typo");
    let sub = configure(&project, "progam = \"dbt\"\n");
    let (code, json, _) = project.ods_in(&sub, &["state", "plan"]);
    assert_eq!(code, 4, "{json:#}");
    let text = json.to_string();
    assert!(text.contains("ODS-E0102"), "{text}");
    assert!(text.contains("providers.dbt.settings.progam"), "{text}");
    assert!(text.contains("expected one of program"), "{text}");

    let sub = configure(&project, "\n[providers.other]\nkind = \"dbt\"\n");
    let (code, json, _) = project.ods_in(&sub, &["state", "plan"]);
    assert_eq!(code, 4, "{json:#}");
    assert!(
        json.to_string().contains("more than one dbt provider"),
        "{json:#}"
    );
}

/// #276: `retry` runs the last command again, with the options it was given, planned
/// afresh: what failed builds again, what succeeded is reused.
#[test]
fn retry_reruns_the_last_command_with_its_options() {
    let mut project = Project::new("retry");
    let db = project.db();
    let retry = |project: &Project, extra: &[&str]| {
        let mut args = vec!["state", "retry", "--state-db", db.to_str().unwrap()];
        args.extend(extra);
        project.ods_in(&project.dir, &args)
    };
    let (code, json, _) = retry(&project, &[]);
    assert_eq!(code, 1, "{json:#}");
    assert!(json.to_string().contains("no run to retry"), "{json:#}");

    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.stg_orders");
    let vars = r#"{"region": "eu"}"#;
    let failing = project.with("FAKE_DBT_FAIL", "orders");
    let (code, json) = failing.command(
        "build",
        &[
            "-s",
            "+orders",
            "--vars",
            vars,
            "--exclude-resource-type",
            "test",
        ],
    );
    assert_eq!(code, 1, "{json:#}");
    project = failing;
    project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");

    // What it keeps: the command and the options typed, nothing from the environment.
    let kept = std::fs::read_to_string(project.dir.join(".ods/state.db.last-run.json")).unwrap();
    let kept: Value = serde_json::from_str(&kept).unwrap();
    assert_eq!(kept["command"], "build");
    assert!(kept["args"].to_string().contains("+orders"), "{kept:#}");
    assert!(!kept.to_string().contains("FAKE_DBT"), "{kept:#}");

    // A dry run plans the same selection and changes nothing, not even what retry reruns.
    let (code, json, _) = retry(&project, &["--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["outcome"], "dry_run");

    let (code, json, stderr) = retry(&project, &[]);
    assert_eq!(code, 0, "{json:#}");
    assert!(stderr.contains("retrying `ods state build"), "{stderr}");
    let result = &json["result"];
    let built = names(&result["execution"]["nodes"]);
    assert!(built.contains(&"orders".to_owned()), "{built:?}");
    assert!(
        !built.contains(&"stg_orders".to_owned()),
        "reused: {built:?}"
    );
    let command = result["execution"]["command"].as_str().unwrap();
    // Retried with its vars, which the command line shows only as given (#321).
    assert!(
        command.contains("--vars '[value removed]'") && !command.contains("region"),
        "{command}"
    );
    // Only +orders was selected: nothing outside it built.
    assert!(!built.contains(&"customers".to_owned()), "{built:?}");
}

/// #276: the database a retry is found in is the one it runs against, even when the
/// run took its database from configuration that now names another.
#[test]
fn retry_runs_against_the_database_it_was_found_in() {
    let project = Project::new("retry-db");
    let sub = configure(&project, "");
    let build = [
        "state",
        "build",
        "--exclude-resource-type",
        "test",
        "--dbt-output",
        "capture",
    ];
    let (code, json, _) = project.ods_in(&sub, &build);
    assert_eq!(code, 0, "{json:#}");
    let first = project.dir.join("state/ods.db");
    assert!(first.is_file());
    // The configuration now names another database.
    let toml = std::fs::read_to_string(project.dir.join("ods.toml")).unwrap();
    std::fs::write(
        project.dir.join("ods.toml"),
        toml.replace("state/ods.db", "state/other.db"),
    )
    .unwrap();
    let (code, json, _) = project.ods_in(
        &sub,
        &["state", "retry", "--state-db", first.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(
        json["result"]["state_db"],
        first.to_str().unwrap(),
        "{json:#}"
    );
    assert_eq!(json["result"]["outcome"], "nothing_to_build", "{json:#}");
    assert!(!project.dir.join("state/other.db").exists());
}

/// #276: `ods state test` has no dry run, so neither has its retry.
#[test]
fn a_test_run_has_no_dry_run_retry() {
    let project = Project::new("retry-test");
    project.run_ok(&["--test"]);
    project.test_ok(&[]);
    let db = project.db();
    let (code, json, _) = project.ods_in(
        &project.dir,
        &[
            "state",
            "retry",
            "--dry-run",
            "--state-db",
            db.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 2, "{json:#}");
    assert!(json.to_string().contains("has no dry run"), "{json:#}");
}

/// `ods state retry <extra>` against the project's database, from its directory.
fn retry(project: &Project, extra: &[&str]) -> (i32, Value, String) {
    let db = project.db();
    let mut args = vec!["state", "retry", "--state-db", db.to_str().unwrap()];
    args.extend(extra);
    project.ods_in(&project.dir, &args)
}

/// #292: `retry --failed` builds exactly what failed or was skipped because of a
/// failure in the last run, still planned, and reports what changed since instead of
/// building it.
#[test]
fn retry_failed_builds_only_what_failed() {
    let mut project = Project::new("retry-failed");
    let failing = project.with("FAKE_DBT_FAIL", "customer_segments");
    let (code, json) = failing.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    project = failing;
    project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");

    // The last-run file keeps what failed, and what was skipped because of it.
    let path = project.dir.join(".ods/state.db.last-run.json");
    let kept: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        kept["schema_version"],
        serde_json::json!({"major": 1, "minor": 2}),
        "{kept:#}"
    );
    // Since 1.2 (#311): the scope it ran for, and its run id.
    assert_eq!(kept["scope"], "jaffle_ods/default", "{kept:#}");
    assert!(kept["run_id"].is_string(), "{kept:#}");
    assert_eq!(
        kept["outcome"]["failed"],
        serde_json::json!(["model.jaffle_ods.customer_segments"]),
        "{kept:#}"
    );
    assert_eq!(
        kept["outcome"]["skipped"],
        serde_json::json!(["model.jaffle_ods.segment_summary"]),
        "{kept:#}"
    );

    // An unrelated model changes in between.
    project.change_code("model.jaffle_ods.order_events");

    // Without --failed, retry is unchanged: planned afresh, the change builds too.
    let (code, json, _) = retry(&project, &["--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert!(json["result"].get("retry").is_none(), "{json:#}");
    assert_eq!(json["result"]["build"], 3, "{json:#}");

    // A dry run plans the retry, and builds and records nothing.
    let history = project.history().len();
    let (code, json, _) = retry(&project, &["--failed", "--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["outcome"], "dry_run", "{json:#}");
    assert_eq!(json["result"]["build"], 2, "{json:#}");
    assert_eq!(project.history().len(), history);

    let (code, json, stderr) = retry(&project, &["--failed"]);
    assert_eq!(code, 0, "{json:#}");
    assert!(
        stderr.contains("retrying what failed in `ods state build"),
        "{stderr}"
    );
    let result = &json["result"];
    assert_eq!(result["outcome"], "succeeded", "{json:#}");
    assert_eq!(
        names(&result["execution"]["nodes"]),
        ["customer_segments", "segment_summary"]
    );
    assert_eq!(
        names(&result["retry"]["changed_since"]),
        ["order_events"],
        "{json:#}"
    );
    // The JSON a retry adds, with the paths of this run taken out.
    let mut shown = serde_json::json!({
        "outcome": result["outcome"],
        "build": result["build"],
        "reuse": result["reuse"],
        "retry": result["retry"],
    });
    let of = shown["retry"]["of"].as_str().unwrap().to_owned();
    shown["retry"]["of"] = Value::String(
        of.replace(fixture("fake-dbt/dbt").to_str().unwrap(), "<fake-dbt>")
            .replace(project.dir.to_str().unwrap(), "<project>"),
    );
    for entry in shown["retry"]["changed_since"].as_array_mut().unwrap() {
        let reason = entry["reason"].as_str().unwrap();
        // Run ids are fresh each time.
        let (before, after) = reason.split_once("since run ").unwrap();
        entry["reason"] = Value::String(format!("{before}since run <run>{}", &after[36..]));
    }
    insta::assert_snapshot!(serde_json::to_string_pretty(&shown).unwrap());

    // What changed since still builds with the next plain run.
    let (code, json, _) = retry(&project, &["--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["build"], 1, "{json:#}");

    // The retry succeeded: nothing is left to retry, and dbt doesn't run.
    std::fs::remove_file(project.dir.join("target/run_results.json")).unwrap();
    let (code, json, _) = retry(&project, &["--failed"]);
    assert_eq!(code, 1, "{json:#}");
    let text = json.to_string();
    assert!(text.contains("ODS-E0403"), "{text}");
    assert!(text.contains("succeeded: nothing failed"), "{text}");
    assert!(!project.dir.join("target/run_results.json").exists());
}

/// #292: a node to retry whose parent changed since, and so isn't built by the retry,
/// isn't built on the parent's stale table: it is held back, with why, and stays to
/// retry.
#[test]
fn retry_failed_holds_back_what_reads_a_parent_it_does_not_build() {
    let mut project = Project::new("retry-failed-held").with("FAKE_DBT_FAIL", "orders");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");
    project.change_code("model.jaffle_ods.stg_orders");

    let (code, json, _) = retry(&project, &["--failed"]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["outcome"], "nothing_to_build", "{json:#}");
    // stg_orders changed, and order_events reads it.
    assert_eq!(
        names(&result["retry"]["changed_since"]),
        ["order_events", "stg_orders"]
    );
    let held = &result["retry"]["held_back"];
    assert!(names(held).contains(&"orders".to_owned()), "{json:#}");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .contains("reads `stg_orders`"),
        "{json:#}"
    );
    // Still to retry.
    let (code, json, _) = retry(&project, &["--failed", "--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert!(names(&json["result"]["retry"]["held_back"]).contains(&"orders".to_owned()));
}

/// #292: a parent outside the failed run's selection counts too. It isn't built by the
/// retry, so a node to retry that reads it after it changed is held back.
#[test]
fn retry_failed_holds_back_what_reads_an_unselected_parent() {
    let mut project = Project::new("retry-failed-unselected");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.orders");
    project = project.with("FAKE_DBT_FAIL", "orders");
    let (code, json) = project.run(&["-s", "orders"]);
    assert_eq!(code, 1, "{json:#}");
    project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");
    project.change_code("model.jaffle_ods.stg_orders");

    let (code, json, _) = retry(&project, &["--failed", "--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["build"], 0, "{json:#}");
    let held = &result["retry"]["held_back"];
    assert_eq!(names(held), ["orders"], "{json:#}");
    assert!(
        held[0]["reason"]
            .as_str()
            .unwrap()
            .contains("reads `stg_orders`, which needs building (code changed"),
        "{json:#}"
    );
}

/// #292: a last-run file from before #292 (version 1.0, no outcome) still reads:
/// `retry` runs it, and `retry --failed` says there is nothing recorded to retry.
#[test]
fn retry_failed_reads_a_last_run_file_without_an_outcome() {
    let mut project = Project::new("retry-failed-old").with("FAKE_DBT_FAIL", "customer_segments");
    let (code, json) = project.run(&[]);
    assert_eq!(code, 1, "{json:#}");
    project.env.retain(|(k, _)| k != "FAKE_DBT_FAIL");
    let path = project.dir.join(".ods/state.db.last-run.json");
    let mut kept: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    kept["schema_version"] = serde_json::json!({"major": 1, "minor": 0});
    kept.as_object_mut().unwrap().remove("outcome");
    std::fs::write(&path, serde_json::to_vec_pretty(&kept).unwrap()).unwrap();

    let (code, json, _) = retry(&project, &["--failed"]);
    assert_eq!(code, 1, "{json:#}");
    let text = json.to_string();
    assert!(text.contains("ODS-E0403"), "{text}");
    assert!(text.contains("nothing is recorded to retry"), "{text}");

    let (code, json, _) = retry(&project, &["--dry-run"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["build"], 2, "{json:#}");
}

/// #292: `--failed` retries builds; `ods state test` already reruns only what hasn't
/// passed.
#[test]
fn retry_failed_refuses_a_test_run() {
    let project = Project::new("retry-failed-test");
    project.run_ok(&["--test"]);
    project.test_ok(&[]);
    let (code, json, _) = retry(&project, &["--failed"]);
    assert_eq!(code, 2, "{json:#}");
    assert!(json.to_string().contains("only ran tests"), "{json:#}");
}

/// #188: the documented recovery procedure. `doctor` checks without changing anything;
/// a damaged database stops the commands that read it, with a pointer to `doctor`;
/// `reset` sets it aside; a copy from `backup` restores it.
#[test]
fn a_damaged_state_database_is_diagnosed_and_recovered() {
    let project = Project::new("recovery");
    let db = project.db();
    let db_arg = ["--state-db", db.to_str().unwrap()];
    let state = |command: &str, extra: &[&str]| {
        let mut all = vec!["state", command];
        all.extend(db_arg);
        all.extend(extra);
        project.ods_in(&project.dir, &all)
    };

    let (code, json, _) = state("doctor", &[]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["exists"], false);

    project.run_ok(&[]);
    let (code, json, _) = state("doctor", &[]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["schema"]["version"], result["schema"]["latest"]);
    assert_eq!(result["scopes"][0]["scope"], "jaffle_ods/default");
    assert_eq!(result["scopes"][0]["snapshots"], 1);

    let copy = project.dir.join("copy.db");
    let (code, json, _) = state("backup", &["--to", copy.to_str().unwrap()]);
    assert_eq!(code, 0, "{json:#}");
    assert!(copy.is_file());

    // A disk fault, or another program, overwrites it. SQLite's journal goes too:
    // committed pages may still be there (on macOS they often are), and SQLite rightly
    // reads them, so a damaged main file alone isn't the same damage everywhere.
    std::fs::write(&db, vec![0x5a; 8192]).unwrap();
    for journal in ["-wal", "-shm"] {
        let mut name = db.clone().into_os_string();
        name.push(journal);
        let _ = std::fs::remove_file(name);
    }
    let (code, json) = project.ods(&["state", "plan"]);
    assert_eq!(code, 1, "{json:#}");
    let text = json.to_string();
    assert!(
        text.contains("ODS-E0405") && text.contains("ods state doctor"),
        "{text}"
    );

    let (code, json, _) = state("doctor", &[]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["result"]["problems"][0]["kind"], "damaged", "{json:#}");
    assert!(json.to_string().contains("ODS-E0405"), "{json:#}");
    assert_eq!(
        std::fs::read(&db).unwrap(),
        vec![0x5a; 8192],
        "doctor changes nothing"
    );

    let (code, json, _) = state("reset", &[]);
    assert_eq!(code, 2, "reset needs --yes: {json:#}");
    let (code, json, _) = state("reset", &["--yes"]);
    assert_eq!(code, 0, "{json:#}");
    assert!(!db.exists());
    let aside = json["result"]["set_aside"][0].as_str().unwrap();
    assert!(Path::new(aside).is_file(), "{json:#}");

    // Restore the copy, as docs/cli.md says: the state is back.
    std::fs::copy(&copy, &db).unwrap();
    let (code, plan) = project.ods(&["state", "plan"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["result"]["reuse"], 13, "{plan:#}");
}

/// #21: `explain`, `why-build` and `why-skip` trace a decision to its root cause.
#[test]
fn explain_traces_a_build_to_its_root_cause() {
    let project = Project::new("explain");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.stg_orders");
    // The fake dbt writes the change to the target directory when it compiles.
    let (code, json) = project.command("compile", &[]);
    assert_eq!(code, 0, "{json:#}");
    let (code, json) = project.ods(&["state", "explain", "customers"]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert_eq!(result["verdict"], "customers would be built");
    assert!(result["last_build"]["run_id"].is_string(), "{json:#}");
    let customers = &result["explanation"];
    assert_eq!(
        customers["entry"]["reasons"][0]["code"],
        "upstream_code_changed"
    );
    let orders = &customers["causes"][0];
    assert_eq!(orders["entry"]["name"], "orders");
    let stg_orders = &orders["causes"][0];
    assert_eq!(stg_orders["entry"]["name"], "stg_orders");
    assert_eq!(stg_orders["entry"]["reasons"][0]["code"], "code_changed");
    assert!(stg_orders["causes"].as_array().unwrap().is_empty());

    // Asked the other way round, the answer says so.
    let (code, json) = project.ods(&["state", "why-skip", "customers"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(
        json["result"]["verdict"],
        "customers isn't reused: it would be built"
    );
    let (_, json) = project.ods(&["state", "why-build", "stg_payments"]);
    assert_eq!(
        json["result"]["verdict"],
        "stg_payments isn't built: it would be reused"
    );
    assert_eq!(
        json["result"]["explanation"]["entry"]["reasons"][0]["code"],
        "unchanged"
    );
    // A unique id works too; an unknown name is a usage error.
    let (code, _) = project.ods(&["state", "explain", "model.jaffle_ods.orders"]);
    assert_eq!(code, 0);
    let (code, json) = project.ods(&["state", "explain", "nope"]);
    assert_eq!(code, 2, "{json:#}");
}

/// #21: `history <node>` explains each past build from what the snapshots record, and
/// `diff` says what changed, now or between two snapshots.
#[test]
fn past_builds_stay_explainable_and_diffs_say_what_changed() {
    let project = Project::new("node-history");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.stg_orders");
    // The fake dbt writes the change to the target directory when it compiles.
    let (code, json) = project.command("compile", &[]);
    assert_eq!(code, 0, "{json:#}");

    // Before building: the project differs from the recorded state.
    let (code, json) = project.ods(&["state", "diff"]);
    assert_eq!(code, 0, "{json:#}");
    let changed = &json["result"]["diff"]["changed"];
    assert_eq!(changed.as_array().unwrap().len(), 1, "{json:#}");
    assert_eq!(changed[0]["node"], "model.jaffle_ods.stg_orders");
    assert_eq!(changed[0]["changes"][0]["kind"], "code");

    project.run_ok(&[]);
    let (code, json) = project.ods(&["state", "history", "stg_orders"]);
    assert_eq!(code, 0, "{json:#}");
    let events = json["result"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{json:#}");
    assert_eq!(events[0]["event"], "built");
    assert_eq!(events[0]["changes"][0]["kind"], "code");
    assert_eq!(events[1]["first"], true);
    // A reader rebuilt because of it says which parent was rebuilt.
    let (_, json) = project.ods(&["state", "history", "orders"]);
    let latest = &json["result"]["events"][0];
    assert!(
        latest["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "upstream" && c["parent"] == "model.jaffle_ods.stg_orders"),
        "{json:#}"
    );

    // Between the two snapshots: what was rebuilt, and why.
    let (code, json) = project.ods(&["state", "diff", "--from", "1", "--to", "2"]);
    assert_eq!(code, 0, "{json:#}");
    let rebuilt: Vec<&str> = json["result"]["diff"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["node"].as_str().unwrap())
        .collect();
    assert!(
        rebuilt.contains(&"model.jaffle_ods.stg_orders"),
        "{rebuilt:?}"
    );
    assert!(
        !rebuilt.contains(&"model.jaffle_ods.stg_payments"),
        "{rebuilt:?}"
    );
    // Now nothing differs.
    let (_, json) = project.ods(&["state", "diff"]);
    assert!(
        json["result"]["diff"]["changed"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{json:#}"
    );
}

/// #21: `graph --changed` shows what would be built and what it reads.
#[test]
fn graph_shows_what_would_be_built() {
    let project = Project::new("state-graph");
    project.run_ok(&[]);
    project.change_code("model.jaffle_ods.stg_orders");
    // The fake dbt writes the change to the target directory when it compiles.
    let (code, json) = project.command("compile", &[]);
    assert_eq!(code, 0, "{json:#}");
    let (code, json) = project.ods(&["state", "graph", "--changed"]);
    assert_eq!(code, 0, "{json:#}");
    let result = &json["result"];
    assert!(
        result["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["action"] == "build"),
        "{json:#}"
    );
    assert!(
        result["edges"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!([
                "model.jaffle_ods.stg_orders",
                "model.jaffle_ods.orders"
            ])),
        "{json:#}"
    );
    assert!(result["text"].as_str().unwrap().starts_with("graph LR"));
}

/// Each source's tests decision in a report: name → (action, first reason's code).
fn source_tests(result: &Value) -> std::collections::BTreeMap<String, (String, String)> {
    result["source_tests"]
        .as_array()
        .unwrap_or_else(|| panic!("no source tests: {result:#}"))
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                (
                    c["action"].as_str().unwrap().to_owned(),
                    c["reasons"][0]["code"].as_str().unwrap().to_owned(),
                ),
            )
        })
        .collect()
}

fn decided(action: &str, code: &str) -> (String, String) {
    (action.to_owned(), code.to_owned())
}

/// The ids of the tests a run selected itself (`resource_type:test`).
fn selected_tests(result: &Value) -> Vec<String> {
    result["execution"]["command"]
        .as_str()
        .unwrap()
        .split(' ')
        .filter(|s| s.ends_with(",resource_type:test"))
        .map(|s| {
            s.trim_start_matches("fqn:jaffle_ods.")
                .trim_end_matches(",resource_type:test")
                .to_owned()
        })
        .collect()
}

/// #232 with the fake dbt: a source's tests run in `ods state build` and `ods state
/// test` when its `max_loaded_at` changes, are skipped when it doesn't, and always run
/// when its data version is unknown (`raw.payments` is never measured).
#[test]
fn source_tests_run_when_the_source_has_new_or_unknown_data() {
    let project = Project::new("source-tests")
        .with("FAKE_DBT_SOURCES", "1")
        .with("FAKE_DBT_LOADED_AT", "raw.orders=2026-01-01T00:00:00Z");
    let first = project.run_ok(&["--test"]);
    assert_eq!(
        source_tests(&first),
        [
            ("raw.orders".into(), decided("test", "not_tested")),
            ("raw.payments".into(), decided("test", "not_tested")),
        ]
        .into()
    );
    assert_eq!(
        selected_tests(&first),
        [
            "source_not_null_raw_orders_id",
            "source_not_null_raw_payments_id"
        ]
    );
    assert_eq!(
        names(&first["execution"]["sources"]),
        ["orders", "payments"],
        "{first:#}"
    );
    assert_eq!(
        first["record"]["source_tests"]["passed"],
        serde_json::json!([
            "source.jaffle_ods.raw.orders",
            "source.jaffle_ods.raw.payments"
        ])
    );

    // Same data: raw.orders' tests are skipped; raw.payments' version is unknown, so
    // they run again.
    let again = project.run_ok(&["--test"]);
    let decisions = source_tests(&again);
    assert_eq!(decisions["raw.orders"], decided("skip", "unchanged"));
    assert_eq!(
        decisions["raw.payments"],
        decided("test", "missing_data_evidence")
    );
    assert_eq!(selected_tests(&again), ["source_not_null_raw_payments_id"]);

    // New data in raw.orders: its tests run, and the report says why.
    let project = project.with("FAKE_DBT_LOADED_AT", "raw.orders=2026-01-02T00:00:00Z");
    let changed = project.run_ok(&["--test"]);
    let orders = changed["source_tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "raw.orders")
        .unwrap();
    assert_eq!(orders["action"], "test");
    assert_eq!(orders["reasons"][0]["code"], "new_upstream_data");
    assert!(
        orders["reasons"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("`raw.orders` has new data"),
        "{orders:#}"
    );
    assert!(
        selected_tests(&changed).contains(&"source_not_null_raw_orders_id".to_owned()),
        "{changed:#}"
    );

    // `ods state test` decides the same way: unchanged raw.orders is skipped, unknown
    // raw.payments runs, and new data in raw.orders runs it.
    let tested = project.test_ok(&[]);
    assert_eq!(
        source_tests(&tested)["raw.orders"],
        decided("skip", "unchanged")
    );
    assert_eq!(
        names(&tested["execution"]["sources"]),
        ["payments"],
        "{tested:#}"
    );
    assert_eq!(
        tested["record"]["source_tests"]["passed"],
        serde_json::json!(["source.jaffle_ods.raw.payments"])
    );
    let project = project.with("FAKE_DBT_LOADED_AT", "raw.orders=2026-01-03T00:00:00Z");
    let tested = project.test_ok(&[]);
    assert_eq!(
        source_tests(&tested)["raw.orders"],
        decided("test", "new_upstream_data")
    );
    assert_eq!(
        names(&tested["execution"]["sources"]),
        ["orders", "payments"]
    );

    // A build without tests runs no source tests either.
    let untested = project.run_ok(&[]);
    assert!(untested.get("source_tests").is_none(), "{untested:#}");
}

/// #232 with the fake dbt: a failing source test fails `ods state build`, and the
/// nodes reading the source are skipped (as `dbt build` does) and keep their last
/// state; the tests run again next time, even without new data.
#[test]
fn a_failing_source_test_fails_the_build_and_skips_its_readers() {
    let project = Project::new("source-test-fails")
        .with("FAKE_DBT_SOURCES", "1")
        .with(
            "FAKE_DBT_LOADED_AT",
            "raw.orders=2026-01-01T00:00:00Z,raw.payments=2026-01-01T00:00:00Z",
        )
        .with("FAKE_DBT_FAIL_TEST", "source_not_null_raw_orders_id");
    let (code, json) = project.run(&["--test"]);
    assert_eq!(code, 1, "{json:#}");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0404");
    let result = &json["result"];
    assert_eq!(result["outcome"], "failed");
    assert_eq!(
        result["execution"]["checks_failed"],
        serde_json::json!(["test.jaffle_ods.source_not_null_raw_orders_id.0000000000"])
    );
    let status = |name: &str| {
        result["execution"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["node"].as_str().unwrap().ends_with(&format!(".{name}")))
            .unwrap()["status"]
            .clone()
    };
    // stg_orders reads raw.orders; orders reads stg_orders.
    assert_eq!(status("stg_orders"), "skipped");
    assert_eq!(status("orders"), "skipped");
    assert_eq!(status("stg_payments"), "success");
    assert_eq!(
        result["record"]["source_tests"]["failed"],
        serde_json::json!(["source.jaffle_ods.raw.orders"])
    );
    assert_eq!(
        result["record"]["source_tests"]["passed"],
        serde_json::json!(["source.jaffle_ods.raw.payments"])
    );
    let advanced = names(&result["record"]["advanced"]);
    assert!(!advanced.contains(&"stg_orders".to_owned()), "{advanced:?}");
    assert!(
        advanced.contains(&"stg_payments".to_owned()),
        "{advanced:?}"
    );

    // Same data, still failing: raw.orders' tests run again, and its readers stay
    // unbuilt; raw.payments' passed on this data, so they are skipped.
    let (code, json) = project.run(&["--test"]);
    assert_eq!(code, 1, "{json:#}");
    let decisions = source_tests(&json["result"]);
    assert_eq!(decisions["raw.orders"], decided("test", "not_tested"));
    assert_eq!(decisions["raw.payments"], decided("skip", "unchanged"));

    // Fixed: they pass, and the readers build.
    let project = project.with("FAKE_DBT_FAIL_TEST", "");
    let fixed = project.run_ok(&["--test"]);
    assert!(
        names(&fixed["record"]["advanced"]).contains(&"stg_orders".to_owned()),
        "{fixed:#}"
    );
    assert_eq!(
        source_tests(&project.run_ok(&["--test"]))["raw.orders"],
        decided("skip", "unchanged")
    );
}

#[test]
fn sources_can_be_tested_before_anything_is_built() {
    let project = Project::new("source-tests-first")
        .with("FAKE_DBT_SOURCES", "1")
        .with(
            "FAKE_DBT_LOADED_AT",
            "raw.orders=2026-01-01T00:00:00Z,raw.payments=2026-01-01T00:00:00Z",
        );
    let first = project.test_ok(&["--all"]);
    assert!(first["based_on"].is_null(), "{first:#}");
    assert_eq!(first["requested"], 0, "no node is built, so none is tested");
    assert_eq!(
        first["record"]["source_tests"]["passed"],
        serde_json::json!([
            "source.jaffle_ods.raw.orders",
            "source.jaffle_ods.raw.payments"
        ])
    );
    // Recorded against their data: the same data needs no second run.
    let again = project.test_ok(&[]);
    assert_eq!(again["outcome"], "nothing_to_test", "{again:#}");
    assert_eq!(
        source_tests(&again)["raw.orders"],
        decided("skip", "unchanged")
    );
}

#[test]
fn a_failing_source_test_keeps_the_nodes_tests_that_passed() {
    let project = Project::new("source-test-fails-in-test")
        .with("FAKE_DBT_SOURCES", "1")
        .with(
            "FAKE_DBT_LOADED_AT",
            "raw.orders=2026-01-01T00:00:00Z,raw.payments=2026-01-01T00:00:00Z",
        );
    project.run_ok(&[]);
    let project = project.with("FAKE_DBT_FAIL_TEST", "source_not_null_raw_orders_id");
    let (code, json) = project.test(&[]);
    assert_eq!(code, 1, "{json:#}");
    let record = &json["result"]["record"];
    assert_eq!(
        record["source_tests"]["failed"],
        serde_json::json!(["source.jaffle_ods.raw.orders"])
    );
    let passed = record["passed"].as_array().unwrap();
    assert!(
        !passed.is_empty(),
        "a source test failing explains the failed run: the nodes' passes count: {json:#}"
    );
    // So only the failed source's tests run next time.
    let (_, again) = project.test(&[]);
    assert_eq!(again["result"]["requested"], 0, "{again:#}");
}

const DETAIL: &str = "DESCRIBE DETAIL {relation}";
const HISTORY: &str = "DESCRIBE HISTORY {relation} LIMIT 1";

/// The fake Databricks workspace the relation probe reads (ADR-0022): source name →
/// `(format, table id, version)`.
fn workspace(project: &Project, tables: &[(&str, &str, &str, &str)]) {
    let doc: serde_json::Map<String, Value> = tables
        .iter()
        .map(|(name, format, id, version)| {
            (
                (*name).to_owned(),
                serde_json::json!({
                    "type": "table",
                    "formats": [format],
                    "rows": {
                        DETAIL: {"id": id, "format": format},
                        HISTORY: {"version": version, "timestamp": "2026-09-29 10:00:00"},
                    },
                }),
            )
        })
        .collect();
    std::fs::write(
        project.dir.join("workspace.json"),
        Value::Object(doc).to_string(),
    )
    .unwrap();
}

/// A project on Databricks, with two sources, whose table versions the fake dbt reads.
fn on_databricks(name: &str) -> Project {
    let project = Project::new(name);
    let workspace = project.dir.join("workspace.json");
    let seen = project.dir.join("seen");
    project
        .with("FAKE_DBT_SOURCES", "1")
        .with("FAKE_DBT_ADAPTER", "databricks")
        .with("FAKE_DBT_PROBE", workspace.to_str().unwrap())
        .with("FAKE_DBT_SEEN", seen.to_str().unwrap())
}

/// ODS compares times to the second: a version read in the same second as the last
/// build can't show data that arrived after it.
fn next_second() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(
        1010 - u64::from(now.subsec_millis()),
    ));
}

/// The value of every `kind` evidence about `source` in `name`'s plan entry, joined.
fn source_evidence(result: &Value, name: &str, kind: &str, source: &str) -> Option<String> {
    entry(result, name)["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind && e["subject"] == source)
        .map(|e| e["value"].as_str().unwrap_or_default().to_owned())
        .reduce(|a, b| format!("{a}; {b}"))
}

/// How many dbt calls ran the relation probe.
fn probes(project: &Project) -> usize {
    project
        .seen()
        .iter()
        .filter(|(argv, _)| argv.iter().any(|a| a.contains("ods_relation_probe")))
        .count()
}

const ORDERS: &str = "source.jaffle_ods.raw.orders";
const PAYMENTS: &str = "source.jaffle_ods.raw.payments";

/// ADR-0022: on Databricks, each source's Delta table version decides whether the
/// models reading it are reused. A first build records it as the baseline; the same
/// version reuses; a new commit builds. `--dry-run` reads versions too, `ods state
/// plan` doesn't.
#[test]
fn delta_table_versions_decide_reuse_on_databricks() {
    let project = on_databricks("delta-versions");
    workspace(
        &project,
        &[
            ("raw.orders", "delta", "t-orders", "3"),
            ("raw.payments", "delta", "t-payments", "8"),
        ],
    );
    let first = project.run_ok(&[]);
    assert_eq!(probes(&project), 1, "every source, in one dbt call");
    assert_eq!(first["record"]["sources_recorded"], true, "{first:#}");

    next_second();
    let same = project.run_ok(&["--dry-run"]);
    assert_eq!(probes(&project), 2, "a dry run reads versions too");
    for (model, source, version) in [
        ("stg_orders", ORDERS, "t-orders/3"),
        ("stg_payments", PAYMENTS, "t-payments/8"),
    ] {
        assert_eq!(entry(&same, model)["action"], "reuse", "{same:#}");
        assert_eq!(
            source_evidence(&same, model, "source_data_version", source).as_deref(),
            Some(version)
        );
        assert_eq!(
            source_evidence(&same, model, "source_version_strategy", source).as_deref(),
            Some("relation_versions")
        );
        assert_eq!(
            source_evidence(&same, model, "source_version_origin", source).as_deref(),
            Some("delta_history")
        );
        // Freshness wasn't measured, so it could have applied: it says why not.
        assert_eq!(
            source_evidence(&same, model, "source_version_skipped", source),
            None,
            "the chosen strategy comes first; nothing before it was skipped"
        );
    }

    // A commit to raw.orders: its readers build, raw.payments' don't.
    workspace(
        &project,
        &[
            ("raw.orders", "delta", "t-orders", "4"),
            ("raw.payments", "delta", "t-payments", "8"),
        ],
    );
    next_second();
    let changed = project.run_ok(&[]);
    let orders = entry(&changed, "stg_orders");
    assert_eq!(orders["action"], "build");
    assert_eq!(orders["reasons"][0]["code"], "new_upstream_data");
    assert!(
        orders["reasons"][0]["message"]
            .as_str()
            .unwrap()
            .contains("raw.orders"),
        "{orders:#}"
    );
    assert_eq!(entry(&changed, "stg_payments")["action"], "reuse");

    // Offline: the plan says it didn't read them.
    let seen = probes(&project);
    let (code, plan) = project.ods(&["state", "plan"]);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(probes(&project), seen);
    let says_so = |json: &Value| {
        json["result"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("table versions weren't read"))
    };
    assert!(says_so(&plan), "{plan:#}");
    // `ods state explain` plans the same way, and says the same.
    let (code, explained) = project.ods(&["state", "explain", "stg_orders"]);
    assert_eq!(code, 0, "{explained:#}");
    assert!(says_so(&explained), "{explained:#}");
    assert_eq!(probes(&project), seen);

    // The step line says what the dbt call is for.
    let dbt = fixture("fake-dbt/dbt");
    let (code, _, stderr) = project.ods_with_stderr(&[
        "state",
        "build",
        "--dry-run",
        "--dbt",
        dbt.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("dbt show: reading table versions for 2 sources"),
        "{stderr}"
    );
}

/// ADR-0022 §4: when the probe fails, every source is unknown: the models reading
/// them build, and a warning names dbt's error.
#[test]
fn a_failed_table_version_probe_builds_every_reader_and_warns() {
    let project = on_databricks("delta-probe-fails");
    workspace(
        &project,
        &[
            ("raw.orders", "delta", "t-orders", "3"),
            ("raw.payments", "delta", "t-payments", "8"),
        ],
    );
    // No `dbt source freshness`, so no sources.json: table versions are all there is.
    project.run_ok(&["--no-source-freshness"]);
    next_second();
    let project = project.with("FAKE_DBT_PROBE_FAIL", "1");
    let failed = project.run_ok(&["--no-source-freshness"]);
    for (model, source) in [("stg_orders", ORDERS), ("stg_payments", PAYMENTS)] {
        let entry = entry(&failed, model);
        assert_eq!(entry["action"], "build", "{failed:#}");
        assert_eq!(entry["reasons"][0]["code"], "missing_data_evidence");
        // dbt's error is in the warning, not in each source's evidence.
        assert_eq!(
            source_evidence(&failed, model, "source_version_skipped", source).as_deref(),
            Some("relation_versions: the table-version probe failed; see the warning")
        );
    }
    // Nothing had a usable version, and there is no sources.json: the usual warning.
    assert!(
        failed["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .unwrap()
            .starts_with("no source freshness results")),
        "{failed:#}"
    );
    // Models reading no source are still reused.
    assert_eq!(entry(&failed, "stg_customers")["action"], "reuse");
    let warnings = failed["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| {
            let w = w.as_str().unwrap();
            w.contains("couldn't read the sources' table versions")
                && w.contains("DESCRIBE HISTORY refused")
        }),
        "{failed:#}"
    );
}

/// ADR-0022 §4: a source that isn't a Delta table is unknown to the probe, and falls
/// back to its `max_loaded_at`, saying why.
#[test]
fn a_source_that_isnt_a_delta_table_falls_back_to_max_loaded_at() {
    let project = on_databricks("delta-fallback").with(
        "FAKE_DBT_LOADED_AT",
        "raw.orders=2026-01-01T00:00:00Z,raw.payments=2026-01-01T00:00:00Z",
    );
    workspace(
        &project,
        &[
            ("raw.orders", "delta", "t-orders", "3"),
            ("raw.payments", "parquet", "t-payments", "8"),
        ],
    );
    project.run_ok(&[]);
    next_second();
    let again = project.run_ok(&[]);
    assert_eq!(
        entry(&again, "stg_payments")["action"],
        "reuse",
        "{again:#}"
    );
    assert_eq!(
        source_evidence(&again, "stg_payments", "source_version_strategy", PAYMENTS).as_deref(),
        Some("source_freshness")
    );
    let skipped =
        source_evidence(&again, "stg_payments", "source_version_skipped", PAYMENTS).unwrap();
    assert!(
        skipped.starts_with("relation_versions: not a Delta table"),
        "{skipped}"
    );
    // The Delta table's version wins over its load time.
    assert_eq!(
        source_evidence(&again, "stg_orders", "source_version_strategy", ORDERS).as_deref(),
        Some("relation_versions")
    );
}

/// Other adapters have no change provider: nothing probes their sources.
#[test]
fn other_adapters_read_no_table_versions() {
    let seen = |p: &Project| p.dir.join("seen");
    let project = Project::new("no-probe").with("FAKE_DBT_SOURCES", "1");
    let project = {
        let path = seen(&project);
        project.with("FAKE_DBT_SEEN", path.to_str().unwrap())
    };
    project.run_ok(&[]);
    next_second();
    project.run_ok(&[]);
    assert_eq!(probes(&project), 0);
}
/// ADR-0022 §2 in the plan's JSON: each source input says which strategy gave its
/// version, where the version came from, and why a strategy that could have applied
/// didn't. `raw.orders` is measured by `dbt source freshness`; `raw.payments` isn't.
#[test]
fn plan_json_says_where_each_source_version_came_from() {
    let project = Project::new("version-evidence")
        .with("FAKE_DBT_SOURCES", "1")
        .with("FAKE_DBT_LOADED_AT", "raw.orders=2026-01-01T00:00:00Z");
    project.run_ok(&[]);
    let planned = project.run_ok(&["--dry-run"]);
    let orders = "source.jaffle_ods.raw.orders";
    let payments = "source.jaffle_ods.raw.payments";
    assert_eq!(
        source_evidence(&planned, "stg_orders", "source_version_strategy", orders).as_deref(),
        Some("source_freshness"),
        "{planned:#}"
    );
    assert_eq!(
        source_evidence(&planned, "stg_orders", "source_version_origin", orders).as_deref(),
        Some("sources.json max_loaded_at")
    );
    // Nothing here reads table versions, so that strategy isn't listed as skipped.
    assert_eq!(
        source_evidence(&planned, "stg_orders", "source_version_skipped", orders),
        None
    );
    assert_eq!(
        source_evidence(
            &planned,
            "stg_payments",
            "source_version_strategy",
            payments
        )
        .as_deref(),
        Some("no_version")
    );
    assert_eq!(
        source_evidence(&planned, "stg_payments", "source_version_skipped", payments).as_deref(),
        Some("source_freshness: not reported")
    );
}

// ---------------------------------------------------------------------------- run journal (#322)

/// The journal of the run `result` reports, read back: one event per line.
fn journal_of(project: &Project, result: &Value) -> (PathBuf, Vec<Value>) {
    let run_id = result["execution"]["run_id"].as_str().unwrap();
    let path = project
        .dir
        .join(format!(".ods/state.db.runs/{run_id}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let events = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect();
    (path, events)
}

fn stats_of<'v>(result: &'v Value, name: &str) -> &'v Value {
    result["run_stats"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"].as_str().unwrap().ends_with(&format!(".{name}")))
        .map_or_else(|| panic!("{name}: {result:#}"), |n| &n["stats"])
}

/// #322: a run appends its events to a journal beside the state database, under the
/// run id `.last-run.json` keeps, and reports each node's stats. Rows a node didn't
/// report are missing, not zero, and the total says "at least".
#[test]
fn a_run_keeps_a_journal_with_each_nodes_stats() {
    let project = Project::new("journal").with("FAKE_DBT_ROWS", "raw_orders=6,orders=99");
    let result = project.run_ok(&[]);
    let (path, events) = journal_of(&project, &result);
    assert_eq!(result["journal"], path.display().to_string());
    assert_eq!(events.first().unwrap()["kind"], "run_started");
    assert_eq!(events.first().unwrap()["live"], true);
    assert_eq!(events.last().unwrap()["kind"], "run_finished");
    assert_eq!(events.last().unwrap()["outcome"], "succeeded");
    let run_id = result["execution"]["run_id"].as_str().unwrap();
    for event in &events {
        assert_eq!(event["schema_version"]["major"], 1, "{event}");
        assert_eq!(event["run_id"], run_id, "{event}");
        assert_eq!(event["scope"], result["scope"], "{event}");
    }
    let last: Value = serde_json::from_slice(
        &std::fs::read(project.dir.join(".ods/state.db.last-run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(last["run_id"], run_id);

    let orders = stats_of(&result, "orders");
    assert_eq!(orders["status"], "success");
    assert_eq!(orders["rows_affected"], 99);
    assert_eq!(orders["duration_ms"], 250);
    assert_eq!(orders["adapter"]["code"], "INSERT");
    assert!(orders["thread"].is_string());
    let view = stats_of(&result, "stg_orders");
    assert!(view["rows_affected"].is_null(), "{view}");
    let totals = &result["run_stats"]["totals"];
    assert_eq!(totals["rows_affected"], 105);
    assert_eq!(totals["rows_unreported"], 11);

    // `history` shows the run's totals beside its snapshot.
    let history = project.history();
    assert_eq!(history[0]["run_id"], run_id);
    assert_eq!(history[0]["run_stats"]["totals"]["rows_affected"], 105);
    assert_eq!(history[0]["run_stats"]["outcome"], "succeeded");
}

/// #322: a run that fails keeps its journal (canonical state still advances only for
/// what succeeded), and neither a `--vars` value nor a value in dbt's error message
/// reaches it. `ods state history --run` shows it, plain and JSON.
#[test]
fn a_failed_run_keeps_a_journal_without_values() {
    let project = Project::new("journal-failed")
        .with("FAKE_DBT_FAIL", "customers")
        .with("FAKE_DBT_ROWS", "raw_orders=6")
        .with(
            "FAKE_DBT_FAIL_MESSAGE",
            "Runtime Error in model customers (models/customers.sql)\n  Conversion Error: Could not convert string 'sk_live_SENTINEL_7' to INT32\n  LINE 3: where cast('sk_live_SENTINEL_7' as integer) = 1",
        );
    let dbt = fixture("fake-dbt/dbt");
    let (code, json, stderr) = project.ods_with_stderr(&[
        "-v",
        "state",
        "build",
        "--dbt",
        dbt.to_str().unwrap(),
        "--dbt-output",
        "capture",
        "--exclude-resource-type",
        "test",
        "--vars",
        r#"{"secret": "VARS_SENTINEL_7"}"#,
    ]);
    assert_eq!(code, 1, "{json:#}");
    // Neither the report (JSON) nor what ODS printed or logged (-v) holds the vars
    // value; the literal only reaches stderr as dbt's own line would (ADR-0024).
    assert!(!json.to_string().contains("SENTINEL_7"), "{json:#}");
    assert!(!stderr.contains("VARS_SENTINEL_7"), "{stderr}");
    let result = &json["result"];
    let (path, events) = journal_of(&project, result);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("SENTINEL_7"), "{text}");
    assert!(!text.to_lowercase().contains("select"), "{text}");
    assert!(!text.contains("as integer"), "{text}");
    assert_eq!(events.last().unwrap()["outcome"], "failed");
    let failed = stats_of(result, "customers");
    assert_eq!(failed["status"], "error");
    assert_eq!(failed["error"]["kind"], "Conversion Error");
    assert_eq!(
        failed["error"]["message"],
        "Conversion Error: Could not convert string [value removed] to INT32"
    );
    let skipped = stats_of(result, "segment_summary");
    assert_eq!(skipped["status"], "skipped");
    assert!(skipped["duration_ms"].is_null(), "{skipped}");
    // The successes were recorded; the failure keeps its last state.
    assert!(result["record"]["snapshot"].is_number(), "{result:#}");

    // The run, from its journal.
    let run_id = result["execution"]["run_id"].as_str().unwrap();
    let (code, shown) = project.ods(&["state", "history", "--run", run_id]);
    assert_eq!(code, 0, "{shown:#}");
    let run = &shown["result"]["run"];
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["run_id"], run_id);
    assert!(!shown.to_string().contains("SENTINEL_7"));
    let redact = |text: &str| {
        text.replace(run_id, "<run>")
            .replace(project.dir.to_str().unwrap(), "<project>")
    };
    let mut stable = serde_json::json!({
        "outcome": run["outcome"],
        "live": run["live"],
        "mode": run["mode"],
        "nodes": run["nodes"].as_array().unwrap().iter().map(|n| {
            let s = &n["stats"];
            serde_json::json!({
                "node": n["node"],
                "status": s["status"],
                "duration_ms": s["duration_ms"],
                "rows_affected": s["rows_affected"],
                "adapter": s["adapter"],
                "error": s["error"],
                "blocked_by": s["blocked_by"],
                "tests": s["tests"],
            })
        }).collect::<Vec<_>>(),
        "totals": run["totals"],
    });
    stable["totals"]["duration_ms"] = Value::String("<ms>".into());
    insta::assert_snapshot!(
        "history_run_json",
        redact(&serde_json::to_string_pretty(&stable).unwrap())
    );
    let plain = project.ods_plain(&["state", "history", "--run", run_id]);
    let plain: String = plain
        .lines()
        .map(|l| {
            // Times of this run.
            if l.starts_with("started:") {
                "started: <time>".to_owned()
            } else if l.starts_with("totals:") {
                format!("totals: <took>{}", &l[l.find(" · ").unwrap()..])
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("history_run_plain", redact(&plain));
}

/// #322: `ods state run`'s plain output shows each node's time taken and rows, `—`
/// when not reported, and the totals.
#[test]
fn the_run_table_shows_time_taken_and_rows() {
    let project = Project::new("journal-plain").with("FAKE_DBT_ROWS", "raw_orders=6");
    let dbt = fixture("fake-dbt/dbt");
    let plain = project.ods_plain(&[
        "state",
        "build",
        "--dbt",
        dbt.to_str().unwrap(),
        "--dbt-output",
        "capture",
        "--exclude-resource-type",
        "test",
        "--select",
        "raw_orders",
        "--select",
        "stg_orders",
    ]);
    let table: Vec<&str> = plain
        .lines()
        .skip_while(|l| !l.contains("why it ran"))
        .take_while(|l| !l.trim().is_empty())
        .collect();
    insta::assert_snapshot!("run_table_plain", table.join("\n"));
    let rows = plain
        .lines()
        .find(|l| l.trim_start().starts_with("rows"))
        .unwrap();
    assert!(
        rows.contains("at least 6 (1 node didn't report rows)"),
        "{plain}"
    );
}

/// Codex review on #327: with the caller's own console level passed through to dbt
/// (`-- --log-level warn`), dbt still streams its debug-level node events, so the
/// stats are live; only lines at `warn` and above are shown.
#[test]
fn a_callers_log_level_only_filters_what_is_shown() {
    let project = Project::new("log-level");
    let seen = project.dir.join("seen");
    let project = project.with("FAKE_DBT_SEEN", seen.to_str().unwrap());
    let dbt = fixture("fake-dbt/dbt");
    let (code, json, stderr) = project.ods_with_stderr(&[
        "state",
        "build",
        "--dbt",
        dbt.to_str().unwrap(),
        "--exclude-resource-type",
        "test",
        "--",
        "--log-level",
        "warn",
    ]);
    assert_eq!(code, 0, "{json:#}");
    let stats = &json["result"]["run_stats"];
    assert_eq!(stats["live"], true, "{stats:#}");
    assert!(
        stats["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["stats"]["thread"].is_string())
    );
    // Shown: the warning, not dbt's info lines.
    assert!(
        stderr.contains("fake dbt: a warning after the nodes"),
        "{stderr}"
    );
    assert!(!stderr.contains("fake dbt: build"), "{stderr}");
    assert!(!stderr.contains("SUCCESS model."), "{stderr}");
    // dbt was asked for debug, and never for the caller's level.
    let (argv, _) = project.seen().pop().unwrap();
    let at = argv.iter().position(|a| a == "--log-level").unwrap();
    assert_eq!(argv[at + 1], "debug", "{argv:?}");
    assert_eq!(
        argv.iter().filter(|a| *a == "--log-level").count(),
        1,
        "{argv:?}"
    );
    assert!(!argv.contains(&"warn".to_owned()), "{argv:?}");
}
