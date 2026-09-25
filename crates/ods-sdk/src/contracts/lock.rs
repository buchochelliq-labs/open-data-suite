//! `LockProvider`: mutual exclusion between concurrent ODS runs (#28, ADR-0006 §1).
//!
//! # Semantics
//! - A key is either free or held by one [`Lease`].
//! - [`acquire`](LockProvider::acquire) grants a lease on a free (or expired) key and
//!   returns `None` if the key is held, even by the same owner; use
//!   [`renew`](LockProvider::renew) to extend a lease you hold.
//! - Every grant on a key has a [`token`](Lease::token) that no other grant on that key
//!   has had, even for the same owner. It identifies the grant: a lease from an earlier
//!   grant never matches a later one, so a retried run that reuses its ID cannot renew
//!   or release its successor's lock.
//! - With [`Capability::FencingTokens`], tokens are also strictly increasing per key, so
//!   storage can reject writes from a holder whose lease was lost.
//! - With [`Capability::LeaseExpiry`], a lease granted or renewed at `t` holds the key
//!   until just before `t + ttl`: at `t + ttl` (inclusive) it has expired and the key is
//!   free. Without it, `ttl` is ignored, `expires_at` is `None`, and leases last until
//!   released.
//! - [`renew`](LockProvider::renew) fails with [`ProviderError::Conflict`] unless the
//!   lease still holds the key; an expired lease cannot be revived, even if nobody took
//!   the key over (the holder must acquire again and get a new token).
//! - [`release`](LockProvider::release) fails with [`ProviderError::Conflict`] if a
//!   different grant holds the key. Releasing a key that is free, including because the
//!   lease expired, succeeds (idempotent).

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

/// How long a lease lasts without renewal: from [`LeaseTtl::MIN`] to [`LeaseTtl::MAX`].
///
/// Bounded so providers never overflow a timestamp or grant an already expired lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseTtl(Duration);

impl LeaseTtl {
    /// The shortest lease: one second, the coarsest precision a lock store may have.
    pub const MIN: Duration = Duration::from_secs(1);
    /// The longest lease: one day. Longer runs renew.
    pub const MAX: Duration = Duration::from_secs(24 * 60 * 60);

    /// Validates a TTL.
    ///
    /// # Errors
    /// Returns a reason if `ttl` is outside [`Self::MIN`]..=[`Self::MAX`].
    pub fn new(ttl: Duration) -> Result<Self, String> {
        if (Self::MIN..=Self::MAX).contains(&ttl) {
            Ok(Self(ttl))
        } else {
            Err(format!(
                "lease ttl must be between {}s and {}s",
                Self::MIN.as_secs(),
                Self::MAX.as_secs()
            ))
        }
    }

    /// The duration.
    pub fn get(self) -> Duration {
        self.0
    }
}

/// A granted lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Lease {
    /// The locked key.
    pub key: LockKey,
    /// Who holds it, e.g. a run ID.
    pub owner: String,
    /// Identifies this grant: unique per key. Strictly increasing per key (a fencing
    /// token) with [`Capability::FencingTokens`].
    pub token: u64,
    /// When the lease expires, with [`Capability::LeaseExpiry`]; `None` otherwise.
    pub expires_at: Option<SystemTime>,
}

impl Lease {
    /// A lease, for providers to return.
    pub fn new(
        key: LockKey,
        owner: impl Into<String>,
        token: u64,
        expires_at: Option<SystemTime>,
    ) -> Self {
        Self {
            key,
            owner: owner.into(),
            token,
            expires_at,
        }
    }

    /// Whether `other` is the same grant: same key and token.
    pub fn same_grant(&self, other: &Lease) -> bool {
        self.key == other.key && self.token == other.token
    }
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
        ttl: LeaseTtl,
    ) -> Result<Option<Lease>, ProviderError>;

    /// Extends a held lease to `ttl` from now, keeping its token.
    ///
    /// # Errors
    /// Returns [`ProviderError::Conflict`] if `lease` no longer holds the key, including
    /// when it expired.
    async fn renew(&self, lease: &Lease, ttl: LeaseTtl) -> Result<Lease, ProviderError>;

    /// Releases a held lease. Releasing a free key (never held, released or expired)
    /// succeeds.
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

    #[test]
    fn lease_ttls_are_bounded() {
        assert!(LeaseTtl::new(Duration::ZERO).is_err());
        assert!(LeaseTtl::new(Duration::MAX).is_err());
        assert!(LeaseTtl::new(LeaseTtl::MIN).is_ok());
        assert!(LeaseTtl::new(LeaseTtl::MAX).is_ok());
    }
}
