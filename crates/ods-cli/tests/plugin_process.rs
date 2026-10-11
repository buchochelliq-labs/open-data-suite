//! Out-of-process plugins end to end (#392, ADR-0031 §5): a project's plugin never runs
//! until trusted; once it runs, its checks are `ods health check`'s and its warehouse is
//! listed; one that can't describe itself fails the command rather than drop its checks;
//! and `ods plugin test` runs the SDK's suites against it.
//!
//! The plugin is this test binary itself: started with `--ods-plugin-fixture <mode>`,
//! it answers as `mode` says, so the tests need neither a shell nor an interpreter.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use ods_core::SchemaVersion;
use ods_core::state::{DataVersion, Exactness};
use ods_sdk::contracts::changes::{CHANGE_PROVIDER, SourceVersion};
use ods_sdk::contracts::health_check::{CheckFinding, CheckInfo, HEALTH_CHECK, Severity, Status};
use ods_sdk::protocol::{
    CheckRequest, CheckResponse, DescribeResponse, Provided, SourceAnswer, VersionsRequest,
    VersionsResponse,
};
use serde_json::Value;

const FIXTURE: &str = "--ods-plugin-fixture";

/// Plays a plugin, as `mode` says. Every call appends its request to `ran` in the
/// directory it runs in.
fn fixture(mode: &str) {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let request: Value = serde_json::from_slice(&input).unwrap();
    let asked = request["request"].as_str().unwrap().to_owned();
    let mut ran = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("ran")
        .unwrap();
    writeln!(ran, "{asked}").unwrap();
    let out = |json: Vec<u8>| std::io::stdout().write_all(&json).unwrap();
    match asked.as_str() {
        "describe" => {
            let mut answer = DescribeResponse::new("acme", "1.2.0");
            let health = if mode == "old" {
                SchemaVersion::new(0, 0)
            } else {
                HEALTH_CHECK.version
            };
            answer
                .provides
                .push(Provided::new(HEALTH_CHECK.name, health));
            answer
                .provides
                .push(Provided::new(CHANGE_PROVIDER.name, CHANGE_PROVIDER.version));
            answer.checks.push(CheckInfo::new(
                "acme.tagged",
                "every model is tagged",
                Severity::Warn,
            ));
            answer.warehouse = Some("acme_db".to_owned());
            answer.versions_read = Some("Acme table versions".to_owned());
            answer.capabilities = vec!["relation_versions".to_owned()];
            out(serde_json::to_vec(&answer).unwrap());
        }
        "check" => {
            let request: CheckRequest = serde_json::from_value(request).unwrap();
            let mut findings: Vec<CheckFinding> = request
                .scope
                .nodes
                .iter()
                .map(|n| {
                    if n.name == "orders" {
                        CheckFinding::new(n.id.clone(), Status::Fail, "untagged")
                    } else {
                        CheckFinding::new(n.id.clone(), Status::Pass, "tagged")
                    }
                    .with_evidence(
                        "manifest",
                        request
                            .artifacts
                            .get("manifest")
                            .cloned()
                            .unwrap_or_default(),
                    )
                })
                .collect();
            if mode == "outside" {
                findings.push(CheckFinding::new(
                    "model.elsewhere.x",
                    Status::Pass,
                    "tagged",
                ));
            }
            out(serde_json::to_vec(&CheckResponse::new(findings)).unwrap());
        }
        "versions" => {
            let request: VersionsRequest = serde_json::from_value(request).unwrap();
            // It reads nothing without its warehouse's account, as a real one would.
            let account = request.settings.values().any(|s| s["account"] == "acme");
            let sources = request
                .sources
                .iter()
                .map(|s| {
                    let version = if !account {
                        SourceVersion::Unknown("no account configured".to_owned())
                    } else if s.name.starts_with("none.") || s.name.starts_with("dark.") {
                        SourceVersion::Unknown("no such table".to_owned())
                    } else {
                        SourceVersion::Version(DataVersion::new(
                            s.name.clone(),
                            Exactness::Exact,
                            "acme",
                        ))
                    };
                    SourceAnswer::new(s.id.clone(), version)
                })
                .collect();
            out(serde_json::to_vec(&VersionsResponse::new(sources)).unwrap());
        }
        other => panic!("no request `{other}`"),
    }
}

fn artifacts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

/// A `[plugins.acme]` that runs this binary as `mode`.
fn plugin(mode: &str) -> String {
    let me = std::env::current_exe().unwrap().display().to_string();
    // Literal strings: a Windows path's backslashes aren't escapes.
    format!("[plugins.acme]\ncommand = ['{me}', '{FIXTURE}', '{mode}']\n")
}

/// A project directory, and a separate home whose configuration directory holds the
/// user's `config.toml` and the trust store.
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

    fn write_user(&self, config_toml: &str) {
        let dir = self.home.path().join("ods");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), config_toml).unwrap();
    }

    /// The requests the plugin was sent, and forgets them.
    fn ran(&self) -> Vec<String> {
        let path = self.dir.path().join("ran");
        let ran = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        ran.lines().map(str::to_owned).collect()
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

/// The plugin check's finding on the node called `name`.
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
        .find(|f| f["check"] == "acme.tagged")
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .clone()
}

fn diagnostic(envelope: &Value) -> &Value {
    &envelope["diagnostics"][0]
}

fn a_projects_plugin_runs_only_once_trusted_and_a_new_command_untrusts_it() {
    let project = Project::new(&plugin("good"));

    // Untrusted: never started, and the command fails rather than drop its checks.
    let (code, envelope) = project.check(&[]);
    assert_eq!(code, 1, "{envelope:#}");
    assert_eq!(diagnostic(&envelope)["code"], "ODS-E0802", "{envelope:#}");
    assert!(
        diagnostic(&envelope)["hint"]
            .as_str()
            .unwrap()
            .contains("ods health trust"),
        "{envelope:#}"
    );
    assert_eq!(project.ran(), Vec::<String>::new());

    // `ods plugin list` names it, and why it doesn't run, without running it.
    let (code, envelope) = project.ods(&["plugin", "list"]);
    assert_eq!(code, 0, "{envelope:#}");
    let declared = &envelope["result"]["declared"][0];
    assert_eq!(declared["name"], "acme", "{envelope:#}");
    assert!(
        declared["held"].as_str().unwrap().contains("isn't trusted"),
        "{envelope:#}"
    );
    assert_eq!(project.ran(), Vec::<String>::new());

    // `ods health trust` shows its command and records it.
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    let plugins = &envelope["result"]["plugins"];
    assert_eq!(plugins[0]["name"], "acme", "{envelope:#}");
    assert_eq!(plugins[0]["standing"], "new");
    assert_eq!(plugins[0]["command"][1], FIXTURE);
    assert_eq!(envelope["result"]["outcome"], "trusted");

    // Trusted: it describes itself, then answers its check, with the artifacts.
    let (code, envelope) = project.check(&[]);
    assert_eq!(project.ran(), ["describe", "check"], "{envelope:#}");
    // At its own default severity, warn: a failure doesn't fail the gate.
    assert_eq!(code, 0, "{envelope:#}");
    let orders = finding(&envelope, "orders");
    assert_eq!(orders["status"], "fail", "{orders:#}");
    assert_eq!(orders["severity"], "warn");
    assert_eq!(orders["source"], "plugin");
    assert!(
        orders["evidence"]["manifest"]
            .as_str()
            .is_some_and(|m| Path::new(m).is_absolute()),
        "{orders:#}"
    );
    assert_eq!(finding(&envelope, "customers")["status"], "pass");

    // Its timeout isn't trusted: changing it needs no trust.
    project.write(&format!("{}timeout = \"2m\"\n", plugin("good")));
    let (code, envelope) = project.check(&[]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(project.ran(), ["describe", "check"]);

    // A new command is untrusted again, and `ods health trust` says it changed.
    project.write(&plugin("outside"));
    let (code, _) = project.check(&[]);
    assert_eq!(code, 1);
    assert_eq!(project.ran(), Vec::<String>::new());
    let (_, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(envelope["result"]["plugins"][0]["standing"], "changed");
}

fn its_checks_are_configured_like_any_plugin_check() {
    let project = Project::new(&format!(
        "{}\n[health.plugins.\"acme.tagged\"]\nseverity = \"error\"\n",
        plugin("good")
    ));
    let (code, envelope) = project.check(&["--allow-scripts"]);
    assert_eq!(
        code, 5,
        "an error-severity fail fails the gate: {envelope:#}"
    );
    assert_eq!(finding(&envelope, "orders")["severity"], "error");
    // `--allow-scripts` trusts it for this run only.
    let (code, _) = project.check(&[]);
    assert_eq!(code, 1);

    // A misspelt id names no check, even one only a plugin describes.
    project.write(&format!(
        "{}\n[health.plugins.\"acme.taged\"]\nseverity = \"off\"\n",
        plugin("good")
    ));
    let (code, envelope) = project.check(&["--allow-scripts"]);
    assert_eq!(code, 4, "{envelope:#}");
    assert!(
        diagnostic(&envelope)["message"]
            .as_str()
            .unwrap()
            .contains("acme.tagged"),
        "{envelope:#}"
    );
}

fn the_users_own_plugin_needs_no_trust() {
    let project = Project::new("");
    project.write_user(&plugin("good"));
    let (code, envelope) = project.check(&[]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(project.ran(), ["describe", "check"]);
    assert_eq!(finding(&envelope, "orders")["status"], "fail");
    // Nothing to trust, and nothing shown.
    let (_, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(envelope["result"]["outcome"], "nothing", "{envelope:#}");
}

fn a_plugin_built_against_another_contract_is_refused_naming_both() {
    let project = Project::new(&plugin("old"));
    let (code, envelope) = project.check(&["--allow-scripts"]);
    assert_eq!(code, 1, "{envelope:#}");
    let message = diagnostic(&envelope)["message"].as_str().unwrap();
    assert!(
        message.contains("was built against health_check 0.0; this ods has"),
        "{message}"
    );
    // It was asked only to describe itself.
    assert_eq!(project.ran(), ["describe"]);
}

fn its_checks_and_warehouse_are_listed_as_it_describes_them() {
    let project = Project::new("");
    project.write_user(&plugin("good"));
    let (code, envelope) = project.ods(&["plugin", "list"]);
    assert_eq!(code, 0, "{envelope:#}");
    let plugins = envelope["result"]["plugins"].as_array().unwrap();
    let check = plugins
        .iter()
        .find(|p| p["name"] == "acme.tagged")
        .unwrap_or_else(|| panic!("{envelope:#}"));
    assert_eq!(check["kind"], "health_check");
    assert_eq!(check["from"], "acme (plugins.acme) 1.2.0");
    assert_eq!(check["builtin"], false);
    let warehouse = plugins
        .iter()
        .find(|p| p["name"] == "acme_db")
        .unwrap_or_else(|| panic!("{envelope:#}"));
    assert_eq!(warehouse["kind"], "warehouse");
    assert_eq!(warehouse["features"][0]["name"], "source_versions");
    assert_eq!(warehouse["features"][0]["detail"], "Acme table versions");
    let declared = &envelope["result"]["declared"][0];
    assert!(declared.get("held").is_none(), "{envelope:#}");
    let provides: Vec<&str> = declared["provides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert!(provides[0].starts_with("health_check "), "{provides:?}");
    assert!(provides[1].starts_with("change_provider "), "{provides:?}");
}

fn plugin_test_runs_the_sdks_suites_against_it() {
    // Its warehouse's settings reach it, as in a run.
    let project = Project::new(&format!(
        "{}\n[providers.warehouse]\nkind = \"acme_db\"\nsettings = {{ account = \"acme\" }}\n",
        plugin("good")
    ));
    // Untrusted: not run without --allow-scripts.
    let (code, envelope) = project.ods(&["plugin", "test", "acme"]);
    assert_eq!(code, 1, "{envelope:#}");
    assert_eq!(diagnostic(&envelope)["code"], "ODS-E0802");

    let (code, envelope) = project.ods(&[
        "plugin",
        "test",
        "acme",
        "--allow-scripts",
        "--source",
        "source.shop.raw.orders",
        "--source",
        "source.shop.raw.customers",
        "--unreadable",
        "source.shop.dark.events",
    ]);
    assert_eq!(code, 0, "{envelope:#}");
    let result = &envelope["result"];
    assert_eq!(result["described_as"], "acme 1.2.0");
    let suites = result["suites"].as_array().unwrap();
    assert_eq!(suites[0]["contract"], "health_check");
    assert_eq!(suites[0]["check"], "acme.tagged");
    assert!(
        suites[0]["passed"].as_array().unwrap().len() >= 5,
        "{envelope:#}"
    );
    assert_eq!(suites[1]["contract"], "change_provider");
    assert!(
        suites[1]["passed"]
            .as_array()
            .unwrap()
            .contains(&Value::from("an_unreadable_source_is_unknown")),
        "{envelope:#}"
    );
    // Changing a source's data is beyond a read-only test.
    assert_eq!(
        suites[1]["skipped"][0]["case"],
        "a_commit_moves_only_its_version"
    );

    // Without sources, the versions suite is skipped, saying what to give.
    let (code, envelope) = project.ods(&["plugin", "test", "acme", "--allow-scripts"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert!(
        envelope["result"]["suites"][1]["skipped"][0]["why"]
            .as_str()
            .unwrap()
            .contains("--source"),
        "{envelope:#}"
    );

    // One that answers about a node it wasn't asked about fails, naming the case.
    project.write(&plugin("outside"));
    let (code, envelope) = project.ods(&["plugin", "test", "acme", "--allow-scripts"]);
    assert_eq!(code, 5, "{envelope:#}");
    assert_eq!(diagnostic(&envelope)["code"], "ODS-E0803");
    let failed = envelope["result"]["suites"][0]["failed"].as_str().unwrap();
    assert!(failed.contains("answers_every_node_once"), "{failed}");

    // A name no `[plugins.<name>]` has is a usage error.
    let (code, envelope) = project.ods(&["plugin", "test", "acme2"]);
    assert_eq!(code, 2, "{envelope:#}");
    assert_eq!(diagnostic(&envelope)["code"], "ODS-E0801");
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
        a_projects_plugin_runs_only_once_trusted_and_a_new_command_untrusts_it,
        its_checks_are_configured_like_any_plugin_check,
        the_users_own_plugin_needs_no_trust,
        a_plugin_built_against_another_contract_is_refused_naming_both,
        its_checks_and_warehouse_are_listed_as_it_describes_them,
        plugin_test_runs_the_sdks_suites_against_it,
    );
}
