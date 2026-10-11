//! Script checks end to end (#392, ADR-0030 §4, §4b): a project's script never runs
//! until its command is trusted, runs in the dbt project's directory with the artifacts
//! as absolute paths, and a script that fails makes its nodes *unknown*, never a pass.
//!
//! The script is this test binary itself: started with `--ods-script-fixture <mode>`,
//! it answers as `mode` says, so the tests need neither a shell nor an interpreter.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use ods_sdk::contracts::health_check::{CheckFinding, Status};
use ods_sdk::protocol::{CheckRequest, CheckResponse};
use serde_json::Value;

const FIXTURE: &str = "--ods-script-fixture";

/// Plays a script, as `mode` says. Every run leaves a `ran` file in its directory.
fn fixture(mode: &str) {
    std::fs::write("ran", mode).unwrap();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    match mode {
        "orders-fail" => {
            let request: CheckRequest = serde_json::from_slice(&input).unwrap();
            let findings: Vec<CheckFinding> = request
                .scope
                .nodes
                .iter()
                .map(|n| {
                    let status = if n.name == "orders" {
                        Status::Fail
                    } else {
                        Status::Pass
                    };
                    CheckFinding::new(n.id.clone(), status, "looked at it")
                        .with_evidence(
                            "manifest",
                            request
                                .artifacts
                                .get("manifest")
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .with_evidence(
                            "params",
                            serde_json::to_string(&request.check.params).unwrap(),
                        )
                })
                .collect();
            let json = serde_json::to_vec(&CheckResponse::new(findings)).unwrap();
            std::io::stdout().write_all(&json).unwrap();
        }
        "exit" => {
            eprintln!("boom: no catalog.json");
            std::process::exit(2);
        }
        other => panic!("no fixture mode `{other}`"),
    }
}

fn artifacts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

/// A `[[health.checks]]` script that runs this binary as `mode`.
fn script(mode: &str, extra: &str) -> String {
    let me = std::env::current_exe().unwrap().display().to_string();
    // Literal strings: a Windows path's backslashes aren't escapes.
    format!(
        "[[health.checks]]\nid = \"acme.orders_ok\"\nkind = \"script\"\ncommand = ['{me}', '{FIXTURE}', '{mode}']\nseverity = \"error\"\n{extra}"
    )
}

/// A project directory, and a separate home whose configuration directory holds the
/// trust store.
struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn new(ods_toml: &str) -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        project.write(ods_toml);
        project
    }

    fn write(&self, ods_toml: &str) {
        std::fs::write(self.dir.path().join("ods.toml"), ods_toml).unwrap();
    }

    fn trust_store(&self) -> PathBuf {
        self.home.path().join("ods/trust.json")
    }

    /// Whether the script ran, and forgets that it did.
    fn ran(&self) -> bool {
        std::fs::remove_file(self.dir.path().join("ran")).is_ok()
    }

    /// `ods` with `args`, `--json`: exit code and envelope.
    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .arg("--json")
            .current_dir(self.dir.path())
            .env_clear()
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("APPDATA", self.home.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap(), json)
    }

    fn check(&self, extra: &[&str]) -> (i32, Value) {
        let target = artifacts();
        let mut args = vec![
            "health",
            "check",
            "--no-record",
            "--target-dir",
            target.to_str().unwrap(),
        ];
        args.extend(extra);
        self.ods(&args)
    }
}

/// The script's finding on the node called `name`.
fn finding(envelope: &Value, name: &str) -> Value {
    let id = format!("model.jaffle_ods.{name}");
    envelope["result"]["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .iter()
        .find(|n| n["id"] == id.as_str())
        .unwrap_or_else(|| panic!("no {id}: {envelope:#}"))["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "acme.orders_ok")
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .clone()
}

fn reason(finding: &Value) -> &str {
    finding["reason"].as_str().unwrap()
}

fn a_projects_script_runs_only_once_trusted_and_a_new_command_untrusts_it() {
    let project = Project::new(&script("orders-fail", "params = { min_rows = 10 }\n"));

    // Untrusted: never started; unknown, saying how to trust it.
    let (_, envelope) = project.check(&[]);
    let orders = finding(&envelope, "orders");
    assert_eq!(orders["status"], "unknown", "{orders:#}");
    assert_eq!(orders["source"], "script");
    assert!(reason(&orders).contains("ods health trust"), "{orders:#}");
    assert!(!project.ran(), "an untrusted script never starts");
    assert!(!project.trust_store().exists());

    // `ods health trust` shows the command and records it.
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    let scripts = &envelope["result"]["scripts"];
    assert_eq!(scripts[0]["id"], "acme.orders_ok", "{envelope:#}");
    assert_eq!(scripts[0]["standing"], "new");
    assert_eq!(scripts[0]["command"][1], FIXTURE);
    assert!(project.trust_store().exists());

    // Trusted: it runs in the project's directory, and its findings are the check's.
    let (code, envelope) = project.check(&[]);
    assert!(project.ran(), "{envelope:#}");
    // Exit 5: a node fails a check at its severity (ADR-0004).
    assert_eq!(
        code, 5,
        "an error-severity fail fails the check: {envelope:#}"
    );
    let orders = finding(&envelope, "orders");
    assert_eq!(orders["status"], "fail", "{orders:#}");
    assert_eq!(orders["severity"], "error");
    assert_eq!(finding(&envelope, "customers")["status"], "pass");
    let manifest = orders["evidence"]["manifest"].as_str().unwrap();
    assert!(
        Path::new(manifest).is_absolute() && manifest.ends_with("manifest.json"),
        "{manifest}"
    );
    assert_eq!(orders["evidence"]["params"], r#"{"min_rows":10}"#);

    // Its severity, timeout and parameters aren't trusted: changing them needs no trust.
    project.write(&script(
        "orders-fail",
        "timeout = \"2m\"\nparams = { min_rows = 20 }\n",
    ));
    let (_, envelope) = project.check(&[]);
    assert!(project.ran(), "{envelope:#}");

    // A new command is untrusted again, and `ods health trust` says it changed.
    project.write(&script("exit", ""));
    let (_, envelope) = project.check(&[]);
    assert!(!project.ran());
    assert!(reason(&finding(&envelope, "orders")).contains("not trusted"));
    let (_, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(envelope["result"]["scripts"][0]["standing"], "changed");
}

fn a_script_that_fails_makes_its_nodes_unknown_saying_why() {
    let project = Project::new(&script("exit", ""));
    let (_, envelope) = project.check(&["--allow-scripts"]);
    assert!(project.ran());
    for name in ["orders", "customers"] {
        let finding = finding(&envelope, name);
        assert_eq!(finding["status"], "unknown", "{finding:#}");
        assert!(
            reason(&finding).contains("boom: no catalog.json"),
            "{finding:#}"
        );
    }
}

fn allow_scripts_runs_it_once_and_never_keeps_it() {
    let project = Project::new(&script("orders-fail", ""));
    let (_, envelope) = project.check(&["--allow-scripts"]);
    assert!(project.ran(), "{envelope:#}");
    assert_eq!(finding(&envelope, "orders")["status"], "fail");
    assert!(!project.trust_store().exists(), "never persisted");
    project.check(&[]);
    assert!(!project.ran());
}

fn scripts_in_the_users_own_configuration_need_no_trust() {
    let project = Project::new("");
    std::fs::create_dir_all(project.home.path().join("ods")).unwrap();
    std::fs::write(
        project.home.path().join("ods/config.toml"),
        script("orders-fail", ""),
    )
    .unwrap();
    let (_, envelope) = project.check(&[]);
    assert!(project.ran(), "{envelope:#}");
    assert_eq!(finding(&envelope, "orders")["status"], "fail");
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(envelope["result"]["outcome"], "not_needed");
}

fn a_script_without_a_command_is_a_configuration_error() {
    let project = Project::new(
        "[[health.checks]]\nid = \"acme.orders_ok\"\nkind = \"script\"\nseverity = \"error\"\n",
    );
    let (code, envelope) = project.check(&[]);
    assert_ne!(code, 0);
    let message = envelope.to_string();
    assert!(message.contains(".command"), "{envelope:#}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(FIXTURE) {
        fixture(&args[2]);
        return;
    }
    // The test harness's own flags (e.g. `--quiet`, a name filter) are ignored: every
    // test runs.
    macro_rules! tests {
        ($($name:ident),* $(,)?) => {
            $(
                $name();
                println!("test {} ... ok", stringify!($name));
            )*
        };
    }
    tests!(
        a_projects_script_runs_only_once_trusted_and_a_new_command_untrusts_it,
        a_script_that_fails_makes_its_nodes_unknown_saying_why,
        allow_scripts_runs_it_once_and_never_keeps_it,
        scripts_in_the_users_own_configuration_need_no_trust,
        a_script_without_a_command_is_a_configuration_error,
    );
}
