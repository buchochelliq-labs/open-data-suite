//! `SecretProvider`: resolving a secret reference to its value, in memory (#126,
//! ADR-0032).
//!
//! Configuration holds only references, `{ secret = "<scheme>:<name>" }` (ADR-0005 §4).
//! A secret provider serves one scheme and turns a reference into the value it names,
//! at the point of use. The value lives in a [`SecretValue`] for as long as the caller
//! needs it and is wiped when dropped; it is never written to configuration, state,
//! events, logs or output (AGENTS rule 9).
//!
//! # Semantics
//! - [`scheme`](SecretProvider::scheme) is one scheme, as
//!   [`SecretRef::is_valid_scheme`] allows, and never changes.
//! - [`resolve`](SecretProvider::resolve) only reads; it changes nothing where the
//!   secret lives.
//! - A reference with another scheme is [`SecretError::UnknownScheme`]; a provider
//!   never guesses.
//! - A secret that isn't there is [`SecretError::NotFound`], and one that is there but
//!   empty is [`SecretError::Empty`]: never an `Ok` with an empty value, so a missing
//!   credential is never taken for one (AGENTS rule 3).
//! - Every error, and [`info`](crate::Provider::info), names at most the reference,
//!   never the value or any part of it.

use std::borrow::Cow;
use std::fmt;

use async_trait::async_trait;
use ods_config::SecretRef;
use ods_core::SchemaVersion;
use secrecy::{ExposeSecret, SecretString};

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `secret_provider` contract.
pub const SECRET_PROVIDER: Contract = Contract {
    name: "secret_provider",
    version: SchemaVersion::new(0, 1),
};

/// What a removed secret reads as, in text that echoed it.
pub const SECRET_REMOVED: &str = "[secret removed]";

/// A resolved secret, held in memory only.
///
/// It is wiped from memory when dropped. Its `Debug` shows no part of it, and it has no
/// `Display`, `Serialize` or `Clone`, so it can't be printed, written out or copied
/// by accident; share one with `Arc`. Reading it takes an explicit
/// [`expose`](Self::expose).
///
/// ```compile_fail
/// fn printable<T: std::fmt::Display>() {}
/// printable::<ods_sdk::contracts::secrets::SecretValue>();
/// ```
///
/// ```compile_fail
/// fn writable<T: serde::Serialize>() {}
/// writable::<ods_sdk::contracts::secrets::SecretValue>();
/// ```
///
/// ```compile_fail
/// fn copyable<T: Clone>() {}
/// copyable::<ods_sdk::contracts::secrets::SecretValue>();
/// ```
pub struct SecretValue(SecretString);

impl SecretValue {
    /// Holds `value` as a secret.
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    /// The value, for the one place that needs it (e.g. an `Authorization` header).
    /// Don't keep, log or format what this returns.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }

    /// `text` with every occurrence of this value replaced by [`SECRET_REMOVED`], for
    /// messages that may echo it back (e.g. a server's error that quotes the request).
    pub fn redact_in<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let value = self.expose();
        if value.is_empty() || !text.contains(value) {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(text.replace(value, SECRET_REMOVED))
        }
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([redacted])")
    }
}

/// Why a reference couldn't be resolved. Messages name the reference, never a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// No secret by that name exists where the reference points.
    #[error("{reference} is not set")]
    NotFound {
        /// The reference, as `secret(<scheme>:<name>)`.
        reference: String,
    },
    /// The secret exists but is empty, which is treated as not set.
    #[error("{reference} is empty")]
    Empty {
        /// The reference.
        reference: String,
    },
    /// No secret provider serves the reference's scheme.
    #[error("no secret provider serves `{scheme}:` references{}", available_suffix(.available))]
    UnknownScheme {
        /// The scheme asked for.
        scheme: String,
        /// The schemes that are served.
        available: Vec<String>,
    },
    /// The store refused to give the secret to this login or process.
    #[error("{reference} could not be read: access was denied")]
    Denied {
        /// The reference.
        reference: String,
    },
    /// The store couldn't be reached; retrying later may succeed.
    #[error("{reference} could not be read: {reason}")]
    Unavailable {
        /// The reference.
        reference: String,
        /// What went wrong, without the value.
        reason: String,
    },
    /// The secret exists but can't be used (e.g. it isn't text).
    #[error("{reference} can't be used: {reason}")]
    Invalid {
        /// The reference.
        reference: String,
        /// What is wrong with it, without the value.
        reason: String,
    },
}

impl SecretError {
    /// [`NotFound`](Self::NotFound) for `reference`.
    pub fn not_found(reference: &SecretRef) -> Self {
        Self::NotFound {
            reference: reference.to_string(),
        }
    }

    /// [`Empty`](Self::Empty) for `reference`.
    pub fn empty(reference: &SecretRef) -> Self {
        Self::Empty {
            reference: reference.to_string(),
        }
    }

    /// [`Denied`](Self::Denied) for `reference`.
    pub fn denied(reference: &SecretRef) -> Self {
        Self::Denied {
            reference: reference.to_string(),
        }
    }

    /// [`Unavailable`](Self::Unavailable) for `reference`, with why.
    pub fn unavailable(reference: &SecretRef, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reference: reference.to_string(),
            reason: reason.into(),
        }
    }

    /// [`Invalid`](Self::Invalid) for `reference`, with why.
    pub fn invalid(reference: &SecretRef, reason: impl Into<String>) -> Self {
        Self::Invalid {
            reference: reference.to_string(),
            reason: reason.into(),
        }
    }

    /// Whether retrying later may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

impl From<SecretError> for ProviderError {
    fn from(error: SecretError) -> Self {
        if error.is_retryable() {
            ProviderError::Unavailable(error.to_string())
        } else {
            ProviderError::Other(error.to_string())
        }
    }
}

fn available_suffix(available: &[String]) -> String {
    if available.is_empty() {
        String::new()
    } else {
        format!(" (served: {})", available.join(", "))
    }
}

/// Resolves references of one scheme to their values.
#[async_trait]
pub trait SecretProvider: Provider {
    /// The scheme this provider serves, e.g. `env` or `keychain`.
    fn scheme(&self) -> &str;

    /// The value `reference` names.
    ///
    /// # Errors
    /// Returns [`SecretError`] if the reference has another scheme, or its secret
    /// isn't there, is empty, can't be read or can't be used.
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_no_part_of_the_value() {
        let value = SecretValue::new("hunter2-SENTINEL".to_owned());
        let shown = format!("{value:?} {:?}", Some(&value));
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("SENTINEL"), "{shown}");
        assert_eq!(value.expose(), "hunter2-SENTINEL");
    }

    #[test]
    fn redact_in_replaces_every_occurrence() {
        let value = SecretValue::new("tok-123".to_owned());
        assert_eq!(
            value.redact_in("Bearer tok-123 rejected; tok-123 expired"),
            "Bearer [secret removed] rejected; [secret removed] expired"
        );
        assert!(matches!(value.redact_in("nothing here"), Cow::Borrowed(_)));
        let empty = SecretValue::new(String::new());
        assert_eq!(empty.redact_in("abc"), "abc");
    }

    #[test]
    fn errors_name_only_the_reference() {
        let reference = SecretRef::parse("env:WAREHOUSE_TOKEN").expect("valid reference");
        assert_eq!(
            SecretError::not_found(&reference).to_string(),
            "secret(env:WAREHOUSE_TOKEN) is not set"
        );
        assert_eq!(
            SecretError::UnknownScheme {
                scheme: "vault".to_owned(),
                available: vec!["env".to_owned()],
            }
            .to_string(),
            "no secret provider serves `vault:` references (served: env)"
        );
        let unavailable = SecretError::unavailable(&reference, "keychain locked");
        assert!(unavailable.is_retryable());
        assert_eq!(
            ProviderError::from(unavailable),
            ProviderError::Unavailable(
                "secret(env:WAREHOUSE_TOKEN) could not be read: keychain locked".to_owned()
            )
        );
        assert!(matches!(
            ProviderError::from(SecretError::denied(&reference)),
            ProviderError::Other(_)
        ));
    }
}
