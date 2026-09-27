//! In-memory [`StateStore`].

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::{SnapshotId, StateSnapshot};
use ods_sdk::contracts::state_store::{
    ProblemKind, ScopeSummary, SnapshotSummary, StateScope, StateStore, StoreCheck, StoreProblem,
    StoredSnapshot, check_readable,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

#[derive(Debug, Default)]
struct Inner {
    last_id: u64,
    /// Every snapshot, by id, with its scope.
    snapshots: BTreeMap<SnapshotId, (StateScope, StateSnapshot)>,
    heads: BTreeMap<StateScope, SnapshotId>,
}

/// An in-memory state store: the executable specification of [`StateStore`].
#[derive(Debug, Default)]
pub struct FakeStateStore {
    inner: Mutex<Inner>,
}

impl FakeStateStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Provider for FakeStateStore {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl StateStore for FakeStateStore {
    async fn latest(&self, scope: &StateScope) -> Result<Option<StoredSnapshot>, ProviderError> {
        let inner = self.inner();
        let Some(id) = inner.heads.get(scope).copied() else {
            return Ok(None);
        };
        let (_, snapshot) = &inner.snapshots[&id];
        check_readable(snapshot)?;
        Ok(Some(StoredSnapshot::new(id, snapshot.clone())))
    }

    async fn get(
        &self,
        scope: &StateScope,
        id: SnapshotId,
    ) -> Result<Option<StoredSnapshot>, ProviderError> {
        let inner = self.inner();
        match inner.snapshots.get(&id) {
            Some((owner, snapshot)) if owner == scope => {
                check_readable(snapshot)?;
                Ok(Some(StoredSnapshot::new(id, snapshot.clone())))
            }
            _ => Ok(None),
        }
    }

    async fn commit(
        &self,
        scope: &StateScope,
        snapshot: &StateSnapshot,
    ) -> Result<SnapshotId, ProviderError> {
        check_readable(snapshot)?;
        let mut inner = self.inner();
        let head = inner.heads.get(scope).copied();
        if head != snapshot.parent {
            return Err(ProviderError::Conflict(format!(
                "`{scope}` is at {}, not {}",
                head.map_or_else(|| "no snapshot".to_owned(), |h| h.to_string()),
                snapshot
                    .parent
                    .map_or_else(|| "no snapshot".to_owned(), |p| p.to_string())
            )));
        }
        inner.last_id += 1;
        let id = SnapshotId(inner.last_id);
        inner
            .snapshots
            .insert(id, (scope.clone(), snapshot.clone()));
        inner.heads.insert(scope.clone(), id);
        Ok(id)
    }

    async fn history(
        &self,
        scope: &StateScope,
        limit: usize,
    ) -> Result<Vec<SnapshotSummary>, ProviderError> {
        let inner = self.inner();
        Ok(inner
            .snapshots
            .iter()
            .rev()
            .filter(|(_, (owner, _))| owner == scope)
            .take(limit)
            .map(|(id, (_, snapshot))| {
                SnapshotSummary::of(&StoredSnapshot::new(*id, snapshot.clone()))
            })
            .collect())
    }

    async fn check(&self) -> Result<StoreCheck, ProviderError> {
        let inner = self.inner();
        let mut problems = Vec::new();
        let mut counts: BTreeMap<&StateScope, usize> = BTreeMap::new();
        for (id, (scope, snapshot)) in &inner.snapshots {
            *counts.entry(scope).or_default() += 1;
            if let Err(e) = check_readable(snapshot) {
                problems.push(StoreProblem::new(
                    ProblemKind::UnreadableSnapshot,
                    format!("snapshot {id} of `{scope}`: {e}"),
                ));
            }
            if let Some(parent) = snapshot.parent
                && inner.snapshots.get(&parent).is_none_or(|(s, _)| s != scope)
            {
                problems.push(StoreProblem::new(
                    ProblemKind::BrokenChain,
                    format!("snapshot {id} of `{scope}` follows {parent}, which isn't in it"),
                ));
            }
        }
        for (scope, head) in &inner.heads {
            if inner.snapshots.get(head).is_none_or(|(s, _)| s != scope) {
                problems.push(StoreProblem::new(
                    ProblemKind::DanglingHead,
                    format!("`{scope}` points at snapshot {head}, which isn't in it"),
                ));
            }
        }
        let scopes = inner
            .heads
            .iter()
            .map(|(scope, head)| {
                ScopeSummary::new(
                    scope.as_str(),
                    Some(*head),
                    counts.get(scope).copied().unwrap_or_default(),
                )
            })
            .collect();
        Ok(StoreCheck::new(None, scopes, problems))
    }
}
