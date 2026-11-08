//! The indexer: keeps the `dirs` and `files` tables in step with the volumes on disk and then
//! enriches the rows. Browse, search, plans and the file operations all read what it writes.
//!
//! `mod.rs` (the [`Indexer`] orchestrator), `scanner` and the scanner's helper modules are owned by
//! todo `implementing-scanner`; every other submodule names its own owner in its docs.
//!
//! # Stages
//! [`Indexer::run`] runs the stages in order over one [`ScanRequest`]:
//! 1. **Scan** ([`Indexer::scan`], `scanner.rs`): walks the requested directories, diffs them with
//!    the index, writes new, changed and removed rows and the sidecar links. Rows it creates or
//!    changes are left `pending` for the stages after it.
//! 2. **Extract** (`extract.rs`): reads the metadata of the files with `meta_state = pending`.
//! 3. **Hash** (`hash.rs`): hashes the files with `hash_state = pending`.
//! 4. **Thumbnails**: pre-generates previews (a later todo).
//!
//! A stage is an `impl Indexer` block in its own file plus one line in [`Indexer::run`]. It reads
//! its `pending` rows, reports through the [`RunContext`] and checks `ctx.cancel` between units of
//! work.
//!
//! A stage reads those rows through the shared stage queue ([`pending::PendingQueue`], `pending.rs`,
//! owned by todo `building-stage-queue`): keyset pages of the pending files of the run's scope
//! ([`Indexer::stage_scope`], planned like the scan's own), in the order `(volume_id, dir_path,
//! name)`, and the totals for its progress. A stage writes no scope or paging SQL of its own.
//!
//! # The volume registry
//! [`VolumeRegistry`] (`AppState.volumes`) is the single place where any module obtains the
//! [`VolumeRoot`](crate::security::VolumeRoot) of a volume: `registry.get(id)` fails with 404
//! `not_found` for an unknown id and 409 `volume_offline` for a volume that is not mounted. Nothing
//! else builds a root from a path. A run does not refresh the registry; the task that starts it
//! calls [`VolumeRegistry::refresh`] first (that is also what picks up a newly mounted share).
//!
//! # Seams for the other modules
//! * **Change reporting** ([`ChangeSink`]): every writer of `files` / `dirs` reports, AFTER its
//!   transaction committed, the directories whose visible listing changed (see `changes.rs` for the
//!   exact contract). The scanner does it through [`RunContext::changes`]; the file operations
//!   reuse the same trait and the same call.
//! * **Progress** ([`ProgressSink`], [`PhaseProgress`]): cumulative snapshots per [`IndexPhase`],
//!   throttled by the producer to about four per second with a final one. A stage creates one
//!   `PhaseProgress` for its phase and calls `advance`; it never calls the sink directly.
//! * **Sidecar links** ([`links::relink_dir`]): recomputes the links of one directory from its
//!   stored names, inside the caller's transaction. The file operations call it after they added,
//!   renamed or removed files (the scanner derives the links from its listings instead).
//! * **Pipeline states**: `meta_state`, `hash_state` and `thumb_state` are `PipelineState` codes
//!   (0 pending, 1 done, 2 failed, 3 not applicable). The scanner writes `pending` for every
//!   non-sidecar row it inserts or changes (a changed file also loses its `hash`) and
//!   `not applicable` for sidecars. The stages decide for themselves what does not apply to a kind.
//! * **`seen_gen`**: `volumes.scan_gen` is bumped at the start of every scan of the volume. A scan
//!   stamps that generation on the rows it writes (new or changed files, new directories,
//!   directories whose modification time changed); unchanged rows are not written, so their stamp
//!   is older. A row is only ever deleted when its stamp is older than the scan's generation: a row
//!   written meanwhile (by this scan, or by a file operation that stamped the volume's current
//!   generation) survives. Do not read `seen_gen < scan_gen` as "was not seen"; the diff decides
//!   that, and scoped scans bump the generation too.
//!
//! # What a scan decides
//! * **Scope.** A [`ScanTarget`] is a directory of a volume, with or without its sub-directories.
//!   An empty target list means every online volume, completely. Targets of one volume are merged
//!   (one that lies inside another recursive one is dropped). A scan touches and removes only rows
//!   inside its scope, except that a non-recursive scan also removes the sub-directories of its
//!   directory that are gone (with everything below them), which is what the watcher relies on
//!   when a whole folder is deleted.
//! * **Gone files.** The listing of each directory is compared with the rows stored for it, and the
//!   stored sub-directories with the listed ones. What is gone is removed only once the walk of the
//!   volume completed without being cancelled, in transactions of a thousand rows, so a cancelled
//!   scan leaves a consistent index (every committed transaction holds whole directories) and
//!   removes nothing. A volume that is offline, or whose root cannot be read, is skipped and never
//!   purged.
//! * **The mass-disappearance guard.** When a scan would remove at least 50 file rows and more than
//!   a fifth of the rows its volume had, ALL removals of the volume are held back: the rest of the
//!   scan is applied, the count is stored in `volumes.held_removals` and returned by
//!   [`Indexer::held_removals`] and [`ScanSummary::held`]. An unmounted or momentarily empty share
//!   looks exactly like this. Only a scan with `confirm_removals` applies held removals (`force`
//!   alone does not). A completed scan of the whole volume replaces the stored count by what it
//!   holds, so it clears it when nothing is held any more.
//! * **Transactions.** Writes go through [`Db::begin_write`] in groups of whole directories of
//!   about a thousand rows; nothing touches the file system while a transaction is open. Unchanged
//!   files cost no write at all.
//!
//! Symbolic links, junk (see `apm_core::junk`), `.apm-trash`, `*.apm-part` files and names that are
//! not valid UTF-8 are never indexed. Names are stored exactly as the disk reports them.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use apm_core::datetime::tz_or_utc;
use apm_core::paths::{RelPath, VolumeId};
use chrono_tz::Tz;
use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, DbError};
use crate::error::AppError;
use crate::settings::SettingsService;

mod changes;
mod diff;
pub mod extract;
pub mod hash;
pub mod links;
pub mod pending;
mod progress;
pub mod scanner;
mod store;
#[cfg(test)]
mod test_support;
mod volumes;
mod walk;
pub mod watcher;
pub mod xmp;

pub use changes::{ChangeCall, ChangeSink, NoopChanges, RecordingChanges};
pub use extract::{
    Embedded, ExiftoolExtractor, ExtractFailure, ExtractOutcome, ExtractRequest, ExtractSummary,
    FakeExtractor, MetadataExtractor,
};
pub use progress::{
    IndexPhase, IndexProgress, NoopProgress, PhaseProgress, ProgressSink, REPORT_INTERVAL,
    RecordingProgress,
};
pub use scanner::ScanSummary;
pub use volumes::VolumeRegistry;

/// A part of a volume to index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanTarget {
    pub volume_id: VolumeId,
    /// The directory to start at; the root of the volume for the whole volume.
    pub dir: RelPath,
    /// Include everything below `dir`. When false only the files of `dir` are scanned, and its
    /// sub-directories are recorded (and the vanished ones removed) without looking inside them.
    pub recursive: bool,
}

impl ScanTarget {
    /// The whole volume.
    pub fn volume(volume_id: VolumeId) -> Self {
        Self {
            volume_id,
            dir: RelPath::root(),
            recursive: true,
        }
    }

    /// `dir` and everything below it.
    pub fn subtree(volume_id: VolumeId, dir: RelPath) -> Self {
        Self {
            volume_id,
            dir,
            recursive: true,
        }
    }

    /// The files of `dir` and the existence of its direct sub-directories.
    pub fn dir(volume_id: VolumeId, dir: RelPath) -> Self {
        Self {
            volume_id,
            dir,
            recursive: false,
        }
    }
}

/// What to index.
#[derive(Debug, Clone, Default)]
pub struct ScanRequest {
    /// The parts to scan. Empty means every online volume, completely.
    pub targets: Vec<ScanTarget>,
    /// Treat every file in scope as changed, so the later stages redo their work. It does not
    /// apply removals the guard holds back.
    pub force: bool,
    /// The user's explicit OK to apply the removals the guard would hold back.
    pub confirm_removals: bool,
}

/// What a run needs besides its request.
#[derive(Clone)]
pub struct RunContext {
    /// Cancelling stops the run at the next directory or write batch; what was committed stays.
    pub cancel: CancellationToken,
    pub progress: Arc<dyn ProgressSink>,
    pub changes: Arc<dyn ChangeSink>,
}

impl RunContext {
    pub fn new(
        cancel: CancellationToken,
        progress: Arc<dyn ProgressSink>,
        changes: Arc<dyn ChangeSink>,
    ) -> Self {
        Self {
            cancel,
            progress,
            changes,
        }
    }

    /// A context that is never cancelled and discards progress and changes.
    pub fn noop() -> Self {
        Self::new(
            CancellationToken::new(),
            Arc::new(NoopProgress),
            Arc::new(NoopChanges),
        )
    }
}

impl fmt::Debug for RunContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunContext")
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Removals the mass-disappearance guard is holding for one volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldRemovals {
    pub volume_id: VolumeId,
    pub volume_name: String,
    /// Index entries whose file could not be found on disk and that were kept.
    pub files: u64,
}

/// Why a run failed.
#[derive(Debug, Error)]
pub enum IndexError {
    /// The run was cancelled; everything committed so far is consistent.
    #[error("the run was cancelled")]
    Cancelled,
    #[error("volume {0} does not exist")]
    UnknownVolume(VolumeId),
    #[error(transparent)]
    Db(#[from] DbError),
    /// A worker thread panicked or was lost.
    #[error("a worker failed: {0}")]
    Worker(String),
}

impl From<sqlx::Error> for IndexError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(err))
    }
}

impl From<IndexError> for AppError {
    fn from(err: IndexError) -> Self {
        match err {
            IndexError::Cancelled => AppError::conflict("cancelled", "the run was cancelled"),
            IndexError::UnknownVolume(id) => {
                AppError::not_found("not_found", format!("no volume with id {id}"))
            }
            IndexError::Db(err) => err.into(),
            IndexError::Worker(message) => AppError::internal(message),
        }
    }
}

/// What a run did, as counts (the result of the index task). Stages fill their own fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IndexSummary {
    /// Files found on disk that the index did not have.
    pub added: u64,
    /// Files whose size, modification time or kind differed (all files of a forced scan).
    pub changed: u64,
    /// Index entries removed because their file is gone.
    pub removed: u64,
    /// Index entries the mass-disappearance guard kept for the user to confirm.
    pub held: u64,
    /// Entries skipped because their name is not valid UTF-8.
    pub skipped_invalid: u64,
    pub extracted: u64,
    pub hashed: u64,
    pub thumbnails: u64,
    pub failed: u64,
    /// The metadata extractor (exiftool) is missing or too old; its files stay pending.
    pub extractor_unavailable: bool,
}

impl IndexSummary {
    fn absorb_extract(&mut self, extract: &ExtractSummary) {
        self.extracted += extract.extracted;
        self.failed += extract.failed;
        self.extractor_unavailable |= extract.unavailable;
    }

    fn absorb_scan(&mut self, scan: &ScanSummary) {
        self.added += scan.added;
        self.changed += scan.changed;
        self.removed += scan.removed;
        self.held += scan.held;
        self.skipped_invalid += scan.skipped_invalid;
    }
}

/// Runs the indexing stages. Cheap to clone; all clones share their per-volume locks.
#[derive(Clone)]
pub struct Indexer {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    settings: SettingsService,
    volumes: VolumeRegistry,
    /// One lock per volume: two scans of the same volume never overlap.
    locks: Arc<Mutex<HashMap<VolumeId, Arc<tokio::sync::Mutex<()>>>>>,
    /// Reads the embedded metadata (stage 2).
    extractor: Arc<dyn MetadataExtractor>,
    /// The zone that file times and offset-less capture dates are read in.
    timezone: Tz,
}

impl fmt::Debug for Indexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Indexer").finish_non_exhaustive()
    }
}

impl Indexer {
    /// An indexer with the production extractor (`exiftool` from `PATH`, a scratch `HOME`) and the
    /// server's `TZ`. [`AppState`](crate::state::AppState) replaces the home with a directory of
    /// the data dir through [`with_extractor`](Self::with_extractor).
    pub fn new(db: Db, settings: SettingsService, volumes: VolumeRegistry) -> Self {
        let concurrency = usize::from(settings.get().index.concurrency);
        let extractor = ExiftoolExtractor::new(
            "exiftool",
            std::env::temp_dir().join("apm-exiftool-home"),
            concurrency,
        );
        Self {
            inner: Arc::new(Inner {
                db,
                settings,
                volumes,
                locks: Arc::new(Mutex::new(HashMap::new())),
                extractor: Arc::new(extractor),
                timezone: tz_or_utc(std::env::var("TZ").ok().as_deref()),
            }),
        }
    }

    /// The same indexer (sharing its volume locks) reading metadata with `extractor`.
    #[must_use]
    pub fn with_extractor(&self, extractor: Arc<dyn MetadataExtractor>) -> Self {
        self.rebuilt(extractor, self.inner.timezone)
    }

    /// The same indexer (sharing its volume locks) resolving local times in `timezone`.
    #[must_use]
    pub fn with_timezone(&self, timezone: Tz) -> Self {
        self.rebuilt(Arc::clone(&self.inner.extractor), timezone)
    }

    fn rebuilt(&self, extractor: Arc<dyn MetadataExtractor>, timezone: Tz) -> Self {
        let inner = &self.inner;
        Self {
            inner: Arc::new(Inner {
                db: inner.db.clone(),
                settings: inner.settings.clone(),
                volumes: inner.volumes.clone(),
                locks: Arc::clone(&inner.locks),
                extractor,
                timezone,
            }),
        }
    }

    pub(crate) fn extractor(&self) -> &Arc<dyn MetadataExtractor> {
        &self.inner.extractor
    }

    pub(crate) fn timezone(&self) -> Tz {
        self.inner.timezone
    }

    /// Runs every stage over `req`, in order.
    pub async fn run(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
    ) -> Result<IndexSummary, IndexError> {
        let mut summary = IndexSummary::default();

        // THE STAGES, in order. A new stage adds one line, implemented as an `impl Indexer`
        // block in its own file; it reads the rows the previous stages left pending.
        let scan = self.scan(req, ctx).await?; // 1. scanner.rs
        summary.absorb_scan(&scan);
        let extract = self.extract_pending(req, ctx).await?; // 2. extract.rs
        summary.absorb_extract(&extract);
        let hashed = self
            .hash_pending(req, ctx, hash::HashLimits::default())
            .await?; // 3. hash.rs
        summary.hashed += hashed.hashed;
        summary.failed += hashed.failed;
        if ctx.cancel.is_cancelled() {
            return Err(IndexError::Cancelled);
        }
        // 4. thumbnails (a later todo)

        Ok(summary)
    }

    pub(crate) fn db(&self) -> &Db {
        &self.inner.db
    }

    pub(crate) fn settings(&self) -> &SettingsService {
        &self.inner.settings
    }

    pub(crate) fn volumes(&self) -> &VolumeRegistry {
        &self.inner.volumes
    }

    /// The lock that serialises the scans of `volume_id`.
    pub(crate) fn volume_lock(&self, volume_id: VolumeId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .inner
            .locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(volume_id).or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    async fn indexer() -> (crate::db::TempDb, Indexer) {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let config = Config::for_tests(temp.dir());
        let settings = SettingsService::load(db.clone(), &config).await.unwrap();
        let volumes = VolumeRegistry::load(db.clone(), config.volumes_dir.clone())
            .await
            .unwrap();
        (temp, Indexer::new(db, settings, volumes))
    }

    #[tokio::test]
    async fn a_volume_has_one_lock_and_volumes_have_their_own() {
        let (_temp, indexer) = indexer().await;

        let first = indexer.volume_lock(1);
        assert!(Arc::ptr_eq(&first, &indexer.volume_lock(1)));
        let second = indexer.volume_lock(2);
        assert!(!Arc::ptr_eq(&first, &second));

        let _held = first.lock().await;
        assert!(
            indexer.volume_lock(1).try_lock().is_err(),
            "the same volume waits"
        );
        assert!(second.try_lock().is_ok(), "another volume does not");
    }

    #[tokio::test]
    async fn the_clones_of_an_indexer_share_their_locks() {
        let (_temp, indexer) = indexer().await;
        let clone = indexer.clone();
        assert!(Arc::ptr_eq(&indexer.volume_lock(7), &clone.volume_lock(7)));
    }

    #[test]
    fn a_summary_adds_up_what_the_scan_found() {
        let scan = ScanSummary {
            added: 3,
            changed: 2,
            removed: 1,
            held: 4,
            skipped_invalid: 5,
            ..ScanSummary::default()
        };
        let mut summary = IndexSummary::default();
        summary.absorb_scan(&scan);
        assert_eq!(
            (
                summary.added,
                summary.changed,
                summary.removed,
                summary.held,
                summary.skipped_invalid
            ),
            (3, 2, 1, 4, 5)
        );
    }
}
