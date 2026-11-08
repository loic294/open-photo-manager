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
//!   SHA-1 re-read from the destination), see `copy.rs`.
//! * A destination is never overwritten: every final rename is a no-replace rename ([`rename`]),
//!   and "replace" sends the existing file to the trash first.
//! * Every path is resolved through [`VolumeRoot`](crate::security::VolumeRoot) right before it is
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
//! | [`FsOps::remove_dir_if_removable`] | a folder that holds only junk (the junk is trashed first) |
//! | [`FsOps::mkdir_all`] / [`FsOps::stat`] | missing parents (with `dirs` rows) / a regular file's size and mtime |
//! | [`FsOps::reconcile`] | crash recovery, called once at startup |
//!
//! `TrashBatch::new(task_id)` names the trash folder of one task. [`OpContext::batch`] hands every
//! clone of a context the same batch, which is what replacing and junk removal use. Per-item
//! failures: store `err.code()`; `From<FsOpError> for AppError` exists for HTTP handlers.
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
//! platform, [`rename::no_replace`] falls back to `hard_link` + `unlink` (atomic, never replaces),
//! and for directories or filesystems without hard links to "exists check, then `rename`", whose
//! check-to-rename window is the one documented race of the module. A plain `rename` over an
//! existing destination never happens, except for a case-only rename of the same inode.
//!
//! # Trash layout
//! `<volume root>/.apm-trash/<batch>/<original relative path>` on the SAME volume, so trashing is a
//! rename and never copies. The original path is encoded in the path (restore works without the
//! database); when that path is taken the leaf gets a `_N` name and `trash_items.orig_path` keeps
//! the real original. `trash_items.meta_json` snapshots the index row (and the tag names) so a
//! restore needs no re-extraction.
//!
//! # Reconcile decision table
//! Rows are processed in ascending id; rows of offline volumes and rows of operations that are
//! still running in this process are left alone.
//!
//! | op | state on disk | action |
//! |---|---|---|
//! | move (same device) | source present, destination absent | `abandoned` (nothing happened) |
//! | | source absent, destination present | finish the index change (and keep-in-trash item) |
//! | | both present, same inode | unlink the source, finish |
//! | | both present, different files | `abandoned`, NOTHING deleted, warning |
//! | copy / cross-device move | leftover part file | deleted |
//! | | destination present and verified against the current source | move: retire the source; finish |
//! | | verification fails | keep both, `abandoned` |
//! | | source absent, destination present | finish |
//! | | destination absent | `abandoned` |
//! | trash | source present, trash path absent | `abandoned` |
//! | | source absent, trash path present | insert the `trash_items` row, drop the `files` rows |
//! | restore | trash path present, destination absent | `abandoned` |
//! | | trash path absent, destination present | recreate the row, drop the `trash_items` row |
//! | | both present | `abandoned` |
//! | rmdir | folder present | `abandoned` |
//! | | folder absent | delete the `dirs` rows |
//! | delete (purge) | path gone | finish (the `trash_items` row goes) |
//! | | path present | `abandoned` (purge again) |
//!
//! Reconcile also deletes finished journal rows after seven days (abandoned ones after thirty) and
//! sweeps stray `.apm-<hex>.apm-part` files older than an hour in the folders the journal names.
//! The [`ReconcileReport`] lists the folders whose index needs a rescan.
//!
//! # Seams for other modules
//! * [`OpHook`] runs INSIDE the transaction that finalises an operation (the Immich outbox
//!   enqueues there); register with [`FsOps::with_hook`]. Hooks must be idempotent.
//! * [`SelfWrites`] (from [`FsOps::self_writes`]) remembers the folders this module changed, so
//!   the filesystem watcher skips the app's own changes.
//! * `OpContext::changes` is told, after each commit, which folders changed
//!   ([`TaskChangeSink`] turns that into the coalesced `fs_change` event).
//! * `ReconcileReport::rescan` lists folders to hand to the indexer.
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
pub use dirs::RmdirOutcome;
pub use error::FsOpError;
pub use executor::{MoveOutcome, MoveRequest, SkipReason};
pub use failpoints::{FailAction, Failpoints, Step};
pub use groups::{GroupOutcome, MoveGroup, SidecarAction, SidecarItem, SidecarOutcome};
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
