//! Conformance suite for [`SecretProvider`].

use std::sync::Arc;

use async_trait::async_trait;
use ods_config::SecretRef;

use super::Report;
use crate::contracts::secrets::{SecretError, SecretProvider};

/// What the suite needs from a provider under test.
#[async_trait]
pub trait SecretsHarness: Send + Sync {
    /// A fresh provider whose store holds [`present`](Self::present) and not
    /// [`absent`](Self::absent). Called once per case.
    async fn provider(&self) -> Arc<dyn SecretProvider>;

    /// A reference to a secret the store holds, and its value. Use a value that can't
    /// appear in a message by chance (e.g. `ods-conformance-7f3a…`): the suite checks
    /// it never does.
    fn present(&self) -> (SecretRef, String);

    /// A reference, in the provider's scheme, to a secret the store doesn't hold.
    fn absent(&self) -> SecretRef;

    /// A reference to a secret the store holds with an empty value, if the store can
    /// hold one. `None` skips the case that needs it.
    fn empty(&self) -> Option<SecretRef> {
        None
    }
}

fn reference(text: &str) -> SecretRef {
    SecretRef::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

async fn scheme_is_valid_and_stable(harness: &dyn SecretsHarness) {
    let case = "scheme_is_valid_and_stable";
    let provider = harness.provider().await;
    let scheme = provider.scheme().to_owned();
    assert!(
        SecretRef::is_valid_scheme(&scheme),
        "{case}: `{scheme}` is not a valid scheme"
    );
    assert_eq!(provider.scheme(), scheme, "{case}: the scheme changed");
    let (present, _) = harness.present();
    assert_eq!(present.scheme(), scheme, "{case}: the harness's reference");
    assert_eq!(
        harness.absent().scheme(),
        scheme,
        "{case}: the absent reference"
    );
}

async fn resolves_what_the_store_holds(harness: &dyn SecretsHarness) {
    let case = "resolves_what_the_store_holds";
    let provider = harness.provider().await;
    let (present, expected) = harness.present();
    for attempt in ["first", "again"] {
        let value = provider
            .resolve(&present)
            .await
            .unwrap_or_else(|e| panic!("{case} ({attempt}): {e}"));
        assert_eq!(value.expose(), expected, "{case} ({attempt})");
    }
}

async fn a_missing_secret_is_not_found(harness: &dyn SecretsHarness) {
    let case = "a_missing_secret_is_not_found";
    let provider = harness.provider().await;
    let absent = harness.absent();
    match provider.resolve(&absent).await {
        Err(SecretError::NotFound { reference }) => {
            assert_eq!(reference, absent.to_string(), "{case}: names the reference");
        }
        Err(other) => panic!("{case}: expected NotFound, got {other}"),
        Ok(_) => panic!("{case}: resolved a secret the store doesn't hold"),
    }
}

/// Whether the case ran.
async fn an_empty_secret_is_empty_never_a_value(harness: &dyn SecretsHarness) -> bool {
    let case = "an_empty_secret_is_empty_never_a_value";
    let Some(empty) = harness.empty() else {
        return false;
    };
    let provider = harness.provider().await;
    match provider.resolve(&empty).await {
        Err(SecretError::Empty { reference }) => {
            assert_eq!(reference, empty.to_string(), "{case}: names the reference");
        }
        Err(other) => panic!("{case}: expected Empty, got {other}"),
        Ok(_) => panic!("{case}: resolved an empty secret to a value"),
    }
    true
}

async fn another_scheme_is_refused(harness: &dyn SecretsHarness) {
    let case = "another_scheme_is_refused";
    let provider = harness.provider().await;
    let (present, _) = harness.present();
    let other = if provider.scheme() == "ods-conformance-other" {
        "ods-conformance-else"
    } else {
        "ods-conformance-other"
    };
    let foreign = reference(&format!("{other}:{}", present.name()));
    match provider.resolve(&foreign).await {
        Err(SecretError::UnknownScheme { scheme, .. }) => {
            assert_eq!(scheme, other, "{case}: names the scheme asked for");
        }
        Err(e) => panic!("{case}: expected UnknownScheme, got {e}"),
        Ok(_) => panic!("{case}: resolved a reference in another scheme"),
    }
}

async fn nothing_shown_holds_the_value(harness: &dyn SecretsHarness) {
    let case = "nothing_shown_holds_the_value";
    let provider = harness.provider().await;
    let (present, secret) = harness.present();
    assert!(!secret.is_empty(), "{case}: the harness's value is empty");
    let value = provider
        .resolve(&present)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let mut shown = vec![
        format!("{value:?}"),
        serde_json::to_string(&provider.info()).unwrap_or_else(|e| panic!("{case}: {e}")),
    ];
    if let Err(e) = provider.resolve(&harness.absent()).await {
        shown.push(e.to_string());
        shown.push(format!("{e:?}"));
    }
    for text in shown {
        assert!(
            !text.contains(&secret),
            "{case}: the value appears in `{}`",
            value.redact_in(&text)
        );
    }
}

/// Runs every case.
pub async fn run(harness: &dyn SecretsHarness) -> Report {
    let mut report = Report::default();
    scheme_is_valid_and_stable(harness).await;
    report.passed.push("scheme_is_valid_and_stable");
    resolves_what_the_store_holds(harness).await;
    report.passed.push("resolves_what_the_store_holds");
    a_missing_secret_is_not_found(harness).await;
    report.passed.push("a_missing_secret_is_not_found");
    if an_empty_secret_is_empty_never_a_value(harness).await {
        report.passed.push("an_empty_secret_is_empty_never_a_value");
    } else {
        report.skipped.push((
            "an_empty_secret_is_empty_never_a_value",
            "the harness has no empty secret".to_owned(),
        ));
    }
    another_scheme_is_refused(harness).await;
    report.passed.push("another_scheme_is_refused");
    nothing_shown_holds_the_value(harness).await;
    report.passed.push("nothing_shown_holds_the_value");
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::EnvSecrets;

    struct Env;

    const VALUE: &str = "ods-conformance-7f3a9c-env";

    #[async_trait]
    impl SecretsHarness for Env {
        async fn provider(&self) -> Arc<dyn SecretProvider> {
            Arc::new(EnvSecrets::from_vars([
                ("ODS_CONFORMANCE_TOKEN", VALUE),
                ("ODS_CONFORMANCE_BLANK", ""),
            ]))
        }
        fn present(&self) -> (SecretRef, String) {
            (reference("env:ODS_CONFORMANCE_TOKEN"), VALUE.to_owned())
        }
        fn absent(&self) -> SecretRef {
            reference("env:ODS_CONFORMANCE_MISSING")
        }
        fn empty(&self) -> Option<SecretRef> {
            Some(reference("env:ODS_CONFORMANCE_BLANK"))
        }
    }

    #[test]
    fn the_env_provider_conforms() {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut run = std::pin::pin!(run(&Env));
        let std::task::Poll::Ready(report) = run.as_mut().poll(&mut cx) else {
            panic!("the env provider never waits");
        };
        assert_eq!(report.passed.len(), 6, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
    }
}
