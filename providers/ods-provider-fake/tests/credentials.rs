//! The in-memory credentials pass their conformance suite.

use std::sync::Arc;

use async_trait::async_trait;
use ods_provider_fake::FakeCredentials;
use ods_sdk::conformance::credentials::{CredentialsHarness, run};
use ods_sdk::contracts::credentials::{AuthMethod, AuthStatus, CredentialError, Credentials};

struct Harness;

#[async_trait]
impl CredentialsHarness for Harness {
    async fn provider(&self) -> Arc<dyn Credentials> {
        Arc::new(
            FakeCredentials::signed_in(AuthStatus::new(
                AuthMethod::ServiceSecret,
                Some("app-1".to_owned()),
                vec!["sql".to_owned()],
                None,
                true,
            ))
            .with_login_hint("fake login"),
        )
    }

    async fn failing(&self) -> Option<Arc<dyn Credentials>> {
        Some(Arc::new(FakeCredentials::failing(
            CredentialError::Refused {
                reason: "invalid_client".to_owned(),
            },
        )))
    }

    fn markers(&self) -> Vec<String> {
        vec!["ods-conformance-5c1d-secret".to_owned()]
    }
}

#[tokio::test]
async fn the_fake_credentials_conform() {
    let report = run(&Harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 3, "{report:?}");
}
