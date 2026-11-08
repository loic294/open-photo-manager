//! Trash, restore and purge.
//!
//! Trashing is a RENAME inside the volume to `.apm-trash/<batch>/<original relative path>`: it
//! never copies and never deletes. Restoring renames the item back (a file goes through the same
//! engine as a move, so conflicts, the journal and the index behave identically). [`FsOps::purge`]
//! is the only place trash content is deleted, and it refuses every path that is not at least
//! three levels inside `.apm-trash/`.

use std::fs;
use std::io;
use std::sync::Arc;

use apm_core::consts::TRASH_DIR;
use apm_core::dto::ConflictPolicy;
use apm_core::junk::JunkRules;
use apm_core::paths::{FileId, RelPath};
use tokio_util::sync::CancellationToken;

use super::apply::{
    Crossing, Mode, TrashApply, TrashItem, TrashMeta, known_before_trash, load_trash_item,
    touched_dirs,
};
use super::backend::Mover;
use super::context::{OpContext, TrashBatch};
use super::dirs::{free_name, in_trash, make_dirs, validate_file_path};
use super::error::FsOpError;
use super::executor::{ATTEMPTS, Job, RestoreInfo, SkipReason, Transferred};
use super::failpoints::Step;
use super::journal::{NewEntry, Op};
use super::{Expect, FileStat, FsOps, Loc, blocking};
use crate::indexer::{NoopProgress, RunContext, ScanRequest, ScanTarget};
use crate::security::{FsError, VolumeRoot};

/// One file or folder to trash.
#[derive(Debug, Clone)]
pub struct TrashRequest {
    pub file_id: Option<FileId>,
    pub loc: Loc,
    pub expect: Option<Expect>,
    pub batch: TrashBatch,
    /// Free text kept in `trash_items.reason` (`user`, `replaced`, `junk`, `moved`, ...).
    pub reason: String,
}

/// A trashed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashOutcome {
    pub trash_item_id: i64,
    /// Where the item is now, relative to the volume root (below `.apm-trash/`).
    pub trash_path: RelPath,
    pub is_dir: bool,
    pub bytes: u64,
    /// How many `files` rows were removed from the index.
    pub files_removed: u64,
}

/// A restored item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored {
        /// Where the item went (a conflict may have renamed it).
        dst: Loc,
        renamed: bool,
        /// The new row of a restored file; `None` for a folder (rescanned instead).
        file_id: Option<FileId>,
        bytes: u64,
    },
    Skipped(SkipReason),
}

/// A purged item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub bytes: u64,
    pub files: u64,
}

/// One item to trash, the way the engine passes it around.
pub(crate) struct TrashSpec<'a> {
    pub loc: &'a Loc,
    pub expect: Option<Expect>,
    pub batch: &'a TrashBatch,
    pub reason: &'a str,
}

/// What is about to be trashed.
#[derive(Debug, Clone, Copy)]
enum Item {
    File(FileStat),
    Dir,
}

/// Where an item goes: `.apm-trash/<batch>/<original path>`.
pub(crate) fn trash_rel(batch: &TrashBatch, orig: &RelPath) -> Result<RelPath, FsOpError> {
    RelPath::root()
        .join(TRASH_DIR)
        .and_then(|rel| rel.join(batch.name()))
        .map(|rel| rel.join_rel(orig))
        .map_err(|_| FsOpError::internal("the trash batch name is not a valid path"))
}

/// Only paths at least three levels inside the trash (`.apm-trash/<batch>/<name>`) may be
/// restored from or purged: the trash folder and a whole batch are never an item.
fn ensure_inside_trash(path: &RelPath) -> Result<(), FsOpError> {
    if in_trash(path) && path.depth() >= 3 {
        Ok(())
    } else {
        Err(FsOpError::OutsideTrash)
    }
}

/// Looks at what is about to be trashed. Links and special files are refused, and a file that
/// is not what the caller expects is left alone.
fn inspect(root: &VolumeRoot, rel: &RelPath, expect: Option<Expect>) -> Result<Item, FsOpError> {
    let meta = root.stat(rel).map_err(FsOpError::from_source)?;
    if meta.is_dir() {
        return Ok(Item::Dir);
    }
    if !meta.is_file() {
        return Err(FsOpError::NotARegularFile);
    }
    let stat = FileStat::from_metadata(&meta);
    if expect.is_some_and(|expect| !stat.matches(&expect)) {
        return Err(FsOpError::SourceChanged);
    }
    Ok(Item::File(stat))
}

/// A free path in the batch for `orig`: the mirrored original path, or its leaf with a `_N`
/// suffix when the batch already holds something there. Creates the batch's folders.
fn pick_trash_path(
    root: &VolumeRoot,
    batch: &TrashBatch,
    orig: &RelPath,
    max: usize,
) -> Result<RelPath, FsOpError> {
    let wanted = trash_rel(batch, orig)?;
    let parent = wanted.parent().unwrap_or_else(RelPath::root);
    make_dirs(root, &parent)?;
    let leaf = free_name(root, &parent, wanted.file_name().unwrap_or_default(), max)?;
    parent
        .join(&leaf)
        .map_err(|_| FsOpError::DestinationInvalid("not a valid file name"))
}

enum Attempt {
    Done,
    /// Something changed under the attempt (the batch folder was pruned): try again.
    Retry,
    /// The path is taken: use this one.
    Retarget(RelPath),
}

/// One try at renaming `orig` to `target` in the trash. Every step re-resolves its path, so a
/// racing writer is noticed instead of trusted.
fn rename_once(
    root: &VolumeRoot,
    mover: &dyn Mover,
    orig: &RelPath,
    target: &RelPath,
    batch: &TrashBatch,
    max: usize,
) -> Result<Attempt, FsOpError> {
    let taken = || pick_trash_path(root, batch, orig, max).map(Attempt::Retarget);
    make_dirs(root, &target.parent().unwrap_or_else(RelPath::root))?;
    let from = root
        .resolve_existing(orig)
        .map_err(FsOpError::from_source)?;
    let to = match root.resolve_for_create(target) {
        Ok(path) => path,
        Err(FsError::AlreadyExists(_)) => return taken(),
        Err(FsError::SymlinkRejected(at)) if &at == target => return taken(),
        Err(err) => return Err(FsOpError::from_destination(err)),
    };
    match mover.rename_no_replace(&from, &to) {
        Ok(()) => Ok(Attempt::Done),
        Err(err) => match err.kind() {
            io::ErrorKind::AlreadyExists => taken(),
            io::ErrorKind::NotFound => match root.resolve_existing(orig) {
                Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => {
                    Err(FsOpError::SourceMissing)
                }
                _ => Ok(Attempt::Retry),
            },
            io::ErrorKind::CrossesDevices => Err(FsOpError::DestinationInvalid(
                "the trash is on another file system",
            )),
            _ => Err(FsOpError::from(err)),
        },
    }
}

/// Removes the empty folders an item left in the trash: from its parent up to and including its
/// batch folder, never `.apm-trash` itself. Best effort: a folder that is not empty (or that a
/// concurrent trash is about to use) ends the climb.
pub(crate) fn prune_trash_dirs(root: &VolumeRoot, trash_path: &RelPath) {
    for dir in trash_path.ancestors() {
        if dir.depth() < 2 {
            break;
        }
        let Ok(path) = root.resolve_dir(&dir) else {
            break;
        };
        if fs::remove_dir(&path).is_err() {
            break;
        }
    }
}

/// What deleting a trash item removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Purged {
    bytes: u64,
    files: u64,
}

/// Deletes the item at `rel` and everything below it, never following a link (a link is
/// unlinked). A path that is already gone is fine: a purge that was interrupted can be run again.
/// Checks `cancel` before every entry; what was deleted stays deleted.
fn purge_path(
    root: &VolumeRoot,
    rel: &RelPath,
    cancel: &CancellationToken,
    on_bytes: &(dyn Fn(u64) + Send + Sync),
) -> Result<Purged, FsOpError> {
    let (Some(parent), Some(name)) = (rel.parent(), rel.file_name()) else {
        return Err(FsOpError::OutsideTrash);
    };
    let parent_path = match root.resolve_dir(&parent) {
        Ok(path) => path,
        Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => return Ok(Purged::default()),
        Err(err) => return Err(FsOpError::from(err)),
    };
    let mut purged = Purged::default();
    // Depth first, children before their folder; `true` marks a folder whose children are gone.
    let mut stack = vec![(parent_path.join(name), false)];
    while let Some((path, emptied)) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(FsOpError::Cancelled);
        }
        if emptied {
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(FsOpError::from(err)),
            }
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(FsOpError::from(err)),
        };
        if meta.is_dir() {
            stack.push((path.clone(), true));
            for entry in fs::read_dir(&path)? {
                stack.push((entry?.path(), false));
            }
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(FsOpError::from(err)),
        }
        purged.files += 1;
        if meta.is_file() {
            purged.bytes += meta.len();
            if meta.len() > 0 {
                on_bytes(meta.len());
            }
        }
    }
    Ok(purged)
}

/// Where a restored folder goes.
enum DirPlace {
    /// The policy says leave it in the trash.
    Skip,
    At {
        rel: RelPath,
        renamed: bool,
        /// The folders that had to be created above it.
        created: Vec<RelPath>,
    },
}

/// Decides where a restored FOLDER goes. A taken name is renamed (`Smart`, `Rename`) or left
/// alone (`Skip`); a folder never replaces anything, and is never merged into one that exists.
fn place_dir(
    root: &VolumeRoot,
    wanted: &RelPath,
    policy: ConflictPolicy,
    max: usize,
) -> Result<DirPlace, FsOpError> {
    let parent = wanted.parent().unwrap_or_else(RelPath::root);
    let created = make_dirs(root, &parent)?;
    match root.resolve_for_create(wanted) {
        Ok(_) => {
            return Ok(DirPlace::At {
                rel: wanted.clone(),
                renamed: false,
                created,
            });
        }
        Err(FsError::AlreadyExists(_)) => {}
        Err(FsError::SymlinkRejected(at)) if &at == wanted => {}
        Err(err) => return Err(FsOpError::from_destination(err)),
    }
    match policy {
        ConflictPolicy::Skip => Ok(DirPlace::Skip),
        ConflictPolicy::Replace => Err(FsOpError::DestinationInvalid(
            "a restored folder never replaces what is there",
        )),
        ConflictPolicy::Smart | ConflictPolicy::Rename => {
            let name = free_name(root, &parent, wanted.file_name().unwrap_or_default(), max)?;
            let rel = parent
                .join(&name)
                .map_err(|_| FsOpError::DestinationInvalid("not a valid file name"))?;
            Ok(DirPlace::At {
                rel,
                renamed: true,
                created,
            })
        }
    }
}

/// Renames the trashed folder back, never replacing anything.
fn rename_dir_back(
    root: &VolumeRoot,
    mover: &dyn Mover,
    from: &RelPath,
    to: &RelPath,
) -> Result<(), FsOpError> {
    let source = root
        .resolve_existing(from)
        .map_err(FsOpError::from_source)?;
    let target = root
        .resolve_for_create(to)
        .map_err(FsOpError::from_destination)?;
    match mover.rename_no_replace(&source, &target) {
        Ok(()) => Ok(()),
        Err(err) => match err.kind() {
            io::ErrorKind::AlreadyExists => Err(FsOpError::DestinationInvalid(
                "the destination was created meanwhile",
            )),
            io::ErrorKind::NotFound => Err(FsOpError::SourceMissing),
            io::ErrorKind::CrossesDevices => Err(FsOpError::DestinationInvalid(
                "the trash is on another file system",
            )),
            _ => Err(FsOpError::from(err)),
        },
    }
}

impl FsOps {
    /// Moves a file or folder into the trash (`.apm-trash/<batch>/<original path>`). Nothing is
    /// copied or deleted; the index rows of the item go (a folder: everything below it), its
    /// metadata is kept in the trash item so a restore needs no re-extraction.
    ///
    /// `req.file_id` is only a hint and is ignored; `req.expect` protects a file that changed
    /// since the caller looked.
    ///
    /// # Errors
    /// `source_missing`, `source_changed`, `not_a_regular_file`, `symlink_rejected`,
    /// `destination_invalid` (the root, or already in the trash), `volume_read_only`,
    /// `volume_offline`, `cancelled`, `internal` (the index change failed: the journal row stays
    /// `pending`, see [`FsOps::reconcile`]).
    pub async fn trash_file(
        &self,
        req: TrashRequest,
        cx: &OpContext,
    ) -> Result<TrashOutcome, FsOpError> {
        self.trash_core(
            TrashSpec {
                loc: &req.loc,
                expect: req.expect,
                batch: &req.batch,
                reason: &req.reason,
            },
            cx,
        )
        .await
    }

    pub(crate) async fn trash_core(
        &self,
        spec: TrashSpec<'_>,
        cx: &OpContext,
    ) -> Result<TrashOutcome, FsOpError> {
        let TrashSpec {
            loc,
            expect,
            batch,
            reason,
        } = spec;
        cx.ensure_active()?;
        let max = self.rules().name_max_len();
        let root = self.writable_volume(loc.volume_id)?;
        if loc.rel.is_root() {
            return Err(FsOpError::DestinationInvalid(
                "the volume root cannot be trashed",
            ));
        }
        if in_trash(&loc.rel) {
            return Err(FsOpError::DestinationInvalid(
                "the item is already in the trash",
            ));
        }

        let (item, mut target) = {
            let (root, rel, batch) = (Arc::clone(&root), loc.rel.clone(), batch.clone());
            blocking(move || {
                let item = inspect(&root, &rel, expect)?;
                let target = pick_trash_path(&root, &batch, &rel, max)?;
                Ok((item, target))
            })
            .await?
        };
        let (is_dir, size) = match item {
            Item::File(stat) => (false, stat.size),
            Item::Dir => (true, 0),
        };
        // Read before anything changes: a scan can drop the rows between the rename and the
        // index transaction, and a restore still needs what they held.
        let known = {
            let mut conn = self.db.read().acquire().await?;
            known_before_trash(&mut conn, loc, is_dir).await?
        };

        let first = Loc::new(loc.volume_id, target.clone());
        let guard = self
            .journal
            .begin(NewEntry {
                task_id: cx.task_id,
                op: Op::Trash,
                src: Some(loc),
                dst: Some(&first),
                size: (!is_dir).then_some(size),
            })
            .await?;
        let journal_id = guard.id();

        self.note_dirs(loc.volume_id, &[loc.dir()]);
        if let Err(err) = self
            .rename_into_trash(&root, loc, batch, &mut target, journal_id, max)
            .await
        {
            if !err.is_abort() {
                self.journal.fail(journal_id, &err).await;
            }
            return Err(err);
        }
        self.check(Step::AfterFinalRename)?;

        let trashed = self
            .apply_trash(&TrashApply {
                journal_id,
                task_id: cx.task_id,
                volume_id: loc.volume_id,
                orig: &loc.rel,
                trash: &target,
                batch,
                is_dir,
                size,
                reason: Some(reason),
                known: known.as_ref(),
            })
            .await?;
        self.announce(cx, &touched_dirs(Some(loc), None, &[]));
        cx.report_bytes(trashed.bytes);
        Ok(TrashOutcome {
            trash_item_id: trashed.trash_item_id,
            trash_path: target,
            is_dir,
            bytes: trashed.bytes,
            files_removed: trashed.files_removed,
        })
    }

    /// The disk step of a trash: renames the item, re-picking the name when it is taken.
    async fn rename_into_trash(
        &self,
        root: &Arc<VolumeRoot>,
        loc: &Loc,
        batch: &TrashBatch,
        target: &mut RelPath,
        journal_id: i64,
        max: usize,
    ) -> Result<(), FsOpError> {
        self.check(Step::AfterJournal)?;
        for _ in 0..ATTEMPTS {
            let attempt = {
                let (root, mover) = (Arc::clone(root), Arc::clone(&self.mover));
                let (orig, wanted, batch) = (loc.rel.clone(), target.clone(), batch.clone());
                blocking(move || rename_once(&root, mover.as_ref(), &orig, &wanted, &batch, max))
                    .await?
            };
            match attempt {
                Attempt::Done => return Ok(()),
                Attempt::Retry => {}
                Attempt::Retarget(next) => {
                    *target = next;
                    let moved = Loc::new(loc.volume_id, target.clone());
                    self.journal.set_dst(journal_id, &moved, None, None).await?;
                }
            }
        }
        Err(FsOpError::DestinationInvalid(
            "the trash path kept changing",
        ))
    }

    /// Brings a trashed file or folder back to where it came from. A taken name is decided by
    /// `policy` like for a move: `Smart` skips an identical file and renames a different one,
    /// `Skip` leaves the item in the trash, `Rename` renames, `Replace` trashes what is there
    /// (reason `replaced`) and takes its place. A restored FOLDER is never merged or replaced:
    /// a taken name renames it (`Smart`, `Rename`), `Skip` skips and `Replace` is refused.
    ///
    /// A file gets a NEW index row, with the metadata the trash item remembered when the file on
    /// disk still has the size and mtime it had. A folder is indexed by a scan of it.
    ///
    /// # Errors
    /// `source_missing` (no such item, or its content is gone: purge the item), `outside_trash`,
    /// `destination_invalid`, `symlink_rejected`, `volume_read_only`, `volume_offline`,
    /// `cancelled`, `no_space` and `verify_failed` (cross-device only), `internal`.
    pub async fn restore(
        &self,
        trash_item_id: i64,
        policy: ConflictPolicy,
        cx: &OpContext,
    ) -> Result<RestoreOutcome, FsOpError> {
        cx.ensure_active()?;
        let item = self.load_trash_item(trash_item_id).await?;
        ensure_inside_trash(&item.trash_path)?;
        if item.is_dir {
            self.restore_dir(&item, policy, cx).await
        } else {
            self.restore_file(&item, policy, cx).await
        }
    }

    async fn load_trash_item(&self, id: i64) -> Result<TrashItem, FsOpError> {
        let mut conn = self.db.read().acquire().await?;
        load_trash_item(&mut conn, id)
            .await?
            .ok_or(FsOpError::SourceMissing)
    }

    async fn restore_file(
        &self,
        item: &TrashItem,
        policy: ConflictPolicy,
        cx: &OpContext,
    ) -> Result<RestoreOutcome, FsOpError> {
        let snap = TrashMeta::file_snapshot(item.meta_json.as_deref());
        let job = Job {
            mode: Mode::Restore,
            src: item.trash_loc(),
            dst: item.orig_loc(),
            policy,
            expect: None,
            allow_rename: true,
            companions: Vec::new(),
            crossing: Crossing::Delete,
            restore: Some(RestoreInfo {
                item_id: item.id,
                snap,
            }),
        };
        match self.transfer(&job, cx).await? {
            Transferred::Done {
                dst,
                renamed,
                bytes,
                indexed,
                ..
            } => {
                self.prune_after(item).await;
                Ok(RestoreOutcome::Restored {
                    dst,
                    renamed,
                    file_id: indexed.file_id,
                    bytes,
                })
            }
            Transferred::Skipped(reason) => Ok(RestoreOutcome::Skipped(reason)),
        }
    }

    async fn restore_dir(
        &self,
        item: &TrashItem,
        policy: ConflictPolicy,
        cx: &OpContext,
    ) -> Result<RestoreOutcome, FsOpError> {
        let rules = self.rules();
        let max = rules.name_max_len();
        let root = self.writable_volume(item.volume_id)?;
        let wanted = item.orig_loc();
        validate_file_path(&rules, &JunkRules::reserved_only(), &wanted.rel)?;

        let placed = {
            let (root, wanted) = (Arc::clone(&root), wanted.rel.clone());
            blocking(move || place_dir(&root, &wanted, policy, max)).await?
        };
        let (rel, renamed, created) = match placed {
            DirPlace::Skip => return Ok(RestoreOutcome::Skipped(SkipReason::DestinationExists)),
            DirPlace::At {
                rel,
                renamed,
                created,
            } => (rel, renamed, created),
        };
        let dst = Loc::new(item.volume_id, rel);
        let src = item.trash_loc();

        let guard = self
            .journal
            .begin(NewEntry {
                task_id: cx.task_id,
                op: Op::Restore,
                src: Some(&src),
                dst: Some(&dst),
                size: Some(item.size),
            })
            .await?;
        let journal_id = guard.id();
        let touched = touched_dirs(None, Some(&dst), &created);
        for (volume_id, dir) in &touched {
            self.note_dirs(*volume_id, std::slice::from_ref(dir));
        }

        let renamed_back = async {
            self.check(Step::AfterJournal)?;
            let (root, mover) = (Arc::clone(&root), Arc::clone(&self.mover));
            let (from, to) = (src.rel.clone(), dst.rel.clone());
            blocking(move || rename_dir_back(&root, mover.as_ref(), &from, &to)).await
        }
        .await;
        if let Err(err) = renamed_back {
            if !err.is_abort() {
                self.journal.fail(journal_id, &err).await;
            }
            return Err(err);
        }
        self.check(Step::AfterFinalRename)?;
        self.apply_restore_dir(journal_id, item, &dst).await?;
        self.announce(cx, &touched);
        self.prune_after(item).await;

        // The folder's content is indexed by a scan of it, outside any transaction.
        let request = ScanRequest {
            targets: vec![ScanTarget::subtree(dst.volume_id, dst.rel.clone())],
            ..ScanRequest::default()
        };
        let run = RunContext::new(
            cx.cancel.clone(),
            Arc::new(NoopProgress),
            Arc::clone(&cx.changes),
        );
        if let Err(err) = self.indexer.scan(&request, &run).await {
            tracing::warn!(error = %err, "indexing a restored folder failed; the next scan will");
        }
        Ok(RestoreOutcome::Restored {
            dst,
            renamed,
            file_id: None,
            bytes: item.size,
        })
    }

    /// Removes the empty folders a restored or purged item left in its batch.
    pub(crate) async fn prune_after(&self, item: &TrashItem) {
        let Ok(root) = self.volume(item.volume_id) else {
            return;
        };
        let trash_path = item.trash_path.clone();
        let pruned = blocking(move || {
            prune_trash_dirs(&root, &trash_path);
            Ok(())
        })
        .await;
        if let Err(err) = pruned {
            tracing::debug!(error = %err, "could not prune the trash folders");
        }
    }

    /// Deletes a trash item for good: its content on disk, then its `trash_items` row. This is the
    /// only place trash content is deleted, and it refuses every path that is not inside
    /// `.apm-trash/<batch>/`. Links in the item are unlinked, never followed. Content that is
    /// already gone is fine, so an interrupted purge can be repeated.
    ///
    /// # Errors
    /// `source_missing` (no such item), `outside_trash`, `symlink_rejected` (a link in the path to
    /// the item), `volume_read_only`, `volume_offline`, `cancelled` (what was deleted stays
    /// deleted; purge again to finish), `internal`.
    pub async fn purge(
        &self,
        trash_item_id: i64,
        cx: &OpContext,
    ) -> Result<PurgeOutcome, FsOpError> {
        cx.ensure_active()?;
        let item = self.load_trash_item(trash_item_id).await?;
        let root = self.writable_volume(item.volume_id)?;
        ensure_inside_trash(&item.trash_path)?;

        let src = item.trash_loc();
        let guard = self
            .journal
            .begin(NewEntry {
                task_id: cx.task_id,
                op: Op::Delete,
                src: Some(&src),
                dst: None,
                size: Some(item.size),
            })
            .await?;
        let journal_id = guard.id();
        let removed = async {
            self.check(Step::AfterJournal)?;
            let (root, rel) = (Arc::clone(&root), item.trash_path.clone());
            let (cancel, on_bytes) = (cx.cancel.clone(), Arc::clone(&cx.on_bytes));
            blocking(move || purge_path(&root, &rel, &cancel, on_bytes.as_ref())).await
        }
        .await;
        let removed = match removed {
            Ok(removed) => removed,
            Err(err) => {
                if !err.is_abort() {
                    self.journal.fail(journal_id, &err).await;
                }
                return Err(err);
            }
        };
        self.apply_purge(journal_id, &item, removed.bytes).await?;
        self.prune_after(&item).await;
        Ok(PurgeOutcome {
            bytes: removed.bytes,
            files: removed.files,
        })
    }
}
