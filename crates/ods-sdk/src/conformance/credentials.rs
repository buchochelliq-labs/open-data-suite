//! Conformance suite for [`Credentials`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::credentials::Credentials;

/// What the suite needs from a provider under test.
#[async_trait]
pub trait CredentialsHarness: Send + Sync {
    /// A fresh provider that can sign in, using [`markers`](Self::markers) as its
    /// secrets and receiving them as tokens. Called once per case.
    async fn provider(&self) -> Arc<dyn Credentials>;

    /// A fresh provider whose sign-in fails, if the harness can make one (e.g. a token
    /// endpoint that refuses, quoting the secret it was sent). `None` skips the case.
    async fn failing(&self) -> Option<Arc<dyn Credentials>> {
        None
    }

    /// Every secret and token the providers use, e.g. `ods-conformance-7f3a…`: values
    /// that can't appear in a message by chance. The suite checks none ever does.
    fn markers(&self) -> Vec<String>;
}

fn assert_hidden(case: &str, markers: &[String], shown: &str) {
    for marker in markers {
        assert!(
            !shown.contains(marker.as_str()),
            "{case}: a secret or token appears in what the provider shows"
        );
    }
}

async fn a_status_holds_no_token(harness: &dyn CredentialsHarness) {
    let case = "a_status_holds_no_token";
    let markers = harness.markers();
    assert!(
        markers.iter().all(|m| !m.is_empty()),
        "{case}: a marker is empty"
    );
    let provider = harness.provider().await;
    let status = provider
        .status()
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let json = serde_json::to_string(&status).unwrap_or_else(|e| panic!("{case}: {e}"));
    let info = serde_json::to_string(&provider.info()).unwrap_or_else(|e| panic!("{case}: {e}"));
    let hint = provider.login_hint().unwrap_or_default();
    for shown in [format!("{status:?}"), json, info, hint] {
        assert_hidden(case, &markers, &shown);
    }
    let mut sorted = status.scopes.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        status.scopes, sorted,
        "{case}: scopes are sorted and unique"
    );
}

async fn a_status_is_repeatable(harness: &dyn CredentialsHarness) {
    let case = "a_status_is_repeatable";
    let provider = harness.provider().await;
    let first = provider
        .status()
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let again = provider
        .status()
        .await
        .unwrap_or_else(|e| panic!("{case} (again): {e}"));
    assert_eq!(first.method, again.method, "{case}: method");
    assert_eq!(first.principal, again.principal, "{case}: principal");
}

/// Whether the case ran.
async fn a_failure_holds_no_secret(harness: &dyn CredentialsHarness) -> bool {
    let case = "a_failure_holds_no_secret";
    let Some(provider) = harness.failing().await else {
        return false;
    };
    let markers = harness.markers();
    match provider.status().await {
        Ok(status) => panic!("{case}: the failing provider signed in: {status:?}"),
        Err(e) => {
            for shown in [e.to_string(), format!("{e:?}")] {
                assert_hidden(case, &markers, &shown);
            }
        }
    }
    true
}

/// Runs every case.
pub async fn run(harness: &dyn CredentialsHarness) -> Report {
    let mut report = Report::default();
    a_status_holds_no_token(harness).await;
    report.passed.push("a_status_holds_no_token");
    a_status_is_repeatable(harness).await;
    report.passed.push("a_status_is_repeatable");
    if a_failure_holds_no_secret(harness).await {
        report.passed.push("a_failure_holds_no_secret");
    } else {
        report.skipped.push((
            "a_failure_holds_no_secret",
            "the harness has no failing provider".to_owned(),
        ));
    }
    report
}
