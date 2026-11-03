//! The in-memory secret store passes its conformance suite, and fails as set up.

use std::sync::Arc;

use async_trait::async_trait;
use ods_config::SecretRef;
use ods_provider_fake::{FakeSecrets, SecretFailure};
use ods_sdk::conformance::secrets::{SecretsHarness, run};
use ods_sdk::contracts::secrets::{SecretError, SecretProvider};

const VALUE: &str = "ods-conformance-1b8e5d-fake";

fn reference(text: &str) -> SecretRef {
    SecretRef::parse(text).expect("valid reference")
}

struct Harness;

#[async_trait]
impl SecretsHarness for Harness {
    async fn provider(&self) -> Arc<dyn SecretProvider> {
        Arc::new(
            FakeSecrets::new("vault")
                .with("db/password", VALUE)
                .with("db/blank", ""),
        )
    }

    fn present(&self) -> (SecretRef, String) {
        (reference("vault:db/password"), VALUE.to_owned())
    }

    fn absent(&self) -> SecretRef {
        reference("vault:db/missing")
    }

    fn empty(&self) -> Option<SecretRef> {
        Some(reference("vault:db/blank"))
    }
}

#[tokio::test]
async fn the_fake_secret_store_conforms() {
    let report = run(&Harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 6, "{report:?}");
}

#[tokio::test]
async fn failures_name_only_the_reference_and_reads_are_counted() {
    let denied = FakeSecrets::new("vault")
        .with("db", VALUE)
        .failing(SecretFailure::Denied);
    let error = denied
        .resolve(&reference("vault:db"))
        .await
        .map(|_| ())
        .expect_err("denied");
    assert_eq!(error, SecretError::denied(&reference("vault:db")));
    assert!(!error.is_retryable());

    let offline = FakeSecrets::new("vault")
        .with("db", VALUE)
        .failing(SecretFailure::Unavailable);
    let error = offline
        .resolve(&reference("vault:db"))
        .await
        .map(|_| ())
        .expect_err("offline");
    assert!(error.is_retryable());
    assert!(!error.to_string().contains(VALUE), "{error}");

    let store = FakeSecrets::new("vault").with("db", VALUE);
    for _ in 0..3 {
        store.resolve(&reference("vault:db")).await.expect("held");
    }
    assert_eq!(store.reads(), 3);
    assert!(!format!("{store:?}").contains(VALUE));
}
