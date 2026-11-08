//! Tests of the handle itself: pragmas, pools, migrations, transactions and error mapping.

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt as _;
use sqlx::migrate::MigrateError;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions as _, Connection as _};

use super::testing::insert_volume;
use super::*;

async fn pragma(pool: &SqlitePool, name: &'static str) -> i64 {
    let sql = format!("PRAGMA {name}");
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count(db: &Db, table: &'static str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(db.read())
        .await
        .unwrap()
}

async fn insert_setting(
    tx: &mut sqlx::SqliteConnection,
    key: &str,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, '{}', 0)")
        .bind(key)
        .execute(tx)
        .await
}

fn raw_options(db: &TempDb) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(db.path())
        .busy_timeout(Duration::ZERO)
}

async fn body_text(err: AppError) -> (StatusCode, String) {
    let response = err.into_response();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn opens_in_wal_mode_with_the_documented_pragmas() {
    let db = Db::open_temp().await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.write())
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    for pool in [db.write(), db.read()] {
        assert_eq!(pragma(pool, "synchronous").await, 1, "NORMAL");
        assert_eq!(pragma(pool, "temp_store").await, 2, "MEMORY");
        assert_eq!(pragma(pool, "busy_timeout").await, 5000);
    }
    assert!(db.path().exists());
}

#[test]
fn rejects_an_injected_wal_unsafe_mount_with_local_volume_guidance() {
    let mount = apm_core::mounts::parse_mountinfo(b"42 31 0:42 / /data rw - 9p C:\\134 rw");
    let fs_type =
        apm_core::mounts::lookup(&mount, b"/data/apm.db").map(|entry| entry.fs_type.as_str());
    let error = wal_unsafe_error(fs_type).unwrap();
    assert!(error.contains("filesystem type 9p"));
    assert!(error.contains("local Docker named volume for /data"));
    assert!(wal_unsafe_error(Some("ext4")).is_none());
}

#[test]
fn failed_wal_probe_recommends_a_local_data_volume() {
    let error = wal_probe_error(Some("virtiofs"));
    assert!(error.contains("filesystem type virtiofs"));
    assert!(error.contains("local Docker named volume for /data"));
}

#[tokio::test]
async fn foreign_keys_are_on_for_the_writer_and_every_reader_connection() {
    let db = Db::open_temp().await.unwrap();
    assert_eq!(pragma(db.write(), "foreign_keys").await, 1);

    // Hold several readers at once so that every connection of the pool is checked.
    let mut held = Vec::new();
    for _ in 0..reader_connections() {
        held.push(db.read().acquire().await.unwrap());
    }
    for conn in &mut held {
        let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        assert_eq!(enforced, 1);
        let read_only: i64 = sqlx::query_scalar("PRAGMA query_only")
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        assert_eq!(read_only, 1);
    }
    assert_eq!(pragma(db.write(), "query_only").await, 0);

    let orphan = sqlx::query(
        "INSERT INTO dirs (volume_id, path, parent_path, name) VALUES (999, 'a', '', 'a')",
    )
    .execute(db.write())
    .await
    .unwrap_err();
    assert!(DbError::from(orphan).is_foreign_key_violation());
}

#[tokio::test]
async fn readers_cannot_write_but_the_writer_can() {
    let db = Db::open_temp().await.unwrap();
    let err = sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('k', '{}', 0)")
        .execute(db.read())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("readonly"), "{err}");
    let mut conn = db.write().acquire().await.unwrap();
    insert_setting(&mut conn, "k").await.unwrap();
}

#[tokio::test]
async fn migrations_apply_once_and_reopening_is_idempotent() {
    let temp = Db::open_temp().await.unwrap();
    let applied: Vec<(i64, bool)> =
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(temp.read())
            .await
            .unwrap();
    assert_eq!(applied[0], (1, true));
    let tables = count_tables(&temp).await;
    assert!(tables >= 13, "{tables} tables");

    let mut conn = temp.write().acquire().await.unwrap();
    insert_setting(&mut conn, "kept").await.unwrap();
    drop(conn);
    temp.close().await;

    let reopened = Db::open(&temp.path()).await.unwrap();
    let again: Vec<(i64, bool)> =
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(reopened.read())
            .await
            .unwrap();
    assert_eq!(again, applied);
    assert_eq!(count(&reopened, "settings").await, 1);
    assert_eq!(count_tables(&reopened).await, tables);
    reopened.close().await;
}

async fn count_tables(db: &Db) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         AND name <> '_sqlx_migrations'",
    )
    .fetch_one(db.read())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_database_migrated_by_a_newer_version_is_refused() {
    let temp = Db::open_temp().await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'from the future', 1, x'00', 0)",
    )
    .execute(temp.write())
    .await
    .unwrap();
    temp.close().await;

    let err = Db::open(&temp.path()).await.unwrap_err();
    assert!(
        matches!(err, DbError::Migrate(MigrateError::VersionMissing(9999))),
        "{err:?}"
    );
}

#[tokio::test]
async fn open_creates_missing_parent_directories() {
    let temp = Db::open_temp().await.unwrap();
    let nested = temp.dir().join("one/two/apm.db");
    let db = Db::open(&nested).await.unwrap();
    assert!(nested.exists());
    db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_queue_without_busy_errors() {
    let db = Db::open_temp().await.unwrap();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let db = db.handle();
        tasks.push(tokio::spawn(async move {
            let mut tx = db.begin_write().await?;
            insert_setting(&mut tx, &format!("key-{i}")).await?;
            // Hold the write lock for a moment so the other tasks really have to wait.
            tokio::time::sleep(Duration::from_millis(5)).await;
            tx.commit().await?;
            Ok::<_, DbError>(())
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(count(&db, "settings").await, 20);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_keep_reading_during_a_write_transaction_and_see_it_only_after_commit() {
    let db = Db::open_temp().await.unwrap();
    let mut tx = db.begin_write().await.unwrap();
    insert_setting(&mut tx, "pending").await.unwrap();

    let during = tokio::time::timeout(Duration::from_secs(2), async {
        count(&db, "settings").await
    })
    .await
    .expect("a reader must not wait for the writer");
    assert_eq!(during, 0, "uncommitted rows are invisible");

    tx.commit().await.unwrap();
    assert_eq!(count(&db, "settings").await, 1);
}

#[tokio::test]
async fn a_read_transaction_keeps_its_snapshot() {
    let db = Db::open_temp().await.unwrap();
    let mut snapshot = db.read().begin().await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();

    let mut tx = db.begin_write().await.unwrap();
    insert_setting(&mut tx, "later").await.unwrap();
    tx.commit().await.unwrap();

    let inside: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();
    assert_eq!((before, inside), (0, 0));
    snapshot.rollback().await.unwrap();
    assert_eq!(count(&db, "settings").await, 1);
}

#[tokio::test]
async fn there_is_only_one_writer_connection() {
    let db = Db::open_temp().await.unwrap();
    let first = db.begin_write().await.unwrap();
    let waiting = tokio::time::timeout(Duration::from_millis(150), db.begin_write()).await;
    assert!(waiting.is_err(), "a second writer must wait for the first");
    first.commit().await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), db.begin_write())
        .await
        .expect("the writer is free again")
        .unwrap();
    second.rollback().await.unwrap();
}

#[tokio::test]
async fn begin_write_takes_the_write_lock_immediately() {
    let db = Db::open_temp().await.unwrap();
    let tx = db.begin_write().await.unwrap();

    // The transaction has not written anything yet: only BEGIN IMMEDIATE holds the lock already.
    let mut other = raw_options(&db).connect().await.unwrap();
    let err = sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut other)
        .await
        .unwrap_err();
    let err = DbError::from(err);
    assert!(err.is_busy(), "{err:?}");

    tx.rollback().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut other)
        .await
        .unwrap();
    sqlx::query("ROLLBACK").execute(&mut other).await.unwrap();
    other.close().await.unwrap();
}

#[tokio::test]
async fn a_dropped_transaction_is_rolled_back() {
    let db = Db::open_temp().await.unwrap();
    {
        let mut tx = db.begin_write().await.unwrap();
        insert_setting(&mut tx, "gone").await.unwrap();
    }
    assert_eq!(count(&db, "settings").await, 0);
    let again = db.begin_write().await.unwrap();
    again.rollback().await.unwrap();
}

#[tokio::test]
async fn close_stops_both_pools() {
    let db = Db::open_temp().await.unwrap();
    db.close().await;
    assert!(db.read().acquire().await.is_err());
    assert!(db.begin_write().await.is_err());
    db.close().await;
}

#[tokio::test]
async fn temp_databases_are_private_and_removed_on_drop() {
    let first = Db::open_temp().await.unwrap();
    let second = Db::open_temp().await.unwrap();
    assert_ne!(first.dir(), second.dir());
    let target = std::fs::canonicalize(TEMP_ROOT).unwrap();
    assert!(first.dir().starts_with(&target), "{:?}", first.dir());
    assert!(!first.dir().starts_with("/tmp"));

    let mut conn = first.write().acquire().await.unwrap();
    insert_setting(&mut conn, "only-here").await.unwrap();
    drop(conn);
    assert_eq!(count(&first, "settings").await, 1);
    assert_eq!(count(&second, "settings").await, 0);

    let dir = first.dir().to_path_buf();
    first.close().await;
    drop(first);
    assert!(!dir.exists());
}

#[tokio::test]
async fn from_config_creates_the_data_dir_and_registers_the_volumes() {
    let temp = Db::open_temp().await.unwrap();
    let config = Config::for_tests(temp.dir());
    std::fs::create_dir_all(config.volumes_dir.join("photos")).unwrap();
    assert!(!config.data_dir.exists());

    let db = Db::from_config(&config).await.unwrap();
    assert!(config.data_dir.join(DB_FILE_NAME).exists());
    let volumes = volumes::list(&db).await.unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].name, "photos");
    assert!(volumes[0].online);
    db.close().await;
}

#[tokio::test]
async fn from_config_starts_without_a_volumes_directory() {
    let temp = Db::open_temp().await.unwrap();
    let config = Config::for_tests(temp.dir());
    let db = Db::from_config(&config).await.unwrap();
    assert!(volumes::list(&db).await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn from_config_names_the_database_file_when_it_cannot_open() {
    let temp = Db::open_temp().await.unwrap();
    let config = Config::for_tests(temp.dir());
    // A file where the data directory should be.
    std::fs::write(&config.data_dir, b"not a directory").unwrap();

    let err = Db::from_config(&config).await.unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("cannot open the database"), "{message}");
    assert!(message.contains(DB_FILE_NAME), "{message}");
}

#[tokio::test]
async fn violations_map_to_conflict_and_never_leak_sql() {
    let db = Db::open_temp().await.unwrap();
    insert_volume(&db, "photos").await;

    let duplicate =
        sqlx::query("INSERT INTO volumes (name, root, created_at) VALUES ('photos', '/x', 0)")
            .execute(db.write())
            .await
            .unwrap_err();
    let duplicate = DbError::from(duplicate);
    assert!(duplicate.is_unique_violation());
    assert!(
        duplicate.to_string().contains("volumes.name"),
        "the log keeps the detail"
    );
    let (status, body) = body_text(AppError::from(duplicate)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains(r#""code":"conflict""#), "{body}");
    for leaked in ["INSERT", "volumes", "UNIQUE", "constraint"] {
        assert!(!body.contains(leaked), "{leaked} leaked: {body}");
    }

    let orphan = sqlx::query(
        "INSERT INTO dirs (volume_id, path, parent_path, name) VALUES (999, 'a', '', 'a')",
    )
    .execute(db.write())
    .await
    .unwrap_err();
    let (status, body) = body_text(DbError::from(orphan).into()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        !body.contains("FOREIGN") && !body.contains("dirs"),
        "{body}"
    );
}

#[tokio::test]
async fn other_failures_map_to_internal_without_the_cause() {
    let db = Db::open_temp().await.unwrap();
    let check = sqlx::query(
        "INSERT INTO volumes (name, root, writable, created_at) VALUES ('v', '/v', 2, 0)",
    )
    .execute(db.write())
    .await
    .unwrap_err();
    let check = DbError::from(check);
    assert!(check.is_check_violation());
    assert!(!check.is_unique_violation() && !check.is_busy() && !check.is_not_found());
    let (status, body) = body_text(check.into()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains(r#""code":"internal""#), "{body}");
    assert!(
        !body.contains("CHECK") && !body.contains("writable"),
        "{body}"
    );

    let setup = DbError::Setup("secret detail /data/apm.db".into());
    let (status, body) = body_text(setup.into()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!body.contains("secret"), "{body}");
}

#[tokio::test]
async fn a_missing_row_maps_to_not_found() {
    let db = Db::open_temp().await.unwrap();
    let err = sqlx::query_scalar::<_, i64>("SELECT id FROM volumes WHERE id = 1")
        .fetch_one(db.read())
        .await
        .unwrap_err();
    let err = DbError::from(err);
    assert!(err.is_not_found());
    let (status, body) = body_text(err.into()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains(r#""code":"not_found""#), "{body}");
}

#[tokio::test]
async fn pool_timeouts_map_to_service_unavailable() {
    let err = DbError::Sqlx(sqlx::Error::PoolTimedOut);
    assert!(err.is_busy());
    let (status, body) = body_text(err.into()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains(r#""code":"db_busy""#), "{body}");

    assert!(!DbError::Sqlx(sqlx::Error::PoolClosed).is_busy());
    assert!(!DbError::Setup("x".into()).is_busy());
}

#[tokio::test]
async fn a_locked_database_is_busy_and_maps_to_service_unavailable() {
    let db = Db::open_temp().await.unwrap();
    let tx = db.begin_write().await.unwrap();
    let mut other = raw_options(&db).connect().await.unwrap();
    let err = sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut other)
        .await
        .unwrap_err();
    let (status, body) = body_text(DbError::from(err).into()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains(r#""code":"db_busy""#), "{body}");
    assert!(!body.contains("locked"), "{body}");
    tx.rollback().await.unwrap();
}

// ---------------------------------------------------------------------------------------------
// planner statistics

const CAMERA_AND_RATING: &str =
    "EXPLAIN QUERY PLAN SELECT id FROM files WHERE camera_id = 1 AND rating = 3";

/// The plan of [`CAMERA_AND_RATING`] on one line. The statement is prepared afresh each time: a
/// cached one keeps the plan it was compiled with.
async fn plan_of_camera_and_rating(conn: &mut sqlx::SqliteConnection) -> String {
    use sqlx::Row as _;
    let rows = sqlx::query(CAMERA_AND_RATING)
        .persistent(false)
        .fetch_all(conn)
        .await
        .unwrap();
    rows.iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 3000 files of one camera with ratings from 0 to 5: `rating = 3` selects far fewer rows than
/// `camera_id = 1`.
async fn fill_files_of_one_camera(db: &Db) {
    let volume = insert_volume(db, "photos").await;
    sqlx::query("INSERT INTO cameras (name) VALUES ('only')")
        .execute(db.write())
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime, \
                            camera_id, rating) \
         SELECT ?, 'd', 'f' || i, 'f' || i, 'f' || i, 'other', 1, 1, 1, i % 6 FROM n",
    )
    .bind(volume)
    .execute(db.write())
    .await
    .unwrap();
}

/// Turns the data of [`fill_files_of_one_camera`] around: every file has its own camera and the
/// same rating, so `camera_id = 1` now selects far fewer rows than `rating = 3`.
async fn give_every_file_its_own_camera(db: &Db) {
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 2 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         INSERT INTO cameras (name) SELECT 'camera ' || i FROM n",
    )
    .execute(db.write())
    .await
    .unwrap();
    sqlx::query("UPDATE files SET camera_id = id, rating = 3")
        .execute(db.write())
        .await
        .unwrap();
}

/// A complete `ANALYZE`: the writer samples by default, and sampled statistics would make the plans
/// below depend on the sample.
async fn analyse(db: &Db) {
    sqlx::query("PRAGMA analysis_limit = 0")
        .execute(db.write())
        .await
        .unwrap();
    sqlx::query("ANALYZE").execute(db.write()).await.unwrap();
}

/// A connection reads `sqlite_stat1` when it loads the schema, and does not read it again when a
/// later `ANALYZE` rewrites it. A reader that is never replaced would plan with the statistics it
/// started with. That is what the reader lifetime is for.
#[tokio::test]
async fn readers_are_recycled_so_that_they_use_new_statistics() {
    let lifetime = Duration::from_millis(100);
    let temp = Db::open_temp().await.unwrap();
    let db = Db::open_recycling_readers_after(&temp.path(), lifetime)
        .await
        .unwrap();
    fill_files_of_one_camera(&db).await;
    analyse(&db).await;

    let mut held = db.read().acquire().await.unwrap();
    let plan = plan_of_camera_and_rating(&mut held).await;
    assert!(plan.contains("files_rating"), "{plan}");

    // `held` is checked out for longer than its lifetime, and the statistics change meanwhile.
    tokio::time::sleep(lifetime * 2).await;
    give_every_file_its_own_camera(&db).await;
    analyse(&db).await;
    let plan = plan_of_camera_and_rating(&mut held).await;
    assert!(
        plan.contains("files_rating"),
        "an open connection is expected to keep the statistics it loaded (if SQLite now refreshes \
         them, the reader lifetime is no longer needed): {plan}"
    );

    // The pool takes a released connection back in a task of its own. Wait until it has either
    // closed the connection (past its lifetime) or parked it for reuse, so that the next `acquire`
    // cannot just open a new one.
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), async {
        while db.read().size() > 0 && db.read().num_idle() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the pool did not take the connection back");
    let mut recycled = db.read().acquire().await.unwrap();
    let plan = plan_of_camera_and_rating(&mut recycled).await;
    assert!(plan.contains("files_camera"), "{plan}");
}

#[tokio::test]
async fn only_the_readers_have_a_lifetime() {
    let db = Db::open_temp().await.unwrap();
    assert_eq!(
        db.read().options().get_max_lifetime(),
        Some(READER_MAX_LIFETIME)
    );
    assert_eq!(db.write().options().get_max_lifetime(), None);
}

/// Rows of `sqlite_stat1` that describe `files`: 0 as long as no `ANALYZE` has created the table.
async fn files_statistics_rows(db: &Db) -> i64 {
    let table: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE name = 'sqlite_stat1'")
            .fetch_optional(db.write())
            .await
            .unwrap();
    if table.is_none() {
        return 0;
    }
    sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'files'")
        .fetch_one(db.write())
        .await
        .unwrap()
}

#[tokio::test]
async fn optimize_analyses_a_table_that_has_no_statistics_yet() {
    let db = Db::open_temp().await.unwrap();
    fill_files_of_one_camera(&db).await;
    assert_eq!(files_statistics_rows(&db).await, 0);

    db.optimize().await.unwrap();
    assert!(
        files_statistics_rows(&db).await > 0,
        "PRAGMA optimize collected no statistics for `files`"
    );
}

#[tokio::test]
async fn close_updates_the_statistics() {
    let db = Db::open_temp().await.unwrap();
    fill_files_of_one_camera(&db).await;
    assert_eq!(files_statistics_rows(&db).await, 0);
    db.close().await;

    let mut conn = raw_options(&db).connect().await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'files'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert!(rows > 0, "closing the database collected no statistics");
}
