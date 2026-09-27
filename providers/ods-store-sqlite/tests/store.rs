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
use ods_sdk::contracts::state_store::{ProblemKind, StateScope, StateStore};
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
    assert_eq!(report.passed.len(), 7, "{report:?}");
}

#[tokio::test]
async fn an_in_memory_database_conforms() {
    let report = run(&MemoryHarness).await;
    assert_eq!(report.passed.len(), 7, "{report:?}");
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

/// A raw connection to the database, as another program (or a disk fault) would
/// change it: without ODS's checks, or its foreign keys.
async fn raw(db: &TempDb) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db.0)
        .foreign_keys(false);
    sqlx::SqlitePool::connect_with(options).await.unwrap()
}

#[tokio::test]
async fn a_file_that_isnt_a_database_is_damaged() {
    let db = TempDb::new("garbage");
    std::fs::create_dir_all(db.0.parent().unwrap()).unwrap();
    std::fs::write(&db.0, vec![0x5a; 8192]).unwrap();
    let error = SqliteStateStore::open(&db.0).await.unwrap_err();
    assert!(matches!(error, ProviderError::Corrupt(_)), "{error:?}");
    let error = SqliteStateStore::open_existing(&db.0).await.unwrap_err();
    assert!(matches!(error, ProviderError::Corrupt(_)), "{error:?}");
    // Nothing was written over it.
    assert_eq!(std::fs::read(&db.0).unwrap(), vec![0x5a; 8192]);
}

#[tokio::test]
async fn damaged_records_are_errors_and_the_check_names_them() {
    let db = TempDb::new("damaged-records");
    let (a, b) = (
        StateScope::new("p", "a").unwrap(),
        StateScope::new("p", "b").unwrap(),
    );
    let store = SqliteStateStore::open(&db.0).await.unwrap();
    let first = store.commit(&a, &snapshot(None, "run-1")).await.unwrap();
    store
        .commit(&a, &snapshot(Some(first), "run-2"))
        .await
        .unwrap();
    store.commit(&b, &snapshot(None, "run-b")).await.unwrap();
    assert!(store.check().await.unwrap().is_sound());

    let pool = raw(&db).await;
    for statement in [
        "UPDATE snapshots SET document = '{' WHERE run_id = 'run-2'",
        "UPDATE snapshots SET parent = 999 WHERE run_id = 'run-b'",
        "UPDATE heads SET snapshot_id = 998 WHERE scope = 'p/b'",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let error = store.latest(&a).await.unwrap_err();
    assert!(matches!(error, ProviderError::Corrupt(_)), "{error:?}");
    let check = store.check().await.unwrap();
    let kinds: Vec<ProblemKind> = check.problems.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            ProblemKind::UnreadableSnapshot,
            ProblemKind::DanglingHead,
            ProblemKind::BrokenChain
        ],
        "{check:#?}"
    );
    assert!(check.problems[0].detail.contains("p/a"), "{check:#?}");
    // Checking reads only.
    let read_only = SqliteStateStore::open_existing(&db.0).await.unwrap();
    assert_eq!(read_only.check().await.unwrap(), check);
}

#[tokio::test]
async fn a_backup_is_a_state_database_and_set_aside_starts_afresh() {
    let db = TempDb::new("backup");
    let scope = StateScope::new("p", "dev").unwrap();
    let store = SqliteStateStore::open(&db.0).await.unwrap();
    let id = store
        .commit(&scope, &snapshot(None, "run-1"))
        .await
        .unwrap();
    let copy = db.0.with_file_name("copy.db");
    store.backup(&copy).await.unwrap();
    assert!(store.backup(&copy).await.is_err(), "never overwrites");
    let restored = SqliteStateStore::open(&copy).await.unwrap();
    assert_eq!(restored.latest(&scope).await.unwrap().unwrap().id, id);
    store.close().await;
    restored.close().await;

    let moved = SqliteStateStore::set_aside(&db.0).unwrap();
    assert!(
        !moved.is_empty() && moved.iter().all(|p| p.is_file()),
        "{moved:?}"
    );
    assert!(!db.0.exists());
    let fresh = SqliteStateStore::open(&db.0).await.unwrap();
    assert!(fresh.latest(&scope).await.unwrap().is_none());
    // What was set aside is still the old state.
    let old = SqliteStateStore::open_existing(&moved[0]).await.unwrap();
    assert_eq!(old.latest(&scope).await.unwrap().unwrap().id, id);
}
