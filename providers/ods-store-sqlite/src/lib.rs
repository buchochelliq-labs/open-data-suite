//! A [`StateStore`] in a local SQLite file (#25, ADR-0013).
//!
//! - Snapshots are stored as versioned JSON documents, one row each, with a `heads` row
//!   per scope.
//! - A commit inserts the snapshot and moves the head in one transaction. The head only
//!   moves if it still points at the snapshot's parent, so a concurrent or stale commit
//!   fails with [`ProviderError::Conflict`] and writes nothing (AGENTS.md rule 5).
//! - WAL mode lets readers run while a commit is in progress; writers wait on each
//!   other up to a busy timeout.
//! - The database schema is migrated forward in a transaction on open. A database
//!   written by a newer ODS is refused.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::{SnapshotId, StateSnapshot, Timestamp};
use ods_sdk::contracts::state_store::{
    SnapshotSummary, StateScope, StateStore, StoredSnapshot, check_readable,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, Sqlite, SqliteConnection};

/// The `kind` this store is configured as.
pub const KIND: &str = "sqlite";

/// Database schema migrations, in order. Never edit one that has shipped; add another.
const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    "CREATE TABLE snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        scope TEXT NOT NULL,
        parent INTEGER,
        created_at TEXT NOT NULL,
        run_id TEXT NOT NULL,
        nodes INTEGER NOT NULL,
        schema_major INTEGER NOT NULL,
        schema_minor INTEGER NOT NULL,
        document TEXT NOT NULL
    );
    CREATE INDEX snapshots_by_scope ON snapshots (scope, id);
    CREATE TABLE heads (
        scope TEXT PRIMARY KEY,
        snapshot_id INTEGER NOT NULL REFERENCES snapshots (id)
    );",
)];

/// How long a writer waits for another writer before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

fn db_error(error: &sqlx::Error) -> ProviderError {
    match error {
        sqlx::Error::Database(e)
            if e.message().contains("locked") || e.message().contains("busy") =>
        {
            ProviderError::Unavailable(format!("the state database is busy: {}", e.message()))
        }
        other => ProviderError::Other(format!("state database: {other}")),
    }
}

/// A state store in a SQLite file.
#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: SqlitePool,
    location: String,
}

impl SqliteStateStore {
    /// Opens (creating if needed) the database at `path` and migrates it.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the file can't be opened, or was written by a newer
    /// ODS.
    pub async fn open(path: &Path) -> Result<Self, ProviderError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| {
                ProviderError::Other(format!("can't create `{}`: {e}", dir.display()))
            })?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        Self::connect(options, path.display().to_string()).await
    }

    /// A private in-memory database, for tests.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if SQLite fails to start.
    pub async fn in_memory() -> Result<Self, ProviderError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| db_error(&e))?
            .foreign_keys(true);
        Self::connect(options, ":memory:".to_owned()).await
    }

    async fn connect(
        options: SqliteConnectOptions,
        location: String,
    ) -> Result<Self, ProviderError> {
        // In memory, every connection would be its own database: use one.
        let connections = if location == ":memory:" { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(connections)
            .connect_with(options)
            .await
            .map_err(|e| db_error(&e))?;
        let store = Self { pool, location };
        store.migrate().await?;
        Ok(store)
    }

    /// Where the database is.
    pub fn location(&self) -> &str {
        &self.location
    }

    /// The database schema version: the last migration applied.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the database can't be read.
    pub async fn schema_version(&self) -> Result<i64, ProviderError> {
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(version), 0) FROM ods_migrations")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error(&e))
    }

    /// Runs `BEGIN IMMEDIATE` on a pooled connection: the write lock is taken up front,
    /// so a transaction never has to upgrade from reading to writing mid-way.
    async fn begin_write(&self) -> Result<WriteTx, ProviderError> {
        let mut conn = self.pool.acquire().await.map_err(|e| db_error(&e))?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *conn)
            .await
            .map_err(|e| db_error(&e))?;
        Ok(WriteTx(Some(conn)))
    }

    /// Commits if `result` is `Ok`, rolls back otherwise. A connection whose transaction
    /// can't be ended is closed rather than returned to the pool.
    async fn finish<T>(
        mut tx: WriteTx,
        result: Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        let Some(mut conn) = tx.0.take() else {
            return result;
        };
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(e) = sqlx::query(end).execute(&mut *conn).await {
            drop(conn.detach());
            return result.and(Err(db_error(&e)));
        }
        result
    }

    async fn migrate(&self) -> Result<(), ProviderError> {
        // Two openers can race to create the migrations table, before either holds a
        // write lock; `IF NOT EXISTS` makes that harmless.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ods_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| db_error(&e))?;
        let mut conn = self.begin_write().await?;
        let result = match conn.conn() {
            Ok(c) => Self::migrate_in(c, &self.location).await,
            Err(e) => Err(e),
        };
        Self::finish(conn, result).await
    }

    async fn migrate_in(conn: &mut SqliteConnection, location: &str) -> Result<(), ProviderError> {
        let applied: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM ods_migrations")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| db_error(&e))?;
        let known = MIGRATIONS.last().map_or(0, |(v, _)| *v);
        if applied > known {
            return Err(ProviderError::Other(format!(
                "the state database `{location}` has schema version {applied}, but this ODS knows up to {known}; upgrade ODS"
            )));
        }
        // Collected first: an iterator adaptor's closure held across `.await` makes the
        // future's `Send` bound unprovable (a known `sqlx` limitation).
        let pending: Vec<(i64, &str)> = MIGRATIONS
            .iter()
            .copied()
            .filter(|(v, _)| *v > applied)
            .collect();
        for (version, sql) in pending {
            // One statement at a time: `sqlx::raw_sql` can't be awaited in a `Send`
            // future (a known `sqlx` limitation). Migrations hold no `;` in literals.
            for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                sqlx::query(statement)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| db_error(&e))?;
            }
            sqlx::query("INSERT INTO ods_migrations (version, applied_at) VALUES (?, ?)")
                .bind(version)
                .bind(Timestamp::now().to_string())
                .execute(&mut *conn)
                .await
                .map_err(|e| db_error(&e))?;
        }
        Ok(())
    }

    fn decode(id: i64, document: &str) -> Result<StoredSnapshot, ProviderError> {
        let snapshot: StateSnapshot = serde_json::from_str(document)
            .map_err(|e| ProviderError::Other(format!("state snapshot {id} can't be read: {e}")))?;
        check_readable(&snapshot)?;
        Ok(StoredSnapshot::new(SnapshotId(unsigned(id)?), snapshot))
    }

    async fn commit_in(
        conn: &mut SqliteConnection,
        scope: &StateScope,
        snapshot: &StateSnapshot,
        document: &str,
    ) -> Result<SnapshotId, ProviderError> {
        let parent = snapshot.parent.map(signed).transpose()?;
        let head: Option<i64> = sqlx::query_scalar("SELECT snapshot_id FROM heads WHERE scope = ?")
            .bind(scope.as_str())
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| db_error(&e))?;
        if head != parent {
            let show =
                |v: Option<i64>| v.map_or_else(|| "no snapshot".to_owned(), |v| v.to_string());
            return Err(ProviderError::Conflict(format!(
                "`{scope}` is at {}, not {}: another run committed first",
                show(head),
                show(parent)
            )));
        }
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO snapshots (scope, parent, created_at, run_id, nodes, schema_major, schema_minor, document)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(scope.as_str())
        .bind(parent)
        .bind(snapshot.created_at.to_string())
        .bind(&snapshot.run_id)
        .bind(i64::try_from(snapshot.nodes.len()).unwrap_or(i64::MAX))
        .bind(i64::from(snapshot.schema_version.major))
        .bind(i64::from(snapshot.schema_version.minor))
        .bind(document)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_error(&e))?;
        sqlx::query(
            "INSERT INTO heads (scope, snapshot_id) VALUES (?, ?)
             ON CONFLICT (scope) DO UPDATE SET snapshot_id = excluded.snapshot_id",
        )
        .bind(scope.as_str())
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_error(&e))?;
        Ok(SnapshotId(unsigned(id)?))
    }
}

/// A connection inside `BEGIN IMMEDIATE`. If it is dropped before
/// [`SqliteStateStore::finish`] (e.g. the future was cancelled), the connection is
/// closed instead of going back to the pool, and SQLite rolls the transaction back.
struct WriteTx(Option<PoolConnection<Sqlite>>);

impl WriteTx {
    fn conn(&mut self) -> Result<&mut SqliteConnection, ProviderError> {
        self.0
            .as_deref_mut()
            .ok_or_else(|| ProviderError::Other("the transaction has ended".into()))
    }
}

impl Drop for WriteTx {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            drop(conn.detach());
        }
    }
}

fn unsigned(id: i64) -> Result<u64, ProviderError> {
    u64::try_from(id).map_err(|_| ProviderError::Other(format!("invalid snapshot id {id}")))
}

fn signed(id: SnapshotId) -> Result<i64, ProviderError> {
    i64::try_from(id.0).map_err(|_| ProviderError::Other(format!("invalid snapshot id {id}")))
}

impl Provider for SqliteStateStore {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "default",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl StateStore for SqliteStateStore {
    async fn latest(&self, scope: &StateScope) -> Result<Option<StoredSnapshot>, ProviderError> {
        let row = sqlx::query(
            "SELECT s.id, s.document FROM heads h JOIN snapshots s ON s.id = h.snapshot_id WHERE h.scope = ?",
        )
        .bind(scope.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error(&e))?;
        row.map(|r| Self::decode(r.get(0), r.get(1))).transpose()
    }

    async fn get(
        &self,
        scope: &StateScope,
        id: SnapshotId,
    ) -> Result<Option<StoredSnapshot>, ProviderError> {
        let row = sqlx::query("SELECT id, document FROM snapshots WHERE id = ? AND scope = ?")
            .bind(signed(id)?)
            .bind(scope.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error(&e))?;
        row.map(|r| Self::decode(r.get(0), r.get(1))).transpose()
    }

    async fn commit(
        &self,
        scope: &StateScope,
        snapshot: &StateSnapshot,
    ) -> Result<SnapshotId, ProviderError> {
        // Never write what this build couldn't read back.
        check_readable(snapshot)?;
        let document = serde_json::to_string(snapshot)
            .map_err(|e| ProviderError::Other(format!("can't serialize the snapshot: {e}")))?;
        let mut conn = self.begin_write().await?;
        let result = match conn.conn() {
            Ok(c) => Self::commit_in(c, scope, snapshot, &document).await,
            Err(e) => Err(e),
        };
        Self::finish(conn, result).await
    }

    async fn history(
        &self,
        scope: &StateScope,
        limit: usize,
    ) -> Result<Vec<SnapshotSummary>, ProviderError> {
        let rows = sqlx::query(
            "SELECT id, parent, created_at, run_id, nodes FROM snapshots WHERE scope = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(scope.as_str())
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| db_error(&e))?;
        rows.into_iter()
            .map(|r| {
                let parent: Option<i64> = r.get(1);
                let created: String = r.get(2);
                let nodes: i64 = r.get(4);
                Ok(SnapshotSummary::new(
                    SnapshotId(unsigned(r.get(0))?),
                    parent.map(unsigned).transpose()?.map(SnapshotId),
                    Timestamp::parse(&created).map_err(ProviderError::Other)?,
                    r.get::<String, _>(3),
                    usize::try_from(nodes).unwrap_or_default(),
                ))
            })
            .collect()
    }
}
