//! Content hashing: the shared file hasher ([`sha1_file`]) and the hash stage of the indexer
//! ([`Indexer::hash_pending`]). Owned by todo `hashing-files`.
//!
//! # The hasher
//! [`sha1_file`] is THE way to hash a file of a volume (the indexer, the hash-verified cross-volume
//! copy of the file operations and the Immich sync all use it). It opens the file through
//! [`VolumeRoot::open_read`] (symbolic links are never followed) and streams it through
//! `apm_core::hash::sha1_reader`, so there is exactly one SHA-1 loop in the code base. The digest is
//! the plain SHA-1 of the whole content, stored raw in `files.hash`; Immich's `checksum` is the very
//! same 20 bytes in base64. Cancelling stops a multi-GB read within one chunk. It is blocking: call
//! it from `spawn_blocking`.
//!
//! # The stage
//! [`Indexer::hash_pending`] hashes the files whose `hash_state` is `pending` in the scope of a
//! request, reading them through the stage queue (`indexer::pending`, never its own SQL):
//! * **Order.** Phase A first: files whose size is shared with another non-sidecar file anywhere in
//!   the library (only those can be exact duplicates, so duplicates become findable early). Phase B
//!   next: the unique sizes, so the whole library ends up hashed (Immich matching, and files that
//!   arrive later with a colliding size). The collision test is an `EXISTS` on `files o` probing the
//!   `files_size` index, spliced into the queue as a condition (`SIZE_TWIN`, and `NOT` of it for
//!   phase B). Both phases page by `(volume_id, dir_path, name)`, 500 rows a page.
//! * **Concurrency.** `index.hash_concurrency` files at a time, read when the stage starts, each in
//!   `spawn_blocking`.
//! * **Safety.** A file is stat-ed before and after the read; a different size or modification
//!   time, or a different number of bytes read than the first stat said, discards the digest and
//!   leaves the row pending. The write is `UPDATE ... WHERE id = ? AND size = ? AND mtime = ?`, so a
//!   row the scanner changed meanwhile never receives a stale hash. Writes are batched (200 results
//!   or about a second per transaction).
//! * **Failures.** An unreadable file (permission, I/O error) becomes `hash_state = failed`, with a
//!   log line. A file that vanished, became a link or changed is skipped and stays pending for the
//!   next scan; so does one of an offline volume. A skipped row counts in `remaining`.
//! * **Progress.** One [`PhaseProgress`] of [`IndexPhase::Hash`] for both phases: items and bytes
//!   totals from the queue, bytes advanced per chunk.
//! * **Limits and cancellation.** [`HashLimits`] stops a run after a number of files or a time (so
//!   a controller can slice a very long job); cancelling stops between files and inside a file.
//!   Either way everything committed is consistent and a later run continues with what is pending.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use apm_core::hash::{Sha1Digest, sha1_bytes, sha1_reader};
use apm_core::paths::{FileId, RelPath, VolumeId};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::pending::{PendingFile, PendingKey, PendingQueue, PipelineColumn};
use super::{IndexError, IndexPhase, Indexer, PhaseProgress, RunContext, ScanRequest};
use crate::db::Db;
use crate::db::models::PipelineState;
use crate::security::{FsError, VolumeRoot};

/// Rows per page of the stage queue.
const PAGE: usize = 500;
/// Results written per transaction at most, and the longest a result waits for its write.
const BATCH: usize = 200;
const BATCH_INTERVAL: Duration = Duration::from_secs(1);

/// A pending file whose size another non-sidecar file shares. Probes `files_size`.
const SIZE_TWIN: &str =
    "EXISTS (SELECT 1 FROM files o WHERE o.size = f.size AND o.id <> f.id AND o.kind <> 'sidecar')";
/// The complement of [`SIZE_TWIN`]: a size no other non-sidecar file has.
const UNIQUE_SIZE: &str = "NOT EXISTS (SELECT 1 FROM files o WHERE o.size = f.size AND o.id <> f.id AND o.kind <> 'sidecar')";

/// Why [`sha1_file`] gave no digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HashError {
    /// The cancellation token fired.
    #[error("hashing was cancelled")]
    Cancelled,
    /// The path is missing, is or lies below a symbolic link, or is not a regular file.
    #[error(transparent)]
    Gone(FsError),
    /// The file exists but reading it failed (permission denied, I/O error, ...).
    #[error("i/o error: {0}")]
    Io(ErrorKind),
}

impl From<FsError> for HashError {
    fn from(err: FsError) -> Self {
        match err {
            FsError::Io(kind) => Self::Io(kind),
            other => Self::Gone(other),
        }
    }
}

/// The SHA-1 of the whole content of `rel` below `root` (see the module docs). `on_bytes` is called
/// with the size of every chunk read, for progress. A zero-byte file is not read at all.
///
/// Blocking: call it from `spawn_blocking`. `rel` must come from the index or the application,
/// never straight from a client.
pub fn sha1_file(
    root: &VolumeRoot,
    rel: &RelPath,
    cancel: &CancellationToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Sha1Digest, HashError> {
    if cancel.is_cancelled() {
        return Err(HashError::Cancelled);
    }
    let file = root.open_read(rel)?;
    let len = file
        .metadata()
        .map_err(|err| HashError::Io(err.kind()))?
        .len();
    if len == 0 {
        return Ok(sha1_bytes(&[]));
    }
    let digest = sha1_reader(file, |read| {
        on_bytes(read);
        if cancel.is_cancelled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .map_err(|err| HashError::Io(err.kind()))?;
    digest.ok_or(HashError::Cancelled)
}

/// Bounds of one [`Indexer::hash_pending`] call. `None` is unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HashLimits {
    /// Stop after starting this many files.
    pub max_files: Option<u64>,
    /// Stop starting files after this long.
    pub max_duration: Option<Duration>,
}

/// What [`Indexer::hash_pending`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HashSummary {
    /// Rows that received their hash.
    pub hashed: u64,
    /// Rows marked `failed`.
    pub failed: u64,
    /// Files that changed under the read, or whose row changed before the write: left pending.
    pub skipped_changed: u64,
    /// Pending rows of the scope after the run (skipped and vanished files included).
    pub remaining: u64,
}

/// Counts the files being read at once, for tests.
#[derive(Debug, Default)]
struct Probe {
    current: AtomicUsize,
    max: AtomicUsize,
}

impl Probe {
    fn enter(&self) {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.current.fetch_sub(1, Ordering::SeqCst);
    }
}

enum Outcome {
    Hashed(Sha1Digest),
    Failed(ErrorKind),
    Changed,
    Gone,
    Cancelled,
}

struct Finished {
    file: PendingFile,
    outcome: Outcome,
}

struct Write {
    id: FileId,
    size: i64,
    mtime: i64,
    /// `None` marks the row failed.
    digest: Option<Sha1Digest>,
}

impl Indexer {
    /// Hashes the pending files of `req`'s scope; see the module docs. A cancelled run returns what
    /// it did, so the caller decides whether that is an error.
    pub async fn hash_pending(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
        limits: HashLimits,
    ) -> Result<HashSummary, IndexError> {
        self.hash_pending_probed(req, ctx, limits, None).await
    }

    async fn hash_pending_probed(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
        limits: HashLimits,
        probe: Option<Arc<Probe>>,
    ) -> Result<HashSummary, IndexError> {
        let db = self.db();
        let base = PendingQueue::new(PipelineColumn::Hash, self.stage_scope(req));
        let twins = base.clone().with_condition(SIZE_TWIN);
        let uniques = base.with_condition(UNIQUE_SIZE);
        let (twin_totals, unique_totals) = (twins.totals(db).await?, uniques.totals(db).await?);

        let progress = Arc::new(PhaseProgress::new(ctx.progress.clone(), IndexPhase::Hash));
        progress.set_totals(
            Some(twin_totals.files + unique_totals.files),
            Some(twin_totals.bytes + unique_totals.bytes),
        );
        let mut stage = Stage {
            indexer: self.clone(),
            cancel: ctx.cancel.clone(),
            progress: Arc::clone(&progress),
            probe,
            concurrency: usize::from(self.settings().get().index.hash_concurrency).max(1),
            limits,
            started: Instant::now(),
            launched: 0,
            stopped: false,
            roots: HashMap::new(),
            writes: Vec::new(),
            last_flush: Instant::now(),
            summary: HashSummary::default(),
        };
        let walked = async {
            for queue in [&twins, &uniques] {
                stage.pass(queue).await?;
                if stage.stopped {
                    break;
                }
            }
            Ok::<(), IndexError>(())
        }
        .await;
        // What finished is written even when the walk failed or was cancelled.
        let flushed = stage.flush().await;
        progress.finish();
        walked?;
        flushed?;

        let mut summary = stage.summary;
        summary.remaining = twins.totals(db).await?.files + uniques.totals(db).await?.files;
        Ok(summary)
    }
}

struct Stage {
    indexer: Indexer,
    cancel: CancellationToken,
    progress: Arc<PhaseProgress>,
    probe: Option<Arc<Probe>>,
    concurrency: usize,
    limits: HashLimits,
    started: Instant,
    launched: u64,
    stopped: bool,
    /// `None`: the volume is offline, its files are skipped.
    roots: HashMap<VolumeId, Option<Arc<VolumeRoot>>>,
    writes: Vec<Write>,
    last_flush: Instant,
    summary: HashSummary,
}

impl Stage {
    fn db(&self) -> &Db {
        self.indexer.db()
    }

    fn root(&mut self, volume_id: VolumeId) -> Option<Arc<VolumeRoot>> {
        let volumes = self.indexer.volumes();
        self.roots
            .entry(volume_id)
            .or_insert_with(|| volumes.get(volume_id).ok())
            .clone()
    }

    fn should_stop(&mut self) -> bool {
        if !self.stopped {
            self.stopped = self.cancel.is_cancelled()
                || self
                    .limits
                    .max_files
                    .is_some_and(|max| self.launched >= max)
                || self
                    .limits
                    .max_duration
                    .is_some_and(|max| self.started.elapsed() >= max);
        }
        self.stopped
    }

    /// Walks one queue page by page until it is exhausted or the run must stop.
    async fn pass(&mut self, queue: &PendingQueue) -> Result<(), IndexError> {
        let mut after: Option<PendingKey> = None;
        let mut running: JoinSet<Finished> = JoinSet::new();
        loop {
            let page = queue.page(self.db(), after.as_ref(), PAGE).await?;
            let Some(last) = page.last() else { break };
            after = Some(PendingKey::from(last));
            for file in page {
                if self.should_stop() {
                    break;
                }
                let Some(root) = self.root(file.volume_id) else {
                    continue;
                };
                while running.len() >= self.concurrency {
                    self.reap(&mut running).await?;
                }
                self.launched += 1;
                let cancel = self.cancel.clone();
                let progress = Arc::clone(&self.progress);
                let probe = self.probe.clone();
                running.spawn_blocking(move || {
                    if let Some(probe) = &probe {
                        probe.enter();
                    }
                    let outcome = hash_row(&root, &file, &cancel, &progress);
                    if let Some(probe) = &probe {
                        probe.leave();
                    }
                    Finished { file, outcome }
                });
            }
            while !running.is_empty() {
                self.reap(&mut running).await?;
            }
            if self.stopped {
                break;
            }
        }
        Ok(())
    }

    async fn reap(&mut self, running: &mut JoinSet<Finished>) -> Result<(), IndexError> {
        let Some(joined) = running.join_next().await else {
            return Ok(());
        };
        let Finished { file, outcome } =
            joined.map_err(|err| IndexError::Worker(format!("hash worker: {err}")))?;
        let write = |digest| Write {
            id: file.id,
            size: file.size,
            mtime: file.mtime,
            digest,
        };
        match outcome {
            Outcome::Hashed(digest) => self.writes.push(write(Some(digest))),
            Outcome::Failed(kind) => {
                tracing::warn!(
                    volume_id = file.volume_id,
                    dir = %file.dir_path,
                    name = %file.name,
                    error = %kind,
                    "could not hash a file"
                );
                self.writes.push(write(None));
            }
            Outcome::Changed => self.summary.skipped_changed += 1,
            Outcome::Gone | Outcome::Cancelled => {}
        }
        // A file cut short by a cancel is not finished work.
        if !self.cancel.is_cancelled() {
            self.progress.advance(1, 0, None);
        }
        if self.writes.len() >= BATCH || self.last_flush.elapsed() >= BATCH_INTERVAL {
            self.flush().await?;
        }
        Ok(())
    }

    /// Commits the buffered results in one transaction. A row whose size or modification time no
    /// longer matches is not touched and counts as skipped.
    async fn flush(&mut self) -> Result<(), IndexError> {
        self.last_flush = Instant::now();
        if self.writes.is_empty() {
            return Ok(());
        }
        let writes = std::mem::take(&mut self.writes);
        let mut tx = self.db().begin_write().await?;
        let (mut hashed, mut failed, mut stale) = (0_u64, 0_u64, 0_u64);
        for write in &writes {
            let (state, digest) = match &write.digest {
                Some(digest) => (PipelineState::Done, Some(digest.as_slice())),
                None => (PipelineState::Failed, None),
            };
            let result = sqlx::query(
                "UPDATE files SET hash = ?, hash_state = ? WHERE id = ? AND size = ? AND mtime = ?",
            )
            .bind(digest)
            .bind(state)
            .bind(write.id)
            .bind(write.size)
            .bind(write.mtime)
            .execute(&mut *tx)
            .await?;
            match (result.rows_affected(), write.digest.is_some()) {
                (0, _) => stale += 1,
                (_, true) => hashed += 1,
                (_, false) => failed += 1,
            }
        }
        tx.commit().await?;
        self.summary.hashed += hashed;
        self.summary.failed += failed;
        self.summary.skipped_changed += stale;
        Ok(())
    }
}

/// Hashes one row's file, checking that it is still the file the row describes.
fn hash_row(
    root: &VolumeRoot,
    file: &PendingFile,
    cancel: &CancellationToken,
    progress: &PhaseProgress,
) -> Outcome {
    let Ok(rel) = RelPath::new(&file.dir_path).and_then(|dir| dir.join(&file.name)) else {
        return Outcome::Gone;
    };
    let before = match root.stat(&rel) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) | Err(FsError::NotFound(_) | FsError::SymlinkRejected(_) | FsError::NotAFile(_)) => {
            return Outcome::Gone;
        }
        Err(err) => return Outcome::Failed(err.kind()),
    };
    if !matches_row(&before, file) {
        return Outcome::Changed;
    }
    let mut read = 0_u64;
    let digest = sha1_file(root, &rel, cancel, &mut |bytes| {
        read += bytes;
        progress.advance(0, bytes, None);
    });
    let digest = match digest {
        Ok(digest) => digest,
        Err(HashError::Cancelled) => return Outcome::Cancelled,
        Err(HashError::Gone(_)) => return Outcome::Gone,
        Err(HashError::Io(kind)) => return Outcome::Failed(kind),
    };
    match root.stat(&rel) {
        Ok(after) if matches_row(&after, file) && read == before.len() => Outcome::Hashed(digest),
        Ok(_) => Outcome::Changed,
        Err(_) => Outcome::Gone,
    }
}

fn matches_row(meta: &std::fs::Metadata, file: &PendingFile) -> bool {
    i64::try_from(meta.len()).is_ok_and(|len| len == file.size) && meta.mtime() == file.mtime
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::indexer::VolumeRegistry;
    use crate::indexer::test_support::touch;
    use crate::settings::SettingsService;

    #[tokio::test]
    async fn never_more_files_are_open_than_the_setting_allows() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let config = Config::for_tests(temp.dir());
        for index in 0..10_usize {
            touch(
                &config.volumes_dir.join("v"),
                &format!("f{index}.bin"),
                &vec![7; 3_000_000 + index],
            );
        }
        let settings = SettingsService::load(db.clone(), &config).await.unwrap();
        let volumes = VolumeRegistry::load(db.clone(), config.volumes_dir.clone())
            .await
            .unwrap();
        volumes.refresh().await.unwrap();
        let indexer = Indexer::new(db, settings.clone(), volumes);
        let req = ScanRequest::default();
        indexer.scan(&req, &RunContext::noop()).await.unwrap();

        for allowed in [1_u8, 3] {
            let mut dto = (*settings.get()).clone();
            dto.index.hash_concurrency = allowed;
            settings.update(dto).await.unwrap();
            sqlx::query("UPDATE files SET hash = NULL, hash_state = 0")
                .execute(indexer.db().write())
                .await
                .unwrap();
            let probe = Arc::new(Probe::default());
            let summary = indexer
                .hash_pending_probed(
                    &req,
                    &RunContext::noop(),
                    HashLimits::default(),
                    Some(Arc::clone(&probe)),
                )
                .await
                .unwrap();
            assert_eq!(summary.hashed, 10);
            let max = probe.max.load(Ordering::SeqCst);
            assert!(
                (1..=usize::from(allowed)).contains(&max),
                "{max} files open at once with a limit of {allowed}"
            );
        }
    }
}
