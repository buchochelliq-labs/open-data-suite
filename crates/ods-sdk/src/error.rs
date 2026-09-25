//! Errors providers return (ADR-0006 §4).

use ods_core::Capability;

/// Why a provider operation failed.
///
/// Messages must never contain secret values, including resolved credentials and
/// connection strings (AGENTS.md rule 9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// No provider of this kind is registered for the contract.
    #[error("no `{contract}` provider of kind `{kind}` is available{}", available_suffix(.available))]
    UnknownKind {
        /// Contract name.
        contract: &'static str,
        /// Requested kind.
        kind: String,
        /// Kinds that are registered.
        available: Vec<String>,
    },
    /// The provider's `settings` are invalid.
    #[error("invalid settings for provider `{instance}` (kind `{kind}`) at `{key}`: {message}")]
    InvalidSettings {
        /// Configured instance name.
        instance: String,
        /// Provider kind.
        kind: String,
        /// Offending settings key.
        key: String,
        /// What is wrong (never the value, which may be sensitive).
        message: String,
    },
    /// The operation needs a capability this provider does not have.
    #[error("the provider does not support `{0}`")]
    Unsupported(Capability),
    /// The operation conflicts with current state, e.g. a lease is no longer held.
    #[error("conflict: {0}")]
    Conflict(String),
    /// A transient failure; retrying later may succeed.
    #[error("temporarily unavailable: {0}")]
    Unavailable(String),
    /// Any other failure.
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Whether retrying the operation later may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ProviderError::Unavailable(_))
    }
}

fn available_suffix(available: &[String]) -> String {
    if available.is_empty() {
        String::new()
    } else {
        format!("; available kinds: {}", available.join(", "))
    }
}
