//! `DbtExecutor` passes the `RelationInspector` conformance suite, run against the fake
//! dbt in `fixtures/dbt/fake-dbt` (a Python script, so Unix only).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput};
use ods_sdk::conformance::relations::{RelationHarness, run};
use ods_sdk::contracts::executor::RequestedNode;
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence};

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "relations-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        dir.join("target/manifest.json"),
    )
    .unwrap();
    dir
}

fn inspector(dir: &Path) -> DbtExecutor {
    DbtExecutor::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/fake-dbt/dbt"),
        dir.join("target"),
    )
    .env(
        "FAKE_DBT_DROPPED",
        dir.join("dropped").display().to_string(),
    )
    .env("FAKE_DBT_CALLS", dir.join("calls").display().to_string())
    .output(DbtOutput::Capture)
}

#[derive(Default)]
struct Harness {
    dir: Mutex<Option<PathBuf>>,
}

#[async_trait]
impl RelationHarness for Harness {
    async fn inspector(&self) -> Arc<dyn RelationInspector> {
        let dir = scratch("suite");
        let inspector = inspector(&dir);
        *self.dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        Arc::new(inspector)
    }

    fn present(&self) -> Vec<RequestedNode> {
        vec![
            RequestedNode::new("model.jaffle_ods.orders", "orders"),
            RequestedNode::new("seed.jaffle_ods.raw_orders", "raw_orders"),
        ]
    }

    async fn drop_relation(&self, node: &RequestedNode) -> bool {
        let Some(dir) = self
            .dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return false;
        };
        std::fs::write(dir.join("dropped"), format!("{}\n", node.id)).unwrap();
        true
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness::default()).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 3, "{report:?}");
}

#[tokio::test]
async fn one_dbt_call_leaves_the_target_alone_and_failures_are_errors() {
    let dir = scratch("one-call");
    let manifest = std::fs::read(dir.join("target/manifest.json")).unwrap();
    let nodes: Vec<RequestedNode> = ["orders", "customers", "stg_orders", "stg_payments"]
        .iter()
        .map(|n| RequestedNode::new(format!("model.jaffle_ods.{n}"), *n))
        .collect();
    std::fs::write(dir.join("dropped"), "model.jaffle_ods.stg_payments\n").unwrap();
    let report = inspector(&dir).inspect(&nodes).await.unwrap();
    let missing: Vec<&str> = report
        .nodes
        .iter()
        .filter(|(_, p)| *p == RelationPresence::Missing)
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(missing, ["model.jaffle_ods.stg_payments"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("calls")).unwrap(),
        "show\n",
        "one dbt call for every node"
    );
    // The plan's manifest, with its compiled SQL, is untouched.
    assert_eq!(
        std::fs::read(dir.join("target/manifest.json")).unwrap(),
        manifest
    );

    let err = inspector(&dir)
        .env("FAKE_DBT_SHOW_FAIL", "1")
        .inspect(&nodes)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("relation check"), "{err}");
}
