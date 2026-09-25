//! Conformance suite for [`StateStore`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use ods_core::state::{
    DataVersion, Exactness, Fingerprint, NodeState, SnapshotId, StateSnapshot, Timestamp,
};

use super::Report;
use crate::contracts::state_store::{StateScope, StateStore};
use crate::error::ProviderError;

/// What the suite needs from a store under test.
#[async_trait]
pub trait StateStoreHarness: Send + Sync {
    /// A fresh, empty store. Called once per case.
    async fn store(&self) -> Arc<dyn StateStore>;
}

fn scope(environment: &str) -> StateScope {
    StateScope::new("suite", environment).expect("suite scopes are valid")
}

fn snapshot(parent: Option<SnapshotId>, run: &str) -> StateSnapshot {
    let node = NodeState::new(
        Fingerprint::from_content([("file", run), ("config", "{}")]),
        Timestamp::from_unix(1_000),
        run,
        BTreeMap::from([
            (
                "source.suite.raw.orders".to_owned(),
                Some(DataVersion::new(
                    "2026-01-01T00:00:00Z",
                    Exactness::Semantic,
                    "sources.json",
                )),
            ),
            ("source.suite.raw.users".to_owned(), None),
        ]),
    );
    StateSnapshot::new(
        parent,
        Timestamp::from_unix(2_000),
        run,
        BTreeMap::from([("model.suite.orders".to_owned(), node)]),
    )
}

fn is_conflict<T>(result: &Result<T, ProviderError>) -> bool {
    matches!(result, Err(ProviderError::Conflict(_)))
}

async fn empty_store_has_nothing(store: &dyn StateStore) {
    let s = scope("empty");
    assert!(store.latest(&s).await.unwrap().is_none(), "empty: latest");
    assert!(
        store.history(&s, 10).await.unwrap().is_empty(),
        "empty: history"
    );
    assert!(
        store.get(&s, SnapshotId(1)).await.unwrap().is_none(),
        "empty: get"
    );
}

async fn commits_round_trip_exactly(store: &dyn StateStore) {
    let s = scope("round-trip");
    let first = snapshot(None, "run-1");
    let id = store
        .commit(&s, &first)
        .await
        .expect("round trip: first commit");
    let latest = store.latest(&s).await.unwrap().expect("round trip: head");
    assert_eq!(latest.id, id, "round trip: head id");
    assert_eq!(
        latest.snapshot, first,
        "round trip: the snapshot comes back as committed"
    );
    assert_eq!(
        store.get(&s, id).await.unwrap().map(|g| g.snapshot),
        Some(first),
        "round trip: get"
    );
}

async fn stale_parents_conflict_and_change_nothing(store: &dyn StateStore) {
    let s = scope("stale");
    let first = store.commit(&s, &snapshot(None, "run-1")).await.unwrap();
    assert!(
        is_conflict(&store.commit(&s, &snapshot(None, "run-x")).await),
        "stale: an empty-scope commit on a non-empty scope conflicts"
    );
    let second = store
        .commit(&s, &snapshot(Some(first), "run-2"))
        .await
        .unwrap();
    assert!(second > first, "stale: ids increase");
    assert!(
        is_conflict(&store.commit(&s, &snapshot(Some(first), "run-y")).await),
        "stale: a commit on an old head conflicts"
    );
    let head = store.latest(&s).await.unwrap().unwrap();
    assert_eq!(head.id, second, "stale: head unchanged after conflicts");
    assert_eq!(head.snapshot.run_id, "run-2");
    assert_eq!(
        store.history(&s, 10).await.unwrap().len(),
        2,
        "stale: conflicting commits leave no trace"
    );
}

async fn history_is_newest_first_and_limited(store: &dyn StateStore) {
    let s = scope("history");
    let mut parent = None;
    let mut ids = Vec::new();
    for run in ["run-1", "run-2", "run-3"] {
        let id = store.commit(&s, &snapshot(parent, run)).await.unwrap();
        ids.push(id);
        parent = Some(id);
    }
    let history = store.history(&s, 10).await.unwrap();
    let got: Vec<SnapshotId> = history.iter().map(|h| h.id).collect();
    assert_eq!(got, [ids[2], ids[1], ids[0]], "history: newest first");
    assert_eq!(history[0].parent, Some(ids[1]), "history: parents");
    assert_eq!(history[0].run_id, "run-3");
    assert_eq!(history[0].nodes, 1);
    assert_eq!(
        store.history(&s, 2).await.unwrap().len(),
        2,
        "history: limit"
    );
}

async fn scopes_are_isolated(store: &dyn StateStore) {
    let (a, b) = (scope("a"), scope("b"));
    let in_a = store.commit(&a, &snapshot(None, "run-a")).await.unwrap();
    assert!(
        store.latest(&b).await.unwrap().is_none(),
        "isolation: b is empty"
    );
    assert!(
        store.get(&b, in_a).await.unwrap().is_none(),
        "isolation: get across scopes"
    );
    assert!(
        is_conflict(&store.commit(&b, &snapshot(Some(in_a), "run-b")).await),
        "isolation: a parent from another scope conflicts"
    );
    store
        .commit(&b, &snapshot(None, "run-b"))
        .await
        .expect("isolation: b starts its own chain");
    assert_eq!(store.latest(&a).await.unwrap().unwrap().id, in_a);
}

/// Runs every case against fresh stores from `harness`.
///
/// # Panics
/// Panics with the case name when a store breaks the contract.
pub async fn run(harness: &dyn StateStoreHarness) -> Report {
    let mut report = Report::default();
    empty_store_has_nothing(harness.store().await.as_ref()).await;
    report.passed.push("empty_store_has_nothing");
    commits_round_trip_exactly(harness.store().await.as_ref()).await;
    report.passed.push("commits_round_trip_exactly");
    stale_parents_conflict_and_change_nothing(harness.store().await.as_ref()).await;
    report
        .passed
        .push("stale_parents_conflict_and_change_nothing");
    history_is_newest_first_and_limited(harness.store().await.as_ref()).await;
    report.passed.push("history_is_newest_first_and_limited");
    scopes_are_isolated(harness.store().await.as_ref()).await;
    report.passed.push("scopes_are_isolated");
    report
}
