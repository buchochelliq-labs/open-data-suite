//! In-memory [`Credentials`] (#297, ADR-0021 §4).

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_sdk::contracts::credentials::{AuthStatus, CredentialError, Credentials};
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// Signs in as set up: it reports the status it was given, or fails with the error it
/// was given. It holds no token.
#[derive(Debug, Clone)]
pub struct FakeCredentials {
    outcome: Result<AuthStatus, CredentialError>,
    login_hint: Option<String>,
}

impl FakeCredentials {
    /// Signs in, reporting `status`.
    pub fn signed_in(status: AuthStatus) -> Self {
        Self {
            outcome: Ok(status),
            login_hint: None,
        }
    }

    /// Fails to sign in with `error`.
    pub fn failing(error: CredentialError) -> Self {
        Self {
            outcome: Err(error),
            login_hint: None,
        }
    }

    /// Offers `hint` as what to run to sign in.
    #[must_use]
    pub fn with_login_hint(mut self, hint: impl Into<String>) -> Self {
        self.login_hint = Some(hint.into());
        self
    }
}

impl Provider for FakeCredentials {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "credentials",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl Credentials for FakeCredentials {
    async fn status(&self) -> Result<AuthStatus, CredentialError> {
        self.outcome.clone()
    }

    fn login_hint(&self) -> Option<String> {
        self.login_hint.clone()
    }
}
