//! Upgrade the plan schema from 0230 with live child rows and foreign-key enforcement enabled.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

type ForeignKeyDetail = (i64, i64, String, String, String, String, String, String);

async fn pre_rename_database() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let path = dir.path().join("migration.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|migration| migration.version <= 230)
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
    (dir, pool)
}

async fn insert_plan(pool: &sqlx::SqlitePool, id: i64, kind: &str, state: &str) {
    sqlx::query(
        "INSERT INTO plans (id, kind, state, params_json, created_at, expires_at) \
         VALUES (?, ?, ?, '{}', 1, 999)",
    )
    .bind(id)
    .bind(kind)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn rename_migrations_rebuild_plans_without_reusing_ids_or_breaking_children() {
    let (_dir, pool) = pre_rename_database().await;
    sqlx::query("INSERT INTO volumes (name, root, created_at) VALUES ('photos', '/photos', 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime) \
         VALUES (1, '', 'tracked.jpg', 'tracked.jpg', 'tracked', 'image', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    insert_plan(&pool, 7, "move", "ready").await;
    insert_plan(&pool, 17, "cleanup", "ready").await;
    sqlx::query("DELETE FROM plans WHERE id = 17")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO plan_file_tracking (plan_id) VALUES (7)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO plan_file_identities (plan_id, file_id, live) VALUES (7, 99, 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO duplicate_pairs (plan_id, source_id, comparison_id) VALUES (7, 1, 2)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO duplicate_matches (plan_id, side, file_id, volume_id, dir_path) \
         VALUES (7, 'source', 99, 1, '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO move_plans (plan_id, conflict_policy, name_max_len) VALUES (7, 'smart', 255)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO move_items (plan_id, file_id, dst_volume_id, dst_dir, dst_name, \
         dst_name_norm, status, src_volume_id, src_dir, src_name, src_size, src_mtime) \
         VALUES (7, 99, 1, '', 'new.jpg', 'new.jpg', 'move', 1, '', 'tracked.jpg', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cleanup_dirs (plan_id, path, depth) VALUES (7, 'photos', 1)")
        .execute(&pool)
        .await
        .unwrap();

    let before_seq: i64 =
        sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = 'plans'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before_seq, 17);

    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|migration| migration.version > 230)
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

    let remaining_old_plans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans WHERE id = 7")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining_old_plans, 0);
    for (table, statement) in [
        ("duplicate_pairs", "SELECT COUNT(*) FROM duplicate_pairs"),
        (
            "duplicate_matches",
            "SELECT COUNT(*) FROM duplicate_matches",
        ),
        (
            "plan_file_tracking",
            "SELECT COUNT(*) FROM plan_file_tracking",
        ),
        (
            "plan_file_identities",
            "SELECT COUNT(*) FROM plan_file_identities",
        ),
        ("move_plans", "SELECT COUNT(*) FROM move_plans"),
        ("move_items", "SELECT COUNT(*) FROM move_items"),
        ("cleanup_dirs", "SELECT COUNT(*) FROM cleanup_dirs"),
    ] {
        let count: i64 = sqlx::query_scalar(statement)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let next_id: i64 = sqlx::query_scalar(
        "INSERT INTO plans (kind, state, params_json, created_at, expires_at) \
         VALUES ('rename', 'ready', '{}', 1, 999) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        next_id > before_seq,
        "next id {next_id}, prior sequence {before_seq}"
    );
    let invalid = sqlx::query(
        "INSERT INTO plans (kind, state, params_json, created_at, expires_at) \
         VALUES ('unknown', 'ready', '{}', 1, 999)",
    )
    .execute(&pool)
    .await;
    assert!(invalid.is_err(), "unknown plan kinds must remain rejected");

    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    for (table, statement) in [
        (
            "duplicate_pairs",
            "PRAGMA foreign_key_list(duplicate_pairs)",
        ),
        (
            "duplicate_matches",
            "PRAGMA foreign_key_list(duplicate_matches)",
        ),
        (
            "plan_file_tracking",
            "PRAGMA foreign_key_list(plan_file_tracking)",
        ),
        (
            "plan_file_identities",
            "PRAGMA foreign_key_list(plan_file_identities)",
        ),
        ("move_plans", "PRAGMA foreign_key_list(move_plans)"),
        ("move_items", "PRAGMA foreign_key_list(move_items)"),
        ("cleanup_dirs", "PRAGMA foreign_key_list(cleanup_dirs)"),
    ] {
        let details: Vec<ForeignKeyDetail> =
            sqlx::query_as(statement).fetch_all(&pool).await.unwrap();
        assert!(
            details.iter().any(|row| row.2 == "plans"),
            "{table}: {details:?}"
        );
    }
    let leftovers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE '%plans_new%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    for index in [
        "plans_expires",
        "plans_task",
        "plan_file_building",
        "move_items_live_destination",
        "move_items_live_source",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "{index}");
    }
    let seq_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('move_items') WHERE name = 'seq'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(seq_column, 1);

    let building_id: i64 = sqlx::query_scalar(
        "INSERT INTO plans (kind, state, params_json, created_at, expires_at) \
         VALUES ('rename', 'building', '{}', 1, 999) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO plan_file_tracking (plan_id) VALUES (?)")
        .bind(building_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM files WHERE name = 'tracked.jpg'")
        .execute(&pool)
        .await
        .unwrap();
    let dead: i64 = sqlx::query_scalar(
        "SELECT live FROM plan_file_identities WHERE plan_id = ? AND file_id = 1",
    )
    .bind(building_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dead, 0);

    sqlx::query(
        "INSERT INTO move_plans (plan_id, conflict_policy, name_max_len) VALUES (?, 'smart', 255)",
    )
    .bind(building_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO move_items (plan_id, file_id, dst_volume_id, dst_dir, dst_name, \
         dst_name_norm, status, src_volume_id, src_dir, src_name, src_size, src_mtime, seq) \
         VALUES (?, 1, 1, '', 'new.jpg', 'new.jpg', 'move', 1, '', 'tracked.jpg', 1, 1, 1)",
    )
    .bind(building_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM plans WHERE id = ?")
        .bind(building_id)
        .execute(&pool)
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM move_plans WHERE plan_id = ?) + \
         (SELECT COUNT(*) FROM move_items WHERE plan_id = ?) + \
         (SELECT COUNT(*) FROM plan_file_tracking WHERE plan_id = ?)",
    )
    .bind(building_id)
    .bind(building_id)
    .bind(building_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0);
}
