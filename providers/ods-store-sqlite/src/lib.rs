//! A [`StateStore`] in a local SQLite file (#25, ADR-0013).
//!
//! - Snapshots are stored as versioned JSON documents, one row each, with a `heads` row
//!   per scope.
//! - A commit inserts the snapshot and moves the head in one transaction. The head only
//!   moves if it still points at the snapshot's parent, so a concurrent or stale commit
//!   fails with [`ProviderError::Conflict`] and writes nothing (AGENTS.md rule 5).
//! - WAL mode lets readers run while a commit is in progress; writers wait on each
//!   other up to a busy timeout.
//! - The database schema is migrated forward in a transaction on open, after a copy of
//!   the database is kept next to it; a failed migration leaves it as it was. A
//!   database written by a newer ODS is refused (#188, ADR-0018).
//! - A damaged file, or a snapshot that can't be decoded, is
//!   [`ProviderError::Corrupt`]; [`check`](StateStore::check) says what is wrong.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::{SnapshotId, StateSnapshot, Timestamp};
use ods_sdk::contracts::state_store::{
    ProblemKind, ScopeSummary, SnapshotSummary, StateScope, StateStore, StoreCheck, StoreProblem,
    StoreSchema, StoredSnapshot, check_readable,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, Sqlite, SqliteConnection};

/// The `kind` this store is configured as.
pub const KIND: &str = "sqlite";

/// Database schema migrations, in order. Never edit one that has shipped; add another.
type Migrations = &'static [(i64, &'static str)];

/// This build's migrations.
const MIGRATIONS: Migrations = &[(
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
        sqlx::Error::Database(e) if is_corruption(e.code().as_deref(), e.message()) => {
            ProviderError::Corrupt(format!("the state database can't be read: {}", e.message()))
        }
        other => ProviderError::Other(format!("state database: {other}")),
    }
}

/// `SQLITE_CORRUPT` (11) and `SQLITE_NOTADB` (26), and their extended codes.
fn is_corruption(code: Option<&str>, message: &str) -> bool {
    let primary = code.and_then(|c| c.parse::<i32>().ok()).map(|c| c & 0xff);
    matches!(primary, Some(11 | 26))
        || message.contains("malformed")
        || message.contains("not a database")
}

/// Where a copy of the database at `path` is kept before it is migrated from
/// `version`: next to it, named after the version and the time.
fn backup_path(path: &Path, version: i64) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "state.db".into(), |n| n.to_string_lossy().into_owned());
    let at = Timestamp::now().unix();
    path.with_file_name(format!("{name}.v{version}-{at}.bak"))
}

/// A state store in a SQLite file.
#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: SqlitePool,
    location: String,
    /// The latest schema version this store's code knows.
    latest: i64,
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
        Self::open_with(path, MIGRATIONS).await
    }

    async fn open_with(path: &Path, migrations: Migrations) -> Result<Self, ProviderError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let mut store = Self::connect(options, path.display().to_string()).await?;
        store.latest = migrations.last().map_or(0, |(v, _)| *v);
        store.migrate(migrations, Some(path)).await?;
        Ok(store)
    }

    /// Opens an existing database without changing it: no migration, and nothing is
    /// created. For [`check`](StateStore::check) and other inspection; commits fail.
    ///
    /// # Errors
    /// Returns [`ProviderError::Corrupt`] if the file isn't a readable SQLite database,
    /// or [`ProviderError`] if it can't be opened.
    pub async fn open_existing(path: &Path) -> Result<Self, ProviderError> {
        if !path.is_file() {
            return Err(ProviderError::Other(format!(
                "there is no state database at `{}`",
                path.display()
            )));
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .busy_timeout(BUSY_TIMEOUT);
        let store = Self::connect(options, path.display().to_string()).await?;
        // Opening is lazy: read the header now, so a file that isn't a database says so.
        sqlx::query("SELECT count(*) FROM sqlite_master")
            .execute(&store.pool)
            .await
            .map_err(|e| db_error(&e))?;
        Ok(store)
    }

    /// A private in-memory database, for tests.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if SQLite fails to start.
    pub async fn in_memory() -> Result<Self, ProviderError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| db_error(&e))?
            .foreign_keys(true);
        let store = Self::connect(options, ":memory:".to_owned()).await?;
        store.migrate(MIGRATIONS, None).await?;
        Ok(store)
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
        Ok(Self {
            pool,
            location,
            latest: MIGRATIONS.last().map_or(0, |(v, _)| *v),
        })
    }

    /// Closes every connection, waiting for them to finish, so the files can be moved
    /// (on Windows, open files can't be).
    pub async fn close(self) {
        self.pool.close().await;
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

    /// The latest database schema version this build knows.
    pub fn latest_schema_version() -> i64 {
        MIGRATIONS.last().map_or(0, |(v, _)| *v)
    }

    /// Writes a consistent copy of the database to `to`, which must not exist, while
    /// others may keep reading and writing it. The copy is itself a state database.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if `to` exists or can't be written.
    pub async fn backup(&self, to: &Path) -> Result<(), ProviderError> {
        if to.exists() {
            return Err(ProviderError::Other(format!(
                "`{}` already exists",
                to.display()
            )));
        }
        sqlx::query("VACUUM INTO ?")
            .bind(to.display().to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| db_error(&e))?;
        Ok(())
    }

    /// Moves the database at `path`, and SQLite's files beside it, out of the way, so
    /// the next run starts with no state and builds everything. Nothing is deleted:
    /// returns where each file went, the database first. Only do this while nothing
    /// else uses it.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if a file can't be moved; files moved before it stay
    /// moved, and the error names them.
    pub fn set_aside(path: &Path) -> Result<Vec<PathBuf>, ProviderError> {
        let at = Timestamp::now().unix();
        let mut moved = Vec::new();
        // The journal files keep their suffix after the new name, so SQLite still finds
        // them: the set-aside database opens with everything that was committed.
        let with = |base: &Path, suffix: &str| {
            let mut name = base.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        };
        let aside = with(path, &format!(".{at}.set-aside"));
        for suffix in ["", "-wal", "-shm"] {
            let (from, to) = (with(path, suffix), with(&aside, suffix));
            if !from.exists() {
                continue;
            }
            std::fs::rename(&from, &to).map_err(|e| {
                ProviderError::Other(format!(
                    "can't move `{}` aside: {e}{}",
                    from.display(),
                    if moved.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (already moved: {})",
                            moved
                                .iter()
                                .map(|p: &PathBuf| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ))
            })?;
            moved.push(to);
        }
        Ok(moved)
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

    /// Migrates the database forward to the last of `migrations`, in one transaction.
    /// A file database that already holds data is copied first (`path`), so the old
    /// version can be restored whatever happens.
    async fn migrate(
        &self,
        migrations: Migrations,
        path: Option<&Path>,
    ) -> Result<(), ProviderError> {
        // Two openers can race to create the migrations table, before either holds a
        // write lock; `IF NOT EXISTS` makes that harmless.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ods_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| db_error(&e))?;
        let applied = self.schema_version().await?;
        let known = migrations.last().map_or(0, |(v, _)| *v);
        if applied > known {
            return Err(newer_schema(&self.location, applied, known));
        }
        if applied == known {
            return Ok(());
        }
        let backup = match path {
            Some(path) if applied > 0 => {
                let to = backup_path(path, applied);
                self.backup(&to).await.map_err(|e| {
                    ProviderError::Other(format!(
                        "the state database `{}` needs migrating from schema version {applied} to {known}, but a copy couldn't be kept first, so it was left as it is: {e}",
                        self.location
                    ))
                })?;
                Some(to)
            }
            _ => None,
        };
        let mut conn = self.begin_write().await?;
        let result = match conn.conn() {
            Ok(c) => Self::migrate_in(c, &self.location, migrations).await,
            Err(e) => Err(e),
        };
        Self::finish(conn, result).await.map_err(|e| {
            let copy = backup
                .as_ref()
                .map(|b| format!("; a copy from before is at `{}`", b.display()))
                .unwrap_or_default();
            ProviderError::Other(format!(
                "migrating the state database `{}` from schema version {applied} to {known} failed, and nothing was changed{copy}: {e}",
                self.location
            ))
        })
    }

    async fn migrate_in(
        conn: &mut SqliteConnection,
        location: &str,
        migrations: Migrations,
    ) -> Result<(), ProviderError> {
        // Again, under the write lock: another opener may have migrated meanwhile.
        let applied: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM ods_migrations")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| db_error(&e))?;
        let known = migrations.last().map_or(0, |(v, _)| *v);
        if applied > known {
            return Err(newer_schema(location, applied, known));
        }
        // Collected first: an iterator adaptor's closure held across `.await` makes the
        // future's `Send` bound unprovable (a known `sqlx` limitation).
        let pending: Vec<(i64, &str)> = migrations
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
        let snapshot: StateSnapshot = serde_json::from_str(document).map_err(|e| {
            ProviderError::Corrupt(format!("state snapshot {id} can't be decoded: {e}"))
        })?;
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

fn newer_schema(location: &str, applied: i64, known: i64) -> ProviderError {
    ProviderError::Other(format!(
        "the state database `{location}` has schema version {applied}, but this ODS knows up to {known}; upgrade ODS"
    ))
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

    async fn check(&self) -> Result<StoreCheck, ProviderError> {
        let mut problems = Vec::new();
        // Damage anywhere is a finding, not a failure to check.
        match self.check_in(&mut problems).await {
            Ok(check) => Ok(check),
            Err(ProviderError::Corrupt(detail)) => {
                problems.push(StoreProblem::new(ProblemKind::Damaged, detail));
                Ok(StoreCheck::new(None, Vec::new(), problems))
            }
            Err(e) => Err(e),
        }
    }
}

impl SqliteStateStore {
    async fn check_in(
        &self,
        problems: &mut Vec<StoreProblem>,
    ) -> Result<StoreCheck, ProviderError> {
        let damage: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error(&e))?;
        if damage.iter().any(|line| line != "ok") {
            let shown: Vec<&str> = damage.iter().take(5).map(String::as_str).collect();
            problems.push(StoreProblem::new(
                ProblemKind::Damaged,
                format!("SQLite's integrity check failed: {}", shown.join("; ")),
            ));
        }
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| db_error(&e))?;
        if !["ods_migrations", "snapshots", "heads"]
            .iter()
            .all(|t| tables.iter().any(|name| name == t))
        {
            problems.push(StoreProblem::new(
                ProblemKind::Damaged,
                format!(
                    "`{}` isn't an ODS state database: its tables are missing",
                    self.location
                ),
            ));
            return Ok(StoreCheck::new(None, Vec::new(), std::mem::take(problems)));
        }
        let version = self.schema_version().await?;
        let latest = self.latest;
        let schema = Some(StoreSchema::new(
            u32::try_from(version).unwrap_or(u32::MAX),
            u32::try_from(latest).unwrap_or(u32::MAX),
        ));
        if version > latest {
            // Its tables may mean something else now: read no further.
            problems.push(StoreProblem::new(
                ProblemKind::NewerSchema,
                format!("schema version {version} was written by a newer ODS, which knows up to {latest}; upgrade ODS"),
            ));
            return Ok(StoreCheck::new(
                schema,
                Vec::new(),
                std::mem::take(problems),
            ));
        }

        let scopes = self.check_records(problems).await?;
        Ok(StoreCheck::new(schema, scopes, std::mem::take(problems)))
    }

    /// Checks every snapshot decodes, and every head and parent points at a snapshot in
    /// its own scope. Returns every scope.
    async fn check_records(
        &self,
        problems: &mut Vec<StoreProblem>,
    ) -> Result<Vec<ScopeSummary>, ProviderError> {
        let rows = sqlx::query("SELECT id, scope, parent, document FROM snapshots ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error(&e))?;
        let owners: std::collections::BTreeMap<i64, String> = rows
            .iter()
            .map(|r| (r.get::<i64, _>(0), r.get::<String, _>(1)))
            .collect();
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for row in &rows {
            let (id, scope): (i64, String) = (row.get(0), row.get(1));
            *counts.entry(scope.clone()).or_default() += 1;
            if let Err(e) = Self::decode(id, row.get(3)) {
                problems.push(StoreProblem::new(
                    ProblemKind::UnreadableSnapshot,
                    format!("snapshot {id} of `{scope}`: {e}"),
                ));
            }
            let parent: Option<i64> = row.get(2);
            if let Some(parent) = parent
                && owners.get(&parent) != Some(&scope)
            {
                problems.push(StoreProblem::new(
                    ProblemKind::BrokenChain,
                    format!("snapshot {id} of `{scope}` follows {parent}, which isn't in it"),
                ));
            }
        }
        let heads = sqlx::query("SELECT scope, snapshot_id FROM heads")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error(&e))?;
        let mut scopes = Vec::new();
        for row in &heads {
            let (scope, head): (String, i64) = (row.get(0), row.get(1));
            if owners.get(&head) != Some(&scope) {
                problems.push(StoreProblem::new(
                    ProblemKind::DanglingHead,
                    format!("`{scope}` points at snapshot {head}, which isn't in it"),
                ));
            }
            let count = counts.remove(&scope).unwrap_or_default();
            scopes.push(ScopeSummary::new(
                scope,
                Some(SnapshotId(unsigned(head)?)),
                count,
            ));
        }
        for (scope, count) in counts {
            problems.push(StoreProblem::new(
                ProblemKind::DanglingHead,
                format!("`{scope}` has {count} snapshot(s) but no head"),
            ));
            scopes.push(ScopeSummary::new(scope, None, count));
        }
        Ok(scopes)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ods-store-sqlite-unit-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    fn backups(db: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(db.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bak"))
            .collect();
        found.sort();
        found
    }

    async fn tables(store: &SqliteStateStore) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&store.pool)
            .await
            .unwrap()
    }

    fn snapshot(run: &str) -> StateSnapshot {
        StateSnapshot::new(None, Timestamp::from_unix(1), run, BTreeMap::new())
    }

    // Synthetic schema versions after this build's first.
    const V2: Migrations = &[
        (1, MIGRATIONS[0].1),
        (2, "CREATE TABLE synthetic_v2 (x INTEGER)"),
    ];
    const V3: Migrations = &[
        (1, MIGRATIONS[0].1),
        (2, "CREATE TABLE synthetic_v2 (x INTEGER)"),
        (3, "ALTER TABLE snapshots ADD COLUMN synthetic_v3 TEXT"),
    ];
    // Its first statement works, its second fails: the whole migration must not apply.
    const BROKEN_V3: Migrations = &[
        (1, MIGRATIONS[0].1),
        (2, "CREATE TABLE synthetic_v2 (x INTEGER)"),
        (
            3,
            "CREATE TABLE half_done (x INTEGER); CREATE TABLE snapshots (x INTEGER)",
        ),
    ];

    #[tokio::test]
    async fn migrates_forward_across_versions_keeping_a_copy_each_time() {
        let db = temp_db("forward");
        let scope = StateScope::new("p", "dev").unwrap();
        let id = {
            let store = SqliteStateStore::open_with(&db, &MIGRATIONS[..1])
                .await
                .unwrap();
            store.commit(&scope, &snapshot("run-1")).await.unwrap()
        };
        assert!(backups(&db).is_empty(), "a new database needs no copy");

        let store = SqliteStateStore::open_with(&db, V2).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 2);
        assert_eq!(store.latest(&scope).await.unwrap().unwrap().id, id);
        let copies = backups(&db);
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert!(copies[0].to_string_lossy().contains(".v1-"), "{copies:?}");
        drop(store);

        let store = SqliteStateStore::open_with(&db, V3).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 3);
        assert!(store.check().await.unwrap().problems.is_empty());
        assert_eq!(store.latest(&scope).await.unwrap().unwrap().id, id);
        assert_eq!(backups(&db).len(), 2);
        // Opening again at the same version keeps no more copies.
        drop(SqliteStateStore::open_with(&db, V3).await.unwrap());
        assert_eq!(backups(&db).len(), 2);

        // Each copy is the database as it was: the first is at version 1.
        let first = SqliteStateStore::open_existing(&backups(&db)[0])
            .await
            .unwrap();
        assert_eq!(first.schema_version().await.unwrap(), 1);
        assert_eq!(first.latest(&scope).await.unwrap().unwrap().id, id);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[tokio::test]
    async fn a_failed_migration_changes_nothing() {
        let db = temp_db("failed");
        let scope = StateScope::new("p", "dev").unwrap();
        let id = {
            let store = SqliteStateStore::open_with(&db, V2).await.unwrap();
            store.commit(&scope, &snapshot("run-1")).await.unwrap()
        };
        let error = SqliteStateStore::open_with(&db, BROKEN_V3)
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("from schema version 2 to 3 failed"), "{text}");
        assert!(text.contains("nothing was changed"), "{text}");
        assert!(text.contains(".v2-"), "names the copy: {text}");

        let store = SqliteStateStore::open_with(&db, V2).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 2);
        assert!(!tables(&store).await.contains(&"half_done".to_owned()));
        assert_eq!(store.latest(&scope).await.unwrap().unwrap().id, id);
        assert!(store.check().await.unwrap().problems.is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[tokio::test]
    async fn an_older_build_refuses_a_newer_database_and_the_check_says_so() {
        let db = temp_db("older-build");
        drop(SqliteStateStore::open_with(&db, V3).await.unwrap());
        let error = SqliteStateStore::open_with(&db, V2).await.unwrap_err();
        assert!(error.to_string().contains("upgrade ODS"), "{error}");
        // This build's check (latest: 1) reads it without changing it.
        let store = SqliteStateStore::open_existing(&db).await.unwrap();
        let check = store.check().await.unwrap();
        assert_eq!(check.problems.len(), 1, "{check:?}");
        assert_eq!(check.problems[0].kind, ProblemKind::NewerSchema);
        assert_eq!(store.schema_version().await.unwrap(), 3);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn corruption_is_recognised_by_code_and_message() {
        assert!(is_corruption(Some("11"), "x"));
        assert!(is_corruption(Some("267"), "x"));
        assert!(is_corruption(Some("26"), "x"));
        assert!(is_corruption(None, "database disk image is malformed"));
        assert!(!is_corruption(Some("5"), "database is locked"));
    }
}
