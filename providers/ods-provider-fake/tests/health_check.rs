//! The fake health check passes the `HealthCheck` conformance suite.

use std::sync::Arc;

use ods_provider_fake::{FakeHealthCheck, Misbehaviour};
use ods_sdk::conformance::health_check::{HealthCheckHarness, run};
use ods_sdk::contracts::health_check::{CheckScope, HealthCheck, NodeFacts, Severity};

struct Harness;

fn nodes() -> Vec<NodeFacts> {
    ["model.shop.a", "model.shop.b", "seed.shop.c"]
        .map(|id| NodeFacts::new(id, id.rsplit('.').next().unwrap(), "model"))
        .to_vec()
}

impl HealthCheckHarness for Harness {
    fn check(&self) -> Arc<dyn HealthCheck> {
        Arc::new(
            FakeHealthCheck::new("fake.check", Severity::Warn)
                .failing("model.shop.b")
                .unknown("seed.shop.c"),
        )
    }

    fn scope(&self) -> CheckScope {
        CheckScope::new(nodes(), None)
    }

    fn undecidable(&self) -> Option<CheckScope> {
        None
    }
}

#[tokio::test]
async fn conforms() {
    let report = run(&Harness).await;
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

/// A check that can't decide anything answers unknown for every node.
struct Undecided;

impl HealthCheckHarness for Undecided {
    fn check(&self) -> Arc<dyn HealthCheck> {
        let mut check = FakeHealthCheck::new("fake.undecided", Severity::Error);
        for node in nodes() {
            check = check.unknown(node.id);
        }
        Arc::new(check)
    }

    fn scope(&self) -> CheckScope {
        CheckScope::new(nodes(), None)
    }

    fn undecidable(&self) -> Option<CheckScope> {
        Some(self.scope())
    }
}

#[tokio::test]
async fn an_undecided_check_conforms() {
    let report = run(&Undecided).await;
    assert!(report.skipped.is_empty(), "{report:?}");
}

/// The suite catches a check that leaves a node unanswered.
#[tokio::test]
#[should_panic(expected = "answers_every_node_once")]
async fn a_check_that_skips_a_node_does_not_conform() {
    struct Skips;
    impl HealthCheckHarness for Skips {
        fn check(&self) -> Arc<dyn HealthCheck> {
            Arc::new(
                FakeHealthCheck::new("fake.skips", Severity::Warn)
                    .misbehaving(Misbehaviour::SkipsANode),
            )
        }
        fn scope(&self) -> CheckScope {
            CheckScope::new(nodes(), None)
        }
        fn undecidable(&self) -> Option<CheckScope> {
            None
        }
    }
    run(&Skips).await;
}

/// …and one that answers a node outside its scope.
#[tokio::test]
#[should_panic(expected = "outside the scope")]
async fn a_check_that_answers_a_stranger_does_not_conform() {
    struct Stranger;
    impl HealthCheckHarness for Stranger {
        fn check(&self) -> Arc<dyn HealthCheck> {
            Arc::new(
                FakeHealthCheck::new("fake.stranger", Severity::Warn)
                    .misbehaving(Misbehaviour::AnswersAStranger),
            )
        }
        fn scope(&self) -> CheckScope {
            CheckScope::new(nodes(), None)
        }
        fn undecidable(&self) -> Option<CheckScope> {
            None
        }
    }
    run(&Stranger).await;
}
