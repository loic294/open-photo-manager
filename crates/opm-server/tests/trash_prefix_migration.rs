//! Migration 0049 renames the trash folder prefix of `trash_items` without reusing ids or losing
//! the indexes, triggers and totals of 0047 and 0048.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const LAST_BEFORE_RENAME: i64 = 48;

async fn apply(pool: &sqlx::SqlitePool, wanted: impl Fn(i64) -> bool) {
    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|migration| wanted(migration.version))
    {
        let mut transaction = pool.begin().await.unwrap();
        sqlx::raw_sql(migration.sql.clone())
            .execute(&mut *transaction)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "migration {} ({}) failed: {error}",
                    migration.version, migration.description
                )
            });
        transaction.commit().await.unwrap();
    }
}

async fn pre_rename_database() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(dir.path().join("migration.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    apply(&pool, |version| version <= LAST_BEFORE_RENAME).await;
    (dir, pool)
}

async fn trash(pool: &sqlx::SqlitePool, id: i64, trash_path: &str, size: i64) {
    sqlx::query(
        "INSERT INTO trash_items (id, volume_id, batch, orig_path, trash_path, size, reason, trashed_at) \
         VALUES (?, 1, 'b1', 'a.jpg', ?, ?, 'user', 1)",
    )
    .bind(id)
    .bind(trash_path)
    .bind(size)
    .execute(pool)
    .await
    .unwrap();
}

async fn totals(pool: &sqlx::SqlitePool) -> Vec<(i64, i64, i64)> {
    sqlx::query_as("SELECT volume_id, items, bytes FROM trash_volume_totals ORDER BY volume_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn trash_prefix_migration_keeps_ids_indexes_triggers_and_totals() {
    let (_dir, pool) = pre_rename_database().await;
    sqlx::query("INSERT INTO volumes (name, root, created_at) VALUES ('photos', '/photos', 1)")
        .execute(&pool)
        .await
        .unwrap();
    trash(&pool, 3, ".apm-trash/b1/a.jpg", 10).await;
    trash(&pool, 5, ".apm-trash/b1/dir/b.jpg", 20).await;
    trash(&pool, 9, ".apm-trash/b1/c.jpg", 40).await;
    sqlx::query("DELETE FROM trash_items WHERE id = 9")
        .execute(&pool)
        .await
        .unwrap();
    let before_seq: i64 =
        sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = 'trash_items'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before_seq, 9);
    assert_eq!(totals(&pool).await, vec![(1, 2, 30)]);

    apply(&pool, |version| version > LAST_BEFORE_RENAME).await;

    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, trash_path FROM trash_items ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            (3, ".opm-trash/b1/a.jpg".to_owned()),
            (5, ".opm-trash/b1/dir/b.jpg".to_owned()),
        ]
    );
    assert_eq!(totals(&pool).await, vec![(1, 2, 30)]);
    let sequences: Vec<(String, i64)> = sqlx::query_as(
        "SELECT name, seq FROM sqlite_sequence WHERE name LIKE 'trash_items%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(sequences, vec![("trash_items".to_owned(), before_seq)]);

    let mut indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'trash_items' \
         AND name NOT LIKE 'sqlite_autoindex%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    indexes.sort();
    assert_eq!(
        indexes,
        [
            "trash_items_trashed_at",
            "trash_items_volume_batch",
            "trash_items_volume_trash_path",
            "trash_items_volume_trashed",
        ]
    );

    let next_id: i64 = sqlx::query_scalar(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, size, trashed_at) \
         VALUES (1, 'b2', 'd.jpg', '.opm-trash/b2/d.jpg', 5, 2) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        next_id > before_seq,
        "next id {next_id}, prior sequence {before_seq}"
    );
    assert_eq!(totals(&pool).await, vec![(1, 3, 35)]);
    sqlx::query("UPDATE trash_items SET size = 15 WHERE id = ?")
        .bind(next_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(totals(&pool).await, vec![(1, 3, 45)]);
    sqlx::query("DELETE FROM trash_items")
        .execute(&pool)
        .await
        .unwrap();
    assert!(totals(&pool).await.is_empty());

    let old_prefix = sqlx::query(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, trashed_at) \
         VALUES (1, 'b3', 'e.jpg', '.apm-trash/b3/e.jpg', 3)",
    )
    .execute(&pool)
    .await;
    assert!(old_prefix.is_err(), "the old trash folder must be rejected");

    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}
