//! Probe checks and the trust store end to end (#392, ADR-0030 §4a–§4d): a project's
//! probe never runs until its exact definition is trusted, a probe that could write is
//! refused before anything connects, and trust lives in the user's own configuration
//! directory, never in the repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

const PROBE: &str = r#"[[health.checks]]
id = "orders.has_rows"
kind = "probe"
select = { name = ["orders"] }
sql = "select count(*) as n from {relation}"
pass = "n > 0"
severity = "error"
"#;

/// A project directory with `ods_toml` as its `ods.toml`, and a separate home whose
/// configuration directory holds the trust store.
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

    /// `ods` with `args`, `--json`: exit code and envelope.
    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .arg("--json")
            .current_dir(self.dir.path())
            .env_clear()
            .env("XDG_CONFIG_HOME", self.home.path())
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
        let target = fixture();
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

/// The probe's finding on `orders`.
fn probe(envelope: &Value) -> Value {
    envelope["result"]["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .iter()
        .find(|n| n["id"] == "model.jaffle_ods.orders")
        .unwrap()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "orders.has_rows")
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .clone()
}

#[test]
fn a_projects_probe_runs_only_once_trusted_and_a_change_untrusts_it() {
    let project = Project::new(PROBE);

    // Untrusted: unknown, saying how to trust it; never a pass, and no store written.
    let (_, envelope) = project.check(&[]);
    let finding = probe(&envelope);
    assert_eq!(finding["status"], "unknown", "{finding:#}");
    assert_eq!(finding["source"], "probe");
    assert!(
        finding["reason"]
            .as_str()
            .unwrap()
            .contains("ods health trust"),
        "{finding:#}"
    );
    assert!(!project.trust_store().exists());

    // `ods health trust` shows the SQL and records it, in the user's directory only.
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    let result = &envelope["result"];
    assert_eq!(result["outcome"], "trusted");
    assert_eq!(result["probes"][0]["standing"], "new");
    assert_eq!(
        result["probes"][0]["sql"],
        "select count(*) as n from {relation}"
    );
    let store: Value =
        serde_json::from_slice(&std::fs::read(project.trust_store()).unwrap()).unwrap();
    assert_eq!(store["schema_version"]["major"], 1);
    let root = result["project"].as_str().unwrap();
    assert!(
        store["projects"][root]["checks"]["orders.has_rows"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        std::fs::read_dir(project.dir.path())
            .unwrap()
            .all(|e| e.unwrap().file_name() == "ods.toml"),
        "nothing is written in the repository"
    );

    // Trusted: past that guard (probes themselves don't run yet).
    let (_, envelope) = project.check(&[]);
    let reason = probe(&envelope)["reason"].as_str().unwrap().to_owned();
    assert!(!reason.contains("not trusted"), "{reason}");

    // A changed query is untrusted again, and `ods health trust` says what changed.
    project.write(&PROBE.replace("count(*)", "count(1)"));
    let (_, envelope) = project.check(&[]);
    assert!(
        probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
    let (_, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(envelope["result"]["probes"][0]["standing"], "changed");

    // `--revoke` forgets the project.
    let (code, envelope) = project.ods(&["health", "trust", "--revoke"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(envelope["result"]["outcome"], "revoked");
    let (_, envelope) = project.check(&[]);
    assert!(
        probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
}

#[test]
fn allow_scripts_trusts_for_one_run_and_never_keeps_it() {
    let project = Project::new(PROBE);
    let (_, envelope) = project.check(&["--allow-scripts"]);
    let reason = probe(&envelope)["reason"].as_str().unwrap().to_owned();
    assert!(!reason.contains("not trusted"), "{reason}");
    assert!(!project.trust_store().exists(), "never persisted");
    let (_, envelope) = project.check(&[]);
    assert!(
        probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
}

#[test]
fn probes_in_the_users_own_configuration_need_no_trust() {
    let project = Project::new("");
    std::fs::create_dir_all(project.home.path().join("ods")).unwrap();
    std::fs::write(project.home.path().join("ods/config.toml"), PROBE).unwrap();
    let (_, envelope) = project.check(&[]);
    let reason = probe(&envelope)["reason"].as_str().unwrap().to_owned();
    assert!(!reason.contains("not trusted"), "{reason}");
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(envelope["result"]["outcome"], "not_needed");
    assert!(!project.trust_store().exists());
}

#[test]
fn a_probe_that_could_write_is_refused_before_anything_connects() {
    let project = Project::new(&PROBE.replace(
        "select count(*) as n from {relation}",
        "delete from {relation} where n > 0",
    ));
    let (code, envelope) = project.check(&[]);
    assert_eq!(code, 4, "{envelope:#}");
    let message = envelope["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("probe `orders.has_rows` must be one read-only query"),
        "{message}"
    );
    assert!(message.contains("DELETE"), "{message}");
    assert_eq!(envelope["diagnostics"][0]["code"], "ODS-E0102");

    // `ods serve` refuses it too, as it starts.
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["serve", "--target-dir"])
        .arg(fixture())
        .args(["--port", "0", "--json", "--no-watch"])
        .current_dir(project.dir.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", project.home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn an_unreadable_trust_store_trusts_nothing_and_is_never_overwritten() {
    let project = Project::new(PROBE);
    std::fs::create_dir_all(project.home.path().join("ods")).unwrap();
    std::fs::write(project.trust_store(), "{ not json").unwrap();
    let (_, envelope) = project.check(&[]);
    assert!(
        probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 1, "{envelope:#}");
    assert_eq!(envelope["diagnostics"][0]["code"], "ODS-E0703");
    assert_eq!(
        std::fs::read_to_string(project.trust_store()).unwrap(),
        "{ not json",
        "left as it was"
    );
}
