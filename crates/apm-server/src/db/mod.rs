//! SQLite access: the two connection pools, pragmas, migrations, and the rules every later module
//! follows when it touches the database.
//!
//! Owned by todo `designing-database-schema`. The schema is `migrations/0001_core.sql`; read its
//! header, column names and semantics are a contract. Every other todo adds files in its own number
//! block (rule 6 of the parallel work protocol in `AGENTS.md`), `build.rs` makes cargo notice new
//! files, and a migration is append-only once it is on `main`. The migrator is strict: a database
//! that has applied a migration this binary does not know refuses to open, so an older image can not
//! run on a newer database.
//!
//! # The handle
//!
//! [`Db`] holds two pools over one SQLite file, normally WAL-mode `APM_DATA_DIR/apm.db`:
//!
//! * [`Db::read`]: a few read-only connections (`query_only`). In WAL mode readers do not block
//!   writers; with the rollback journal, a reader can delay a commit until it releases its snapshot.
//! * [`Db::write`] and [`Db::begin_write`]: ONE connection, so writers queue inside the pool and
//!   SQLite never answers `SQLITE_BUSY`. [`Db::begin_write`] starts `BEGIN IMMEDIATE`, the right
//!   mode for a transaction that writes (a deferred one can fail when it upgrades its lock).
//!
//! Rules:
//!
//! * Keep write transactions short: at most about 1000 rows, no file system or network work, no
//!   long `await` inside. Insert in batches with `QueryBuilder::push_values`.
//! * Never call [`Db::write`] or [`Db::begin_write`] while you hold a write transaction: the only
//!   writer connection is yours, so the call waits for the pool timeout (30 s) and fails.
//! * Never hold a read connection or a read transaction across slow work (for example while
//!   streaming a response): it pins a snapshot and keeps the WAL from being checkpointed.
//! * Runtime `sqlx::query`, `query_as` and `QueryBuilder` only, never the compile-time macros. Bind
//!   every value. Split id lists into chunks of at most 30 000 binds (SQLite allows 32 766).
//! * Functions that touch the database return [`DbError`], not `AppError`: `?` in a handler then
//!   turns a unique or foreign key violation into 409 and keeps SQL text out of the response.
//! * Prefer `STRICT` tables in new migrations, and give every unique index an explicit name.
//! * A migration that adds a column to `files` must also extend [`models::FileRow`] and
//!   [`models::FILE_COLUMNS`] (a test fails until it does).
//! * WAL is preferred and verified. When the data directory is on a known WAL-unsafe mount or the
//!   live WAL probe fails, startup uses the rollback journal with a longer busy timeout and warns;
//!   the index remains available, but concurrent reads can delay writes. A Docker named volume is
//!   recommended for `/data`.
//! * Statistics come from `ANALYZE`, and the planner picks worse plans for tables that have none.
//!   Call [`Db::optimize`] after a big batch of changes (the end of a scan or of an import): it only
//!   analyses a table that has no statistics yet or has grown a lot, so it is cheap. [`Db::close`]
//!   calls it too. The writer connection uses the new statistics at once; a reader only after the
//!   pool has replaced it, which happens [`READER_MAX_LIFETIME`] after it opened (a connection
//!   does not reload the statistics when `ANALYZE` rewrites them).
//!
//! # Paths and names
//!
//! `dirs.path` and `files.dir_path` hold [`apm_core::paths::RelPath`] strings exactly as the file
//! system reports them: there is no Unicode normalisation (macOS stores NFD). Compare names through
//! `name_norm` and `stem_norm`, built with `apm_core::names::normalize_name`. `COLLATE NOCASE` (tags,
//! cameras) folds ASCII only.
//!
//! # Subtree idiom
//!
//! The files of directory `D` and of every directory below it, on volume `V`:
//!
//! ```sql
//! volume_id = :V AND dir_path >= :D AND dir_path < :D || '0'
//!   AND (dir_path = :D OR dir_path >= :D || '/')
//! ```
//!
//! `'0'` is the character after `'/'`. The volume root (`D = ''`) is every file of the volume, so it
//! needs no condition at all. The range is answered by the unique index
//! `files_volume_dir_name (volume_id, dir_path, name)`. Siblings that merely share the prefix of
//! `D` (`2019.old`, `2019-x`, `2019 b`) sort inside the range, which is why the last line is there:
//! it keeps `D` itself and everything below `D/`. Do not shorten it to
//! `dir_path = :D OR (dir_path >= :D || '/' AND dir_path < :D || '0')`: that is just as correct, but
//! SQLite plans it as a scan of the whole volume while the table has no statistics. Do not write it
//! by hand either: [`subtree::Subtree::push_predicate`] emits it, for `dirs.path` too.
//!
//! # Tests
//!
//! `Db::open_temp()` gives a migrated database in a private directory under `target/tmp`. An
//! in-memory database does not work: the reader and the writer pool are separate connections.

pub mod models;
pub mod subtree;
pub mod volumes;

#[cfg(test)]
mod pool_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
pub(crate) mod testing;

use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use anyhow::bail;
use axum::http::StatusCode;
use sqlx::error::DatabaseError;
use sqlx::migrate::MigrateError;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Connection, Row, Sqlite, SqlitePool, Transaction};
use tempfile::TempDir;
use thiserror::Error;

use crate::config::Config;
use crate::error::AppError;

/// Name of the database file inside `APM_DATA_DIR`.
pub const DB_FILE_NAME: &str = "apm.db";

/// How long a reader connection lives before the pool replaces it. A connection keeps the planner
/// statistics it loaded when it opened, so a reader kept forever would ignore every `ANALYZE` that
/// ran later; replacing it costs about a millisecond. The writer is never replaced: it runs the
/// `ANALYZE` itself and always has the latest statistics.
pub const READER_MAX_LIFETIME: Duration = Duration::from_secs(10 * 60);

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ROLLBACK_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long [`Db::close`] waits for connections that are still checked out.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Where [`Db::open_temp`] creates its directories: inside `target/`, never in `/tmp`.
const TEMP_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tmp");

/// Handle to the SQLite index database: a reader pool and a single-connection writer pool. Cheap to
/// clone. See the module docs for the rules of use.
#[derive(Clone, Debug)]
pub struct Db {
    reader: SqlitePool,
    writer: SqlitePool,
    journal_mode: &'static str,
    journal_fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalPolicy {
    Wal,
    Rollback { reason: String },
}

impl Db {
    /// Opens (creating it when missing) the database file at `path`, applies the pending migrations
    /// and checks that WAL mode and foreign key enforcement are really on.
    pub async fn open(path: &Path) -> Result<Self, DbError> {
        Self::open_with(path, JournalPolicy::Wal).await
    }

    /// Opens with the already-determined SQLite journal policy. Startup probes the target mount;
    /// ordinary callers and tests continue to request WAL through [`Db::open`].
    pub async fn open_with(path: &Path, policy: JournalPolicy) -> Result<Self, DbError> {
        Self::open_recycling_readers_after_with_policy(path, READER_MAX_LIFETIME, policy).await
    }

    /// [`Db::open`] with another reader lifetime, so that a test need not wait for minutes.
    #[cfg(test)]
    async fn open_recycling_readers_after(
        path: &Path,
        reader_lifetime: Duration,
    ) -> Result<Self, DbError> {
        Self::open_recycling_readers_after_with_policy(path, reader_lifetime, JournalPolicy::Wal)
            .await
    }

    async fn open_recycling_readers_after_with_policy(
        path: &Path,
        reader_lifetime: Duration,
        policy: JournalPolicy,
    ) -> Result<Self, DbError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let journal_mode = match policy {
            JournalPolicy::Wal => "wal",
            JournalPolicy::Rollback { .. } => "delete",
        };
        let fallback_reason = match &policy {
            JournalPolicy::Wal => None,
            JournalPolicy::Rollback { reason } => Some(reason.clone()),
        };
        if let Some(reason) = &fallback_reason {
            tracing::warn!(reason, "SQLite WAL is unavailable; using rollback journal");
        }
        let writer = pool_options(1)
            .connect_with(writer_options(path, journal_mode))
            .await?;
        // Migrate before any reader exists, so that none ever sees a half-built schema.
        sqlx::migrate!("./migrations").run(&writer).await?;
        let readers = if journal_mode == "delete" {
            2
        } else {
            reader_connections()
        };
        let reader = pool_options(readers)
            .max_lifetime(Some(reader_lifetime))
            .connect_with(reader_options(path, journal_mode))
            .await?;
        let db = Self {
            reader,
            writer,
            journal_mode,
            journal_fallback_reason: fallback_reason,
        };
        db.verify(path).await?;
        Ok(db)
    }

    /// Opens `APM_DATA_DIR/apm.db` and registers the volumes found in `APM_VOLUMES_DIR`. This is
    /// the startup path (`AppState::new`), so every failure carries a message fit for the log.
    pub async fn from_config(config: &Config) -> anyhow::Result<Self> {
        Self::from_config_with_capabilities(config)
            .await
            .map(|(db, _)| db)
    }

    pub async fn from_config_with_capabilities(
        config: &Config,
    ) -> anyhow::Result<(
        Self,
        HashMap<apm_core::paths::VolumeId, crate::hostfs::VolumeCapabilities>,
    )> {
        tokio::fs::create_dir_all(&config.data_dir)
            .await
            .with_context(|| {
                format!(
                    "cannot open the database {}: cannot create data directory {}",
                    config.data_dir.join(DB_FILE_NAME).display(),
                    config.data_dir.display()
                )
            })?;
        let canonical_data_dir = tokio::fs::canonicalize(&config.data_dir)
            .await
            .with_context(|| {
                format!(
                    "cannot resolve data directory {}",
                    config.data_dir.display()
                )
            })?;
        let fs_type = crate::hostfs::mount_type(&canonical_data_dir);
        let policy = if crate::hostfs::probe::wal_unsafe(fs_type.as_deref()) {
            journal_policy(fs_type.as_deref(), Ok(()))
        } else {
            let probe_result = probe_wal(&canonical_data_dir).await.map_err(|error| {
                tracing::warn!(error = %error, "SQLite WAL probe failed");
                "the SQLite WAL probe failed".to_owned()
            });
            journal_policy(fs_type.as_deref(), probe_result)
        };
        tracing::info!(
            fs_type = fs_type.as_deref().unwrap_or("unknown"),
            journal_policy = match policy {
                JournalPolicy::Wal => "wal",
                JournalPolicy::Rollback { .. } => "rollback",
            },
            "data directory capabilities detected"
        );
        let path = config.data_dir.join(DB_FILE_NAME);
        let db = Self::open_with(&path, policy)
            .await
            .with_context(|| format!("cannot open the database {}", path.display()))?;
        let (volumes, capabilities) =
            volumes::sync_volumes_with_capabilities(&db, &config.volumes_dir)
                .await
                .with_context(|| {
                    format!(
                        "cannot register the volumes found in {}",
                        config.volumes_dir.display()
                    )
                })?;
        tracing::info!(
            database = %path.display(),
            volumes = volumes.len(),
            online = volumes.iter().filter(|volume| volume.online).count(),
            "database ready"
        );
        Ok((db, capabilities))
    }

    /// The reader pool: read-only connections (a write fails), for every `SELECT`.
    pub fn read(&self) -> &SqlitePool {
        &self.reader
    }

    /// The writer pool: its only connection runs single statements (autocommit). Use
    /// [`Db::begin_write`] for anything that must be atomic.
    pub fn write(&self) -> &SqlitePool {
        &self.writer
    }

    pub fn journal_mode(&self) -> &'static str {
        self.journal_mode
    }

    pub fn journal_fallback_reason(&self) -> Option<&str> {
        self.journal_fallback_reason.as_deref()
    }

    /// Starts a write transaction (`BEGIN IMMEDIATE`) on the writer connection; it is rolled back
    /// when dropped without `commit`. Writers queue here, one at a time.
    pub async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, DbError> {
        Ok(self.writer.begin_with("BEGIN IMMEDIATE").await?)
    }

    /// Refreshes the planner statistics: `PRAGMA optimize` on the writer connection. It analyses a
    /// table only when it has no statistics yet or has grown a lot since the last analysis, so it is
    /// cheap to call after every big batch of changes. Readers use the new statistics once the pool
    /// has replaced them (see [`READER_MAX_LIFETIME`]).
    pub async fn optimize(&self) -> Result<(), DbError> {
        sqlx::query("PRAGMA optimize").execute(&self.writer).await?;
        Ok(())
    }

    /// Updates the planner statistics and closes both pools. Called last by
    /// `AppState::shutdown`. It never waits longer than a few seconds for a connection that is
    /// still checked out.
    pub async fn close(&self) {
        if let Err(err) = self.optimize().await {
            tracing::debug!(error = %err, "PRAGMA optimize skipped");
        }
        let closing = async {
            self.reader.close().await;
            self.writer.close().await;
        };
        if tokio::time::timeout(CLOSE_TIMEOUT, closing).await.is_err() {
            tracing::warn!("database connections were still in use at shutdown");
        }
    }

    /// A migrated database in a private directory under `target/tmp`, removed when the returned
    /// [`TempDb`] is dropped. For tests.
    pub async fn open_temp() -> Result<TempDb, DbError> {
        tokio::fs::create_dir_all(TEMP_ROOT).await?;
        let root = tokio::fs::canonicalize(TEMP_ROOT).await?;
        let dir = tempfile::Builder::new()
            .prefix("apm-db-")
            .tempdir_in(root)?;
        let db = Self::open(&dir.path().join(DB_FILE_NAME)).await?;
        Ok(TempDb { db, dir })
    }

    async fn verify(&self, path: &Path) -> Result<(), DbError> {
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.writer)
            .await?;
        if !mode.eq_ignore_ascii_case(self.journal_mode) {
            return Err(DbError::Setup(format!(
                "SQLite could not switch {} to {} mode (journal_mode is {mode:?})",
                path.display(),
                self.journal_mode,
            )));
        }
        for (role, pool) in [("writer", &self.writer), ("reader", &self.reader)] {
            let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(pool)
                .await?;
            if enforced != 1 {
                return Err(DbError::Setup(format!(
                    "foreign key enforcement is off on the {role} connections"
                )));
            }
        }
        Ok(())
    }
}

fn rollback_reason(fs_type: Option<&str>, cause: &str) -> String {
    format!(
        "SQLite is using the rollback journal{} because {cause}; the app works but is slower. Use a Docker named volume for /data to avoid this.",
        fs_type.map_or_else(String::new, |fs| format!(" on filesystem type {fs}")),
    )
}

fn journal_policy(fs_type: Option<&str>, wal_probe: Result<(), String>) -> JournalPolicy {
    if crate::hostfs::probe::wal_unsafe(fs_type) {
        return JournalPolicy::Rollback {
            reason: rollback_reason(fs_type, "the mount type is not WAL-safe"),
        };
    }
    match wal_probe {
        Ok(()) => JournalPolicy::Wal,
        Err(_) => JournalPolicy::Rollback {
            reason: rollback_reason(fs_type, "the SQLite WAL probe failed"),
        },
    }
}

async fn probe_wal(data_dir: &Path) -> anyhow::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = data_dir.join(format!(
        ".apm-wal-probe-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let cleanup = WalProbeFiles(path.clone());
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(ROLLBACK_BUSY_TIMEOUT);
    let mut writer = SqliteConnection::connect_with(&options).await?;
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut writer)
        .await?;
    if !mode.eq_ignore_ascii_case("wal") {
        bail!("SQLite returned journal_mode={mode:?}");
    }
    sqlx::query("CREATE TABLE probe (value INTEGER NOT NULL)")
        .execute(&mut writer)
        .await?;
    sqlx::query("INSERT INTO probe (value) VALUES (42)")
        .execute(&mut writer)
        .await?;

    let mut reader = SqliteConnection::connect_with(&options).await?;
    let value: i64 = sqlx::query_scalar("SELECT value FROM probe")
        .fetch_one(&mut reader)
        .await?;
    if value != 42 {
        bail!("the second SQLite connection did not read the WAL row");
    }
    let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&mut writer)
        .await?;
    let busy: i64 = checkpoint.try_get("busy")?;
    if busy != 0 {
        bail!("SQLite WAL checkpoint was busy");
    }
    reader.close().await?;
    writer.close().await?;
    drop(cleanup);
    Ok(())
}

struct WalProbeFiles(PathBuf);

impl Drop for WalProbeFiles {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            if let Err(error) = std::fs::remove_file(PathBuf::from(path))
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(error = %error, "could not remove a SQLite WAL probe file");
            }
        }
    }
}

/// A [`Db`] in a private temporary directory, removed with it. Derefs to the [`Db`].
#[derive(Debug)]
pub struct TempDb {
    // Declared first: the pools are dropped before the directory they live in.
    db: Db,
    dir: TempDir,
}

impl TempDb {
    /// A handle that can be moved into tasks or services; the directory stays owned by `self`.
    pub fn handle(&self) -> Db {
        self.db.clone()
    }

    /// The private directory: the database file lives in it, and tests may create more (a volumes
    /// directory, ...) next to it. It is removed with `self`.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The database file.
    pub fn path(&self) -> PathBuf {
        self.dir.path().join(DB_FILE_NAME)
    }
}

impl Deref for TempDb {
    type Target = Db;

    fn deref(&self) -> &Db {
        &self.db
    }
}

/// Current time as epoch seconds (UTC), the unit of every timestamp column.
pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn reader_connections() -> u32 {
    let cpus = std::thread::available_parallelism().map_or(2, NonZeroUsize::get);
    u32::try_from(cpus).unwrap_or(8).clamp(2, 8)
}

fn pool_options(max_connections: u32) -> SqlitePoolOptions {
    SqlitePoolOptions::new()
        .max_connections(max_connections)
        // No periodic health checks or idle reaping: startup selects a safe journal policy, and a
        // warm page cache is worth keeping. The reader pool adds a lifetime on top, see
        // `READER_MAX_LIFETIME`.
        .test_before_acquire(false)
        .idle_timeout(None)
        .max_lifetime(None)
}

fn base_options(path: &Path, journal_mode: &str) -> SqliteConnectOptions {
    let (journal, synchronous, timeout) = if journal_mode == "wal" {
        (
            SqliteJournalMode::Wal,
            SqliteSynchronous::Normal,
            BUSY_TIMEOUT,
        )
    } else {
        (
            SqliteJournalMode::Delete,
            SqliteSynchronous::Full,
            ROLLBACK_BUSY_TIMEOUT,
        )
    };
    SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(journal)
        .synchronous(synchronous)
        .foreign_keys(true)
        .busy_timeout(timeout)
        // Sorts spill to memory: the image may run with a read-only root file system.
        .pragma("temp_store", "MEMORY")
}

fn writer_options(path: &Path, journal_mode: &str) -> SqliteConnectOptions {
    let options = base_options(path, journal_mode)
        .create_if_missing(true)
        .pragma("cache_size", "-16384")
        // `ANALYZE` and `PRAGMA optimize` sample instead of reading every row.
        .analysis_limit(1000);
    if journal_mode == "wal" {
        options
            .pragma("wal_autocheckpoint", "1000")
            .pragma("journal_size_limit", "67108864")
    } else {
        options
    }
}

fn reader_options(path: &Path, journal_mode: &str) -> SqliteConnectOptions {
    base_options(path, journal_mode)
        .pragma("cache_size", "-8192")
        // Applied last (custom pragmas keep their order): nothing may write through a reader.
        .pragma("query_only", "ON")
}

/// Everything that can go wrong below the HTTP layer. Services return it so that callers can still
/// tell a constraint violation from a broken disk.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migrate(#[from] MigrateError),
    #[error("database I/O error: {0}")]
    Io(#[from] io::Error),
    /// The environment is not usable (not WAL, foreign keys off, a worker panicked, ...).
    #[error("database setup problem: {0}")]
    Setup(String),
}

impl DbError {
    fn database_error(&self) -> Option<&(dyn DatabaseError + 'static)> {
        match self {
            Self::Sqlx(sqlx::Error::Database(err)) => Some(err.as_ref()),
            _ => None,
        }
    }

    /// A `UNIQUE` or `PRIMARY KEY` constraint failed.
    pub fn is_unique_violation(&self) -> bool {
        self.database_error()
            .is_some_and(|err| err.is_unique_violation())
    }

    /// A foreign key constraint failed.
    pub fn is_foreign_key_violation(&self) -> bool {
        self.database_error()
            .is_some_and(|err| err.is_foreign_key_violation())
    }

    /// A `CHECK` constraint failed.
    pub fn is_check_violation(&self) -> bool {
        self.database_error()
            .is_some_and(|err| err.is_check_violation())
    }

    /// `fetch_one` found no row.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Sqlx(sqlx::Error::RowNotFound))
    }

    /// Transient contention: no connection became free in time, or SQLite answered `SQLITE_BUSY` or
    /// `SQLITE_LOCKED`. Retrying later can succeed.
    pub fn is_busy(&self) -> bool {
        match self {
            Self::Sqlx(sqlx::Error::PoolTimedOut) => true,
            Self::Sqlx(sqlx::Error::Database(err)) => err
                .code()
                .and_then(|code| code.parse::<i32>().ok())
                // The primary result code is the low byte of the extended one.
                .is_some_and(|code| matches!(code & 0xff, 5 | 6)),
            _ => false,
        }
    }
}

impl From<DbError> for AppError {
    /// Violations are the caller's conflict (409), a missing row is a 404, contention is a 503;
    /// everything else is an internal error whose cause is logged and never sent.
    fn from(err: DbError) -> Self {
        if err.is_unique_violation() || err.is_foreign_key_violation() {
            Self::conflict("conflict", "the change conflicts with existing data")
        } else if err.is_not_found() {
            Self::not_found("not_found", "resource not found")
        } else if err.is_busy() {
            Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "db_busy",
                "the database is busy, try again in a moment",
            )
        } else {
            Self::internal(err)
        }
    }
}
