//! Safe file operations: the ONLY module that changes the user's files.
//!
//! Owned by todo `implementing-safe-file-ops`. Moving, copying, trashing, restoring, purging,
//! creating and removing folders all go through [`FsOps`]; a test (`tests/fsops_guardrails.rs`)
//! fails the build when other code calls `remove_file`, `remove_dir`, `remove_dir_all`,
//! `fs::rename` or `fs::copy` (the allow-list there carries a justification per entry).
//!
//! # The safety model
//! * The disk is authoritative. Every change goes through the journal ([`journal`]) FIRST: a
//!   `pending` `fs_journal` row is written before the first file is touched, and the transaction
//!   that updates the index marks it `done`. [`FsOps::reconcile`] looks at what a crash left on the
//!   disk and finishes or abandons each `pending` row, never deleting anything it cannot prove is a
//!   duplicate.
//! * A source is never unlinked or overwritten before its copy is durable and verified (size and
//!   SHA-1 re-read from the destination), see `copy.rs`. A source that changed since it was
//!   copied is not unlinked either: both files stay and the call returns `source_changed`.
//! * A destination is never overwritten: every final rename is a no-replace rename ([`rename`]),
//!   and "replace" sends the existing file to the trash first.
//! * Every path is resolved through [`VolumeRoot`] right before it is
//!   used, so a symbolic link is never followed.
//! * A failure of one item is a return value ([`FsOpError`] with a stable
//!   [`code`](FsOpError::code)), never a panic and never the end of the other items.
//!
//! # Calling it
//! Every operation takes an [`OpContext`]: [`OpContext::from_task`] inside a task runner,
//! [`OpContext::detached`] elsewhere. All of them are `async` and run their disk work on the
//! blocking pool.
//!
//! | call | what |
//! |---|---|
//! | [`FsOps::move_file`] | rename, or copy+verify+unlink across devices; conflicts per [`ConflictPolicy`](apm_core::dto::ConflictPolicy) |
//! | [`FsOps::copy_file`] | the same engine, the source stays; the copy gets a NEW index row |
//! | [`FsOps::move_group`] | an owner and its sidecars: the sidecars follow the owner's FINAL name |
//! | [`FsOps::trash_file`] | a file or a whole folder into `.apm-trash/<batch>/<original path>` |
//! | [`FsOps::restore`] / [`FsOps::purge`] | a trash item back to its place / deleted for good |
//! | [`FsOps::remove_dir_if_removable`] | a folder that holds only junk (the junk is trashed first); a folder that holds a Synology `#recycle` or `#snapshot` folder is never removed |
//! | [`FsOps::mkdir_all`] / [`FsOps::stat`] | missing parents (with `dirs` rows; not journaled: it only creates empty folders) / a regular file's size and mtime |
//! | [`FsOps::reconcile_at_startup`] / [`FsOps::reconcile`] | crash recovery: once at startup, which also rescans what it could not settle / a pass that can be repeated at any time |
//!
//! `TrashBatch::new(task_id)` names the trash folder of one task. [`OpContext::batch`] hands every
//! clone of a context the same batch, which is what replacing and junk removal use. Per-item
//! failures: store `err.code()`; `From<FsOpError> for AppError` exists for HTTP handlers.
//!
//! A destination the scanner would ignore (a junk name, a name inside the trash, a name longer
//! than the setting allows) is refused with `destination_invalid` / `name_too_long`: the file
//! would vanish from the index. A restore is the exception for junk names (the file was there
//! before). A copy (a copy across devices included) needs room for the file plus a 16 MiB margin
//! on the destination, or it is refused with `no_space` before a byte is written. The trash
//! `reason` is stored in `trash_items`, not in the journal, so an item that recovery re-creates
//! has none.
//!
//! # Groups
//! [`FsOps::move_group`] moves the owner with the full conflict handling, then each sidecar to the
//! name derived from the owner's FINAL name; a sidecar never gets a name of its own (a taken name
//! is `SidecarSkip::SidecarConflict`, identical content is `AlreadyPresent`, `Replace` trashes the
//! existing file). Every member is its own journaled transfer with its own index transaction, so a
//! crash between two members leaves every file somewhere complete and recoverable; there is no
//! group-level atomicity. A sidecar that fails is reported in its [`SidecarOutcome::Failed`] and
//! the group goes on; a group whose owner landed finishes its sidecars even when cancelled. A
//! malformed group (a sidecar outside the owner's folder, listed twice, or whose derived name is
//! invalid) is refused before anything is touched. The link rows are repaired once at the end
//! (links that now cross folders are dropped, every destination folder is linked again with the
//! scanner's own `links::relink_dir`); the startup sweep repairs what a crash left.
//!
//! # Error codes
//! `source_missing`, `source_changed`, `not_a_regular_file`, `symlink_rejected`, `volume_offline`,
//! `volume_read_only`, `destination_invalid`, `name_too_long`, `no_space`, `verify_failed`,
//! `cancelled`, `not_empty`, `outside_trash`, `io_error` (with the `ErrorKind`), plus
//! `unknown_volume`, `internal` (the database, a hook or a worker failed: the disk operation may
//! have happened and the journal row stays `pending` for `reconcile`) and `aborted` (a test
//! failpoint simulated a crash).
//!
//! # No-replace rename, per platform
//! Linux (production) and macOS use `renameat2(RENAME_NOREPLACE)` / `renamex_np(RENAME_EXCL)`
//! through the safe `rustix` API. When the filesystem rejects the flag, and on every other
//! platform, `rename::no_replace` falls back to `hard_link` + `unlink` (atomic, never replaces),
//! and for directories or filesystems without hard links to "exists check, then `rename`", whose
//! check-to-rename window is one of the two documented races of the module (see "Known
//! limits"). A plain `rename` over an existing destination never happens, except for a case-only
//! rename of the same inode.
//!
//! # Trash layout
//! `<volume root>/.apm-trash/<batch>/<original relative path>` on the SAME volume, so trashing is a
//! rename and never copies. The original path is encoded in the path (restore works without the
//! database); when that path is taken the leaf gets a `_N` name and `trash_items.orig_path` keeps
//! the real original. `trash_items.meta_json` snapshots the index row (and the tag names) so a
//! restore needs no re-extraction.
//!
//! # Reconcile decision table
//! Rows are processed in ascending id. A row of an offline or read-only volume stays `pending`
//! (call [`FsOps::reconcile`] again when the volume is back), a row of an operation that still
//! runs in this process is left alone (its journal guard exists), and a row that is no longer
//! `pending` when its turn comes is skipped. Every step is a no-op when repeated, so a pass is
//! safe at any time.
//!
//! "Unclear" is a state this module never produces. Nothing is deleted or changed, the row is
//! closed as `abandoned` with the reason in `fs_journal.error`, a warning is logged, the folders
//! go to [`ReconcileReport::rescan`], and it counts in `abandoned`.
//!
//! | op | state on disk | action |
//! |---|---|---|
//! | move (one rename) | source present, destination absent | `abandoned` (nothing happened) |
//! | | source absent, destination present (the journaled size) | finish the index change |
//! | | both present, one inode under both names (the link-and-unlink rename stopped in the middle) | unlink the source, which is proven to be the same inode; finish |
//! | | both present, different files | unclear |
//! | | neither present, or another size at the destination | unclear |
//! | copy, or a move that crossed devices | a copy with no part file journaled | `abandoned` (it never started) |
//! | | a leftover part file | deleted (`parts_removed`), then the rows below |
//! | | destination absent | `abandoned` |
//! | | the journal has no digest (the copy was never verified) | `abandoned`: the file there is not ours |
//! | | destination present, size and SHA-1 equal the digest the journal recorded | copy: finish. Move: retire the source (only when its SHA-1 matches too: unlinked, or kept in the trash when the setting says so), finish |
//! | | the destination differs from the recorded copy | with a leftover part: `abandoned` (the final rename never happened); without one: unclear |
//! | | the source differs from the copy | unclear: both are kept |
//! | trash | source present, trash path absent | `abandoned` (nothing was trashed) |
//! | | source absent, trash path present | insert the `trash_items` row (no reason), drop the `files` rows |
//! | | both present, or neither | unclear |
//! | restore | trash path present, destination absent | `abandoned` |
//! | | trash path absent, destination present | recreate the row from the trash snapshot, delete the `trash_items` row (a folder is indexed by the rescan the report asks for) |
//! | | both present | unclear |
//! | rmdir | folder present | `abandoned` |
//! | | folder absent | delete the `dirs` rows |
//! | delete (purge) | path gone | finish (the `trash_items` row goes) |
//! | | path present | `abandoned` (purge again) |
//!
//! A pass also deletes finished journal rows after seven days (abandoned ones after thirty),
//! sweeps `.apm-<hex>.apm-part` files older than an hour that no pending row owns in the folders
//! the journal names, and drops sidecar links that cross folders (a group move that stopped half
//! way; their owners read the metadata again). It never scans: [`ReconcileReport::rescan`] lists
//! the folders (never a whole volume, at most 1000) whose index may be stale.
//! [`FsOps::reconcile_at_startup`] hands them to ONE background scan; a caller of
//! [`FsOps::reconcile`] decides.
//!
//! # Races with the scanner
//! A scan can run while an operation does: it can see a file at its source, at its destination,
//! at neither or at both, and can insert or delete rows in between. Every index transaction is
//! written to cope with that without losing what the index knew about the file (rating, flag,
//! capture time, camera, size, tags):
//! * A move keeps the id of the source row: it deletes a row the scanner created at the
//!   destination and relocates the source row. When a scan already dropped the source row, the
//!   scanner's destination row is adopted, with what the index knew carried over from the
//!   snapshot taken BEFORE the rename (rating and flag always, the metadata only while the row
//!   was not processed again and the file is the same size and mtime), and when there is no row
//!   at all one is inserted from that snapshot. A copy gets a new row the same way.
//! * Trashing snapshots what the index knows before the rename. When a scan dropped the rows
//!   meanwhile, the snapshot fills `trash_items.meta_json` and the byte totals, and the
//!   [`AppliedOp::Trashed`] hook sees an empty or short `file_ids`.
//! * A row an operation writes is stamped with the DESTINATION volume's current scan generation,
//!   so the scan in flight leaves it alone. Generations are counted per volume and are not
//!   comparable across volumes, which is why the scanner only removes rows of ITS volume
//!   (`indexer::store::delete_files`): a file moved to another volume during a scan keeps its
//!   row, its rating and its tags.
//! * A scan that inserts a row after the operation's transaction only re-queues that row's
//!   pipeline states (the scanner's own upsert keeps rating, flag, size and tags). A scan that
//!   links a sidecar to its owner can set the owner's `meta_state` back to pending.
//! * Moves keep the id of the row; a copy, a restore and (by deleting the rows) a trash do not.
//!
//! # Known limits
//! * Between its last check of the source and the `unlink` that retires it (a window of
//!   microseconds, after the copy was verified), a writer that renames another file onto the
//!   source path would lose that file. The file system offers nothing that closes it; the copy
//!   itself is never at risk.
//! * Folders created for a transfer that then fails are left (empty): `remove_dir_if_removable`
//!   removes them on request.
//! * A file system that supports neither the no-replace flag nor hard links (SMB, exFAT) gets
//!   "exists check, then `rename`": a writer that creates the destination in those few
//!   microseconds would lose its file.
//!
//! # Seams for other modules
//! * [`OpHook`] runs INSIDE the transaction that finalises an operation (the Immich outbox
//!   enqueues there); register with [`FsOps::with_hook`] BEFORE `reconcile_at_startup`, which
//!   runs the hooks of the operations it finishes. Hooks must be idempotent.
//! * [`SelfWrites`] (from [`FsOps::self_writes`]) remembers the folders this module changed, so
//!   the filesystem watcher skips the app's own changes.
//! * `OpContext::changes` is told, after each commit, which folders changed
//!   ([`TaskChangeSink`] turns that into the coalesced `fs_change` event).
//! * `ReconcileReport::rescan` lists folders (`ScanTarget`s) to hand to the indexer.
//! * Tests: [`FsOps::with_mover`] ([`Mover::crosses_devices`] makes two folders of one file
//!   system look like two devices, which forces the copy path), [`FsOps::with_free_space`] (a full
//!   disk) and `FsOps::with_failpoints` ([`Failpoints`] stops or fails an operation at a [`Step`]
//!   to simulate a crash; the crash tests then build a fresh `FsOps` over the same database and
//!   files and call `reconcile`).
//!
//! # Logging
//! `error!`, `warn!` and `info!` lines carry ids, counts and error codes only; a path appears at
//! `debug` level at most, so a default-level log never holds a name from the user's library
//! (`tests/fsops_guardrails.rs` enforces it).
//!
//! # Progress bytes
//! `OpContext::on_bytes` gets the file's size once after a rename and one call per 1 MiB chunk of
//! a copy. Skipped files report nothing; hashing done to decide a conflict is not reported.

mod apply;
pub mod backend;
mod context;
mod copy;
mod dirs;
mod error;
pub mod executor;
mod failpoints;
mod groups;
mod hooks;
pub mod journal;
mod reconcile;
pub mod rename;
mod self_writes;
mod stat;
#[cfg(test)]
mod test_support;
pub mod trash;

use std::fmt;
use std::sync::Arc;

use apm_core::junk::JunkRules;
use apm_core::paths::{RelPath, VolumeId};
use apm_core::sidecar::SidecarMatcher;

pub use backend::{FreeSpace, Mover, SystemFreeSpace, SystemMover};
pub use context::{OpContext, TaskChangeSink, TrashBatch};
pub use dirs::{RmdirOutcome, is_protected_path};
pub use error::FsOpError;
pub use executor::{MoveOutcome, MoveRequest, SkipReason};
pub use failpoints::{FailAction, Failpoints, Step};
pub use groups::{
    GroupOutcome, MoveGroup, SidecarAction, SidecarItem, SidecarOutcome, SidecarSkip,
};
pub use hooks::{AppliedOp, OpHook};
pub use reconcile::ReconcileReport;
pub use self_writes::SelfWrites;
pub use stat::{Expect, FileStat};
pub use trash::{PurgeOutcome, RestoreOutcome, TrashOutcome, TrashRequest};

use crate::db::Db;
use crate::error::AppError;
use crate::indexer::Indexer;
use crate::indexer::VolumeRegistry;
use crate::security::VolumeRoot;
use crate::settings::{Settings, SettingsService};
use journal::Journal;

/// A place on a volume: the volume and the path below its root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Loc {
    pub volume_id: VolumeId,
    pub rel: RelPath,
}

impl Loc {
    pub fn new(volume_id: VolumeId, rel: RelPath) -> Self {
        Self { volume_id, rel }
    }

    /// The folder that contains this place (the root for a top-level entry or the root itself).
    pub fn dir(&self) -> RelPath {
        self.rel.parent().unwrap_or_else(RelPath::root)
    }

    /// The last path component (empty for the volume root).
    pub fn name(&self) -> &str {
        self.rel.file_name().unwrap_or_default()
    }

    /// The same folder with another leaf name.
    pub(crate) fn with_name(&self, name: &str) -> Result<Self, FsOpError> {
        let rel = self
            .dir()
            .join(name)
            .map_err(|_| FsOpError::DestinationInvalid("not a valid file name"))?;
        Ok(Self::new(self.volume_id, rel))
    }

    /// The entry `name` inside this folder.
    pub(crate) fn child(&self, name: &str) -> Result<Self, FsOpError> {
        let rel = self
            .rel
            .join(name)
            .map_err(|_| FsOpError::DestinationInvalid("not a valid file name"))?;
        Ok(Self::new(self.volume_id, rel))
    }
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "volume {}:{}", self.volume_id, self.rel.as_str())
    }
}

/// The settings-derived rules one operation works with, taken once so the operation sees one
/// consistent document.
pub(crate) struct Rules {
    pub settings: Arc<Settings>,
    pub junk: JunkRules,
    pub matcher: SidecarMatcher,
}

impl Rules {
    pub(crate) fn name_max_len(&self) -> usize {
        usize::from(self.settings.name_max_len)
    }
}

/// The file operations service. Cheap to clone; the clones share the journal bookkeeping, the
/// [`SelfWrites`] registry and the hooks.
#[derive(Clone)]
pub struct FsOps {
    pub(crate) db: Db,
    pub(crate) volumes: VolumeRegistry,
    pub(crate) settings: SettingsService,
    pub(crate) indexer: Indexer,
    pub(crate) journal: Journal,
    hooks: Arc<Vec<Arc<dyn OpHook>>>,
    self_writes: SelfWrites,
    pub(crate) mover: Arc<dyn Mover>,
    pub(crate) free_space: Arc<dyn FreeSpace>,
    failpoints: Option<Failpoints>,
    /// One recovery pass at a time: two passes over the same row would race.
    reconcile_lock: Arc<tokio::sync::Mutex<()>>,
}

impl fmt::Debug for FsOps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FsOps")
            .field("hooks", &self.hooks.len())
            .field("failpoints", &self.failpoints.is_some())
            .finish_non_exhaustive()
    }
}

impl FsOps {
    pub fn new(
        db: Db,
        volumes: VolumeRegistry,
        settings: SettingsService,
        indexer: Indexer,
    ) -> Self {
        Self {
            journal: Journal::new(db.clone()),
            db,
            volumes,
            settings,
            indexer,
            hooks: Arc::default(),
            self_writes: SelfWrites::new(),
            mover: Arc::new(SystemMover),
            free_space: Arc::new(SystemFreeSpace),
            failpoints: None,
            reconcile_lock: Arc::default(),
        }
    }

    /// Registers a hook that runs inside the finalising transaction of every operation. Hooks
    /// run in registration order.
    #[must_use]
    pub fn with_hook(mut self, hook: Arc<dyn OpHook>) -> Self {
        let mut hooks = self.hooks.as_ref().clone();
        hooks.push(hook);
        self.hooks = Arc::new(hooks);
        self
    }

    /// Replaces the rename backend (tests force the cross-device copy with it).
    #[must_use]
    pub fn with_mover(mut self, mover: Arc<dyn Mover>) -> Self {
        self.mover = mover;
        self
    }

    /// Replaces the free-space probe.
    #[must_use]
    pub fn with_free_space(mut self, free_space: Arc<dyn FreeSpace>) -> Self {
        self.free_space = free_space;
        self
    }

    /// Installs crash/failure injection for tests.
    #[doc(hidden)]
    #[must_use]
    pub fn with_failpoints(mut self, failpoints: Failpoints) -> Self {
        self.failpoints = Some(failpoints);
        self
    }

    /// The folders this service changed recently, for the watcher.
    pub fn self_writes(&self) -> &SelfWrites {
        &self.self_writes
    }

    /// An online volume's root.
    pub(crate) fn volume(&self, id: VolumeId) -> Result<Arc<VolumeRoot>, FsOpError> {
        if self.volumes.volume(id).is_none() {
            return Err(FsOpError::UnknownVolume);
        }
        self.volumes
            .get(id)
            .map_err(|_: AppError| FsOpError::VolumeOffline)
    }

    /// An online volume's root, refused when the volume is read-only.
    pub(crate) fn writable_volume(&self, id: VolumeId) -> Result<Arc<VolumeRoot>, FsOpError> {
        let root = self.volume(id)?;
        match self.volumes.volume(id) {
            Some(volume) if volume.writable => Ok(root),
            _ => Err(FsOpError::VolumeReadOnly),
        }
    }

    /// The settings-derived rules, one consistent snapshot.
    pub(crate) fn rules(&self) -> Rules {
        let settings = self.settings.get();
        Rules {
            junk: JunkRules::new(&settings.junk_names, &settings.junk_prefixes),
            matcher: SidecarMatcher::new(&settings.sidecar_extensions),
            settings,
        }
    }

    /// Fires a failpoint (a no-op unless a test installed some).
    pub(crate) fn check(&self, step: Step) -> Result<(), FsOpError> {
        match &self.failpoints {
            Some(failpoints) => failpoints.check(step),
            None => Ok(()),
        }
    }

    pub(crate) fn failpoints(&self) -> Option<Failpoints> {
        self.failpoints.clone()
    }

    /// Runs the registered hooks on the finalising transaction.
    pub(crate) async fn run_hooks(
        &self,
        tx: &mut sqlx::SqliteConnection,
        op: &AppliedOp,
    ) -> Result<(), sqlx::Error> {
        for hook in self.hooks.iter() {
            hook.on_applied(&mut *tx, op).await?;
        }
        Ok(())
    }

    /// Remembers that `dirs` of the volume changed (before and after the disk step).
    pub(crate) fn note_dirs(&self, volume_id: VolumeId, dirs: &[RelPath]) {
        for dir in dirs {
            self.self_writes.note(volume_id, dir);
        }
    }
}

/// Runs blocking file-system work on the blocking pool.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T, FsOpError>
where
    F: FnOnce() -> Result<T, FsOpError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| FsOpError::internal(format!("a file worker failed: {err}")))?
}
