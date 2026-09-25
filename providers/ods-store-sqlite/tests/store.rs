//! The SQLite store passes the `StateStore` conformance suite, survives reopening, and
//! lets exactly one of two racing commits win.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use ods_core::state::{Fingerprint, NodeState, StateSnapshot, Timestamp};
use ods_sdk::ProviderError;
use ods_sdk::conformance::state_store::{StateStoreHarness, run};
use ods_sdk::contracts::state_store::{StateScope, StateStore};
use ods_store_sqlite::SqliteStateStore;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh database file path, removed when dropped.
struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ods-store-sqlite-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir.join("nested").join("state.db"))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent().and_then(|p| p.parent()) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct FileHarness(std::sync::Mutex<Vec<TempDb>>);

#[async_trait]
impl StateStoreHarness for FileHarness {
    async fn store(&self) -> Arc<dyn StateStore> {
        let db = TempDb::new("conformance");
        let store = SqliteStateStore::open(&db.0).await.unwrap();
        self.0.lock().unwrap().push(db);
        Arc::new(store)
    }
}

struct MemoryHarness;

#[async_trait]
impl StateStoreHarness for MemoryHarness {
    async fn store(&self) -> Arc<dyn StateStore> {
        Arc::new(SqliteStateStore::in_memory().await.unwrap())
    }
}

#[tokio::test]
async fn a_file_database_conforms() {
    let report = run(&FileHarness(std::sync::Mutex::default())).await;
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

#[tokio::test]
async fn an_in_memory_database_conforms() {
    let report = run(&MemoryHarness).await;
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

fn snapshot(parent: Option<ods_core::state::SnapshotId>, run: &str) -> StateSnapshot {
    StateSnapshot::new(
        parent,
        Timestamp::from_unix(1),
        run,
        BTreeMap::from([(
            "model.p.a".to_owned(),
            NodeState::new(
                Fingerprint::from_content([("file", run)]),
                Timestamp::from_unix(1),
                run,
                BTreeMap::new(),
            ),
        )]),
    )
}

#[tokio::test]
async fn state_survives_reopening_and_migrations_are_recorded() {
    let db = TempDb::new("reopen");
    let scope = StateScope::new("p", "dev").unwrap();
    let id = {
        let store = SqliteStateStore::open(&db.0).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 1);
        store
            .commit(&scope, &snapshot(None, "run-1"))
            .await
            .unwrap()
    };
    let store = SqliteStateStore::open(&db.0).await.unwrap();
    assert_eq!(
        store.schema_version().await.unwrap(),
        1,
        "migrations run once"
    );
    assert_eq!(store.latest(&scope).await.unwrap().unwrap().id, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn of_two_racing_commits_exactly_one_wins() {
    let db = TempDb::new("race");
    let scope = StateScope::new("p", "prod").unwrap();
    let first = SqliteStateStore::open(&db.0).await.unwrap();
    let base = first
        .commit(&scope, &snapshot(None, "run-0"))
        .await
        .unwrap();
    // Two separate connections pools, as two `ods` processes would have.
    let second = SqliteStateStore::open(&db.0).await.unwrap();
    for round in 0..10 {
        let head = first.latest(&scope).await.unwrap().unwrap().id;
        let (mine, theirs) = (
            snapshot(Some(head), &format!("a-{round}")),
            snapshot(Some(head), &format!("b-{round}")),
        );
        let (a, b) = tokio::join!(first.commit(&scope, &mine), second.commit(&scope, &theirs));
        let wins = [a.is_ok(), b.is_ok()].iter().filter(|w| **w).count();
        assert_eq!(wins, 1, "round {round}: {a:?} {b:?}");
        for loser in [a, b].into_iter().filter_map(Result::err) {
            assert!(matches!(loser, ProviderError::Conflict(_)), "{loser:?}");
        }
    }
    let history = first.history(&scope, 100).await.unwrap();
    assert_eq!(history.len(), 11, "one base and one winner per round");
    assert!(history.iter().all(|h| h.id >= base));
}

#[tokio::test]
async fn a_database_from_a_newer_ods_is_refused() {
    let db = TempDb::new("newer");
    drop(SqliteStateStore::open(&db.0).await.unwrap());
    // Record a migration this build doesn't know, as a newer ODS would.
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.0.display()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO ods_migrations (version, applied_at) VALUES (99, 'later')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let error = SqliteStateStore::open(&db.0).await.unwrap_err();
    assert!(error.to_string().contains("upgrade ODS"), "{error}");
}
