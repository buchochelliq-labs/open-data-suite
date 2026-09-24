//! `LockProvider`: mutual exclusion between concurrent ODS runs (#28, ADR-0006 §1).
//!
//! # Semantics
//! - A key is either free or held by one [`Lease`].
//! - [`acquire`](LockProvider::acquire) grants a lease on a free (or expired) key and
//!   returns `None` if the key is held, even by the same owner; use
//!   [`renew`](LockProvider::renew) to extend a lease you hold.
//! - With [`Capability::LeaseExpiry`], a lease expires `ttl` after it was granted or
//!   renewed, and the key is free again. Without it, `ttl` is ignored and leases last
//!   until released.
//! - With [`Capability::FencingTokens`], every grant on a key carries a token strictly
//!   greater than any earlier grant on that key, so storage can reject writes from a
//!   holder whose lease was lost.
//! - [`renew`](LockProvider::renew) and [`release`](LockProvider::release) fail with
//!   [`ProviderError::Conflict`] when the lease is no longer the one holding the key.
//!   Releasing a key that is already free succeeds (idempotent).

use std::fmt;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use ods_core::{Capability, SchemaVersion};
use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `lock_provider` contract.
pub const LOCK_PROVIDER: Contract = Contract {
    name: "lock_provider",
    version: SchemaVersion::new(0, 1),
};

/// Capabilities a lock provider may advertise.
pub const LOCK_CAPABILITIES: [Capability; 2] = [Capability::LeaseExpiry, Capability::FencingTokens];

/// The name of a lock, e.g. `state/jaffle_shop/prod`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LockKey(String);

impl LockKey {
    /// Maximum key length in bytes.
    pub const MAX_LEN: usize = 256;

    /// Validates a key: non-empty, at most [`Self::MAX_LEN`] bytes, no control characters.
    ///
    /// # Errors
    /// Returns a reason if the key is invalid.
    pub fn new(key: impl Into<String>) -> Result<Self, String> {
        let key = key.into();
        if key.is_empty() {
            return Err("lock key must not be empty".into());
        }
        if key.len() > Self::MAX_LEN {
            return Err(format!("lock key is longer than {} bytes", Self::MAX_LEN));
        }
        if key.chars().any(char::is_control) {
            return Err("lock key must not contain control characters".into());
        }
        Ok(Self(key))
    }

    /// The key as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LockKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A granted lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Lease {
    /// The locked key.
    pub key: LockKey,
    /// Who holds it, e.g. a run ID.
    pub owner: String,
    /// Fencing token; strictly increasing per key with [`Capability::FencingTokens`].
    pub token: u64,
    /// When the lease expires, with [`Capability::LeaseExpiry`]; `None` otherwise.
    pub expires_at: Option<SystemTime>,
}

/// Mutual exclusion between concurrent runs (contract [`LOCK_PROVIDER`]).
#[async_trait]
pub trait LockProvider: Provider {
    /// Grants a lease on `key` to `owner` if the key is free (or its lease expired);
    /// returns `Ok(None)` if it is held.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the lock store cannot be reached.
    async fn acquire(
        &self,
        key: &LockKey,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<Lease>, ProviderError>;

    /// Extends a held lease by `ttl` from now, keeping its token.
    ///
    /// # Errors
    /// Returns [`ProviderError::Conflict`] if `lease` no longer holds the key.
    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease, ProviderError>;

    /// Releases a held lease. Releasing an already free key succeeds.
    ///
    /// # Errors
    /// Returns [`ProviderError::Conflict`] if a different lease holds the key.
    async fn release(&self, lease: &Lease) -> Result<(), ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_keys_are_validated() {
        assert!(LockKey::new("state/proj/prod").is_ok());
        assert!(LockKey::new("").is_err());
        assert!(LockKey::new("a\nb").is_err());
        assert!(LockKey::new("x".repeat(LockKey::MAX_LEN + 1)).is_err());
    }
}
