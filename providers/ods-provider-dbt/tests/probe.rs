//! `DbtExecutor` passes the `RelationProbe` conformance suite, run against the fake dbt
//! in `fixtures/dbt/fake-dbt` (a Python script, so Unix only), and, with
//! `ODS_TEST_DBT` set to a dbt with the `duckdb` adapter, against real dbt.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput};
use ods_sdk::conformance::probe::{ProbeHarness, run};
use ods_sdk::contracts::probe::{
    ProbeAnswer, ProbeFilter, ProbeRequest, ProbeRow, ProbeStatement, ProbeTarget, RelationProbe,
};

const DETAIL: &str = "DESCRIBE DETAIL {relation}";
const HISTORY: &str = "DESCRIBE HISTORY {relation} LIMIT 1";

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "probe-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("target")).unwrap();
    dir
}

/// The fake warehouse: `raw.orders` and `raw.payments` are Delta tables with a row for
/// each statement, `raw.customers` a view.
fn warehouse(dir: &Path, version: &str) {
    let table = |id: &str| {
        serde_json::json!({
            "type": "table",
            "formats": ["delta"],
            "rows": {
                DETAIL: {"id": id, "format": "delta", "name": "ignored"},
                HISTORY: {"version": version, "timestamp": "2026-09-29 10:00:00", "operation": "WRITE"},
            },
        })
    };
    let doc = serde_json::json!({
        "raw.orders": table("0f1e-orders"),
        "raw.payments": table("it's \\ \"odd\""),
        "raw.customers": {"type": "view"},
    });
    std::fs::write(dir.join("warehouse.json"), doc.to_string()).unwrap();
}

fn probe(dir: &Path) -> DbtExecutor {
    DbtExecutor::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/fake-dbt/dbt"),
        dir.join("target"),
    )
    .env("FAKE_DBT_SOURCES", "3")
    .env(
        "FAKE_DBT_PROBE",
        dir.join("warehouse.json").display().to_string(),
    )
    .env("FAKE_DBT_CALLS", dir.join("calls").display().to_string())
    .output(DbtOutput::Capture)
}

fn request() -> ProbeRequest {
    ProbeRequest::new(
        ProbeFilter::kinds(["table"])
            .unwrap()
            .with_format("delta")
            .unwrap(),
        vec![
            ProbeStatement::new(DETAIL, ["id", "format"]).unwrap(),
            ProbeStatement::new(HISTORY, ["version", "timestamp"]).unwrap(),
        ],
    )
    .unwrap()
}

fn source(table: &str) -> ProbeTarget {
    ProbeTarget::new(
        format!("source.jaffle_ods.raw.{table}"),
        format!("raw.{table}"),
    )
}

struct Harness;

#[async_trait]
impl ProbeHarness for Harness {
    async fn probe(&self) -> Arc<dyn RelationProbe> {
        let dir = scratch("suite");
        warehouse(&dir, "7");
        Arc::new(probe(&dir))
    }

    fn request(&self) -> ProbeRequest {
        request()
    }

    fn matching(&self) -> Vec<ProbeTarget> {
        vec![source("orders"), source("payments")]
    }

    fn excluded(&self) -> Option<ProbeTarget> {
        Some(source("customers"))
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 4, "{report:?}");
}

fn row(pairs: &[(&str, &str)]) -> ProbeRow {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[tokio::test]
async fn one_dbt_call_answers_only_what_was_asked_and_failures_are_errors() {
    let dir = scratch("one-call");
    warehouse(&dir, "7");
    // The plan's manifest, which the probe must leave alone.
    std::fs::write(dir.join("target/manifest.json"), "{\"plan\": 1}").unwrap();
    let asked = [
        source("payments"),
        source("customers"),
        source("gone"),
        ProbeTarget::new("model.jaffle_ods.orders", "orders"),
        ProbeTarget::new("source.jaffle_ods.raw.{{ x }}", "odd"),
    ];
    let report = probe(&dir).probe(&request(), &asked).await.unwrap();
    let ids: Vec<&str> = report.targets.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "source.jaffle_ods.raw.payments",
            "source.jaffle_ods.raw.customers",
            "source.jaffle_ods.raw.gone",
            "model.jaffle_ods.orders",
            "source.jaffle_ods.raw.{{ x }}",
        ],
        "only the requested targets, in order; `orders` is ignored"
    );
    assert_eq!(
        report.targets[0].1,
        ProbeAnswer::Rows(vec![
            // Quotes and backslashes survive the SQL string that carries them.
            row(&[("id", "it's \\ \"odd\""), ("format", "delta")]),
            row(&[("version", "7"), ("timestamp", "2026-09-29 10:00:00")]),
        ])
    );
    assert!(
        matches!(&report.targets[1].1, ProbeAnswer::Skipped(why) if why.contains("a view, not a table")),
        "{report:?}"
    );
    assert!(
        matches!(&report.targets[2].1, ProbeAnswer::Unknown(why) if why.contains("not a source")),
        "{report:?}"
    );
    assert!(
        matches!(&report.targets[3].1, ProbeAnswer::Unknown(why) if why.contains("not a source")),
        "{report:?}"
    );
    assert!(
        matches!(&report.targets[4].1, ProbeAnswer::Unknown(why) if why.contains("characters")),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "show\n",
        "one dbt call for every source"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("target/manifest.json")).unwrap(),
        "{\"plan\": 1}"
    );
    assert!(
        dir.join("target/ods-relation-probe/manifest.json")
            .is_file()
    );

    let err = probe(&dir)
        .env("FAKE_DBT_PROBE_FAIL", "1")
        .probe(&request(), &asked)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("relation probe"), "{err}");
}

#[tokio::test]
async fn a_relation_the_adapter_cant_confirm_the_format_of_is_skipped() {
    let dir = scratch("format");
    let doc = serde_json::json!({
        "raw.orders": {"type": "table", "rows": {DETAIL: {"id": "x", "format": "delta"}}},
        "raw.payments": {"type": "table", "formats": ["parquet"]},
    });
    std::fs::write(dir.join("warehouse.json"), doc.to_string()).unwrap();
    let report = probe(&dir)
        .probe(&request(), &[source("orders"), source("payments")])
        .await
        .unwrap();
    for (_, answer) in &report.targets {
        assert!(
            matches!(answer, ProbeAnswer::Skipped(why) if why.contains("doesn't confirm it is stored as delta")),
            "{report:?}"
        );
    }
}

/// The query against real dbt and `duckdb`: the jaffle project with sources over its
/// seeds. Its relations say nothing about a format, so a format filter skips them.
#[tokio::test]
async fn real_dbt_runs_the_query() {
    let Ok(dbt) = std::env::var("ODS_TEST_DBT") else {
        eprintln!("skipped: set ODS_TEST_DBT to run against real dbt");
        return;
    };
    let dir = scratch("real");
    let project = dir.join("project");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods"),
        &project,
    );
    let _ = std::fs::remove_dir_all(project.join("artifacts"));
    // An absolute database path: dbt resolves a relative one against where it runs.
    std::fs::write(
        project.join("profiles.yml"),
        format!(
            "jaffle_ods:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: \"{}\"\n      threads: 1\n",
            project.join("jaffle_ods.duckdb").display()
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("models/sources.yml"),
        "version: 2\nsources:\n  - name: raw\n    schema: main\n    tables:\n      - name: raw_orders\n      - name: gone\n      - name: a_view\n        identifier: stg_orders\n",
    )
    .unwrap();
    let executor = DbtExecutor::new(&dbt, project.join("target"))
        .project_dir(&project)
        .profiles_dir(&project)
        .output(DbtOutput::Capture);
    let status = std::process::Command::new(&dbt)
        .args(["build", "--quiet", "--project-dir"])
        .arg(&project)
        .arg("--profiles-dir")
        .arg(&project)
        .status()
        .unwrap();
    assert!(status.success());
    let request = ProbeRequest::new(
        ProbeFilter::kinds(["table"]).unwrap(),
        vec![
            ProbeStatement::new(
                "select count(*) as n, 'it''s \\ \"q\"' as odd, null as nothing from {relation}",
                ["n", "odd", "nothing", "absent"],
            )
            .unwrap(),
            ProbeStatement::new("select 1 as one from {relation} where 1 = 0", ["one"]).unwrap(),
        ],
    )
    .unwrap();
    let asked = ["raw_orders", "gone", "a_view"]
        .map(|t| ProbeTarget::new(format!("source.jaffle_ods.raw.{t}"), t));
    let report = executor.probe(&request, &asked).await.unwrap();
    assert_eq!(
        report.targets[0].1,
        ProbeAnswer::Rows(vec![
            row(&[("n", "6"), ("odd", "it's \\ \"q\"")]),
            ProbeRow::new()
        ])
    );
    assert!(matches!(report.targets[1].1, ProbeAnswer::Unknown(_)));
    assert!(matches!(report.targets[2].1, ProbeAnswer::Skipped(_)));
    let formatted = ProbeRequest::new(
        ProbeFilter::kinds(["table"])
            .unwrap()
            .with_format("delta")
            .unwrap(),
        vec![ProbeStatement::new("select 1 as one from {relation}", ["one"]).unwrap()],
    )
    .unwrap();
    let report = executor.probe(&formatted, &asked[..1]).await.unwrap();
    assert!(
        matches!(report.targets[0].1, ProbeAnswer::Skipped(_)),
        "{report:?}"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[tokio::test]
async fn statements_run_only_against_the_requested_relations() {
    let dir = scratch("only-asked");
    warehouse(&dir, "7");
    let probed = dir.join("probed");
    let report = probe(&dir)
        .env("FAKE_DBT_PROBED", probed.display().to_string())
        .probe(&request(), &[source("payments")])
        .await
        .unwrap();
    assert!(
        matches!(report.targets[..], [(_, ProbeAnswer::Rows(_))]),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&probed).unwrap(),
        "source.jaffle_ods.raw.payments\n",
        "`orders` matches the filter too, but wasn't asked about"
    );
}

#[tokio::test]
async fn a_probe_that_takes_too_long_is_stopped_and_fails() {
    let dir = scratch("timeout");
    warehouse(&dir, "7");
    let started = std::time::Instant::now();
    let err = probe(&dir)
        .env("FAKE_DBT_PROBE_DELAY", "30")
        .probe(
            &request().with_timeout(std::time::Duration::from_secs(1)),
            &[source("orders")],
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("took longer than 1s"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}
