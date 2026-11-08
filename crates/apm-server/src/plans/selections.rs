//! Server-side selections: stored id sets that a `Filter` can reference through
//! `selection_id`.
//!
//! A selection is made once (`POST /api/selections`, or handed over by a plan with
//! `POST /api/plans/{id}/selection`) and then used by value: browse and facets restrict to it with
//! `Filter::selection_id`, a planner restricts what it builds with the same filter, and
//! `ExecutePlanRequest::only` limits an execution to its members. Ids are checked against the index
//! when the selection is made (an id with no file is dropped), and the rows follow the files: the
//! foreign key cascade removes a member whose file row goes away.
//!
//! This file is the SQL layer, stateless functions over [`Db`] that `PlanService` drives with its
//! clock, limits and throttle. Every statement that writes is short: ids go in and out in chunks,
//! so the one writer connection is never held for long.
//!
//! Owned by todo `building-plan-engine` (with `api/selections.rs`).

use apm_core::dto::SelectionDto;
use apm_core::paths::{FileId, SelectionId};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};
use tokio_util::sync::CancellationToken;

use super::planner::PlanError;
use crate::db::{Db, DbError};

/// Ids per statement: far below SQLite's 32,766 binds, and a write of a few milliseconds.
pub(super) const ID_CHUNK: usize = 5_000;

/// Selections looked at per sweep step.
const SWEEP_BATCH: i64 = 100;

#[derive(Debug, FromRow)]
struct SelectionRow {
    id: SelectionId,
    label: Option<String>,
    created_at: i64,
    expires_at: i64,
}

/// Creates a selection of the files among `ids` that exist. `ids` must be unique (the caller sorts
/// and de-duplicates them); the others are dropped silently. A failure removes what was written.
pub(super) async fn create(
    db: &Db,
    ids: &[FileId],
    label: Option<&str>,
    now: i64,
    expires_at: i64,
) -> Result<SelectionId, DbError> {
    create_chunked(db, ids, label, now, expires_at, ID_CHUNK).await
}

async fn create_chunked(
    db: &Db,
    ids: &[FileId],
    label: Option<&str>,
    now: i64,
    expires_at: i64,
    chunk: usize,
) -> Result<SelectionId, DbError> {
    let id: SelectionId = sqlx::query_scalar(
        "INSERT INTO selections (created_at, expires_at, label, item_count) \
         VALUES (?1, ?2, ?3, 0) RETURNING id",
    )
    .bind(now)
    .bind(expires_at)
    .bind(label)
    .fetch_one(db.write())
    .await?;
    match fill(db, id, ids, chunk).await {
        Ok(()) => Ok(id),
        Err(err) => {
            if let Err(cleanup) = sqlx::query("DELETE FROM selections WHERE id = ?1")
                .bind(id)
                .execute(db.write())
                .await
            {
                tracing::warn!(selection_id = id, error = %cleanup, "could not remove a half-made selection");
            }
            Err(err)
        }
    }
}

async fn fill(db: &Db, id: SelectionId, ids: &[FileId], chunk: usize) -> Result<(), DbError> {
    let mut added: i64 = 0;
    for part in ids.chunks(chunk.max(1)) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO selection_items (selection_id, file_id) SELECT ",
        );
        query.push_bind(id);
        query.push(", id FROM files WHERE id IN (");
        let mut list = query.separated(", ");
        for file_id in part {
            list.push_bind(*file_id);
        }
        list.push_unseparated(")");
        let done = query.build().execute(db.write()).await?;
        added += i64::try_from(done.rows_affected()).unwrap_or(i64::MAX);
    }
    sqlx::query("UPDATE selections SET item_count = ?2 WHERE id = ?1")
        .bind(id)
        .bind(added)
        .execute(db.write())
        .await?;
    Ok(())
}

/// The selection a client may see (not expired), with the number and total size of the files that
/// are in it now.
pub(super) async fn get(
    pool: &SqlitePool,
    id: SelectionId,
    now: i64,
) -> Result<Option<SelectionDto>, DbError> {
    let row: Option<SelectionRow> = sqlx::query_as(
        "SELECT id, label, created_at, expires_at FROM selections \
         WHERE id = ?1 AND expires_at > ?2",
    )
    .bind(id)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let (count, size): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(f.size), 0) \
         FROM selection_items si JOIN files f ON f.id = si.file_id \
         WHERE si.selection_id = ?1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(Some(SelectionDto {
        id: row.id,
        item_count: u64::try_from(count).unwrap_or_default(),
        total_size: u64::try_from(size).unwrap_or_default(),
        label: row.label,
        created_at: row.created_at,
        expires_at: row.expires_at,
    }))
}

/// The expiry of a selection a client may see (`None` when it is unknown or expired): the cheap
/// existence check, and what the throttled extension compares against.
pub(super) async fn expiry(
    pool: &SqlitePool,
    id: SelectionId,
    now: i64,
) -> Result<Option<i64>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT expires_at FROM selections WHERE id = ?1 AND expires_at > ?2")
            .bind(id)
            .bind(now)
            .fetch_optional(pool)
            .await?,
    )
}

/// Pushes the expiry of a live selection forward, never back and never from the dead.
pub(super) async fn extend(
    db: &Db,
    id: SelectionId,
    now: i64,
    expires_at: i64,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE selections SET expires_at = ?3 \
         WHERE id = ?1 AND expires_at > ?2 AND expires_at < ?3",
    )
    .bind(id)
    .bind(now)
    .bind(expires_at)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The ids of the members of a selection, for an execution restricted with `only`. A selection
/// that is gone has no members: the restriction then lets nothing through.
pub(super) async fn file_ids(db: &Db, id: SelectionId) -> Result<Vec<FileId>, PlanError> {
    Ok(
        sqlx::query_scalar("SELECT file_id FROM selection_items WHERE selection_id = ?1")
            .bind(id)
            .fetch_all(db.read())
            .await?,
    )
}

/// Deletes the selections that expired by `now`, a chunk of items at a time. Returns how many
/// selections it removed; stops early when `cancel` fires (the rest waits for the next sweep).
pub(super) async fn purge_expired(
    db: &Db,
    cancel: &CancellationToken,
    now: i64,
) -> Result<u64, DbError> {
    purge_expired_chunked(db, cancel, now, i64::try_from(ID_CHUNK).unwrap_or(5_000)).await
}

async fn purge_expired_chunked(
    db: &Db,
    cancel: &CancellationToken,
    now: i64,
    chunk: i64,
) -> Result<u64, DbError> {
    let mut purged = 0;
    loop {
        let batch: Vec<SelectionId> = sqlx::query_scalar(
            "SELECT id FROM selections WHERE expires_at <= ?1 ORDER BY expires_at, id LIMIT ?2",
        )
        .bind(now)
        .bind(SWEEP_BATCH)
        .fetch_all(db.read())
        .await?;
        if batch.is_empty() {
            return Ok(purged);
        }
        for id in batch {
            if !purge_one(db, cancel, id, chunk).await? {
                return Ok(purged);
            }
            purged += 1;
        }
    }
}

/// Empties a selection chunk by chunk, then deletes it. `false` when `cancel` fired first.
async fn purge_one(
    db: &Db,
    cancel: &CancellationToken,
    id: SelectionId,
    chunk: i64,
) -> Result<bool, DbError> {
    loop {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let removed = sqlx::query(
            "DELETE FROM selection_items WHERE selection_id = ?1 AND file_id IN \
             (SELECT file_id FROM selection_items WHERE selection_id = ?1 LIMIT ?2)",
        )
        .bind(id)
        .bind(chunk)
        .execute(db.write())
        .await?
        .rows_affected();
        if i64::try_from(removed).unwrap_or(i64::MAX) < chunk {
            break;
        }
    }
    sqlx::query("DELETE FROM selections WHERE id = ?1")
        .bind(id)
        .execute(db.write())
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::{insert_file, insert_volume};

    const NOW: i64 = 1_000_000;

    async fn files(db: &Db, count: usize) -> Vec<FileId> {
        let volume = insert_volume(db, "photos").await;
        let mut ids = Vec::new();
        for n in 0..count {
            ids.push(insert_file(db, volume, "2024", &format!("IMG_{n:04}.jpg")).await);
        }
        ids
    }

    async fn item_rows(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM selection_items")
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_selection_keeps_the_files_that_exist_and_drops_the_other_ids() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let ids = files(&db, 3).await;
        sqlx::query("UPDATE files SET size = id * 100")
            .execute(db.write())
            .await
            .unwrap();
        let asked = [ids[0], ids[2], 9_999_999];

        let id = create(&db, &asked, Some("from a test"), NOW, NOW + 100)
            .await
            .unwrap();
        let dto = get(db.read(), id, NOW).await.unwrap().unwrap();
        assert_eq!(dto.item_count, 2);
        assert_eq!(
            dto.total_size,
            u64::try_from(ids[0] * 100 + ids[2] * 100).unwrap()
        );
        assert_eq!(dto.label.as_deref(), Some("from a test"));
        assert_eq!((dto.created_at, dto.expires_at), (NOW, NOW + 100));

        let mut members = file_ids(&db, id).await.unwrap();
        members.sort_unstable();
        assert_eq!(members, vec![ids[0], ids[2]]);
        let stored: i64 = sqlx::query_scalar("SELECT item_count FROM selections WHERE id = ?1")
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(stored, 2);
    }

    #[tokio::test]
    async fn an_empty_selection_is_valid() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = create(&db, &[], None, NOW, NOW + 100).await.unwrap();
        let dto = get(db.read(), id, NOW).await.unwrap().unwrap();
        assert_eq!((dto.item_count, dto.total_size, dto.label), (0, 0, None));
        assert!(file_ids(&db, id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ids_are_written_a_chunk_at_a_time_without_losing_any() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let ids = files(&db, 7).await;
        let id = create_chunked(&db, &ids, None, NOW, NOW + 100, 3)
            .await
            .unwrap();
        assert_eq!(
            get(db.read(), id, NOW).await.unwrap().unwrap().item_count,
            7
        );
        let mut members = file_ids(&db, id).await.unwrap();
        members.sort_unstable();
        assert_eq!(members, ids);
    }

    #[tokio::test]
    async fn the_live_count_follows_the_files() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let ids = files(&db, 3).await;
        let id = create(&db, &ids, None, NOW, NOW + 100).await.unwrap();

        sqlx::query("DELETE FROM files WHERE id = ?1")
            .bind(ids[1])
            .execute(db.write())
            .await
            .unwrap();
        let dto = get(db.read(), id, NOW).await.unwrap().unwrap();
        assert_eq!(dto.item_count, 2, "the cascade removed the member");
        assert_eq!(dto.total_size, 2, "files of the test weigh 1 byte");
    }

    #[tokio::test]
    async fn an_expired_selection_is_invisible_and_cannot_be_revived() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = create(&db, &[], None, NOW, NOW + 100).await.unwrap();
        assert_eq!(
            expiry(db.read(), id, NOW + 99).await.unwrap(),
            Some(NOW + 100)
        );
        assert_eq!(expiry(db.read(), id, NOW + 100).await.unwrap(), None);
        assert!(get(db.read(), id, NOW + 100).await.unwrap().is_none());

        assert!(!extend(&db, id, NOW + 100, NOW + 500).await.unwrap());
        assert_eq!(expiry(db.read(), id, NOW + 100).await.unwrap(), None);
        assert_eq!(expiry(db.read(), 4_242, NOW).await.unwrap(), None);
    }

    #[tokio::test]
    async fn extending_pushes_forward_only() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = create(&db, &[], None, NOW, NOW + 100).await.unwrap();
        assert!(!extend(&db, id, NOW, NOW + 50).await.unwrap());
        assert!(extend(&db, id, NOW, NOW + 400).await.unwrap());
        assert_eq!(
            get(db.read(), id, NOW).await.unwrap().unwrap().expires_at,
            NOW + 400
        );
    }

    #[tokio::test]
    async fn the_sweep_removes_expired_selections_and_their_items_only() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let ids = files(&db, 5).await;
        let old = create_chunked(&db, &ids, None, NOW, NOW + 10, 2)
            .await
            .unwrap();
        let live = create(&db, &ids[..2], None, NOW, NOW + 1_000)
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let purged = purge_expired_chunked(&db, &cancel, NOW + 20, 2)
            .await
            .unwrap();
        assert_eq!(purged, 1);
        assert!(file_ids(&db, old).await.unwrap().is_empty());
        assert_eq!(file_ids(&db, live).await.unwrap().len(), 2);
        assert_eq!(item_rows(&db).await, 2);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM selections")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(purge_expired(&db, &cancel, NOW + 20).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_cancelled_sweep_leaves_the_rest_for_the_next_one() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let ids = files(&db, 3).await;
        let old = create(&db, &ids, None, NOW, NOW + 10).await.unwrap();

        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(purge_expired(&db, &cancel, NOW + 20).await.unwrap(), 0);
        assert_eq!(file_ids(&db, old).await.unwrap().len(), 3);

        let fresh = CancellationToken::new();
        assert_eq!(purge_expired(&db, &fresh, NOW + 20).await.unwrap(), 1);
        assert_eq!(item_rows(&db).await, 0);
    }
}
