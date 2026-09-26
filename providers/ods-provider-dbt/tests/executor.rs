//! `DbtExecutor` passes the `Executor` conformance suite, run against the fake dbt in
//! `fixtures/dbt/fake-dbt` (a Python script, so Unix only; no warehouse or network).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput};
use ods_sdk::conformance::executor::{ExecutorHarness, run};
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionRequest, ExecutionStatus, Executor, PrepareRequest, RequestedNode,
};

fn fake_dbt() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/fake-dbt/dbt")
}

/// A fresh directory under the target dir, removed first if a previous run left it.
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "executor-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An executor over a target directory that already holds a manifest, as after
/// `dbt compile`: it selects nodes exactly from it.
fn executor(dir: &Path) -> DbtExecutor {
    let target = dir.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        target.join("manifest.json"),
    )
    .unwrap();
    DbtExecutor::new(fake_dbt(), dir.join("target"))
        .env("FAKE_DBT_LOG", dir.join("built.log").display().to_string())
        .env("FAKE_DBT_FAIL", "stg_orders")
        .output(DbtOutput::Capture)
}

#[derive(Default)]
struct Harness {
    dir: Mutex<Option<PathBuf>>,
}

#[async_trait]
impl ExecutorHarness for Harness {
    async fn executor(&self) -> Arc<dyn Executor> {
        let dir = scratch("suite");
        let executor = executor(&dir);
        *self.dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        Arc::new(executor)
    }

    fn buildable(&self) -> Vec<RequestedNode> {
        vec![
            RequestedNode::new("seed.jaffle_ods.raw_orders", "raw_orders"),
            RequestedNode::new("seed.jaffle_ods.raw_customers", "raw_customers"),
        ]
    }

    fn failing(&self) -> Option<RequestedNode> {
        Some(RequestedNode::new(
            "model.jaffle_ods.stg_orders",
            "stg_orders",
        ))
    }

    fn checked_and_unchecked(&self) -> Option<(RequestedNode, RequestedNode)> {
        Some((
            RequestedNode::new("model.jaffle_ods.customers", "customers"),
            RequestedNode::new("seed.jaffle_ods.raw_orders", "raw_orders"),
        ))
    }

    async fn built(&self) -> Option<Vec<String>> {
        let dir = self
            .dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let log = std::fs::read_to_string(dir.join("built.log")).unwrap_or_default();
        Some(log.lines().map(str::to_owned).collect())
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 9, "{report:?}");
}

#[tokio::test]
async fn failed_parents_skip_their_children_and_failed_tests_are_checks() {
    let dir = scratch("skip");
    let executor = executor(&dir).env("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![
                RequestedNode::new("model.jaffle_ods.stg_orders", "stg_orders"),
                RequestedNode::new("model.jaffle_ods.stg_payments", "stg_payments"),
                RequestedNode::new("model.jaffle_ods.orders", "orders"),
            ],
            ExecutionMode::Build,
        ))
        .await
        .unwrap();
    let statuses: Vec<_> = report.nodes.iter().map(|n| n.status).collect();
    assert_eq!(
        statuses,
        [
            ExecutionStatus::Failed,
            ExecutionStatus::Success,
            ExecutionStatus::Skipped
        ]
    );
    assert!(!report.succeeded);
    assert!(
        report
            .command
            .as_deref()
            .unwrap()
            .contains("--select fqn:jaffle_ods.marts.orders,resource_type:model fqn:jaffle_ods.staging.stg_orders,resource_type:model fqn:jaffle_ods.staging.stg_payments,resource_type:model")
    );

    let executor = crate::executor(&dir)
        .env("FAKE_DBT_FAIL", "")
        .env("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![
                RequestedNode::new("model.jaffle_ods.stg_orders", "stg_orders"),
                RequestedNode::new("model.jaffle_ods.stg_payments", "stg_payments"),
                RequestedNode::new("model.jaffle_ods.orders", "orders"),
            ],
            ExecutionMode::Build,
        ))
        .await
        .unwrap();
    assert!(
        report
            .nodes
            .iter()
            .all(|n| n.status == ExecutionStatus::Success)
    );
    assert_eq!(
        report.checks_failed,
        ["test.jaffle_ods.unique_orders_order_id.fed79b3a6e"]
    );
    // The failed test is listed on the node it checks, and only there.
    let failed_on: Vec<(&str, usize)> = report
        .nodes
        .iter()
        .map(|n| (n.node.as_str(), n.checks_failed.len()))
        .collect();
    assert_eq!(
        failed_on,
        [
            ("model.jaffle_ods.stg_orders", 0),
            ("model.jaffle_ods.stg_payments", 0),
            ("model.jaffle_ods.orders", 1)
        ]
    );
    assert!(!report.succeeded);
}

#[tokio::test]
async fn run_mode_leaves_out_tests() {
    let dir = scratch("run-mode");
    let executor = executor(&dir).env("FAKE_DBT_FAIL_TEST", "unique_orders_order_id");
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new("model.jaffle_ods.orders", "orders")],
            ExecutionMode::Run,
        ))
        .await
        .unwrap();
    assert!(report.succeeded, "{report:?}");
    // Only models: `dbt run`, which never runs tests (#229).
    let command = report.command.unwrap();
    assert!(command.contains(" run --select "), "{command}");

    // Mixed types: `dbt build` without tests.
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![
                RequestedNode::new("seed.jaffle_ods.raw_orders", "raw_orders"),
                RequestedNode::new("model.jaffle_ods.orders", "orders"),
            ],
            ExecutionMode::Run,
        ))
        .await
        .unwrap();
    assert!(report.succeeded, "{report:?}");
    let command = report.command.unwrap();
    assert!(command.contains(" build --select "), "{command}");
    assert!(
        command.contains("--exclude-resource-type test"),
        "{command}"
    );
}

#[tokio::test]
async fn a_stale_run_results_file_is_never_read_as_this_runs() {
    let dir = scratch("stale");
    let ok = executor(&dir);
    ok.execute(&ExecutionRequest::new(
        vec![RequestedNode::new(
            "seed.jaffle_ods.raw_orders",
            "raw_orders",
        )],
        ExecutionMode::Run,
    ))
    .await
    .unwrap();
    // dbt dies before writing anything: the previous file must not pass for this run.
    let broken = executor(&dir).env("FAKE_DBT_EXIT", "2");
    let err = broken
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new(
                "seed.jaffle_ods.raw_orders",
                "raw_orders",
            )],
            ExecutionMode::Run,
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("wrote no run results"), "{err}");
    assert!(
        err.to_string().contains("failing before writing anything"),
        "{err}"
    );
}

#[tokio::test]
async fn prepare_compiles_and_measures_sources() {
    let dir = scratch("prepare");
    let report = executor(&dir)
        .prepare(&PrepareRequest::new().measuring_sources())
        .await
        .unwrap();
    assert!(report.sources_measured, "{report:?}");
    assert!(dir.join("target/manifest.json").is_file());

    let err = executor(&dir)
        .env("FAKE_DBT_EXIT", "1")
        .prepare(&PrepareRequest::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("compile"), "{err}");
}

#[tokio::test]
async fn nodes_are_selected_exactly_or_not_at_all() {
    // Without a manifest there is nothing to select from: nothing runs.
    let dir = scratch("no-manifest");
    let err = DbtExecutor::new(fake_dbt(), dir.join("target"))
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new("model.jaffle_ods.orders", "orders")],
            ExecutionMode::Run,
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("select nodes exactly"), "{err}");
    assert!(!dir.join("target/run_results.json").exists());
}

#[tokio::test]
async fn a_missing_program_is_an_error() {
    let dir = scratch("missing");
    // A manifest to select from, so the failure is the missing program.
    executor(&dir);
    let err = DbtExecutor::new(dir.join("no-such-dbt"), dir.join("target"))
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new(
                "seed.jaffle_ods.raw_orders",
                "raw_orders",
            )],
            ExecutionMode::Run,
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("couldn't start"), "{err}");
}

/// What the fake dbt saw on each call: its arguments and the `DBT_*` names in its
/// environment.
fn seen(dir: &Path) -> Vec<(Vec<String>, Vec<String>)> {
    std::fs::read_to_string(dir.join("seen"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let strings = |key: &str| {
                v[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            (strings("argv"), strings("env"))
        })
        .collect()
}

/// #227: dbt settings from the environment are handled like dbt's flags. ODS passes
/// its own settings as flags only, beats the ones a flag can beat, and refuses the rest.
#[tokio::test]
async fn dbt_settings_from_the_environment_are_owned_overridden_or_refused() {
    let dir = scratch("env");
    let run = executor(&dir)
        .env("FAKE_DBT_SEEN", dir.join("seen").display().to_string())
        .env("DBT_DEFER", "true")
        .env("DBT_EMPTY", "1")
        .env("DBT_TARGET", "elsewhere")
        .env("DBT_LOG_LEVEL", "debug")
        .env("DBT_SCHEMA", "a project's own")
        .target("prod")
        .profile("warehouse");
    assert_eq!(run.env_warnings().len(), 2, "{:?}", run.env_warnings());
    let report = run
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new(
                "seed.jaffle_ods.raw_orders",
                "raw_orders",
            )],
            ExecutionMode::Run,
        ))
        .await
        .unwrap();
    assert!(report.succeeded, "{report:?}");
    let calls = seen(&dir);
    let (argv, env) = calls.last().unwrap();
    for flag in [
        "--no-defer",
        "--write-json",
        "--target",
        "prod",
        "--profile",
        "warehouse",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag} missing: {argv:?}");
    }
    // `dbt seed` has no --empty, so no --no-empty either.
    assert!(!argv.iter().any(|a| a == "--no-empty"), "{argv:?}");
    // ODS's own settings reach dbt as flags only; the rest pass through.
    assert!(!env.iter().any(|n| n == "DBT_TARGET"), "{env:?}");
    for name in ["DBT_DEFER", "DBT_LOG_LEVEL", "DBT_SCHEMA"] {
        assert!(env.iter().any(|n| n == name), "{name} missing: {env:?}");
    }

    // A setting nothing beats: refused before dbt runs.
    let refused = executor(&dir)
        .env("FAKE_DBT_SEEN", dir.join("seen").display().to_string())
        .env("DBT_STATE", "prod-artifacts")
        .execute(&ExecutionRequest::new(
            vec![RequestedNode::new(
                "seed.jaffle_ods.raw_orders",
                "raw_orders",
            )],
            ExecutionMode::Run,
        ))
        .await
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .starts_with("unset DBT_STATE for ODS runs"),
        "{refused}"
    );
    assert_eq!(seen(&dir).len(), calls.len(), "dbt didn't run");
}
