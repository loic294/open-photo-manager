//! Crash recovery: finishes or abandons the journal rows a crash left `pending`, and tidies up
//! what a crash leaves behind.
//!
//! The disk is authoritative. A row says what an operation was about to do; the disk says how far
//! it got, and the index and the journal are repaired from that. Nothing is deleted unless it is
//! proven to be a duplicate (the SHA-1 of both files matches) or a second name of the very same
//! inode, and a row whose state looks like nothing this module produces is abandoned with both
//! sides untouched (the folders go to the indexer for a rescan). The decision table is in the
//! [module docs](super).
//!
//! A pass can be repeated at any time: a row it cannot decide stays `pending`, every step is a
//! no-op when repeated, and rows of operations that still run in this process (their journal
//! guard exists) are never touched.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use apm_core::hash::{self, Sha1Digest};
use apm_core::paths::{FileId, RelPath, VolumeId};
use sqlx::SqliteConnection;
use tokio_util::sync::CancellationToken;

use super::apply::{
    Crossing, KeptSource, Mode, Snap, TransferApply, TrashApply, TrashMeta, conclude, load_snap,
    touched_dirs, trash_item_at,
};
use super::backend::Mover;
use super::context::{OpContext, TrashBatch};
use super::copy::{Retired, discard_part, is_part_name, keep_source, unlink_source};
use super::error::FsOpError;
use super::executor::{hash_file, names_with_inode};
use super::journal::{Op, PendingRow};
use super::stat::FileStat;
use super::trash::{prune_trash_dirs, trash_rel};
use super::{FsOps, Loc, Rules, blocking};
use crate::db::now_secs;
use crate::indexer::{NoopChanges, NoopProgress, RunContext, ScanRequest, ScanTarget, links};
use crate::security::{FsError, VolumeRoot};

/// The report asks for at most this many folders to be rescanned; whatever a crash left beyond is
/// found by the next full scan.
const MAX_RESCAN: usize = 1000;
/// The report keeps at most this many error messages (all of them are logged).
const MAX_ERRORS: usize = 200;
/// A part file younger than this is never swept: it may belong to a copy that is just starting.
const STRAY_PART_MIN_AGE_SECS: i64 = 3600;
/// How many journal rows, and how many distinct folders, the stray-part sweep looks at.
const STRAY_PART_ROWS: i64 = 20_000;
const STRAY_PART_DIRS: usize = 2000;

/// What `FsOps::reconcile` found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Pending journal rows looked at (rows of operations still running here included).
    pub examined: usize,
    /// Rows finished: the index now matches the disk.
    pub completed: usize,
    /// Rows given up on: the disk shows nothing to finish, or something that is not recognised
    /// (then nothing was changed on the disk and the folders are in `rescan`).
    pub abandoned: usize,
    /// Rows left for a later run: an offline or read-only volume, an operation still running in
    /// this process, or an error.
    pub still_pending: usize,
    /// Leftover part files of crashed copies deleted.
    pub parts_removed: usize,
    /// Old part files no journal row owns deleted.
    pub stray_parts_swept: usize,
    /// Links between a file and a sidecar in another folder dropped (a crashed group move).
    pub links_dropped: usize,
    /// Old journal rows deleted.
    pub pruned: u64,
    /// Folders whose index may be stale: hand them to the indexer. `reconcile_at_startup` does;
    /// a caller of `reconcile` decides. Never a whole volume.
    pub rescan: Vec<ScanTarget>,
    /// Rows that could not be processed, with the reason.
    pub errors: Vec<String>,
}

impl FsOps {
    /// Crash recovery at startup. Call it once, after the hooks are registered and before the
    /// first scan; it never fails (what went wrong is logged and the rows stay `pending`). The
    /// folders the report asks to rescan are indexed by one background scan.
    pub async fn reconcile_at_startup(&self, shutdown: CancellationToken) {
        let report = match self.reconcile_with(&shutdown).await {
            Ok(report) => report,
            Err(err) => {
                tracing::error!(error = %err, "crash recovery failed; it runs again at the next start");
                return;
            }
        };
        if report.examined > 0 || report.stray_parts_swept > 0 || report.links_dropped > 0 {
            tracing::info!(
                examined = report.examined,
                completed = report.completed,
                abandoned = report.abandoned,
                still_pending = report.still_pending,
                parts_removed = report.parts_removed,
                stray_parts_swept = report.stray_parts_swept,
                links_dropped = report.links_dropped,
                rescan = report.rescan.len(),
                errors = report.errors.len(),
                "file operation recovery finished"
            );
        }
        if !report.rescan.is_empty() && !shutdown.is_cancelled() {
            self.rescan_in_background(report.rescan, shutdown);
        }
    }

    /// One recovery pass; see the decision table in the module docs. Safe at any time (rows of
    /// operations that are running are skipped). Call it again when a volume that was offline
    /// comes back, or after an `internal` error left a row `pending`. The caller decides what to
    /// do with `rescan`.
    ///
    /// # Errors
    /// `internal` when the journal itself cannot be read.
    pub async fn reconcile(&self) -> Result<ReconcileReport, FsOpError> {
        self.reconcile_with(&CancellationToken::new()).await
    }

    /// [`FsOps::reconcile`] that stops (leaving the rest `pending`) when `cancel` fires.
    pub(crate) async fn reconcile_with(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ReconcileReport, FsOpError> {
        let _serial = self.reconcile_lock.lock().await;
        let rows = self.journal.pending().await?;
        let mut pass = Pass::default();
        for row in &rows {
            if cancel.is_cancelled() {
                break;
            }
            if self.journal.is_inflight(row.id) {
                pass.report.examined += 1;
                pass.report.still_pending += 1;
                continue;
            }
            // The operation may have ended since the list was read.
            if !self.journal.is_pending(row.id).await? {
                continue;
            }
            pass.report.examined += 1;
            match self.recover(row, cancel).await {
                Ok(recovered) => self.settle(&mut pass, row, recovered).await,
                Err(FsOpError::Cancelled) => {
                    pass.report.still_pending += 1;
                    break;
                }
                Err(err) => {
                    tracing::error!(
                        journal_id = row.id, op = ?row.op, error = %err,
                        "could not recover an interrupted file operation; it stays pending"
                    );
                    pass.report.still_pending += 1;
                    pass.error(format!("journal row {}: {err}", row.id));
                }
            }
        }
        if !cancel.is_cancelled() {
            self.sweep_stray_parts(&mut pass, cancel).await;
            self.sweep_crossing_links(&mut pass).await;
            match self.journal.prune().await {
                Ok(pruned) => pass.report.pruned = pruned,
                Err(err) => pass.error(format!("pruning the journal: {err}")),
            }
        }
        Ok(pass.finish())
    }

    fn rescan_in_background(&self, targets: Vec<ScanTarget>, shutdown: CancellationToken) {
        let indexer = self.indexer.clone();
        tokio::spawn(async move {
            let folders = targets.len();
            let request = ScanRequest {
                targets,
                ..ScanRequest::default()
            };
            let run = RunContext::new(shutdown, Arc::new(NoopProgress), Arc::new(NoopChanges));
            match indexer.scan(&request, &run).await {
                Ok(_) => tracing::info!(folders, "rescanned what crash recovery could not settle"),
                Err(err) => tracing::warn!(
                    error = %err,
                    "rescanning what crash recovery could not settle failed; the next scan repairs it"
                ),
            }
        });
    }

    /// Books the outcome of one row: closes it when it is abandoned.
    async fn settle(&self, pass: &mut Pass, row: &PendingRow, recovered: Recovered) {
        pass.report.parts_removed += recovered.parts_removed;
        pass.scan(recovered.rescan);
        match recovered.verdict {
            Verdict::Completed => pass.report.completed += 1,
            Verdict::Pending => pass.report.still_pending += 1,
            Verdict::Abandoned(reason) => {
                tracing::info!(
                    journal_id = row.id, op = ?row.op, reason,
                    "abandoned an interrupted file operation: it changed nothing"
                );
                self.journal.abandon(row.id, reason).await;
                pass.report.abandoned += 1;
            }
            Verdict::Unclear(reason) => {
                tracing::warn!(
                    journal_id = row.id, op = ?row.op, src = ?row.src, dst = ?row.dst, reason,
                    "an interrupted file operation left a state recovery does not recognise; \
                     nothing was deleted or changed"
                );
                self.journal.abandon(row.id, reason).await;
                pass.report.abandoned += 1;
            }
        }
    }

    async fn recover(
        &self,
        row: &PendingRow,
        cancel: &CancellationToken,
    ) -> Result<Recovered, FsOpError> {
        let recovered = match row.op {
            Some(Op::Move) => self.recover_transfer(row, Mode::Move, cancel).await,
            Some(Op::Copy) => self.recover_transfer(row, Mode::Copy, cancel).await,
            Some(Op::Restore) => self.recover_transfer(row, Mode::Restore, cancel).await,
            Some(Op::Trash) => self.recover_trash(row).await,
            Some(Op::Rmdir) => self.recover_rmdir(row).await,
            Some(Op::Delete) => self.recover_purge(row).await,
            None => {
                tracing::warn!(
                    journal_id = row.id,
                    "a journal row has an operation this version does not know; leaving it"
                );
                return Ok(Recovered::wait());
            }
        };
        match recovered {
            Err(FsOpError::VolumeOffline | FsOpError::VolumeReadOnly) => Ok(Recovered::wait()),
            Err(FsOpError::UnknownVolume) => Ok(Recovered::abandoned("the volume was removed")),
            other => other,
        }
    }

    // --- move, copy and restore ------------------------------------------------------------

    async fn recover_transfer(
        &self,
        row: &PendingRow,
        mode: Mode,
        cancel: &CancellationToken,
    ) -> Result<Recovered, FsOpError> {
        let (Some(src), Some(dst)) = (&row.src, &row.dst) else {
            return Ok(Recovered::abandoned(
                "the journal row has no source or no destination",
            ));
        };
        // The source of a copy is only read, so a read-only volume is fine for it.
        let src_root = if mode == Mode::Copy {
            self.volume(src.volume_id)?
        } else {
            self.writable_volume(src.volume_id)?
        };
        let ends = Ends {
            src: src.clone(),
            dst: dst.clone(),
            src_root,
            dst_root: self.writable_volume(dst.volume_id)?,
        };
        match (&row.part_path, mode) {
            (Some(part), _) => self.recover_copied(row, mode, ends, part, cancel).await,
            (None, Mode::Copy) => Ok(Recovered::abandoned("the copy never started")),
            (None, Mode::Move | Mode::Restore) => self.recover_renamed(row, mode, &ends).await,
        }
    }

    /// A move or restore that was a single rename: the disk says whether it happened.
    async fn recover_renamed(
        &self,
        row: &PendingRow,
        mode: Mode,
        ends: &Ends,
    ) -> Result<Recovered, FsOpError> {
        let state = {
            let ends = ends.clone();
            let (size, restore) = (row.size, mode == Mode::Restore);
            blocking(move || classify_rename(&ends, size, restore)).await?
        };
        match state {
            RenameState::NeverStarted => Ok(Recovered::abandoned("the file never moved")),
            RenameState::Unclear(reason) => Ok(Recovered::unclear(reason).scanning([
                ScanTarget::dir(ends.src.volume_id, ends.src.dir()),
                ScanTarget::dir(ends.dst.volume_id, ends.dst.dir()),
            ])),
            RenameState::PlacedFolder => self.finish_restored_folder(row, ends).await,
            RenameState::Placed(dst_stat) => {
                self.finish_transfer(Landed {
                    mode,
                    row,
                    ends,
                    dst_stat,
                    hash: None,
                    kept: None,
                })
                .await
            }
        }
    }

    /// A copy (or a move or restore that crossed devices) with a part file: the part goes, and the
    /// destination is trusted only when it is proven to be the copy.
    async fn recover_copied(
        &self,
        row: &PendingRow,
        mode: Mode,
        ends: Ends,
        part: &RelPath,
        cancel: &CancellationToken,
    ) -> Result<Recovered, FsOpError> {
        let job = CopyRecovery {
            ends: ends.clone(),
            mode,
            journal_id: row.id,
            created_at: row.created_at,
            recorded: row.hash.as_deref().and_then(hash::from_slice),
            size: row.size,
            part: part.clone(),
            keep_source: mode == Mode::Move
                && self.rules().settings.keep_cross_volume_source_in_trash,
            mover: Arc::clone(&self.mover),
            cancel: cancel.clone(),
        };
        match blocking(move || settle_copy(&job)).await? {
            CopyOutcome::Stop(recovered) => Ok(recovered),
            CopyOutcome::Land(landing) => {
                let Landing {
                    dst_stat,
                    digest,
                    kept,
                    parts_removed,
                } = landing;
                let recovered = self
                    .finish_transfer(Landed {
                        mode,
                        row,
                        ends: &ends,
                        dst_stat,
                        hash: Some(digest),
                        kept,
                    })
                    .await?;
                Ok(recovered.removed_parts(parts_removed))
            }
        }
    }

    /// The file is at its destination: the index change that was missing, in one transaction that
    /// also closes the journal row.
    async fn finish_transfer(&self, landed: Landed<'_>) -> Result<Recovered, FsOpError> {
        let Landed {
            mode,
            row,
            ends,
            dst_stat,
            hash,
            kept,
        } = landed;
        let (src, dst) = (&ends.src, &ends.dst);
        let rules: Rules = self.rules();
        let (pre, restore_item) = self.index_state_of(mode, src).await?;
        let indexed = !rules.junk.is_junk_path(&dst.rel);
        self.apply_transfer(
            &rules,
            &TransferApply {
                mode,
                journal_id: row.id,
                task_id: row.task_id,
                src,
                dst,
                pre: pre.as_ref(),
                dst_stat: &dst_stat,
                hash,
                restore_item,
                kept: kept.as_ref(),
                crossing: Crossing::Delete,
                indexed,
            },
        )
        .await?;
        let left = (mode == Mode::Move).then_some(src);
        self.announce(&OpContext::detached(), &touched_dirs(left, Some(dst), &[]));
        if mode == Mode::Restore {
            let (root, trash_path) = (Arc::clone(&ends.src_root), src.rel.clone());
            blocking(move || {
                prune_trash_dirs(&root, &trash_path);
                Ok(())
            })
            .await?;
        }
        Ok(Recovered::completed())
    }

    /// What the index remembers of the source: its snapshot (a trashed file keeps its own in the
    /// trash item) and, for a restore, the trash item the restore consumes.
    async fn index_state_of(
        &self,
        mode: Mode,
        src: &Loc,
    ) -> Result<(Option<Snap>, Option<i64>), FsOpError> {
        let mut conn = self.db.read().acquire().await?;
        match mode {
            Mode::Restore => {
                let item = trash_item_at(&mut conn, src.volume_id, &src.rel).await?;
                let snap = item
                    .as_ref()
                    .and_then(|item| TrashMeta::file_snapshot(item.meta_json.as_deref()));
                Ok((snap, item.map(|item| item.id)))
            }
            Mode::Move | Mode::Copy => {
                let found = load_snap(&mut conn, src).await?;
                Ok((found.map(|(_, snap)| snap), None))
            }
        }
    }

    /// A restored folder is at its place: its trash item goes, the folders above get their rows,
    /// and the content is indexed by the rescan the report asks for.
    async fn finish_restored_folder(
        &self,
        row: &PendingRow,
        ends: &Ends,
    ) -> Result<Recovered, FsOpError> {
        let item = {
            let mut conn = self.db.read().acquire().await?;
            trash_item_at(&mut conn, ends.src.volume_id, &ends.src.rel).await?
        };
        match &item {
            Some(item) => self.apply_restore_dir(row.id, item, &ends.dst).await?,
            None => self.journal.finish(row.id).await?,
        }
        self.announce(
            &OpContext::detached(),
            &touched_dirs(None, Some(&ends.dst), &[]),
        );
        if let Some(item) = &item {
            self.prune_after(item).await;
        }
        Ok(Recovered::completed().scanning([ScanTarget::subtree(
            ends.dst.volume_id,
            ends.dst.rel.clone(),
        )]))
    }

    // --- trash, folders, purge -------------------------------------------------------------

    async fn recover_trash(&self, row: &PendingRow) -> Result<Recovered, FsOpError> {
        let (Some(orig), Some(trash)) = (&row.src, &row.dst) else {
            return Ok(Recovered::abandoned(
                "the journal row has no source or no destination",
            ));
        };
        if orig.volume_id != trash.volume_id {
            return Ok(Recovered::unclear("the trash path is on another volume"));
        }
        let root = self.writable_volume(orig.volume_id)?;
        let (at_orig, at_trash) = {
            let (orig, trash) = (orig.rel.clone(), trash.rel.clone());
            blocking(move || Ok((look(&root, &orig)?, look(&root, &trash)?))).await?
        };
        let (is_dir, size) = match (at_orig, at_trash) {
            (Seen::Absent, Seen::File(stat)) => (false, stat.size),
            (Seen::Absent, Seen::Dir) => (true, 0),
            (Seen::Absent, Seen::Absent) => {
                return Ok(
                    Recovered::unclear("the item is neither at its place nor in the trash")
                        .scanning([ScanTarget::dir(orig.volume_id, orig.dir())]),
                );
            }
            (_, Seen::Absent) => return Ok(Recovered::abandoned("nothing was trashed")),
            _ => {
                return Ok(
                    Recovered::unclear("the item is at its place and in the trash")
                        .scanning([ScanTarget::dir(orig.volume_id, orig.dir())]),
                );
            }
        };
        let batch = TrashBatch::from_trash_path(&trash.rel).unwrap_or_else(TrashBatch::adhoc);
        self.apply_trash(&TrashApply {
            journal_id: row.id,
            task_id: row.task_id,
            volume_id: orig.volume_id,
            orig: &orig.rel,
            trash: &trash.rel,
            batch: &batch,
            is_dir,
            size,
            reason: None,
        })
        .await?;
        self.announce(&OpContext::detached(), &[(orig.volume_id, orig.dir())]);
        Ok(Recovered::completed())
    }

    async fn recover_rmdir(&self, row: &PendingRow) -> Result<Recovered, FsOpError> {
        let Some(dir) = &row.src else {
            return Ok(Recovered::abandoned("the journal row has no folder"));
        };
        let root = self.writable_volume(dir.volume_id)?;
        let rel = dir.rel.clone();
        match blocking(move || look(&root, &rel)).await? {
            Seen::Absent => {
                self.apply_rmdir(row.id, dir.volume_id, &dir.rel).await?;
                self.announce(&OpContext::detached(), &[(dir.volume_id, dir.dir())]);
                Ok(Recovered::completed())
            }
            _ => Ok(Recovered::abandoned("the folder still exists")),
        }
    }

    /// Purge deletes the content first and the row second, so a row whose content is gone only
    /// needs its `trash_items` row removed. Recovery never deletes content itself.
    async fn recover_purge(&self, row: &PendingRow) -> Result<Recovered, FsOpError> {
        let Some(path) = &row.src else {
            return Ok(Recovered::abandoned("the journal row has no trash path"));
        };
        let root = self.writable_volume(path.volume_id)?;
        let rel = path.rel.clone();
        if !matches!(blocking(move || look(&root, &rel)).await?, Seen::Absent) {
            return Ok(Recovered::abandoned(
                "the trash item still exists; purge it again",
            ));
        }
        let item = {
            let mut conn = self.db.read().acquire().await?;
            trash_item_at(&mut conn, path.volume_id, &path.rel).await?
        };
        match &item {
            Some(item) => {
                let bytes = row.size.unwrap_or(item.size);
                self.apply_purge(row.id, item, bytes).await?;
                self.prune_after(item).await;
            }
            None => self.journal.finish(row.id).await?,
        }
        Ok(Recovered::completed())
    }

    // --- sweeps ---------------------------------------------------------------------------

    /// Deletes the `.apm-<hex>.apm-part` files old crashes left in the folders the journal names,
    /// except the ones a pending row still owns.
    async fn sweep_stray_parts(&self, pass: &mut Pass, cancel: &CancellationToken) {
        let dirs = match self.part_folders().await {
            Ok(dirs) => dirs,
            Err(err) => return pass.error(format!("listing part folders: {err}")),
        };
        if dirs.is_empty() {
            return;
        }
        let owned = match self.pending_parts().await {
            Ok(owned) => Arc::new(owned),
            Err(err) => return pass.error(format!("listing pending parts: {err}")),
        };
        let older_than = now_secs() - STRAY_PART_MIN_AGE_SECS;
        for (volume_id, dir) in dirs {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(root) = self.writable_volume(volume_id) else {
                continue;
            };
            let owned = Arc::clone(&owned);
            let swept =
                blocking(move || Ok(sweep_folder(&root, volume_id, &dir, &owned, older_than)));
            match swept.await {
                Ok(count) => pass.report.stray_parts_swept += count,
                Err(err) => pass.error(format!("sweeping part files: {err}")),
            }
        }
    }

    async fn part_folders(&self) -> Result<Vec<(VolumeId, RelPath)>, FsOpError> {
        let rows: Vec<(VolumeId, String)> = sqlx::query_as(
            "SELECT dst_volume_id, part_path FROM fs_journal \
             WHERE part_path IS NOT NULL AND dst_volume_id IS NOT NULL AND state <> 'done' \
             ORDER BY id DESC LIMIT ?",
        )
        .bind(STRAY_PART_ROWS)
        .fetch_all(self.db.read())
        .await?;
        let mut folders = BTreeSet::new();
        for (volume_id, part) in rows {
            let Ok(part) = RelPath::new(&part) else {
                continue;
            };
            folders.insert((volume_id, part.parent().unwrap_or_else(RelPath::root)));
            if folders.len() >= STRAY_PART_DIRS {
                break;
            }
        }
        Ok(folders.into_iter().collect())
    }

    async fn pending_parts(&self) -> Result<HashSet<(VolumeId, RelPath)>, FsOpError> {
        let rows: Vec<(VolumeId, String)> = sqlx::query_as(
            "SELECT dst_volume_id, part_path FROM fs_journal \
             WHERE state = 'pending' AND part_path IS NOT NULL AND dst_volume_id IS NOT NULL",
        )
        .fetch_all(self.db.read())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(volume_id, part)| Some((volume_id, RelPath::new(&part).ok()?)))
            .collect())
    }

    /// A group move changes its members one by one with their sidecar links kept, and relinks the
    /// destination folders at the end. A crash in between leaves links that cross folders; this
    /// drops them (their owners read the metadata again) and relinks the folders they touched.
    async fn sweep_crossing_links(&self, pass: &mut Pass) {
        match self.drop_crossing_links().await {
            Ok(dropped) => pass.report.links_dropped = dropped,
            Err(err) => pass.error(format!("dropping crossing sidecar links: {err}")),
        }
    }

    async fn drop_crossing_links(&self) -> Result<usize, FsOpError> {
        let crossing: Vec<CrossingLink> = sqlx::query_as(
            "SELECT fs.file_id, fs.sidecar_id, o.volume_id, o.dir_path, s.volume_id, s.dir_path \
             FROM file_sidecars fs \
             JOIN files o ON o.id = fs.file_id \
             JOIN files s ON s.id = fs.sidecar_id \
             WHERE o.volume_id <> s.volume_id OR o.dir_path <> s.dir_path",
        )
        .fetch_all(self.db.read())
        .await?;
        if crossing.is_empty() {
            return Ok(0);
        }
        let rules = self.rules();
        let mut tx = self.db.begin_write().await?;
        let result = unlink_crossing(&mut tx, &crossing, &rules).await;
        conclude(tx, result).await?;
        Ok(crossing.len())
    }
}

/// `(owner id, sidecar id, owner volume, owner folder, sidecar volume, sidecar folder)`.
type CrossingLink = (FileId, FileId, VolumeId, String, VolumeId, String);

async fn unlink_crossing(
    conn: &mut SqliteConnection,
    crossing: &[CrossingLink],
    rules: &Rules,
) -> Result<(), sqlx::Error> {
    let mut owners = Vec::with_capacity(crossing.len());
    let mut folders = BTreeSet::new();
    for (owner, sidecar, owner_volume, owner_dir, sidecar_volume, sidecar_dir) in crossing {
        sqlx::query("DELETE FROM file_sidecars WHERE file_id = ? AND sidecar_id = ?")
            .bind(owner)
            .bind(sidecar)
            .execute(&mut *conn)
            .await?;
        owners.push(*owner);
        folders.insert((*owner_volume, owner_dir.clone()));
        folders.insert((*sidecar_volume, sidecar_dir.clone()));
    }
    links::reextract(&mut *conn, &owners).await?;
    for (volume_id, dir) in &folders {
        links::relink_dir(&mut *conn, *volume_id, dir, &rules.matcher).await?;
    }
    Ok(())
}

/// Deletes the old part files of one folder that no pending row owns; returns how many are gone.
fn sweep_folder(
    root: &VolumeRoot,
    volume_id: VolumeId,
    dir: &RelPath,
    owned: &HashSet<(VolumeId, RelPath)>,
    older_than: i64,
) -> usize {
    let Ok(path) = root.resolve_dir(dir) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(&path) else {
        return 0;
    };
    let mut swept = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_part_name(name) {
            continue;
        }
        let Ok(rel) = dir.join(name) else {
            continue;
        };
        if owned.contains(&(volume_id, rel.clone())) {
            continue;
        }
        // `DirEntry::metadata` does not follow a link: a link named like a part is left alone.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.mtime() >= older_than {
            continue;
        }
        discard_part(root, &rel);
        if matches!(look(root, &rel), Ok(Seen::Absent)) {
            swept += 1;
        }
    }
    swept
}

// --- what a row turned out to be -------------------------------------------------------------

/// How far a row got, as far as the disk can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The index matches the disk and the row is closed.
    Completed,
    /// There is nothing to finish: the operation changed nothing, or its work is not there.
    Abandoned(&'static str),
    /// The disk shows something this module never produces; nothing was touched (warning).
    Unclear(&'static str),
    /// Cannot be decided now (offline volume): left `pending`.
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Recovered {
    verdict: Verdict,
    parts_removed: usize,
    rescan: Vec<ScanTarget>,
}

impl Recovered {
    fn with(verdict: Verdict) -> Self {
        Self {
            verdict,
            parts_removed: 0,
            rescan: Vec::new(),
        }
    }

    fn completed() -> Self {
        Self::with(Verdict::Completed)
    }

    fn abandoned(reason: &'static str) -> Self {
        Self::with(Verdict::Abandoned(reason))
    }

    fn unclear(reason: &'static str) -> Self {
        Self::with(Verdict::Unclear(reason))
    }

    fn wait() -> Self {
        Self::with(Verdict::Pending)
    }

    fn scanning(mut self, targets: impl IntoIterator<Item = ScanTarget>) -> Self {
        self.rescan.extend(targets);
        self
    }

    fn removed_parts(mut self, count: usize) -> Self {
        self.parts_removed += count;
        self
    }
}

/// The totals of one pass.
#[derive(Default)]
struct Pass {
    report: ReconcileReport,
    rescan: BTreeSet<(VolumeId, RelPath, bool)>,
}

impl Pass {
    fn scan(&mut self, targets: impl IntoIterator<Item = ScanTarget>) {
        for target in targets {
            if self.rescan.len() >= MAX_RESCAN {
                tracing::warn!(
                    "too many folders to rescan after recovery; the next scan finds the rest"
                );
                break;
            }
            self.rescan
                .insert((target.volume_id, target.dir, target.recursive));
        }
    }

    fn error(&mut self, message: String) {
        tracing::error!(%message, "crash recovery problem");
        if self.report.errors.len() < MAX_ERRORS {
            self.report.errors.push(message);
        }
    }

    fn finish(mut self) -> ReconcileReport {
        self.report.rescan = self
            .rescan
            .into_iter()
            .map(|(volume_id, dir, recursive)| ScanTarget {
                volume_id,
                dir,
                recursive,
            })
            .collect();
        self.report
    }
}

// --- looking at the disk ---------------------------------------------------------------------

/// What is at a path, never following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Absent,
    File(FileStat),
    Dir,
    /// A link, a device, a socket: nothing this module creates.
    Other,
}

fn look(root: &VolumeRoot, rel: &RelPath) -> Result<Seen, FsOpError> {
    match root.stat(rel) {
        Ok(meta) if meta.is_file() => Ok(Seen::File(FileStat::from_metadata(&meta))),
        Ok(meta) if meta.is_dir() => Ok(Seen::Dir),
        Ok(_) | Err(FsError::SymlinkRejected(_)) => Ok(Seen::Other),
        Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => Ok(Seen::Absent),
        Err(err) => Err(FsOpError::from(err)),
    }
}

/// The two ends of a transfer and their volumes.
#[derive(Clone)]
struct Ends {
    src: Loc,
    dst: Loc,
    src_root: Arc<VolumeRoot>,
    dst_root: Arc<VolumeRoot>,
}

/// What a rename-family row (a move or restore without a part file) found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameState {
    /// The file is still where it was and nothing is at the destination.
    NeverStarted,
    /// The file is at the destination.
    Placed(FileStat),
    /// A restored folder is at the destination.
    PlacedFolder,
    /// The disk does not look like anything this module leaves behind.
    Unclear(&'static str),
}

fn classify_rename(
    ends: &Ends,
    recorded_size: Option<u64>,
    restore: bool,
) -> Result<RenameState, FsOpError> {
    let at_src = look(&ends.src_root, &ends.src.rel)?;
    let at_dst = look(&ends.dst_root, &ends.dst.rel)?;
    Ok(match (at_src, at_dst) {
        (Seen::File(_), Seen::Absent) => RenameState::NeverStarted,
        (Seen::Dir, Seen::Absent) if restore => RenameState::NeverStarted,
        (Seen::Absent, Seen::File(stat)) => {
            if recorded_size.is_some_and(|size| size != stat.size) {
                RenameState::Unclear("the destination is not the file that was moved")
            } else {
                RenameState::Placed(stat)
            }
        }
        (Seen::Absent, Seen::Dir) if restore => RenameState::PlacedFolder,
        (Seen::Absent, Seen::Absent) => {
            RenameState::Unclear("neither the source nor the destination exists")
        }
        (Seen::File(from), Seen::File(to)) => classify_pair(ends, from, to)?,
        _ => RenameState::Unclear("the source and the destination are not what a move leaves"),
    })
}

/// Both ends are files. Two different files are never touched. One inode is either a rename the
/// file system lets be seen under both names (a case-only rename on a case-insensitive file
/// system, or a crashed rename of a folder entry), or two hard links: the link-and-unlink rename
/// crashed between its two steps, and the source name is the redundant one.
fn classify_pair(ends: &Ends, from: FileStat, to: FileStat) -> Result<RenameState, FsOpError> {
    if from.dev != to.dev || from.ino != to.ino {
        return Ok(RenameState::Unclear(
            "both the source and the destination exist and they are different files",
        ));
    }
    let (src, dst) = (&ends.src, &ends.dst);
    let (src_leaf, dst_leaf) = (src.name().as_bytes(), dst.name().as_bytes());
    let src_dir = folder_identity(&ends.src_root, &src.dir())?;
    let dst_dir = folder_identity(&ends.dst_root, &dst.dir())?;
    let names_in_dst = names_with_inode(&ends.dst_root, &dst.dir(), to.ino)?;
    let has = |names: &[Vec<u8>], leaf: &[u8]| names.iter().any(|name| name == leaf);
    let hard_links = if src_dir == dst_dir {
        match names_in_dst.as_slice() {
            [only] if only == dst_leaf => return Ok(RenameState::Placed(to)),
            [only] if only == src_leaf => return Ok(RenameState::NeverStarted),
            names => names.len() >= 2 && has(names, src_leaf) && has(names, dst_leaf),
        }
    } else {
        let names_in_src = names_with_inode(&ends.src_root, &src.dir(), from.ino)?;
        has(&names_in_src, src_leaf) && has(&names_in_dst, dst_leaf)
    };
    if !hard_links {
        return Ok(RenameState::Unclear(
            "the source and the destination are the same file under unexpected names",
        ));
    }
    Ok(match unlink_source(&ends.src_root, &src.rel, &from)? {
        Retired::Removed | Retired::Kept => RenameState::Placed(to),
        Retired::Changed => RenameState::Unclear("the source changed during recovery"),
    })
}

/// Which folder a path is, however it was spelled: `(device, inode)`.
fn folder_identity(root: &VolumeRoot, dir: &RelPath) -> Result<(u64, u64), FsOpError> {
    let path = root.resolve_dir(dir).map_err(FsOpError::from_destination)?;
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// The ends of a rename that was found completed, and what the index change needs to know.
struct Landed<'a> {
    mode: Mode,
    row: &'a PendingRow,
    ends: &'a Ends,
    dst_stat: FileStat,
    hash: Option<Sha1Digest>,
    kept: Option<KeptSource>,
}

// --- copies ----------------------------------------------------------------------------------

/// Everything the blocking part of a copy recovery needs.
struct CopyRecovery {
    ends: Ends,
    mode: Mode,
    journal_id: i64,
    created_at: i64,
    /// The digest the copy was verified to have (recorded before the final rename).
    recorded: Option<Sha1Digest>,
    /// The size the journal recorded.
    size: Option<u64>,
    part: RelPath,
    /// A move whose source the setting keeps in the trash.
    keep_source: bool,
    mover: Arc<dyn Mover>,
    cancel: CancellationToken,
}

enum CopyOutcome {
    /// Nothing more to do with this row.
    Stop(Recovered),
    /// The destination holds the verified copy: the index change is due.
    Land(Landing),
}

struct Landing {
    dst_stat: FileStat,
    digest: Sha1Digest,
    kept: Option<KeptSource>,
    parts_removed: usize,
}

/// What became of the source of a copy-and-delete move.
enum Retirement {
    /// The source is gone (unlinked now, already gone, or kept in the trash).
    Gone(Option<KeptSource>),
    Unclear(&'static str),
}

fn settle_copy(job: &CopyRecovery) -> Result<CopyOutcome, FsOpError> {
    let Ends {
        src,
        dst,
        src_root,
        dst_root,
    } = &job.ends;
    let parts_removed = discard_leftover(dst_root, &job.part)?;
    let stop = |recovered: Recovered| Ok(CopyOutcome::Stop(recovered.removed_parts(parts_removed)));
    let dst_dir = || ScanTarget::dir(dst.volume_id, dst.dir());
    let dst_stat = match look(dst_root, &dst.rel)? {
        Seen::File(stat) => stat,
        Seen::Absent => {
            return stop(Recovered::abandoned(
                "the copy never reached its destination",
            ));
        }
        Seen::Dir | Seen::Other => {
            return stop(
                Recovered::unclear("something that is not a file is at the destination")
                    .scanning([dst_dir()]),
            );
        }
    };
    // Without a recorded digest the copy was not verified, so the file there is not ours.
    let Some(recorded) = job.recorded else {
        return stop(Recovered::abandoned("the copy was never finished").scanning([dst_dir()]));
    };
    let not_the_copy = || {
        Recovered::unclear("the destination is not the copy that was made").scanning([dst_dir()])
    };
    if job.size != Some(dst_stat.size) {
        return stop(not_the_copy());
    }
    let digest = match hash_file(dst_root, &dst.rel, &dst_stat, &job.cancel) {
        Ok(digest) => digest,
        Err(FsOpError::SourceChanged | FsOpError::SourceMissing) => return stop(not_the_copy()),
        Err(err) => return Err(err),
    };
    if digest != recorded {
        return stop(not_the_copy());
    }
    let kept = if job.mode == Mode::Copy {
        None
    } else {
        match retire_source(job, src_root, src, &digest)? {
            Retirement::Gone(kept) => kept,
            Retirement::Unclear(reason) => {
                return stop(
                    Recovered::unclear(reason)
                        .scanning([dst_dir(), ScanTarget::dir(src.volume_id, src.dir())]),
                );
            }
        }
    };
    Ok(CopyOutcome::Land(Landing {
        dst_stat,
        digest,
        kept,
        parts_removed,
    }))
}

/// Deletes the leftover part file (it is a copy of a source that is still there, or was retired
/// only after the part became the destination); returns how many were deleted.
fn discard_leftover(root: &VolumeRoot, part: &RelPath) -> Result<usize, FsOpError> {
    if !matches!(look(root, part)?, Seen::File(_)) {
        return Ok(0);
    }
    discard_part(root, part);
    Ok(usize::from(matches!(look(root, part)?, Seen::Absent)))
}

/// The destination is a verified copy: retires the source of a move or restore, but only when it
/// is proven to be the same content.
fn retire_source(
    job: &CopyRecovery,
    src_root: &VolumeRoot,
    src: &Loc,
    digest: &Sha1Digest,
) -> Result<Retirement, FsOpError> {
    let batch = TrashBatch::for_journal(job.created_at, job.journal_id);
    let trash_path = trash_rel(&batch, &src.rel)?;
    let src_stat = match look(src_root, &src.rel)? {
        Seen::Absent => {
            // Already retired; a move that kept its source left it under the batch of the row.
            let kept =
                job.mode == Mode::Move && matches!(look(src_root, &trash_path)?, Seen::File(_));
            return Ok(Retirement::Gone(
                kept.then_some(KeptSource { batch, trash_path }),
            ));
        }
        Seen::File(stat) => stat,
        Seen::Dir | Seen::Other => {
            return Ok(Retirement::Unclear(
                "something that is not the copied file is at the source",
            ));
        }
    };
    let same = match hash_file(src_root, &src.rel, &src_stat, &job.cancel) {
        Ok(source_digest) => source_digest == *digest,
        Err(FsOpError::SourceChanged | FsOpError::SourceMissing) => false,
        Err(err) => return Err(err),
    };
    if !same {
        return Ok(Retirement::Unclear(
            "the source differs from the copy; both were kept",
        ));
    }
    let retired = if job.keep_source {
        keep_source(
            src_root,
            job.mover.as_ref(),
            &src.rel,
            &src_stat,
            &trash_path,
        )?
    } else {
        unlink_source(src_root, &src.rel, &src_stat)?
    };
    Ok(match retired {
        Retired::Removed => Retirement::Gone(None),
        Retired::Kept => Retirement::Gone(Some(KeptSource { batch, trash_path })),
        Retired::Changed => Retirement::Unclear("the source changed during recovery"),
    })
}
