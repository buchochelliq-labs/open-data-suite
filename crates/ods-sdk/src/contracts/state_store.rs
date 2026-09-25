//! `StateStore`: where the last successful state lives (#11, #25, ADR-0013).
//!
//! # Semantics
//! - A store keeps, per [`StateScope`], a chain of immutable [`StateSnapshot`]s and a
//!   head: the latest committed one.
//! - [`commit`](StateStore::commit) is a compare-and-swap. It succeeds only if the scope's
//!   head is the snapshot's [`parent`](StateSnapshot::parent), where `None` means the
//!   scope must be empty. Otherwise it fails with [`ProviderError::Conflict`] and changes
//!   nothing. The new snapshot and head move together, or neither does.
//! - Snapshot ids are unique within a store and increase with every commit.
//! - Scopes are independent: a snapshot from one is never visible in another.
//! - A snapshot is returned exactly as committed. A store refuses to return a snapshot
//!   whose `schema_version` this build can't read, rather than guessing.

use std::fmt;

use async_trait::async_trait;
use ods_core::SchemaVersion;
use ods_core::state::{STATE_SCHEMA_VERSION, SnapshotId, StateSnapshot, Timestamp};
use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `state_store` contract.
pub const STATE_STORE: Contract = Contract {
    name: "state_store",
    version: SchemaVersion::new(0, 1),
};

/// Whose state: `<project>/<environment>`, e.g. `jaffle_shop/prod`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct StateScope(String);

impl StateScope {
    /// Maximum scope length in bytes.
    pub const MAX_LEN: usize = 256;

    /// The scope of a project in an environment.
    ///
    /// # Errors
    /// Returns a reason if either part is empty, contains `/` or control characters, or
    /// the scope is longer than [`Self::MAX_LEN`] bytes.
    pub fn new(project: &str, environment: &str) -> Result<Self, String> {
        for (what, part) in [("project", project), ("environment", environment)] {
            if part.is_empty() {
                return Err(format!("the {what} name must not be empty"));
            }
            if part.contains('/') || part.chars().any(char::is_control) {
                return Err(format!(
                    "the {what} name `{}` must not contain `/` or control characters",
                    part.escape_debug()
                ));
            }
        }
        let scope = format!("{project}/{environment}");
        if scope.len() > Self::MAX_LEN {
            return Err(format!("a state scope is at most {} bytes", Self::MAX_LEN));
        }
        Ok(Self(scope))
    }

    /// The scope as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StateScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A committed snapshot and its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct StoredSnapshot {
    /// Its id in the store.
    pub id: SnapshotId,
    /// The snapshot, as committed.
    pub snapshot: StateSnapshot,
}

impl StoredSnapshot {
    /// A committed snapshot, for stores to return.
    pub fn new(id: SnapshotId, snapshot: StateSnapshot) -> Self {
        Self { id, snapshot }
    }
}

/// A line of history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SnapshotSummary {
    /// Its id.
    pub id: SnapshotId,
    /// The snapshot it follows.
    pub parent: Option<SnapshotId>,
    /// When it was committed.
    pub created_at: Timestamp,
    /// The run it records.
    pub run_id: String,
    /// How many nodes it records.
    pub nodes: usize,
}

impl SnapshotSummary {
    /// A history line, for stores that read it without the whole document.
    pub fn new(
        id: SnapshotId,
        parent: Option<SnapshotId>,
        created_at: Timestamp,
        run_id: impl Into<String>,
        nodes: usize,
    ) -> Self {
        Self {
            id,
            parent,
            created_at,
            run_id: run_id.into(),
            nodes,
        }
    }

    /// Summarizes a committed snapshot.
    pub fn of(stored: &StoredSnapshot) -> Self {
        Self {
            id: stored.id,
            parent: stored.snapshot.parent,
            created_at: stored.snapshot.created_at,
            run_id: stored.snapshot.run_id.clone(),
            nodes: stored.snapshot.nodes.len(),
        }
    }
}

/// Checks a stored document's version before returning it.
///
/// # Errors
/// Returns [`ProviderError::Other`] if this build can't read it.
pub fn check_readable(snapshot: &StateSnapshot) -> Result<(), ProviderError> {
    if STATE_SCHEMA_VERSION.can_read(snapshot.schema_version) {
        Ok(())
    } else {
        Err(ProviderError::Other(format!(
            "the stored state is schema version {}.{}, which this ODS (reads {}.{}) can't read; upgrade ODS",
            snapshot.schema_version.major,
            snapshot.schema_version.minor,
            STATE_SCHEMA_VERSION.major,
            STATE_SCHEMA_VERSION.minor
        )))
    }
}

/// Keeps the last successful state (contract [`STATE_STORE`]).
#[async_trait]
pub trait StateStore: Provider {
    /// The scope's head, if anything was committed.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the store can't be read, or holds a newer schema.
    async fn latest(&self, scope: &StateScope) -> Result<Option<StoredSnapshot>, ProviderError>;

    /// A snapshot of this scope by id.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the store can't be read, or holds a newer schema.
    async fn get(
        &self,
        scope: &StateScope,
        id: SnapshotId,
    ) -> Result<Option<StoredSnapshot>, ProviderError>;

    /// Commits `snapshot` as the scope's new head if the head is still
    /// `snapshot.parent`, and returns its id.
    ///
    /// # Errors
    /// Returns [`ProviderError::Conflict`] if the head moved (or the scope isn't empty
    /// when `parent` is `None`); nothing is written.
    async fn commit(
        &self,
        scope: &StateScope,
        snapshot: &StateSnapshot,
    ) -> Result<SnapshotId, ProviderError>;

    /// The scope's snapshots, newest first, at most `limit`.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the store can't be read.
    async fn history(
        &self,
        scope: &StateScope,
        limit: usize,
    ) -> Result<Vec<SnapshotSummary>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_validated() {
        assert_eq!(
            StateScope::new("jaffle", "prod").unwrap().as_str(),
            "jaffle/prod"
        );
        assert!(StateScope::new("", "prod").is_err());
        assert!(StateScope::new("a/b", "prod").is_err());
        assert!(StateScope::new("a", "p\nrod").is_err());
        assert!(StateScope::new(&"x".repeat(300), "prod").is_err());
    }

    #[test]
    fn newer_documents_are_refused() {
        let mut snapshot = StateSnapshot::new(
            None,
            Timestamp::from_unix(0),
            "r",
            std::collections::BTreeMap::default(),
        );
        assert!(check_readable(&snapshot).is_ok());
        snapshot.schema_version =
            SchemaVersion::new(STATE_SCHEMA_VERSION.major, STATE_SCHEMA_VERSION.minor + 1);
        assert!(check_readable(&snapshot).is_err());
    }
}
