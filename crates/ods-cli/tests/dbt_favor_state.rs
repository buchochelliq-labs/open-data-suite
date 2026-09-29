//! dbt's own behaviour with `--defer --favor-state` and `dbt retry` (#296, ADR-0020 §7),
//! against real dbt and `DuckDB`. Set `ODS_TEST_DBT` to a dbt executable with
//! `dbt-duckdb` installed; without it the tests skip.
//!
//! These tests run dbt only, not ODS. They pin what dbt does today, so that a dbt
//! release that changes it is noticed, and they settle what ADR-0020 was unsure of:
//! which retry command reads a state directory that points refs at this target.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// A copy of `fixtures/dbt/favor-state`, with prod built and its manifest kept in
/// `prod/`, as a team's deployment would.
struct Deferred {
    dir: PathBuf,
    dbt: String,
    _guard: tempfile::TempDir,
}

impl Deferred {
    fn new(dbt: String) -> Self {
        let guard = tempfile::Builder::new()
            .prefix("ods-favor-state-")
            .tempdir()
            .unwrap();
        let dir = guard.path().to_path_buf();
        copy(&fixture(), &dir);
        let project = Self {
            dir,
            dbt,
            _guard: guard,
        };
        project.ok(&["build", "--target", "prod"], false);
        std::fs::create_dir_all(project.dir.join("prod")).unwrap();
        std::fs::copy(
            project.dir.join("target/manifest.json"),
            project.dir.join("prod/manifest.json"),
        )
        .unwrap();
        project
    }

    /// `dbt <args>` in the project; `b_fails` makes model `b` fail.
    fn dbt(&self, args: &[&str], b_fails: bool) -> std::process::Output {
        Command::new(&self.dbt)
            .args(args)
            .args(["--profiles-dir", "."])
            .current_dir(&self.dir)
            .env("DBT_SEND_ANONYMOUS_USAGE_STATS", "false")
            .env("B_FAILS", if b_fails { "1" } else { "0" })
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str], b_fails: bool) {
        let out = self.dbt(args, b_fails);
        assert!(
            out.status.success(),
            "dbt {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The developer's run: `a` builds in dev, `b` fails.
    fn failing_dev_build(&self) {
        let out = self.dbt(
            &[
                "build",
                "--target",
                "dev",
                "--defer",
                "--favor-state",
                "--state",
                "prod",
            ],
            true,
        );
        assert!(!out.status.success(), "b was meant to fail");
    }

    /// The schema of the `a` that dev's `b` was last compiled against.
    fn b_compiled_against(&self) -> &'static str {
        let sql =
            std::fs::read_to_string(self.dir.join("target/compiled/favor_state/models/b.sql"))
                .unwrap();
        match (sql.contains(r#""prod"."a""#), sql.contains(r#""dev"."a""#)) {
            (true, false) => "prod",
            (false, true) => "dev",
            _ => panic!("can't tell which `a` b reads:\n{sql}"),
        }
    }

    /// The target whose `a` dev's `b` holds data from.
    fn b_read(&self) -> String {
        let out = self.dbt(
            &[
                "show",
                "--inline",
                "select a_built_in from {{ ref('b') }}",
                "--target",
                "dev",
                "--output",
                "json",
                "--quiet",
            ],
            false,
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let json: Value = serde_json::from_slice(&out.stdout).unwrap();
        json["show"][0]["a_built_in"].as_str().unwrap().to_owned()
    }

    /// A state directory like the one `ods state export --dbt-state` will write: prod's
    /// manifest, with `a` pointing at dev's relation. Only `schema` and `relation_name`
    /// differ between the two targets here.
    fn export_pointing_a_at_dev(&self, into: &str) {
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(self.dir.join("prod/manifest.json")).unwrap())
                .unwrap();
        let mut manifest = manifest;
        let a = &mut manifest["nodes"]["model.favor_state.a"];
        assert_eq!(a["schema"], "prod", "{a:#}");
        a["schema"] = Value::from("dev");
        let relation = a["relation_name"]
            .as_str()
            .unwrap()
            .replace(r#""prod""#, r#""dev""#);
        a["relation_name"] = Value::from(relation);
        let dir = self.dir.join(into);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/favor-state")
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

fn real_dbt() -> Option<String> {
    let dbt = std::env::var("ODS_TEST_DBT").ok();
    if dbt.is_none() {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
    }
    dbt
}

/// The problem (#296): `a` built in dev, `b` failed, and the retry builds `b` on
/// **prod's** `a`. dbt retry selects only `b`, so `a` is unselected, and
/// `--favor-state` sends refs to unselected nodes to the state manifest.
#[test]
fn real_dbt_favor_state_retry_reads_prod() {
    let Some(dbt) = real_dbt() else { return };
    let project = Deferred::new(dbt);
    project.failing_dev_build();
    project.ok(&["retry"], false);
    assert_eq!(project.b_compiled_against(), "prod");
    assert_eq!(project.b_read(), "prod");
}

/// The fix ADR-0020 chooses: a state directory in which `a` points at dev. dbt retry
/// honours `--defer-state` over the original run's saved `--state`, and reads its run
/// results from the target path as usual.
#[test]
fn real_dbt_retry_with_defer_state_reads_this_target() {
    let Some(dbt) = real_dbt() else { return };
    let project = Deferred::new(dbt);
    project.failing_dev_build();
    project.export_pointing_a_at_dev("export");
    project.ok(&["retry", "--defer-state", "export"], false);
    assert_eq!(project.b_compiled_against(), "dev");
    assert_eq!(project.b_read(), "dev");

    // A later deferred run keeps prod for `state:modified` selection and defers to the
    // export.
    project.ok(
        &[
            "build",
            "--target",
            "dev",
            "-s",
            "b",
            "--defer",
            "--favor-state",
            "--state",
            "prod",
            "--defer-state",
            "export",
        ],
        false,
    );
    assert_eq!(project.b_compiled_against(), "dev");
}

/// What doesn't work, pinned so the docs never recommend it: `dbt retry --state <dir>`
/// reads the previous run results from `<dir>`, but still defers to the original run's
/// saved `--state`.
#[test]
fn real_dbt_retry_with_state_still_defers_to_the_saved_state() {
    let Some(dbt) = real_dbt() else { return };
    let project = Deferred::new(dbt);
    project.failing_dev_build();
    project.export_pointing_a_at_dev("export");
    std::fs::copy(
        project.dir.join("target/run_results.json"),
        project.dir.join("export/run_results.json"),
    )
    .unwrap();
    project.ok(&["retry", "--state", "export"], false);
    assert_eq!(project.b_compiled_against(), "prod");
}
