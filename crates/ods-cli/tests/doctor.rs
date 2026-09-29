//! `ods doctor` end to end (#181), with the fake dbt in `fixtures/dbt/fake-dbt` standing
//! in for dbt (a Python script, so Unix only; no warehouse or network): a healthy
//! project, a warning (stale artifacts), and failures (no manifest, broken
//! configuration, a failing live check), with their exit statuses.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt")
        .join(path)
}

fn fake_dbt() -> PathBuf {
    fixture("fake-dbt/dbt")
}

fn touch(path: &Path, seconds: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

/// A dbt project whose manifest was written after its code (times are fixed, so
/// output is the same on every run), and an empty home for configuration.
struct Project {
    guard: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Project {
    fn new() -> Self {
        let guard = tempfile::Builder::new()
            .prefix("ods-doctor-")
            .tempdir()
            .unwrap();
        let root = guard.path();
        for dir in ["models", "target", "home"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::copy(
            fixture("jaffle-ods/dbt_project.yml"),
            root.join("dbt_project.yml"),
        )
        .unwrap();
        std::fs::write(root.join("models/orders.sql"), "select 1 as id\n").unwrap();
        std::fs::copy(
            fixture("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
            root.join("target/manifest.json"),
        )
        .unwrap();
        touch(&root.join("dbt_project.yml"), 1_767_225_600);
        touch(&root.join("models/orders.sql"), 1_767_225_600);
        touch(&root.join("target/manifest.json"), 1_767_229_200);
        Self {
            guard,
            env: Vec::new(),
        }
    }

    fn dir(&self) -> &Path {
        self.guard.path()
    }

    fn with(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Runs `ods doctor <args>` in the project, with the fake dbt first on `PATH` (so
    /// output shows the default program, `dbt`); returns the exit status, stdout and
    /// stderr.
    fn doctor(&self, args: &[&str]) -> (i32, String, String) {
        // The fake dbt is `#!/usr/bin/env python3`, so the system's PATH follows it.
        let path = std::env::join_paths(std::iter::once(fixture("fake-dbt")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .arg("doctor")
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("XDG_CONFIG_HOME", self.dir().join("home"))
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(self.dir())
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let (code, stdout, stderr) = self.doctor(&all);
        let json =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}\n{stderr}"));
        (code, json)
    }

    /// Replaces what differs between machines and runs: the scratch directory (also
    /// in its canonical form: macOS reports `/var` as `/private/var`) and ODS's version.
    fn redact(&self, text: &str) -> String {
        let mut text = text.to_owned();
        if let Ok(canonical) = self.dir().canonicalize() {
            text = text.replace(&canonical.display().to_string(), "[dir]");
        }
        text.replace(&self.dir().display().to_string(), "[dir]")
            .replace(env!("CARGO_PKG_VERSION"), "[ods-version]")
    }
}

fn check<'a>(result: &'a Value, id: &str) -> &'a Value {
    result["result"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no check {id} in {result:#}"))
}

fn statuses(result: &Value) -> Vec<(String, String)> {
    result["result"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_healthy_project_exits_0_and_every_check_passes_or_is_skipped() {
    let project = Project::new();
    let (code, json) = project.json(&[]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["verdict"], "healthy");
    assert_eq!(json["diagnostics"], serde_json::json!([]));
    for (id, status) in statuses(&json) {
        let expected = if id.starts_with("connectivity.") {
            "skipped"
        } else {
            "ok"
        };
        assert_eq!(status, expected, "{id}: {:#}", check(&json, &id));
    }
    let target = check(&json, "target.identity");
    assert_eq!(target["message"], "dbt builds in target `dev`");
    let dbt = check(&json, "tools.dbt");
    assert_eq!(dbt["message"], "dbt 1.10.23");
    // Nothing was written into the project but dbt's own target check.
    assert!(!project.dir().join(".ods").exists());

    // Snapshots of all three renderings. Output is the same on every run.
    insta::assert_snapshot!(
        "doctor_healthy_json",
        project.redact(&serde_json::to_string_pretty(&json).unwrap())
    );
    let (_, plain, _) = project.doctor(&["-o", "plain"]);
    let (_, again, _) = project.doctor(&["-o", "plain"]);
    assert_eq!(plain, again);
    insta::assert_snapshot!("doctor_healthy_plain", project.redact(&plain));
    let (code, human, _) = project.doctor(&["-o", "human", "--color", "never", "--width", "400"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!("doctor_healthy_human", project.redact(&human));
}

#[test]
fn stale_artifacts_warn_exit_0_and_fail_with_strict() {
    let project = Project::new();
    touch(&project.dir().join("models/orders.sql"), 1_767_232_800);
    let (code, json) = project.json(&[]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(json["result"]["verdict"], "warnings");
    let freshness = check(&json, "project.freshness");
    assert_eq!(freshness["status"], "warning");
    assert_eq!(freshness["code"], "ODS-W0206");

    let (code, json) = project.json(&["--strict"]);
    assert_eq!(code, 5);
    assert_eq!(json["result"]["verdict"], "failed");
    assert_eq!(json["diagnostics"][0]["code"], "ODS-E0501");
    assert_eq!(
        json["diagnostics"][0]["message"],
        "1 check failed: project.freshness"
    );
    let (code, plain, stderr) = project.doctor(&["--strict", "-o", "plain"]);
    assert_eq!(code, 5);
    insta::assert_snapshot!("doctor_stale_plain", project.redact(&plain));
    assert!(
        stderr.contains("error[ODS-E0501]: 1 check failed"),
        "{stderr}"
    );
}

#[test]
fn no_manifest_fails_with_exit_5_and_what_needs_it_is_unknown() {
    let project = Project::new();
    std::fs::remove_file(project.dir().join("target/manifest.json")).unwrap();
    let (code, json) = project.json(&["--project"]);
    assert_eq!(code, 5, "{json:#}");
    assert_eq!(
        statuses(&json),
        [
            ("project.dbt_project".to_owned(), "ok".to_owned()),
            ("project.manifest".to_owned(), "error".to_owned()),
            ("project.name".to_owned(), "unknown".to_owned()),
            ("project.freshness".to_owned(), "unknown".to_owned()),
        ]
    );
    assert_eq!(check(&json, "project.manifest")["code"], "ODS-E0201");
    assert_eq!(check(&json, "project.name")["code"], "ODS-U0001");
    assert_eq!(json["result"]["scope"]["project_only"], true);
    let (_, plain, _) = project.doctor(&["--project", "-o", "plain"]);
    insta::assert_snapshot!("doctor_no_manifest_plain", project.redact(&plain));
}

#[test]
fn broken_configuration_is_a_finding_not_an_early_exit() {
    let project = Project::new();
    std::fs::write(project.dir().join("ods.toml"), "[state\n").unwrap();
    let (code, json) = project.json(&[]);
    // Other commands stop with 4; the doctor reports it and exits 5.
    assert_eq!(code, 5, "{json:#}");
    let load = check(&json, "config.load");
    assert_eq!(load["status"], "error");
    assert_eq!(load["code"], "ODS-E0101");
    assert_eq!(check(&json, "project.manifest")["status"], "unknown");
    assert_eq!(check(&json, "tools.dbt")["status"], "unknown");

    let (code, _, _) = project.doctor(&["--json"]);
    assert_eq!(code, 5);
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "plan", "--json"])
        .env_clear()
        .current_dir(project.dir())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn plaintext_credentials_are_refused_and_never_shown() {
    let project = Project::new();
    std::fs::write(
        project.dir().join("ods.toml"),
        "[providers.wh]\nkind = \"x\"\nsettings = { token = \"s3cr3t-value\" }\n",
    )
    .unwrap();
    for mode in [
        &["--json"][..],
        &["-o", "plain"][..],
        &["-o", "human", "--color", "always"][..],
    ] {
        let (code, stdout, stderr) = project.doctor(mode);
        assert_eq!(code, 5, "{mode:?}");
        assert!(stdout.contains("ODS-E0103"), "{mode:?}: {stdout}");
        assert!(
            !stdout.contains("s3cr3t") && !stderr.contains("s3cr3t"),
            "{mode:?}: {stdout}\n{stderr}"
        );
    }
}

/// Every dbt command a default `ods doctor` makes: `--version`, and the target check,
/// which renders the profile without connecting. Never `show` (the relation check and
/// table-version probe, which query the warehouse), and never a build.
#[test]
fn a_default_run_queries_no_warehouse() {
    let project = Project::new();
    let seen = project.dir().join("seen.jsonl");
    let calls = project.dir().join("calls.txt");
    let project = project
        .with("FAKE_DBT_SEEN", seen.to_str().unwrap())
        .with("FAKE_DBT_CALLS", calls.to_str().unwrap());
    let (code, json) = project.json(&[]);
    assert_eq!(code, 0, "{json:#}");
    let argvs: Vec<Vec<String>> = std::fs::read_to_string(&seen)
        .unwrap()
        .lines()
        .map(|line| {
            let seen: Value = serde_json::from_str(line).unwrap();
            serde_json::from_value(seen["argv"].clone()).unwrap()
        })
        .collect();
    assert_eq!(argvs.len(), 2, "{argvs:?}");
    assert_eq!(argvs[0], ["--version"]);
    let target = &argvs[1];
    assert_eq!(target[0], "compile", "{target:?}");
    for flag in ["--inline", "--no-populate-cache", "--no-introspect"] {
        assert!(
            target.iter().any(|a| a == flag),
            "{flag} missing: {target:?}"
        );
    }
    let inline = target.iter().position(|a| a == "--inline").unwrap();
    assert!(target[inline + 1].contains("ods_target"), "{target:?}");
    // The commands dbt was asked to run, by name: only the target check's compile.
    let words = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(words, "compile\n");
    assert!(!words.contains("show"));
}

#[test]
fn a_missing_dbt_fails_and_the_target_is_unknown() {
    let project = Project::new();
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args([
            "doctor",
            "--provider",
            "dbt",
            "--json",
            "--dbt",
            "./no-such-dbt",
        ])
        .env_clear()
        .current_dir(project.dir())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(check(&json, "tools.dbt")["code"], "ODS-E0502");
    assert_eq!(check(&json, "target.identity")["status"], "unknown");
    assert_eq!(check(&json, "target.identity")["required"], true);
}

#[test]
fn a_profile_dbt_cant_render_fails_the_target_check() {
    let project = Project::new().with("FAKE_DBT_TARGET_FAIL", "1");
    let (code, json) = project.json(&[]);
    assert_eq!(code, 5);
    assert_eq!(check(&json, "target.identity")["code"], "ODS-E0509");
}

#[test]
fn a_missing_adapter_is_an_error() {
    let project = Project::new().with("FAKE_DBT_ADAPTER", "postgres");
    // The manifest was written with DuckDB; dbt lists only postgres.
    let (code, json) = project.json(&["--provider", "dbt"]);
    assert_eq!(code, 5);
    let adapter = check(&json, "tools.adapter");
    assert_eq!(adapter["code"], "ODS-E0506");
}

#[test]
fn connect_runs_the_live_checks_through_dbt() {
    let project = Project::new();
    let (code, json) = project.json(&["--connect"]);
    assert_eq!(code, 0, "{json:#}");
    let relations = check(&json, "connectivity.relations");
    assert_eq!(relations["status"], "ok", "{relations:#}");
    // DuckDB has no table versions to read.
    assert_eq!(
        check(&json, "connectivity.table_versions")["status"],
        "skipped"
    );

    let failing = Project::new().with("FAKE_DBT_SHOW_FAIL", "1");
    let (code, json) = failing.json(&["--connect", "--provider", "dbt"]);
    assert_eq!(code, 5);
    assert_eq!(check(&json, "connectivity.relations")["code"], "ODS-E0603");
}

#[test]
fn table_versions_are_probed_on_databricks() {
    let project = Project::new()
        .with("FAKE_DBT_ADAPTER", "databricks")
        .with("FAKE_DBT_SOURCES", "1");
    // The manifest ODS reads is the one dbt writes: ask the fake for it.
    let out = Command::new(fake_dbt())
        .args(["compile", "--target-path", "target"])
        .env("FAKE_DBT_ADAPTER", "databricks")
        .env("FAKE_DBT_SOURCES", "1")
        .current_dir(project.dir())
        .output()
        .unwrap();
    assert!(out.status.success());
    touch(&project.dir().join("target/manifest.json"), 1_767_229_200);
    // The fake workspace (ADR-0022): source name -> its Delta table's answers.
    let probe = project.dir().join("probe.json");
    let table = |id: &str| {
        serde_json::json!({
            "type": "table",
            "formats": ["delta"],
            "rows": {
                "DESCRIBE DETAIL {relation}": {"id": id, "format": "delta"},
                "DESCRIBE HISTORY {relation} LIMIT 1": {"version": "7", "timestamp": "2026-09-29 10:00:00"},
            },
        })
    };
    let write = |doc: Value| std::fs::write(&probe, doc.to_string()).unwrap();
    write(serde_json::json!({"raw.orders": table("t1"), "raw.payments": table("t2")}));
    let project = project.with("FAKE_DBT_PROBE", probe.to_str().unwrap());
    let (code, json) = project.json(&["--connect", "--provider", "databricks"]);
    assert_eq!(code, 0, "{json:#}");
    assert_eq!(
        check(&json, "capabilities.relation_versions")["status"],
        "ok"
    );
    let versions = check(&json, "connectivity.table_versions");
    assert_eq!(versions["status"], "ok", "{versions:#}");
    assert_eq!(versions["provider"], "databricks");

    // One source isn't a table the probe can read: a warning naming it.
    write(serde_json::json!({"raw.orders": table("t1")}));
    let (code, json) = project.json(&["--connect", "--provider", "databricks"]);
    assert_eq!(code, 0, "{json:#}");
    let versions = check(&json, "connectivity.table_versions");
    assert_eq!(versions["code"], "ODS-W0606", "{versions:#}");
    // None is: the probe ran, but concluded nothing.
    write(serde_json::json!({}));
    let (_, json) = project.json(&["--connect", "--provider", "databricks"]);
    let versions = check(&json, "connectivity.table_versions");
    assert_eq!(versions["status"], "unknown", "{versions:#}");
    assert_eq!(versions["code"], "ODS-U0607");

    let failing = project.with("FAKE_DBT_PROBE_FAIL", "1");
    let (code, json) = failing.json(&["--connect", "--provider", "databricks"]);
    assert_eq!(code, 5);
    assert_eq!(
        check(&json, "connectivity.table_versions")["code"],
        "ODS-E0604"
    );
}

#[test]
fn an_unknown_provider_is_a_usage_error() {
    let project = Project::new();
    let (code, _, stderr) = project.doctor(&["--provider", "nope"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("databricks"), "{stderr}");
}
