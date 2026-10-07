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
        self.ods_with(args, &[])
    }

    /// `ods` with `args`, `--json` and the environment variables `env`.
    fn ods_with(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(args)
            .arg("--json")
            .current_dir(self.dir.path())
            .env_clear()
            .env("XDG_CONFIG_HOME", self.home.path())
            // The fake dbt is `#!/usr/bin/env python3`.
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(env.iter().copied())
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

    // Trusted: past that guard (with no `[health.probes]` target, nothing to run on).
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
fn a_probe_with_queries_per_warehouse_runs_the_projects_and_checks_every_one() {
    // The fixture project is on DuckDB; this probe has a query for Databricks only.
    let databricks_only = PROBE.replace(
        r#"sql = "select count(*) as n from {relation}""#,
        r#"sql = { databricks = "select count_if(id is not null) as n from {relation}" }"#,
    );
    let project = Project::new(&databricks_only);
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    let reviewed = &envelope["result"]["probes"][0];
    assert_eq!(
        reviewed["per_warehouse"]["databricks"],
        "select count_if(id is not null) as n from {relation}",
        "{reviewed:#}"
    );
    let (_, envelope) = project.check(&[]);
    let finding = probe(&envelope);
    assert_eq!(finding["status"], "unknown", "{finding:#}");
    assert!(
        finding["reason"]
            .as_str()
            .unwrap()
            .contains("no query for this warehouse"),
        "{finding:#}"
    );

    // A query for another warehouse that could write is refused all the same.
    project.write(&PROBE.replace(
        r#"sql = "select count(*) as n from {relation}""#,
        r#"sql = { databricks = "delete from {relation} where n > 0", default = "select count(*) as n from {relation}" }"#,
    ));
    let (code, envelope) = project.check(&[]);
    assert_eq!(code, 4, "{envelope:#}");
    let message = envelope["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains(
            "health.checks[0].sql.databricks: probe `orders.has_rows` must be one read-only query"
        ),
        "{message}"
    );
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

// ------------------------------------------- running probes through dbt (ADR-0030 §4c)

/// A project whose probe runs through the fake dbt on `[health.probes]`'s target, with
/// its own copy of the artifacts (the probe writes beside them), and the fake warehouse:
/// `orders` has `rows` rows.
#[cfg(unix)]
struct Probing {
    project: Project,
    target: PathBuf,
    warehouse: PathBuf,
    probed: PathBuf,
    seen: PathBuf,
}

#[cfg(unix)]
impl Probing {
    fn new(health_probes: &str, rows: &str) -> Self {
        let project = Project::new(&format!("{PROBE}{health_probes}"));
        let target = project.dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        for file in ["manifest.json", "catalog.json", "run_results.json"] {
            std::fs::copy(fixture().join(file), target.join(file)).unwrap();
        }
        let warehouse = project.home.path().join("warehouse.json");
        let doc = serde_json::json!({
            "orders": {"type": "table", "rows": {"select count(*) as n from {relation}": {"n": rows}}},
        });
        std::fs::write(&warehouse, doc.to_string()).unwrap();
        Self {
            probed: project.home.path().join("probed"),
            seen: project.home.path().join("seen"),
            project,
            target,
            warehouse,
        }
    }

    fn check(&self, extra: &[&str]) -> (i32, Value) {
        let dbt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/fake-dbt/dbt");
        let mut args = vec![
            "health",
            "check",
            "--no-record",
            "--allow-scripts",
            "--dbt-output",
            "capture",
            "--target-dir",
            self.target.to_str().unwrap(),
            "--dbt",
            dbt.to_str().unwrap(),
        ];
        args.extend(extra);
        self.project.ods_with(
            &args,
            &[
                ("FAKE_DBT_BASE", fixture().to_str().unwrap()),
                ("FAKE_DBT_PROBE", self.warehouse.to_str().unwrap()),
                ("FAKE_DBT_PROBED", self.probed.to_str().unwrap()),
                ("FAKE_DBT_SEEN", self.seen.to_str().unwrap()),
            ],
        )
    }
}

const READ_ONLY_TARGET: &str = "\n[health.probes]\ntarget = \"health_ro\"\n";

#[cfg(unix)]
#[test]
fn without_a_probe_target_probes_never_run_and_say_how_to_set_one() {
    let probing = Probing::new("", "5");
    let (_, envelope) = probing.check(&["--allow-elevated-login"]);
    let finding = probe(&envelope);
    assert_eq!(finding["status"], "unknown", "{finding:#}");
    assert!(
        finding["reason"]
            .as_str()
            .unwrap()
            .contains("[health.probes] target"),
        "{finding:#}"
    );
    assert!(
        !probing.seen.exists(),
        "dbt never ran: no build target is borrowed"
    );
}

#[cfg(unix)]
#[test]
fn a_login_that_cant_be_shown_to_only_read_is_refused_and_nothing_runs() {
    let probing = Probing::new(READ_ONLY_TARGET, "5");
    let (_, envelope) = probing.check(&[]);
    let finding = probe(&envelope);
    assert_eq!(finding["status"], "unknown", "{finding:#}");
    let reason = finding["reason"].as_str().unwrap();
    for part in [
        "refused",
        "can't report what its login may do",
        "--allow-elevated-login",
    ] {
        assert!(reason.contains(part), "{part}: {reason}");
    }
    assert_eq!(finding["evidence"]["login_check"], "refused");
    assert!(!probing.seen.exists(), "dbt never ran");
    assert!(envelope["result"].get("elevated_login").is_none());
}

#[cfg(unix)]
#[test]
fn allow_elevated_login_runs_the_probe_on_its_own_target_and_says_so() {
    let probing = Probing::new(READ_ONLY_TARGET, "5");
    let (code, envelope) = probing.check(&["--allow-elevated-login"]);
    assert_eq!(code, 0, "{envelope:#}");
    let finding = probe(&envelope);
    assert_eq!(finding["status"], "pass", "{finding:#}");
    assert_eq!(finding["evidence"]["login_check"], "overridden");
    assert_eq!(finding["evidence"]["row.n"], "5");
    let result = &envelope["result"];
    assert!(
        result["elevated_login"]["found"]["model.jaffle_ods.orders"]
            .as_str()
            .unwrap()
            .contains("couldn't be shown to only read")
    );
    assert_eq!(
        std::fs::read_to_string(&probing.probed).unwrap(),
        "model.jaffle_ods.orders\n",
        "only the node the probe selects"
    );
    let warning = &envelope["diagnostics"][0];
    assert_eq!(warning["level"], "warning", "{envelope:#}");
    assert_eq!(warning["code"], "ODS-W0704");
    for part in ["own risk", "no warranty", "dbt target `health_ro`"] {
        assert!(
            warning["message"].as_str().unwrap().contains(part),
            "{part}: {warning:#}"
        );
    }
    assert_eq!(
        finding["evidence"]["connection"], "dbt target `health_ro`",
        "{finding:#}"
    );
    let seen = std::fs::read_to_string(&probing.seen).unwrap();
    assert!(
        seen.contains(r#""--target", "health_ro""#),
        "the probe target, not the build's: {seen}"
    );

    // A failing row fails the gate at severity error.
    let probing = Probing::new(READ_ONLY_TARGET, "0");
    let (code, envelope) = probing.check(&["--allow-elevated-login"]);
    assert_eq!(code, 5, "{envelope:#}");
    assert_eq!(probe(&envelope)["status"], "fail");
}

#[cfg(unix)]
#[test]
fn the_override_warns_in_text_too() {
    let probing = Probing::new(READ_ONLY_TARGET, "5");
    let dbt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/fake-dbt/dbt");
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["health", "check", "--no-record", "--allow-scripts"])
        .args(["--allow-elevated-login", "--dbt-output", "capture"])
        .arg("--target-dir")
        .arg(&probing.target)
        .arg("--dbt")
        .arg(&dbt)
        .args(["--output", "plain"])
        .current_dir(probing.project.dir.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", probing.project.home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", fixture())
        .env("FAKE_DBT_PROBE", &probing.warehouse)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: --allow-elevated-login"),
        "warned before anything ran: {stderr}"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    for part in [
        "--allow-elevated-login",
        "own risk",
        "no warranty",
        "changed or destroyed data",
        "`model.jaffle_ods.orders`",
    ] {
        assert!(text.contains(part), "{part}: {text}");
    }
}

#[cfg(unix)]
#[test]
fn probes_never_run_on_the_build_target() {
    let probing = Probing::new(READ_ONLY_TARGET, "5");
    let (code, envelope) = probing.check(&["--allow-elevated-login", "--target", "health_ro"]);
    assert_eq!(code, 4, "{envelope:#}");
    let message = envelope["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("the target the project builds with"),
        "{message}"
    );
    assert!(!probing.seen.exists(), "dbt never ran");
}

#[cfg(unix)]
#[test]
fn a_node_resolved_elsewhere_on_the_probe_target_is_unknown() {
    let probing = Probing::new(READ_ONLY_TARGET, "5");
    let doc = serde_json::json!({
        "orders": {
            "type": "table",
            "relation": "\"jaffle_ods\".\"readonly\".\"orders\"",
            "rows": {"select count(*) as n from {relation}": {"n": "5"}},
        },
    });
    std::fs::write(&probing.warehouse, doc.to_string()).unwrap();
    let (_, envelope) = probing.check(&["--allow-elevated-login"]);
    let finding = probe(&envelope);
    assert_eq!(
        finding["status"], "unknown",
        "never other data: {finding:#}"
    );
    assert!(
        finding["reason"]
            .as_str()
            .unwrap()
            .contains("\"jaffle_ods\".\"readonly\".\"orders\""),
        "{finding:#}"
    );
    assert!(!probing.probed.exists(), "nothing ran against it");
}

#[test]
fn a_project_that_points_the_users_probes_at_a_target_needs_trust() {
    let project = Project::new(READ_ONLY_TARGET);
    std::fs::create_dir_all(project.home.path().join("ods")).unwrap();
    std::fs::write(project.home.path().join("ods/config.toml"), PROBE).unwrap();
    let (_, envelope) = project.check(&[]);
    let reason = probe(&envelope)["reason"].as_str().unwrap().to_owned();
    assert!(reason.contains("not trusted"), "{reason}");
}

#[test]
fn a_probe_that_selects_nothing_says_so_and_fails_strict() {
    let project = Project::new(&PROBE.replace("[\"orders\"]", "[\"no_such_model\"]"));
    let (code, envelope) = project.check(&["--allow-scripts"]);
    assert_eq!(code, 0, "{envelope:#}");
    assert_eq!(envelope["result"]["unmatched_probes"][0], "orders.has_rows");
    let warning = &envelope["diagnostics"][0];
    assert_eq!(warning["code"], "ODS-W0705", "{envelope:#}");
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("checked nothing"),
        "{warning:#}"
    );
    let (code, _) = project.check(&["--allow-scripts", "--strict"]);
    assert_eq!(
        code, 5,
        "an error-severity probe that checked nothing fails --strict"
    );
}

#[test]
fn changing_the_dbt_profile_a_project_configures_needs_trust_again() {
    let dbt =
        "\n[providers.dbt]\nkind = \"dbt\"\n\n[providers.dbt.settings]\nprofile = \"jaffle\"\n";
    let project = Project::new(&format!("{PROBE}{dbt}"));
    let (code, envelope) = project.ods(&["health", "trust"]);
    assert_eq!(code, 0, "{envelope:#}");
    let (_, envelope) = project.check(&[]);
    assert!(
        !probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
    project.write(&format!("{PROBE}{}", dbt.replace("jaffle", "elsewhere")));
    let (_, envelope) = project.check(&[]);
    assert!(
        probe(&envelope)["reason"]
            .as_str()
            .unwrap()
            .contains("not trusted"),
        "{envelope:#}"
    );
}
