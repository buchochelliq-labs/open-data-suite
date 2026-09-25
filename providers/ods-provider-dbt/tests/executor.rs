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

fn executor(dir: &Path) -> DbtExecutor {
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
    assert_eq!(report.passed.len(), 7, "{report:?}");
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
            .contains("--select stg_orders stg_payments orders")
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
    assert!(
        report
            .command
            .unwrap()
            .contains("--exclude-resource-type test")
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
async fn a_missing_program_is_an_error() {
    let dir = scratch("missing");
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
