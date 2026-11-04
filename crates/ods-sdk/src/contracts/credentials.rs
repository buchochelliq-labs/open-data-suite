//! `Credentials`: how a provider signs in to its warehouse, and whether it can (#297,
//! ADR-0021 §4).
//!
//! A provider that talks to a warehouse's own APIs gets its tokens itself (from a
//! service principal's secret, a tool's login, a static token, …) and uses them
//! internally. This contract only lets a host ask how it signs in and whether that
//! works, e.g. for `ods doctor`. **No token crosses it** (AGENTS rule 9).
//!
//! # Semantics
//! - [`status`](Credentials::status) signs in if it has to (it may fetch a token) and
//!   reports the method, the principal and scopes when known, and when the credential
//!   expires. It changes nothing in the warehouse.
//! - [`AuthStatus::verified`] is true only when the warehouse accepted the credential
//!   (e.g. its token endpoint granted a token). A credential nothing checked, such as
//!   a static token, is not verified (AGENTS rule 3).
//! - Nothing in an [`AuthStatus`] or a [`CredentialError`] holds a token, a secret, or
//!   any part of one. Text a warehouse sent back is truncated and has every secret the
//!   provider used removed first.
//! - [`login_hint`](Credentials::login_hint) is what a person can run to sign in, when
//!   the method signs people in through a tool (e.g. a CLI's login command).

use async_trait::async_trait;
use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::{Deserialize, Serialize};

use crate::contracts::secrets::SecretError;
use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `credentials` contract.
pub const CREDENTIALS: Contract = Contract {
    name: "credentials",
    version: SchemaVersion::new(0, 1),
};

/// How a provider signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    /// A person's own login (e.g. OAuth with their browser).
    User,
    /// A service principal's secret (e.g. an OAuth client-credentials grant).
    ServiceSecret,
    /// A workload's identity, exchanged for a token (e.g. a CI platform's OIDC token).
    Workload,
    /// A static token, sent as is.
    StaticToken,
    /// Delegated to an external tool's profile, which picks the method itself.
    Delegated {
        /// The tool's profile.
        profile: String,
        /// The method the tool reports for that profile, in its own words, if it said.
        reported: Option<String>,
    },
    /// The method can't be told.
    Unknown,
}

/// How a provider signs in, and what that gave it. Never holds a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AuthStatus {
    /// How it signs in.
    pub method: AuthMethod,
    /// Who it signs in as (e.g. a user name or an application id), when known.
    pub principal: Option<String>,
    /// The scopes granted, sorted; empty when unknown.
    pub scopes: Vec<String>,
    /// When the credential expires, when known.
    pub expires_at: Option<Timestamp>,
    /// Whether the warehouse accepted the credential.
    pub verified: bool,
}

impl AuthStatus {
    /// A status, for implementations to return. `scopes` are sorted and deduplicated.
    pub fn new(
        method: AuthMethod,
        principal: Option<String>,
        mut scopes: Vec<String>,
        expires_at: Option<Timestamp>,
        verified: bool,
    ) -> Self {
        scopes.sort();
        scopes.dedup();
        Self {
            method,
            principal,
            scopes,
            expires_at,
            verified,
        }
    }
}

/// Why a provider couldn't sign in. Messages never hold a token or a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialError {
    /// A setting the method needs isn't set.
    #[error("`{setting}` isn't set")]
    NotConfigured {
        /// The setting.
        setting: String,
    },
    /// A setting's value can't be used. Never repeats the value.
    #[error("`{setting}` {why}")]
    InvalidSetting {
        /// The setting.
        setting: String,
        /// What is wrong with it.
        why: String,
    },
    /// A secret the method needs couldn't be resolved.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The warehouse refused to sign in, with what it said (truncated, secrets removed).
    #[error("sign-in was refused: {reason}")]
    Refused {
        /// What the warehouse said.
        reason: String,
    },
    /// The warehouse couldn't be reached; retrying later may succeed.
    #[error("sign-in failed: {reason}")]
    Unavailable {
        /// What went wrong.
        reason: String,
    },
}

impl CredentialError {
    /// Whether retrying later may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Unavailable { .. } => true,
            Self::Secret(e) => e.is_retryable(),
            _ => false,
        }
    }
}

impl From<CredentialError> for ProviderError {
    fn from(error: CredentialError) -> Self {
        if error.is_retryable() {
            ProviderError::Unavailable(error.to_string())
        } else {
            ProviderError::Other(error.to_string())
        }
    }
}

/// Reports how a provider signs in to its warehouse.
#[async_trait]
pub trait Credentials: Provider {
    /// How it signs in, after signing in if it has to.
    ///
    /// # Errors
    /// Returns [`CredentialError`] if it can't sign in.
    async fn status(&self) -> Result<AuthStatus, CredentialError>;

    /// What a person can run to sign in, if the method signs people in through a tool.
    fn login_hint(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_sort_their_scopes_and_serialize_by_kind() {
        let status = AuthStatus::new(
            AuthMethod::ServiceSecret,
            Some("app-1".to_owned()),
            vec!["sql".to_owned(), "all-apis".to_owned(), "sql".to_owned()],
            Some(Timestamp::from_unix(0)),
            true,
        );
        assert_eq!(status.scopes, ["all-apis", "sql"]);
        let json = serde_json::to_value(&status).expect("serializes");
        assert_eq!(json["method"]["kind"], "service_secret");
        assert_eq!(json["expires_at"], "1970-01-01T00:00:00Z");
        let delegated = serde_json::to_value(AuthMethod::Delegated {
            profile: "free".to_owned(),
            reported: Some("tool-login".to_owned()),
        })
        .expect("serializes");
        assert_eq!(delegated["kind"], "delegated");
        assert_eq!(delegated["profile"], "free");
    }

    #[test]
    fn errors_convert_keeping_whether_a_retry_may_help() {
        let unavailable = CredentialError::Unavailable {
            reason: "timed out".to_owned(),
        };
        assert!(unavailable.is_retryable());
        assert_eq!(
            ProviderError::from(unavailable),
            ProviderError::Unavailable("sign-in failed: timed out".to_owned())
        );
        let refused = CredentialError::Refused {
            reason: "invalid_client".to_owned(),
        };
        assert!(matches!(
            ProviderError::from(refused),
            ProviderError::Other(_)
        ));
        let missing = CredentialError::NotConfigured {
            setting: "client_id".to_owned(),
        };
        assert_eq!(missing.to_string(), "`client_id` isn't set");
    }
}
