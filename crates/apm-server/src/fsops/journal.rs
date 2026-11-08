//! The write-ahead journal that makes file operations recoverable after a crash.
//!
//! Every operation that changes the disk inserts a `pending` row BEFORE it touches a file and
//! marks it `done` in the SAME transaction that updates the index (or `failed` when it rolled
//! back its own work). A row that is still `pending` when the server starts is the trace of an
//! operation a crash interrupted; [`FsOps::reconcile`](super::FsOps::reconcile) decides from
//! what is on the disk how to finish or abandon it, see its docs.
//!
//! The journal never holds an absolute path and never a file's content: the volume ids and
//! volume-relative paths are enough to find everything again.
//!
//! The table is written through the single writer connection: never call a method that takes
//! `&self` while a write transaction is open on the same task (it would wait for the connection
//! the transaction holds). [`Journal::finish_tx`] is the one function that runs inside the
//! caller's transaction.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use apm_core::paths::{RelPath, TaskId};
use sqlx::{Row, SqliteConnection};

use super::Loc;
use super::error::FsOpError;
use crate::db::{Db, now_secs};

/// Finished rows are kept this long (seconds) for diagnosis.
const KEEP_FINISHED_SECS: i64 = 7 * 24 * 3600;
/// Abandoned rows are anomalies somebody may want to look at: kept longer.
const KEEP_ABANDONED_SECS: i64 = 30 * 24 * 3600;

/// The `fs_journal.op` values the module writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Move,
    Copy,
    Trash,
    Restore,
    Rmdir,
    Delete,
}

impl Op {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Copy => "copy",
            Self::Trash => "trash",
            Self::Restore => "restore",
            Self::Rmdir => "rmdir",
            Self::Delete => "delete",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "move" => Self::Move,
            "copy" => Self::Copy,
            "trash" => Self::Trash,
            "restore" => Self::Restore,
            "rmdir" => Self::Rmdir,
            "delete" => Self::Delete,
            _ => return None,
        })
    }
}

/// What a new journal row records.
pub(crate) struct NewEntry<'a> {
    pub task_id: Option<TaskId>,
    pub op: Op,
    pub src: Option<&'a Loc>,
    pub dst: Option<&'a Loc>,
    pub size: Option<u64>,
}

/// A `pending` row, as recovery reads it.
#[derive(Debug, Clone)]
pub(crate) struct PendingRow {
    pub id: i64,
    pub task_id: Option<TaskId>,
    /// `None` for an `op` this version does not know (written by a newer one).
    pub op: Option<Op>,
    pub src: Option<Loc>,
    pub dst: Option<Loc>,
    /// The part file of a copy, relative to the destination volume.
    pub part_path: Option<RelPath>,
    pub size: Option<u64>,
    pub hash: Option<Vec<u8>>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct Journal {
    db: Db,
    inflight: Arc<Mutex<HashSet<i64>>>,
    /// Held while a row is inserted and registered, and while recovery lists the pending rows:
    /// a listed row therefore has a live guard or belongs to an operation that already ended.
    gate: Arc<tokio::sync::Mutex<()>>,
}

/// Proof that a journal row belongs to a live operation of this process: while it exists,
/// `reconcile` leaves the row alone.
#[derive(Debug)]
pub(crate) struct Guard {
    id: i64,
    created_at: i64,
    inflight: Arc<Mutex<HashSet<i64>>>,
}

impl Guard {
    pub(crate) fn id(&self) -> i64 {
        self.id
    }

    /// When the row was inserted (the keep-in-trash batch of a cross-device move is derived from
    /// it, so recovery computes the same name).
    pub(crate) fn created_at(&self) -> i64 {
        self.created_at
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        lock(&self.inflight).remove(&self.id);
    }
}

fn lock(set: &Mutex<HashSet<i64>>) -> MutexGuard<'_, HashSet<i64>> {
    set.lock().unwrap_or_else(PoisonError::into_inner)
}

fn to_db_size(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

impl Journal {
    pub(crate) fn new(db: Db) -> Self {
        Self {
            db,
            inflight: Arc::default(),
            gate: Arc::default(),
        }
    }

    /// Inserts the `pending` row. Call it right before the disk change it describes.
    pub(crate) async fn begin(&self, entry: NewEntry<'_>) -> Result<Guard, FsOpError> {
        let _gate = self.gate.lock().await;
        let created_at = now_secs();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO fs_journal (task_id, op, state, src_volume_id, src_path, dst_volume_id, \
             dst_path, size, created_at) VALUES (?, ?, 'pending', ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(entry.task_id)
        .bind(entry.op.as_str())
        .bind(entry.src.map(|loc| loc.volume_id))
        .bind(entry.src.map(|loc| loc.rel.as_str()))
        .bind(entry.dst.map(|loc| loc.volume_id))
        .bind(entry.dst.map(|loc| loc.rel.as_str()))
        .bind(entry.size.map(to_db_size))
        .bind(created_at)
        .fetch_one(self.db.write())
        .await?;
        lock(&self.inflight).insert(id);
        Ok(Guard {
            id,
            created_at,
            inflight: Arc::clone(&self.inflight),
        })
    }

    /// Records the part file BEFORE it is created, so a crash leaves a name to clean up.
    pub(crate) async fn set_part(&self, id: i64, part: &RelPath) -> Result<(), FsOpError> {
        sqlx::query("UPDATE fs_journal SET part_path = ? WHERE id = ?")
            .bind(part.as_str())
            .bind(id)
            .execute(self.db.write())
            .await?;
        Ok(())
    }

    /// Updates where the operation really goes (a conflict renamed it) and what was copied.
    pub(crate) async fn set_dst(
        &self,
        id: i64,
        dst: &Loc,
        hash: Option<&[u8]>,
        size: Option<u64>,
    ) -> Result<(), FsOpError> {
        sqlx::query(
            "UPDATE fs_journal SET dst_volume_id = ?, dst_path = ?, hash = COALESCE(?, hash), \
             size = COALESCE(?, size) WHERE id = ?",
        )
        .bind(dst.volume_id)
        .bind(dst.rel.as_str())
        .bind(hash)
        .bind(size.map(to_db_size))
        .bind(id)
        .execute(self.db.write())
        .await?;
        Ok(())
    }

    /// The operation undid its own work (or never changed anything): the row is closed as
    /// `failed`. A journal that cannot be written is logged, not fatal: the row stays `pending`
    /// and recovery looks at the disk, which finds nothing to do.
    pub(crate) async fn fail(&self, id: i64, error: &FsOpError) {
        self.close(id, "failed", &format!("{}: {error}", error.code()))
            .await;
    }

    /// The operation turned out to have nothing to do (a skip, a raced attempt): the row is
    /// closed as `failed` with a free-text `reason`.
    pub(crate) async fn cancel(&self, id: i64, reason: &str) {
        self.close(id, "failed", reason).await;
    }

    /// The operation cannot be finished or undone automatically: `reason` says why.
    pub(crate) async fn abandon(&self, id: i64, reason: &str) {
        self.close(id, "abandoned", reason).await;
    }

    async fn close(&self, id: i64, state: &str, message: &str) {
        let result = sqlx::query(
            "UPDATE fs_journal SET state = ?, error = ?, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(state)
        .bind(message)
        .bind(now_secs())
        .bind(id)
        .execute(self.db.write())
        .await;
        if let Err(err) = result {
            tracing::warn!(journal_id = id, error = %err, "could not close a journal row");
        }
    }

    /// Marks the row `done` outside any index change (recovery found nothing in the index to
    /// repair).
    pub(crate) async fn finish(&self, id: i64) -> Result<(), FsOpError> {
        sqlx::query(
            "UPDATE fs_journal SET state = 'done', error = NULL, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(now_secs())
        .bind(id)
        .execute(self.db.write())
        .await?;
        Ok(())
    }

    /// Marks the row `done`, inside the transaction that makes the index change final.
    pub(crate) async fn finish_tx(tx: &mut SqliteConnection, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE fs_journal SET state = 'done', error = NULL, finished_at = ? WHERE id = ?",
        )
        .bind(now_secs())
        .bind(id)
        .execute(tx)
        .await?;
        Ok(())
    }

    /// Whether a live operation of this process owns the row.
    pub(crate) fn is_inflight(&self, id: i64) -> bool {
        lock(&self.inflight).contains(&id)
    }

    /// Whether the row is still `pending` (an operation that ended since the list was read has
    /// closed it).
    pub(crate) async fn is_pending(&self, id: i64) -> Result<bool, FsOpError> {
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM fs_journal WHERE id = ?")
            .bind(id)
            .fetch_optional(self.db.read())
            .await?;
        Ok(state.as_deref() == Some("pending"))
    }

    /// Every `pending` row, oldest first.
    pub(crate) async fn pending(&self) -> Result<Vec<PendingRow>, FsOpError> {
        let _gate = self.gate.lock().await;
        let rows = sqlx::query(
            "SELECT id, task_id, op, src_volume_id, src_path, dst_volume_id, dst_path, part_path, \
             size, hash, created_at FROM fs_journal WHERE state = 'pending' ORDER BY id",
        )
        .fetch_all(self.db.read())
        .await?;
        rows.iter().map(pending_row).collect()
    }

    /// Deletes the finished rows nobody needs any more; returns how many.
    pub(crate) async fn prune(&self) -> Result<u64, FsOpError> {
        let now = now_secs();
        let result = sqlx::query(
            "DELETE FROM fs_journal WHERE (state IN ('done', 'failed') AND finished_at < ?) \
             OR (state = 'abandoned' AND finished_at < ?)",
        )
        .bind(now - KEEP_FINISHED_SECS)
        .bind(now - KEEP_ABANDONED_SECS)
        .execute(self.db.write())
        .await?;
        Ok(result.rows_affected())
    }
}

fn loc_of(volume_id: Option<i64>, path: Option<String>) -> Option<Loc> {
    let rel = RelPath::new(&path?).ok()?;
    Some(Loc::new(volume_id?, rel))
}

fn pending_row(row: &sqlx::sqlite::SqliteRow) -> Result<PendingRow, FsOpError> {
    let op: String = row.try_get("op")?;
    let size: Option<i64> = row.try_get("size")?;
    let part: Option<String> = row.try_get("part_path")?;
    Ok(PendingRow {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        op: Op::parse(&op),
        src: loc_of(row.try_get("src_volume_id")?, row.try_get("src_path")?),
        dst: loc_of(row.try_get("dst_volume_id")?, row.try_get("dst_path")?),
        part_path: part.and_then(|path| RelPath::new(&path).ok()),
        size: size.and_then(|size| u64::try_from(size).ok()),
        hash: row.try_get("hash")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn loc(volume_id: i64, path: &str) -> Loc {
        Loc::new(volume_id, RelPath::new(path).unwrap())
    }

    async fn state_of(db: &Db, id: i64) -> (String, Option<String>, Option<i64>) {
        sqlx::query_as("SELECT state, error, finished_at FROM fs_journal WHERE id = ?")
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_new_row_is_pending_and_owned_until_its_guard_drops() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let journal = Journal::new(db.clone());
        let src = loc(1, "a/b.jpg");
        let dst = loc(2, "c/b.jpg");

        let guard = journal
            .begin(NewEntry {
                task_id: Some(7),
                op: Op::Move,
                src: Some(&src),
                dst: Some(&dst),
                size: Some(10),
            })
            .await
            .unwrap();
        assert!(journal.is_inflight(guard.id()));

        let pending = journal.pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.id, guard.id());
        assert_eq!(row.task_id, Some(7));
        assert_eq!(row.op, Some(Op::Move));
        assert_eq!(row.src.as_ref(), Some(&src));
        assert_eq!(row.dst.as_ref(), Some(&dst));
        assert_eq!(row.size, Some(10));
        assert!(row.part_path.is_none() && row.hash.is_none());

        let id = guard.id();
        drop(guard);
        assert!(!journal.is_inflight(id));
    }

    #[tokio::test]
    async fn part_and_destination_updates_are_visible_to_recovery() {
        let temp = Db::open_temp().await.unwrap();
        let journal = Journal::new(temp.handle());
        let src = loc(1, "a.jpg");
        let dst = loc(1, "b.jpg");
        let guard = journal
            .begin(NewEntry {
                task_id: None,
                op: Op::Copy,
                src: Some(&src),
                dst: Some(&dst),
                size: Some(3),
            })
            .await
            .unwrap();
        journal
            .set_part(
                guard.id(),
                &RelPath::new(".apm-0123456789abcdef.apm-part").unwrap(),
            )
            .await
            .unwrap();
        let renamed = loc(1, "b (1).jpg");
        journal
            .set_dst(guard.id(), &renamed, Some(&[1, 2, 3]), Some(4))
            .await
            .unwrap();

        let row = journal.pending().await.unwrap().remove(0);
        assert_eq!(
            row.part_path.unwrap().as_str(),
            ".apm-0123456789abcdef.apm-part"
        );
        assert_eq!(row.dst, Some(renamed));
        assert_eq!(row.hash, Some(vec![1, 2, 3]));
        assert_eq!(row.size, Some(4));
    }

    #[tokio::test]
    async fn closing_a_row_records_the_reason_and_removes_it_from_pending() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let journal = Journal::new(db.clone());
        let src = loc(1, "a.jpg");
        let failed = journal
            .begin(NewEntry {
                task_id: None,
                op: Op::Trash,
                src: Some(&src),
                dst: None,
                size: None,
            })
            .await
            .unwrap();
        let abandoned = journal
            .begin(NewEntry {
                task_id: None,
                op: Op::Delete,
                src: Some(&src),
                dst: None,
                size: None,
            })
            .await
            .unwrap();
        journal.fail(failed.id(), &FsOpError::VerifyFailed).await;
        journal.abandon(abandoned.id(), "both files exist").await;

        assert!(journal.pending().await.unwrap().is_empty());
        let (state, error, finished) = state_of(&db, failed.id()).await;
        assert_eq!(state, "failed");
        assert!(error.unwrap().starts_with("verify_failed"));
        assert!(finished.is_some());
        let (state, error, _) = state_of(&db, abandoned.id()).await;
        assert_eq!(state, "abandoned");
        assert_eq!(error.as_deref(), Some("both files exist"));

        // A closed row cannot be closed again.
        journal.abandon(failed.id(), "late").await;
        assert_eq!(state_of(&db, failed.id()).await.0, "failed");
    }

    #[tokio::test]
    async fn finish_tx_marks_done_inside_the_callers_transaction() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let journal = Journal::new(db.clone());
        let src = loc(1, "a.jpg");
        let guard = journal
            .begin(NewEntry {
                task_id: None,
                op: Op::Move,
                src: Some(&src),
                dst: None,
                size: None,
            })
            .await
            .unwrap();

        let mut tx = db.begin_write().await.unwrap();
        Journal::finish_tx(&mut tx, guard.id()).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(state_of(&db, guard.id()).await.0, "pending");

        let mut tx = db.begin_write().await.unwrap();
        Journal::finish_tx(&mut tx, guard.id()).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(state_of(&db, guard.id()).await.0, "done");
    }

    #[tokio::test]
    async fn pending_rows_come_oldest_first_and_unknown_ops_are_kept_as_none() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let journal = Journal::new(db.clone());
        let src = loc(1, "a.jpg");
        let mut guards = Vec::new();
        for _ in 0..3 {
            guards.push(
                journal
                    .begin(NewEntry {
                        task_id: None,
                        op: Op::Move,
                        src: Some(&src),
                        dst: None,
                        size: None,
                    })
                    .await
                    .unwrap(),
            );
        }
        let ids: Vec<i64> = journal
            .pending()
            .await
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        assert_eq!(ids.len(), 3);

        sqlx::query("UPDATE fs_journal SET src_path = '/absolute' WHERE id = ?")
            .bind(ids[0])
            .execute(db.write())
            .await
            .unwrap();
        let rows = journal.pending().await.unwrap();
        assert!(rows[0].src.is_none(), "a malformed path is not trusted");
    }

    #[tokio::test]
    async fn prune_removes_only_old_finished_rows() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let journal = Journal::new(db.clone());
        let now = now_secs();
        for (state, age_days) in [
            ("done", 10),
            ("done", 1),
            ("failed", 10),
            ("abandoned", 10),
            ("abandoned", 40),
            ("pending", 40),
        ] {
            sqlx::query(
                "INSERT INTO fs_journal (op, state, created_at, finished_at) VALUES ('move', ?, ?, ?)",
            )
            .bind(state)
            .bind(now - age_days * 86_400)
            .bind(if state == "pending" { None } else { Some(now - age_days * 86_400) })
            .execute(db.write())
            .await
            .unwrap();
        }
        assert_eq!(journal.prune().await.unwrap(), 3);
        let left: Vec<(String,)> = sqlx::query_as("SELECT state FROM fs_journal ORDER BY id")
            .fetch_all(db.read())
            .await
            .unwrap();
        assert_eq!(left.len(), 3);
    }
}
