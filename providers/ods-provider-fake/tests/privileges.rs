//! The in-memory relation privileges pass their conformance suite.

use std::sync::Arc;

use async_trait::async_trait;
use ods_provider_fake::FakeRelationPrivileges;
use ods_sdk::conformance::privileges::{PrivilegesHarness, run};
use ods_sdk::contracts::privileges::{Access, RelationPrivileges};
use ods_sdk::contracts::probe::ProbeTarget;

struct Harness;

#[async_trait]
impl PrivilegesHarness for Harness {
    async fn provider(&self) -> Arc<dyn RelationPrivileges> {
        Arc::new(
            FakeRelationPrivileges::new()
                .with_login("reader")
                .read_only("model.suite.a")
                .read_only("source.suite.raw.b")
                .elevated("model.suite.owned", ["owner of schema suite"]),
        )
    }

    fn read_only(&self) -> Vec<ProbeTarget> {
        vec![
            ProbeTarget::new("model.suite.a", "a"),
            ProbeTarget::new("source.suite.raw.b", "raw.b"),
        ]
    }

    fn elevated(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("model.suite.owned", "owned"))
    }
}

#[tokio::test]
async fn the_relation_privileges_conform() {
    let report = run(&Harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 4, "{report:?}");
}

#[tokio::test]
async fn it_names_the_login_and_fails_when_told() {
    let fake = FakeRelationPrivileges::new()
        .with_login("reader")
        .unknown("model.suite.a", "no grants visible");
    let target = [ProbeTarget::new("model.suite.a", "a")];
    let report = fake.privileges(&target).await.unwrap();
    assert_eq!(report.login.as_deref(), Some("reader"));
    assert_eq!(
        report.targets[0].1,
        Access::Unknown("no grants visible".to_owned())
    );
    assert!(fake.failing().privileges(&target).await.is_err());
}
