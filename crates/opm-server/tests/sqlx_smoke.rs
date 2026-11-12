//! Regression spike for the sqlx + SQLite combination the rest of the code base
//! relies on: runtime queries only (never the compile-time `query!` macros),
//! an in-memory pool shared across connections, `QueryBuilder` with
//! `push_bind`, `FromRow`, transactions, and an on-disk WAL database with
//! foreign keys enforced.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
struct FileRow {
    id: i64,
    dir: String,
    name: String,
    size: i64,
    rating: Option<i64>,
}

fn row(dir: &str, name: &str, size: i64, rating: Option<i64>) -> FileRow {
    FileRow {
        id: 0,
        dir: dir.to_owned(),
        name: name.to_owned(),
        size,
        rating,
    }
}

const CREATE_FILES: &str = "CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    dir TEXT NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    rating INTEGER
)";

/// A multi-connection in-memory pool holding the `files` table. Connections are
/// never recycled, so the shared in-memory database survives for the whole test.
async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(CREATE_FILES).execute(&pool).await.unwrap();
    pool
}

async fn count_files(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn in_memory_pool_shares_one_database_across_connections() {
    let pool = memory_pool().await;
    let mut first = pool.acquire().await.unwrap();
    let mut second = pool.acquire().await.unwrap();

    sqlx::query("INSERT INTO files (dir, name, size) VALUES ('', 'a.jpg', 1)")
        .execute(&mut *first)
        .await
        .unwrap();
    let seen: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(&mut *second)
        .await
        .unwrap();
    assert_eq!(seen, 1);
}

#[tokio::test]
async fn query_builder_binds_bulk_inserts_and_dynamic_filters() {
    let pool = memory_pool().await;
    let rows = vec![
        row("2024", "a.jpg", 10, Some(5)),
        row("2024", "b.jpg", 20, Some(4)),
        row("2024", "c.jpg", 30, None),
        row("2023", "d.jpg", 40, Some(5)),
    ];

    let mut insert = QueryBuilder::<Sqlite>::new("INSERT INTO files (dir, name, size, rating) ");
    insert.push_values(&rows, |mut values, item| {
        values
            .push_bind(&item.dir)
            .push_bind(&item.name)
            .push_bind(item.size)
            .push_bind(item.rating);
    });
    let inserted = insert.build().execute(&pool).await.unwrap();
    assert_eq!(inserted.rows_affected(), 4);

    let mut select =
        QueryBuilder::<Sqlite>::new("SELECT id, dir, name, size, rating FROM files WHERE 1 = 1");
    select.push(" AND dir = ").push_bind("2024");
    select.push(" AND rating IN (");
    let mut ratings = select.separated(", ");
    for rating in [4_i64, 5] {
        ratings.push_bind(rating);
    }
    ratings.push_unseparated(")");
    select.push(" ORDER BY name LIMIT ").push_bind(10_i64);

    let found: Vec<FileRow> = select.build_query_as().fetch_all(&pool).await.unwrap();
    let names: Vec<&str> = found.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["a.jpg", "b.jpg"]);
    assert_eq!(found[0].rating, Some(5));
    assert_eq!(found[0].size, 10);
}

#[tokio::test]
async fn bound_values_are_never_interpreted_as_sql() {
    let pool = memory_pool().await;
    let hostile = "x'); DROP TABLE files; --";
    sqlx::query("INSERT INTO files (dir, name, size) VALUES (?, ?, 0)")
        .bind("")
        .bind(hostile)
        .execute(&pool)
        .await
        .unwrap();

    let name: String = sqlx::query_scalar("SELECT name FROM files")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, hostile);
}

#[tokio::test]
async fn transactions_commit_and_roll_back() {
    let pool = memory_pool().await;

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO files (dir, name, size) VALUES ('', 'rolled-back.jpg', 1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(count_files(&pool).await, 0);

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO files (dir, name, size) VALUES ('', 'committed.jpg', 1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count_files(&pool).await, 1);
}

#[tokio::test]
async fn bind_parameters_per_statement_are_limited() {
    // SQLite allows 32766 bound parameters per statement (the bundled build keeps
    // the default): split bigger id sets into chunks of at most 30 000.
    async fn count_in(pool: &SqlitePool, size: i64) -> Result<i64, sqlx::Error> {
        let mut query = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM files WHERE id IN (");
        let mut ids = query.separated(", ");
        for id in 1..=size {
            ids.push_bind(id);
        }
        ids.push_unseparated(")");
        query.build_query_scalar().fetch_one(pool).await
    }

    let pool = memory_pool().await;
    assert_eq!(count_in(&pool, 30_000).await.unwrap(), 0);
    assert!(count_in(&pool, 100_000).await.is_err());
}

#[tokio::test]
async fn on_disk_database_uses_wal_and_enforces_foreign_keys() {
    let dir = common::scratch_dir();
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("index.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();

    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");

    sqlx::query("CREATE TABLE volumes (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE entries (
            id INTEGER PRIMARY KEY,
            volume_id INTEGER NOT NULL REFERENCES volumes(id)
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let orphan = sqlx::query("INSERT INTO entries (volume_id) VALUES (999)")
        .execute(&pool)
        .await;
    assert!(orphan.is_err(), "foreign key violations must be rejected");

    sqlx::query("INSERT INTO volumes (id, name) VALUES (1, 'photos')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entries (volume_id) VALUES (1)")
        .execute(&pool)
        .await
        .unwrap();
}
