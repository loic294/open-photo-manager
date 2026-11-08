//! What the trash page relies on in the database, pinned: ids are never given out twice, the table
//! that migration 0047 rebuilt kept its rows and its rules, and the shapes that must stay fast at a
//! million rows are answered from an index (`EXPLAIN QUERY PLAN`), never by a scan and a sort.
//!
//! The plans are those of the statements the service itself runs (`list.rs`, `empty.rs`,
//! `retention.rs`), so a change to one of them is judged here. The data set is small; its
//! statistics are gathered with `ANALYZE` as production does, and the indexes were chosen on
//! 1,000,000 rows (see migration 0048).

use std::path::{Path, PathBuf};

use apm_core::dto::{SortOrder, TrashBatchesQuery, TrashListQuery};
use apm_core::paths::VolumeId;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, QueryBuilder, Sqlite, SqlitePool};
use tempfile::TempDir;

use super::empty::{PurgeSelector, select_query};
use super::list::{
    MAX_ID_SQL, TOTALS_SQL, batches_count_query, batches_query, count_query, list_query,
    reasons_query,
};
use super::retention::DUE_SQL;
use crate::db::testing::insert_volume;
use crate::db::{Db, TempDb};

/// Items per deletion in the data set.
const PER_BATCH: i64 = 50;

struct Seeded {
    _temp: TempDb,
    db: Db,
    first: VolumeId,
    second: VolumeId,
}

/// Two volumes with 2,000 and 1,000 items in deletions of 50, and the statistics gathered.
async fn seeded() -> Seeded {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let first = insert_volume(&db, "photos").await;
    let second = insert_volume(&db, "archive").await;
    seed_volume(&db, first, 2_000).await;
    seed_volume(&db, second, 1_000).await;
    sqlx::query("ANALYZE").execute(db.write()).await.unwrap();
    Seeded {
        _temp: temp,
        db,
        first,
        second,
    }
}

async fn seed_volume(db: &Db, volume_id: VolumeId, items: i64) {
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?2) \
         INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, reason, \
         task_id, trashed_at) \
         SELECT ?1, printf('20240101-%06d-t%d', i / ?3, i / ?3), \
         printf('dir%d/file%d.jpg', i % 7, i), \
         printf('.apm-trash/20240101-%06d-t%d/dir%d/file%d.jpg', i / ?3, i / ?3, i % 7, i), \
         0, i + 1, CASE i % 3 WHEN 0 THEN 'user' WHEN 1 THEN 'replaced' END, i / ?3, \
         1700000000 + (i / ?3) * 60 FROM n",
    )
    .bind(volume_id)
    .bind(items)
    .bind(PER_BATCH)
    .execute(db.write())
    .await
    .unwrap();
}

/// The name of the first deletion of a volume, as the seed built it.
const FIRST_BATCH: &str = "20240101-000000-t0";

/// The `detail` lines of the `EXPLAIN QUERY PLAN` of a statement. It runs on the writer
/// connection, which always has the statistics of the last `ANALYZE`.
async fn plan_of(db: &Db, builder: &QueryBuilder<Sqlite>) -> Vec<String> {
    plan_of_sql(db, builder.sql().as_str()).await
}

async fn plan_of_sql(db: &Db, sql: &str) -> Vec<String> {
    let rows: Vec<(i64, i64, i64, String)> =
        sqlx::query_as(AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
            .fetch_all(db.write())
            .await
            .unwrap();
    rows.into_iter().map(|(.., detail)| detail).collect()
}

fn has(plan: &[String], prefix: &str) -> bool {
    plan.iter().any(|line| line.starts_with(prefix))
}

fn sorts(plan: &[String]) -> bool {
    plan.iter().any(|line| line.contains("TEMP B-TREE"))
}

#[tokio::test]
async fn the_newest_items_come_from_the_trashed_at_index_without_a_sort() {
    let seeded = seeded().await;
    let plan = plan_of(&seeded.db, &list_query(&TrashListQuery::default())).await;
    assert!(
        has(&plan, "SCAN t USING INDEX trash_items_trashed_at"),
        "{plan:#?}"
    );
    assert!(!sorts(&plan), "{plan:#?}");
}

#[tokio::test]
async fn the_oldest_items_walk_the_same_index_backwards() {
    let seeded = seeded().await;
    let query = TrashListQuery {
        order: Some(SortOrder::Asc),
        ..TrashListQuery::default()
    };
    let plan = plan_of(&seeded.db, &list_query(&query)).await;
    assert!(
        has(&plan, "SCAN t USING INDEX trash_items_trashed_at"),
        "{plan:#?}"
    );
    assert!(!sorts(&plan), "{plan:#?}");
}

#[tokio::test]
async fn one_volume_is_read_from_its_own_index_without_a_sort() {
    let seeded = seeded().await;
    let query = TrashListQuery {
        volume_id: Some(seeded.second),
        ..TrashListQuery::default()
    };
    let plan = plan_of(&seeded.db, &list_query(&query)).await;
    assert!(
        has(
            &plan,
            "SEARCH t USING INDEX trash_items_volume_trashed (volume_id=?)"
        ),
        "{plan:#?}"
    );
    assert!(!sorts(&plan), "{plan:#?}");
}

#[tokio::test]
async fn one_deletion_is_read_from_the_batch_index_without_a_sort() {
    let seeded = seeded().await;
    let query = TrashListQuery {
        volume_id: Some(seeded.first),
        batch: Some(FIRST_BATCH.to_owned()),
        ..TrashListQuery::default()
    };
    let plan = plan_of(&seeded.db, &list_query(&query)).await;
    assert!(
        has(
            &plan,
            "SEARCH t USING INDEX trash_items_volume_batch (volume_id=? AND batch=?)"
        ),
        "{plan:#?}"
    );
    assert!(!sorts(&plan), "{plan:#?}");

    let count = plan_of(&seeded.db, &count_query(&query)).await;
    assert!(
        has(
            &count,
            "SEARCH t USING COVERING INDEX trash_items_volume_batch (volume_id=? AND batch=?)"
        ),
        "{count:#?}"
    );
}

#[tokio::test]
async fn the_summary_reads_the_running_totals_and_one_descent_for_the_highest_id() {
    let seeded = seeded().await;
    let totals = plan_of_sql(&seeded.db, TOTALS_SQL).await;
    assert!(has(&totals, "SCAN trash_volume_totals"), "{totals:#?}");
    assert!(
        totals.iter().all(|line| !line.contains("trash_items")),
        "{totals:#?}"
    );

    let max_id = plan_of_sql(&seeded.db, MAX_ID_SQL).await;
    assert!(has(&max_id, "SEARCH trash_items"), "{max_id:#?}");
    assert!(
        max_id.iter().all(|line| !line.starts_with("SCAN")),
        "{max_id:#?}"
    );
}

#[tokio::test]
async fn the_deletions_never_touch_the_table_itself() {
    let seeded = seeded().await;
    for volume_id in [None, Some(seeded.first)] {
        let query = TrashBatchesQuery {
            volume_id,
            ..TrashBatchesQuery::default()
        };
        for plan in [
            plan_of(&seeded.db, &batches_query(&query)).await,
            plan_of(&seeded.db, &batches_count_query(&query)).await,
        ] {
            let reads: Vec<&String> = plan
                .iter()
                .filter(|line| line.contains("trash_items"))
                .collect();
            assert!(!reads.is_empty(), "{plan:#?}");
            assert!(
                reads
                    .iter()
                    .all(|line| line.contains("COVERING INDEX trash_items_volume_batch")),
                "{plan:#?}"
            );
        }
    }
}

#[tokio::test]
async fn the_reasons_of_a_deletion_are_one_lookup_of_the_covering_index() {
    let seeded = seeded().await;
    let batches = vec![
        (seeded.first, FIRST_BATCH.to_owned()),
        (seeded.second, FIRST_BATCH.to_owned()),
    ];
    let plan = plan_of(&seeded.db, &reasons_query(&batches)).await;
    assert!(
        has(
            &plan,
            "SEARCH i USING COVERING INDEX trash_items_volume_batch (volume_id=? AND batch=?)"
        ),
        "{plan:#?}"
    );
}

#[tokio::test]
async fn the_retention_sweeper_asks_the_trashed_at_index() {
    let seeded = seeded().await;

    let due = plan_of_sql(&seeded.db, DUE_SQL).await;
    assert!(
        has(
            &due,
            "SEARCH trash_items USING COVERING INDEX trash_items_trashed_at (trashed_at<?)"
        ),
        "{due:#?}"
    );

    let doomed = plan_of(
        &seeded.db,
        &select_query(PurgeSelector::OlderThan { cutoff: 1 }),
    )
    .await;
    assert!(
        has(
            &doomed,
            "SEARCH trash_items USING INDEX trash_items_trashed_at (trashed_at<?)"
        ),
        "{doomed:#?}"
    );
    assert!(!sorts(&doomed), "{doomed:#?}");
}

#[tokio::test]
async fn the_seed_has_the_shape_the_plans_were_judged_on() {
    let seeded = seeded().await;
    let (items, batches): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT volume_id || batch) FROM trash_items")
            .fetch_one(seeded.db.read())
            .await
            .unwrap();
    assert_eq!((items, batches), (3_000, 60));
    let (rows,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'trash_items'")
            .fetch_one(seeded.db.read())
            .await
            .unwrap();
    assert!(rows > 0, "ANALYZE must have gathered statistics");
}

async fn insert_item(
    pool: &SqlitePool,
    volume_id: VolumeId,
    name: &str,
) -> Result<i64, sqlx::Error> {
    insert_sized(pool, volume_id, name, 0).await
}

async fn insert_sized(
    pool: &SqlitePool,
    volume_id: VolumeId,
    name: &str,
    size: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, size, trashed_at) \
         VALUES (?, 'b', ?, ?, ?, 1) RETURNING id",
    )
    .bind(volume_id)
    .bind(name)
    .bind(format!(".apm-trash/b/{name}"))
    .bind(size)
    .fetch_one(pool)
    .await
}

/// `(volume_id, items, bytes)` of every volume, as the triggers keep them.
async fn kept_totals(pool: &SqlitePool) -> Vec<(VolumeId, i64, i64)> {
    sqlx::query_as("SELECT volume_id, items, bytes FROM trash_volume_totals ORDER BY volume_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The same, counted from the rows: what the kept totals must always equal.
async fn counted_totals(pool: &SqlitePool) -> Vec<(VolumeId, i64, i64)> {
    sqlx::query_as(
        "SELECT volume_id, COUNT(*), COALESCE(SUM(size), 0) FROM trash_items \
         GROUP BY volume_id ORDER BY volume_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn assert_totals(pool: &SqlitePool, expected: &[(VolumeId, i64, i64)]) {
    assert_eq!(kept_totals(pool).await, expected);
    assert_eq!(counted_totals(pool).await, expected);
}

#[tokio::test]
async fn the_totals_follow_every_change_of_the_rows() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let pool = db.write();
    let first = insert_volume(&db, "photos").await;
    let second = insert_volume(&db, "archive").await;
    assert_totals(pool, &[]).await;

    let a = insert_sized(pool, first, "a.jpg", 10).await.unwrap();
    let b = insert_sized(pool, first, "b.jpg", 20).await.unwrap();
    let c = insert_sized(pool, second, "c.jpg", 5).await.unwrap();
    assert_totals(pool, &[(first, 2, 30), (second, 1, 5)]).await;

    sqlx::query("DELETE FROM trash_items WHERE id = ?")
        .bind(a)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[(first, 1, 20), (second, 1, 5)]).await;

    sqlx::query("UPDATE trash_items SET size = 50 WHERE id = ?")
        .bind(b)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[(first, 1, 50), (second, 1, 5)]).await;

    // A change of another column leaves the totals alone.
    sqlx::query("UPDATE trash_items SET reason = 'user', trashed_at = 9 WHERE id = ?")
        .bind(b)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[(first, 1, 50), (second, 1, 5)]).await;

    // The last item of a volume moves away: the volume has no row any more, not a row of zeros.
    sqlx::query("UPDATE trash_items SET volume_id = ? WHERE id = ?")
        .bind(second)
        .bind(b)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[(second, 2, 55)]).await;

    // Taking out everything of a volume leaves the table empty.
    sqlx::query("DELETE FROM trash_items WHERE id IN (?, ?)")
        .bind(b)
        .bind(c)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[]).await;
}

#[tokio::test]
async fn a_volume_that_goes_takes_its_totals_with_it() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let pool = db.write();
    let first = insert_volume(&db, "photos").await;
    let second = insert_volume(&db, "archive").await;
    insert_sized(pool, first, "a.jpg", 10).await.unwrap();
    insert_sized(pool, second, "b.jpg", 7).await.unwrap();
    insert_sized(pool, second, "c.jpg", 8).await.unwrap();

    sqlx::query("DELETE FROM volumes WHERE id = ?")
        .bind(second)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[(first, 1, 10)]).await;
}

#[tokio::test]
async fn the_totals_equal_the_rows_after_bulk_statements() {
    let seeded = seeded().await;
    let pool = seeded.db.write();
    let expected = |first: (i64, i64), second: (i64, i64)| {
        vec![
            (seeded.first, first.0, first.1),
            (seeded.second, second.0, second.1),
        ]
    };
    // 2,000 items of sizes 1..=2000 and 1,000 items of sizes 1..=1000.
    assert_totals(pool, &expected((2_000, 2_001_000), (1_000, 500_500))).await;

    // One deletion of 50 items (sizes 1..=50).
    sqlx::query("DELETE FROM trash_items WHERE volume_id = ? AND batch = ?")
        .bind(seeded.first)
        .bind(FIRST_BATCH)
        .execute(pool)
        .await
        .unwrap();
    assert_totals(
        pool,
        &expected((1_950, 2_001_000 - 1_275), (1_000, 500_500)),
    )
    .await;

    // Every third row of the second volume grows by 1,000 bytes.
    sqlx::query("UPDATE trash_items SET size = size + 1000 WHERE volume_id = ? AND id % 3 = 0")
        .bind(seeded.second)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(kept_totals(pool).await, counted_totals(pool).await);

    // Half of the first volume goes to the second (under paths the second does not use yet).
    sqlx::query(
        "UPDATE trash_items SET volume_id = ?, trash_path = '.apm-trash/moved/' || id \
         WHERE volume_id = ? AND id % 2 = 0",
    )
    .bind(seeded.second)
    .bind(seeded.first)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(kept_totals(pool).await, counted_totals(pool).await);

    sqlx::query("DELETE FROM trash_items WHERE id % 5 < 3")
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(kept_totals(pool).await, counted_totals(pool).await);

    sqlx::query("DELETE FROM trash_items")
        .execute(pool)
        .await
        .unwrap();
    assert_totals(pool, &[]).await;
}

#[tokio::test]
async fn an_id_is_never_given_out_twice() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume_id = insert_volume(&db, "photos").await;

    let first = insert_item(db.write(), volume_id, "a.jpg").await.unwrap();
    let second = insert_item(db.write(), volume_id, "b.jpg").await.unwrap();
    assert!(second > first);

    // The newest rows go away (restored, deleted for good): the next one is still above them.
    sqlx::query("DELETE FROM trash_items")
        .execute(db.write())
        .await
        .unwrap();
    let third = insert_item(db.write(), volume_id, "c.jpg").await.unwrap();
    assert!(third > second, "{third} must be above {second}");
}

#[tokio::test]
async fn the_table_enforces_its_rules() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume_id = insert_volume(&db, "photos").await;
    insert_item(db.write(), volume_id, "a.jpg").await.unwrap();

    let refused = |sql: &'static str| {
        let pool = db.write().clone();
        async move {
            sqlx::query(sql)
                .bind(volume_id)
                .execute(&pool)
                .await
                .unwrap_err()
                .to_string()
        }
    };
    let outside = refused(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, trashed_at) \
         VALUES (?, 'b', 'x', 'elsewhere/x', 1)",
    )
    .await;
    assert!(outside.contains("CHECK"), "{outside}");
    let not_json = refused(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, meta_json, trashed_at) \
         VALUES (?, 'b', 'x', '.apm-trash/b/x', 'not json', 1)",
    )
    .await;
    assert!(not_json.contains("CHECK"), "{not_json}");
    let bad_flag = refused(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, trashed_at) \
         VALUES (?, 'b', 'x', '.apm-trash/b/x', 2, 1)",
    )
    .await;
    assert!(bad_flag.contains("CHECK"), "{bad_flag}");
    let twice = refused(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, trashed_at) \
         VALUES (?, 'b', 'a.jpg', '.apm-trash/b/a.jpg', 1)",
    )
    .await;
    assert!(twice.contains("UNIQUE"), "{twice}");
    let text_size = refused(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, size, trashed_at) \
         VALUES (?, 'b', 'x', '.apm-trash/b/x', 'big', 1)",
    )
    .await;
    assert!(text_size.contains("INTEGER"), "{text_size}");

    // A volume that goes takes its trash rows with it.
    sqlx::query("DELETE FROM volumes WHERE id = ?")
        .bind(volume_id)
        .execute(db.write())
        .await
        .unwrap();
    let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM trash_items")
        .fetch_one(db.write())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

fn scratch() -> TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("apm-trash-migration-")
        .tempdir_in(base.canonicalize().unwrap())
        .unwrap()
}

/// The migration files whose number is below `before` (or from `from`), in order.
fn migration_files(from: u32, before: u32) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| {
            let number = path.file_name()?.to_str()?.get(..4)?.parse().ok()?;
            (from..before).contains(&number).then_some((number, path))
        })
        .collect();
    files.sort();
    files.into_iter().map(|(_, path)| path).collect()
}

async fn apply(pool: &SqlitePool, files: Vec<PathBuf>) {
    for file in files {
        let sql = std::fs::read_to_string(&file).unwrap();
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap_or_else(|err| panic!("{}: {err}", file.display()));
    }
}

type Row = (
    i64,
    i64,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
    i64,
);

async fn rows(pool: &SqlitePool) -> Vec<Row> {
    sqlx::query_as(
        "SELECT id, volume_id, batch, orig_path, trash_path, is_dir, size, reason, task_id, \
         meta_json, trashed_at FROM trash_items ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_rebuild_of_the_table_keeps_every_row_with_its_id() {
    let dir = scratch();
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("before.db"))
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    apply(&pool, migration_files(0, 47)).await;
    sqlx::query(
        "INSERT INTO volumes (name, root, writable, online, created_at) \
         VALUES ('photos', '/volumes/photos', 1, 1, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Ids with gaps, a folder, a snapshot and a missing reason: all of it must survive.
    sqlx::query(
        "INSERT INTO trash_items (id, volume_id, batch, orig_path, trash_path, is_dir, size, reason, \
         task_id, meta_json, trashed_at) VALUES \
         (1, 1, 'b1', 'x/a.jpg', '.apm-trash/b1/x/a.jpg', 0, 10, 'user', 3, '{\"k\":1}', 100), \
         (2, 1, 'b1', 'x/b.jpg', '.apm-trash/b1/x/b.jpg', 0, 20, NULL, NULL, NULL, 200), \
         (7, 1, 'b2', 'y', '.apm-trash/b2/y', 1, 30, 'moved', NULL, NULL, 300)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before = rows(&pool).await;
    assert_eq!(before.len(), 3);

    apply(&pool, migration_files(47, 49)).await;
    assert_eq!(rows(&pool).await, before);
    // The totals were counted from the rows the migration found.
    assert_totals(&pool, &[(1, 3, 60)]).await;

    // The ids continue after the highest one and are never taken again.
    let next = insert_item(&pool, 1, "c.jpg").await.unwrap();
    assert_eq!(next, 8);
    sqlx::query("DELETE FROM trash_items WHERE id >= 7")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(insert_item(&pool, 1, "d.jpg").await.unwrap(), 9);
    assert_totals(&pool, &[(1, 3, 30)]).await;

    // The unique index and the three of the trash page are there.
    let indexes: Vec<(String, i64)> = sqlx::query_as(
        "SELECT name, \"unique\" FROM pragma_index_list('trash_items') ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<(&str, i64)> = indexes
        .iter()
        .map(|(name, unique)| (name.as_str(), *unique))
        .collect();
    assert_eq!(
        names,
        [
            ("trash_items_trashed_at", 0),
            ("trash_items_volume_batch", 0),
            ("trash_items_volume_trash_path", 1),
            ("trash_items_volume_trashed", 0),
        ]
    );
}
