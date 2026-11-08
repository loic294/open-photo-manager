//! The copy-and-verify path: a copy ([`FsOps::copy_file`]), a move across file systems, and a rename
//! that turns out to cross one all go through it.
//!
//! NEVER unlink or overwrite a source before the destination is durable and verified. The order is
//! fixed:
//! 1. the journal row is `pending` and names the part file;
//! 2. the bytes are streamed into `.apm-<hex>.apm-part` in the DESTINATION folder (so the final
//!    rename never leaves the file system) while a SHA-1 is computed on the way;
//! 3. the part is fsynced, then read back through the same descriptor and its size and SHA-1 are
//!    compared with what was read from the source;
//! 4. the part is renamed to its final name. The rename never replaces: "replace" sends the old
//!    file to the trash right before it;
//! 5. the destination folder is fsynced;
//! 6. only now the source is unlinked (or kept in the trash), after checking it is still the file
//!    that was copied;
//! 7. one transaction updates the index and closes the journal row.
//!
//! A crash before step 4 leaves a stray part file (which [`FsOps::reconcile`] deletes) and the
//! source; a crash after it leaves both files (which reconcile verifies before it retires the
//! source). Step 6 is the only step that can destroy data, and it is guarded by everything above.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use apm_core::consts::PART_SUFFIX;
use apm_core::hash::{CHUNK_SIZE, HashingReader, Sha1Digest, sha1_reader};
use apm_core::paths::RelPath;
use tokio_util::sync::CancellationToken;

use super::apply::{KeptSource, Mode, TransferApply, touched_dirs};
use super::backend::{FreeSpace, Mover};
use super::context::TrashBatch;
use super::dirs::make_dirs;
use super::error::FsOpError;
use super::executor::{ATTEMPTS, Flow, Resolution, Run, SkipReason, Transferred};
use super::failpoints::{FailAction, Failpoints, Step};
use super::stat::{Expect, FileStat};
use super::trash::{TrashSpec, trash_rel};
use super::{FsOps, blocking};
use crate::security::{FsError, VolumeRoot};

/// Room that must stay free on the destination beyond the file itself: a disk filled to the last
/// byte makes everything else on a NAS fail.
const SPACE_MARGIN: u64 = 16 << 20;

/// What the blocking copy needs; owned, so it can move to the blocking pool.
pub(crate) struct CopyJob {
    pub src_root: Arc<VolumeRoot>,
    pub src: RelPath,
    pub dst_root: Arc<VolumeRoot>,
    /// Where the part file goes (in the destination folder).
    pub part: RelPath,
    /// The source as the operation saw it; a source that differs now is `source_changed`.
    pub expected: FileStat,
    pub cancel: CancellationToken,
    pub on_bytes: Arc<dyn Fn(u64) + Send + Sync>,
    pub free_space: Arc<dyn FreeSpace>,
    pub failpoints: Option<Failpoints>,
}

/// A finished, verified part file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopiedPart {
    /// SHA-1 of the content, computed while reading the source and confirmed by the read-back.
    pub hash: Sha1Digest,
    pub size: u64,
    /// The part file as it is on disk; its inode is the destination's once it is renamed.
    pub stat: FileStat,
}

/// `.apm-<16 lowercase hex digits>.apm-part`: a name the scanner ignores (`*.apm-part` is junk)
/// and that user files practically never have.
pub(crate) fn part_name() -> String {
    format!(".apm-{:016x}{PART_SUFFIX}", rand::random::<u64>())
}

/// Whether `name` is exactly a name [`part_name`] makes. Recovery only ever deletes files that
/// match, so a user file is never mistaken for a leftover.
pub(crate) fn is_part_name(name: &str) -> bool {
    name.strip_prefix(".apm-")
        .and_then(|rest| rest.strip_suffix(PART_SUFFIX))
        .is_some_and(|hex| {
            hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Deletes a part file this module created. Refuses a name that is not a part name, and never
/// fails: a part that cannot be deleted is logged and swept by the next recovery.
pub(crate) fn discard_part(root: &VolumeRoot, part: &RelPath) {
    if !part.file_name().is_some_and(is_part_name) {
        tracing::error!(path = %part.as_str(), "refusing to delete a file that is not a part file");
        return;
    }
    match root.resolve_existing(part) {
        Ok(path) => match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(path = %part.as_str(), error = %err, "could not delete a part file");
            }
        },
        Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => {}
        Err(err) => {
            tracing::warn!(path = %part.as_str(), error = %err, "could not resolve a part file");
        }
    }
}

fn fire(job: &CopyJob, step: Step) -> FailAction {
    job.failpoints
        .as_ref()
        .map_or(FailAction::Continue, |points| points.fire(step))
}

fn point(job: &CopyJob, step: Step) -> Result<(), FsOpError> {
    job.failpoints
        .as_ref()
        .map_or(Ok(()), |points| points.check(step))
}

/// The room check: refuses a destination that could not hold the file plus a safety margin. A file
/// system that cannot say how much is free is not a reason to refuse.
fn check_space(job: &CopyJob, size: u64) -> Result<(), FsOpError> {
    if size == 0 {
        return Ok(());
    }
    let dir = job.part.parent().unwrap_or_else(RelPath::root);
    let path = job
        .dst_root
        .resolve_dir(&dir)
        .map_err(FsOpError::from_destination)?;
    match job.free_space.available(&path) {
        Ok(available) if available < size.saturating_add(SPACE_MARGIN) => Err(FsOpError::NoSpace),
        _ => Ok(()),
    }
}

/// Copies the source into the part file and verifies it. The part is deleted again on every
/// failure except a simulated crash ([`FsOpError::Aborted`]); a part this function did not create
/// is never touched.
pub(crate) fn copy_and_verify(job: &CopyJob) -> Result<CopiedPart, FsOpError> {
    let mut source = job
        .src_root
        .open_read(&job.src)
        .map_err(FsOpError::from_source)?;
    let before = FileStat::from_metadata(&source.metadata()?);
    if !before.same_file(&job.expected) {
        return Err(FsOpError::SourceChanged);
    }
    check_space(job, before.size)?;
    let part_path = job
        .dst_root
        .resolve_for_create(&job.part)
        .map_err(FsOpError::from_destination)?;
    let mut part = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&part_path)?;
    match fill_and_verify(job, &mut source, &mut part, &before) {
        Ok(copied) => Ok(copied),
        Err(err) => {
            // A simulated crash leaves the part where it is, like a killed process would.
            if !err.is_abort() {
                drop(part);
                discard_part(&job.dst_root, &job.part);
            }
            Err(err)
        }
    }
}

fn fill_and_verify(
    job: &CopyJob,
    source: &mut File,
    part: &mut File,
    before: &FileStat,
) -> Result<CopiedPart, FsOpError> {
    point(job, Step::AfterPartCreate)?;
    let (hash, bytes) = stream(job, source, part)?;
    let after = FileStat::from_metadata(&source.metadata()?);
    if bytes != before.size || !after.same_file(before) {
        return Err(FsOpError::SourceChanged);
    }
    match fire(job, Step::AfterCopy) {
        FailAction::Continue => {}
        FailAction::Error => return Err(FsOpError::Io(io::ErrorKind::Other)),
        FailAction::Abort => return Err(FsOpError::Aborted),
        FailAction::Corrupt => corrupt(part, bytes)?,
    }
    copy_attributes(part, before);
    part.sync_all()?;
    forget_cached_pages(part);
    point(job, Step::AfterFsync)?;
    verify(job, part, bytes, &hash)?;
    point(job, Step::AfterVerify)?;
    Ok(CopiedPart {
        hash,
        size: bytes,
        stat: FileStat::from_metadata(&part.metadata()?),
    })
}

/// Streams the source into the part in [`CHUNK_SIZE`] chunks, hashing on the way. Checks for
/// cancellation before every chunk.
fn stream(
    job: &CopyJob,
    source: &mut File,
    part: &mut File,
) -> Result<(Sha1Digest, u64), FsOpError> {
    let mut reader = HashingReader::new(source);
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    loop {
        if job.cancel.is_cancelled() {
            return Err(FsOpError::Cancelled);
        }
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FsOpError::from(err)),
        };
        part.write_all(buffer.get(..read).unwrap_or_default())?;
        (job.on_bytes)(u64::try_from(read).unwrap_or(0));
    }
    let bytes = reader.bytes_read();
    Ok((reader.digest(), bytes))
}

/// Test seam for [`FailAction::Corrupt`]: flips the first byte (an empty file gets one).
fn corrupt(part: &File, size: u64) -> io::Result<()> {
    if size == 0 {
        return part.write_all_at(&[0xff], 0);
    }
    let mut byte = [0_u8; 1];
    part.read_exact_at(&mut byte, 0)?;
    byte[0] ^= 0xff;
    part.write_all_at(&byte, 0)
}

/// Gives the part the source's modification time and permission bits, so a copy is a faithful
/// copy. Best effort: a file system that refuses is no reason to fail the copy.
fn copy_attributes(part: &File, source: &FileStat) {
    if let Err(err) = part.set_modified(system_time(source.mtime, source.mtime_nanos)) {
        tracing::warn!(error = %err, "could not set the modification time of a copy");
    }
    let permissions = fs::Permissions::from_mode(source.mode & 0o777);
    if let Err(err) = part.set_permissions(permissions) {
        tracing::debug!(error = %err, "could not set the permissions of a copy");
    }
}

fn system_time(secs: i64, nanos: i64) -> SystemTime {
    let nanos = u32::try_from(nanos).unwrap_or(0).min(999_999_999);
    let base = SystemTime::UNIX_EPOCH;
    let moment = if secs >= 0 {
        base.checked_add(Duration::new(secs.unsigned_abs(), nanos))
    } else {
        base.checked_sub(Duration::from_secs(secs.unsigned_abs()))
            .and_then(|time| time.checked_add(Duration::from_nanos(u64::from(nanos))))
    };
    moment.unwrap_or(base)
}

/// Asks the kernel to drop the (now clean) cached pages of the part, so the read-back that follows
/// reads what the disk holds. Best effort; only Linux has the call.
fn forget_cached_pages(part: &File) {
    #[cfg(target_os = "linux")]
    if let Err(err) = rustix::fs::fadvise(part, 0, None, rustix::fs::Advice::DontNeed) {
        tracing::debug!(error = %err, "could not drop the cached pages of a copy");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = part;
}

/// Reads the part back through the same descriptor and compares size and SHA-1 with what was read
/// from the source.
fn verify(
    job: &CopyJob,
    part: &mut File,
    size: u64,
    expected: &Sha1Digest,
) -> Result<(), FsOpError> {
    if part.metadata()?.len() != size {
        return Err(FsOpError::VerifyFailed);
    }
    part.seek(SeekFrom::Start(0))?;
    let digest = sha1_reader(&mut *part, |_| {
        if job.cancel.is_cancelled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })?;
    match digest {
        None => Err(FsOpError::Cancelled),
        Some(digest) if &digest == expected => Ok(()),
        Some(_) => Err(FsOpError::VerifyFailed),
    }
}

/// What looking at the source right before retiring it found.
enum Checked {
    Gone,
    Changed,
    Same(PathBuf),
}

/// Whether the file at `rel` is still the very file that was copied (same inode, size and
/// modification time down to the nanosecond), without following a link.
fn check_source(
    root: &VolumeRoot,
    rel: &RelPath,
    expected: &FileStat,
) -> Result<Checked, FsOpError> {
    let path = match root.resolve_existing(rel) {
        Ok(path) => path,
        Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => return Ok(Checked::Gone),
        // A link where the file was is not the file that was copied.
        Err(FsError::SymlinkRejected(_)) => return Ok(Checked::Changed),
        Err(err) => return Err(FsOpError::from(err)),
    };
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Checked::Gone),
        Err(err) => return Err(FsOpError::from(err)),
    };
    if meta.is_file() && FileStat::from_metadata(&meta).same_file(expected) {
        Ok(Checked::Same(path))
    } else {
        Ok(Checked::Changed)
    }
}

/// What became of the source of a copy-and-delete move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retired {
    /// Unlinked, or already gone.
    Removed,
    /// Renamed into the trash of its volume.
    Kept,
    /// Not the file that was copied any more: nothing was touched.
    Changed,
}

/// Unlinks the source, but only when it is still the file that was copied.
pub(crate) fn unlink_source(
    root: &VolumeRoot,
    rel: &RelPath,
    expected: &FileStat,
) -> Result<Retired, FsOpError> {
    match check_source(root, rel, expected)? {
        Checked::Gone => Ok(Retired::Removed),
        Checked::Changed => Ok(Retired::Changed),
        Checked::Same(path) => match fs::remove_file(&path) {
            Ok(()) => Ok(Retired::Removed),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Retired::Removed),
            Err(err) => Err(FsOpError::from(err)),
        },
    }
}

/// Renames the source into the trash of its volume instead of deleting it, but only when it is
/// still the file that was copied.
pub(crate) fn keep_source(
    root: &VolumeRoot,
    mover: &dyn Mover,
    rel: &RelPath,
    expected: &FileStat,
    trash_path: &RelPath,
) -> Result<Retired, FsOpError> {
    let path = match check_source(root, rel, expected)? {
        Checked::Gone => return Ok(Retired::Removed),
        Checked::Changed => return Ok(Retired::Changed),
        Checked::Same(path) => path,
    };
    make_dirs(root, &trash_path.parent().unwrap_or_else(RelPath::root))?;
    let target = root
        .resolve_for_create(trash_path)
        .map_err(FsOpError::from_destination)?;
    match mover.rename_no_replace(&path, &target) {
        Ok(()) => Ok(Retired::Kept),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            match check_source(root, rel, expected)? {
                Checked::Gone => Ok(Retired::Removed),
                _ => Err(FsOpError::DestinationInvalid("the trash folder vanished")),
            }
        }
        Err(err) => Err(FsOpError::from(err)),
    }
}

/// Flushes the folder's entry for the new file. A file system that cannot sync a folder is not a
/// failure: the file's data is already on disk.
fn sync_dir(root: &VolumeRoot, dir: &RelPath) -> Result<(), FsOpError> {
    let path = root.resolve_dir(dir).map_err(FsOpError::from_destination)?;
    match File::open(&path)?.sync_all() {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(FsOpError::from(err)),
    }
}

/// Where a verified part ended up.
enum Placement {
    Placed,
    Skipped(SkipReason),
}

/// The result of the final no-replace rename of the part.
enum Renamed {
    Placed,
    /// Something is at the destination now.
    Taken,
}

/// The final no-replace rename of the part file to the destination.
fn rename_part(
    root: &VolumeRoot,
    mover: &dyn Mover,
    part: &RelPath,
    target: &RelPath,
) -> Result<Renamed, FsOpError> {
    let from = match root.resolve_existing(part) {
        Ok(path) => path,
        Err(FsError::NotFound(_) | FsError::NotADirectory(_)) => {
            return Err(FsOpError::Io(io::ErrorKind::NotFound));
        }
        Err(err) => return Err(FsOpError::from(err)),
    };
    let to = match root.resolve_for_create(target) {
        Ok(path) => path,
        Err(FsError::AlreadyExists(_)) => return Ok(Renamed::Taken),
        Err(FsError::SymlinkRejected(at)) if &at == target => return Ok(Renamed::Taken),
        Err(err) => return Err(FsOpError::from_destination(err)),
    };
    match mover.rename_no_replace(&from, &to) {
        Ok(()) => Ok(Renamed::Placed),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(Renamed::Taken),
        Err(err) => Err(FsOpError::from(err)),
    }
}

impl FsOps {
    /// Copies the source next to the target, verifies the copy, puts it in place and (for a move
    /// or a restore) retires the source. `replace` is the existing destination file the policy
    /// decided to replace: it is trashed right before the final rename, never earlier.
    ///
    /// Opens the journal row unless the caller already has (a rename that crossed devices).
    pub(crate) async fn copy_path(
        &self,
        run: &mut Run<'_>,
        replace: Option<FileStat>,
    ) -> Result<Flow, FsOpError> {
        let id = self.open_row(run).await?;
        let Ok(part) = run.target.dir().join(&part_name()) else {
            let err = FsOpError::internal("could not name a part file");
            return Err(self.fail_row(id, err).await);
        };
        let copied = match self.stage_part(run, id, &part).await {
            Ok(copied) => copied,
            Err(err) => return Err(self.fail_row(id, err).await),
        };
        match self.place_part(run, id, &part, &copied, replace).await {
            Ok(Placement::Placed) => {}
            Ok(Placement::Skipped(reason)) => {
                self.journal
                    .cancel(id, "the destination already has the file")
                    .await;
                return Ok(Flow::Done(Transferred::Skipped(reason)));
            }
            Err(err) => return Err(self.fail_row(id, err).await),
        }
        self.finish_copy(run, id, &copied).await
    }

    /// Writes the part file: journal first, then the verified copy.
    async fn stage_part(
        &self,
        run: &Run<'_>,
        id: i64,
        part: &RelPath,
    ) -> Result<CopiedPart, FsOpError> {
        run.cx.ensure_active()?;
        self.journal.set_part(id, part).await?;
        let job = CopyJob {
            src_root: Arc::clone(&run.src_root),
            src: run.job.src.rel.clone(),
            dst_root: Arc::clone(&run.dst_root),
            part: part.clone(),
            expected: run.src_stat,
            cancel: run.cx.cancel.clone(),
            on_bytes: Arc::clone(&run.cx.on_bytes),
            free_space: Arc::clone(&self.free_space),
            failpoints: self.failpoints(),
        };
        let copied = blocking(move || copy_and_verify(&job)).await?;
        let recorded = self
            .journal
            .set_dst(
                id,
                &run.target,
                Some(copied.hash.as_slice()),
                Some(copied.size),
            )
            .await;
        if let Err(err) = recorded {
            self.discard(run, part).await;
            return Err(err);
        }
        Ok(copied)
    }

    /// Puts the verified part at the destination; deletes the part when it is not wanted any more
    /// (a skip or a failure, but not a simulated crash).
    async fn place_part(
        &self,
        run: &mut Run<'_>,
        id: i64,
        part: &RelPath,
        copied: &CopiedPart,
        replace: Option<FileStat>,
    ) -> Result<Placement, FsOpError> {
        let result = self.place_loop(run, id, part, copied, replace).await;
        match &result {
            Ok(Placement::Placed) => {}
            Err(err) if err.is_abort() => {}
            Ok(Placement::Skipped(_)) | Err(_) => self.discard(run, part).await,
        }
        result
    }

    async fn place_loop(
        &self,
        run: &mut Run<'_>,
        id: i64,
        part: &RelPath,
        copied: &CopiedPart,
        mut replace: Option<FileStat>,
    ) -> Result<Placement, FsOpError> {
        let mut resolve = false;
        for _ in 0..ATTEMPTS {
            run.cx.ensure_active()?;
            if std::mem::take(&mut resolve) {
                let before = run.target.clone();
                let probe = self.probe(run).await?;
                match self.resolve_conflict(run, probe, Some(copied.hash)).await? {
                    Resolution::Skip(reason) => return Ok(Placement::Skipped(reason)),
                    Resolution::CaseOnly => {
                        return Ok(Placement::Skipped(SkipReason::IdenticalAtDestination));
                    }
                    Resolution::Retry => {
                        resolve = true;
                        continue;
                    }
                    Resolution::Free => {}
                    Resolution::Replace(existing) => replace = Some(existing),
                }
                if run.target != before {
                    let hash = Some(copied.hash.as_slice());
                    self.journal
                        .set_dst(id, &run.target, hash, Some(copied.size))
                        .await?;
                }
            }
            if let Some(existing) = replace.take() {
                match self.trash_existing(run, existing).await {
                    Ok(()) => {}
                    // The destination changed while the file was being copied: look again.
                    Err(
                        FsOpError::SourceMissing
                        | FsOpError::SourceChanged
                        | FsOpError::NotARegularFile,
                    ) => {
                        resolve = true;
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
            let (root, mover) = (Arc::clone(&run.dst_root), Arc::clone(&self.mover));
            let (from, to) = (part.clone(), run.target.rel.clone());
            match blocking(move || rename_part(&root, mover.as_ref(), &from, &to)).await? {
                Renamed::Placed => return Ok(Placement::Placed),
                Renamed::Taken => resolve = true,
            }
        }
        Err(FsOpError::DestinationInvalid(
            "the destination kept changing",
        ))
    }

    /// Sends the file that is about to be replaced to the trash (reason `replaced`).
    pub(crate) async fn trash_existing(
        &self,
        run: &Run<'_>,
        existing: FileStat,
    ) -> Result<(), FsOpError> {
        let spec = TrashSpec {
            loc: &run.target,
            expect: Some(Expect {
                size: existing.size,
                mtime: existing.mtime,
            }),
            batch: run.cx.batch(),
            reason: "replaced",
        };
        self.trash_core(spec, run.cx).await.map(|_| ())
    }

    async fn discard(&self, run: &Run<'_>, part: &RelPath) {
        let (root, part) = (Arc::clone(&run.dst_root), part.clone());
        let discarded = blocking(move || {
            discard_part(&root, &part);
            Ok(())
        })
        .await;
        if let Err(err) = discarded {
            tracing::warn!(error = %err, "could not delete a part file");
        }
    }

    /// After the part is in place: flushes the folder, retires the source of a move or restore,
    /// and commits the index change. Every error from here on leaves the journal row `pending`
    /// and deletes nothing: recovery decides from what is on the disk.
    async fn finish_copy(
        &self,
        run: &mut Run<'_>,
        id: i64,
        copied: &CopiedPart,
    ) -> Result<Flow, FsOpError> {
        self.check(Step::AfterFinalRename)?;
        let (root, dir) = (Arc::clone(&run.dst_root), run.target.dir());
        blocking(move || sync_dir(&root, &dir)).await?;
        self.check(Step::BeforeUnlink)?;
        let kept = match run.job.mode {
            Mode::Copy => None,
            Mode::Move | Mode::Restore => match self.retire_source(run).await? {
                Retired::Removed => None,
                Retired::Kept => Some(self.kept_source(run)?),
                Retired::Changed => {
                    self.journal.fail(id, &FsOpError::SourceChanged).await;
                    self.announce(run.cx, &touched_dirs(None, Some(&run.target), &run.created));
                    return Err(FsOpError::SourceChanged);
                }
            },
        };
        let indexed = self
            .apply_transfer(
                run.rules,
                &TransferApply {
                    mode: run.job.mode,
                    journal_id: id,
                    task_id: run.cx.task_id,
                    src: &run.job.src,
                    dst: &run.target,
                    pre: run.pre.as_ref(),
                    dst_stat: &copied.stat,
                    hash: Some(copied.hash),
                    restore_item: run.job.restore.as_ref().map(|info| info.item_id),
                    kept: kept.as_ref(),
                    crossing: run.job.crossing,
                    indexed: run.indexed,
                },
            )
            .await?;
        let src = (run.job.mode == Mode::Move).then_some(&run.job.src);
        self.announce(run.cx, &touched_dirs(src, Some(&run.target), &run.created));
        Ok(Flow::Done(Transferred::Done {
            dst: run.target.clone(),
            renamed: run.renamed,
            cross_device: run.job.mode != Mode::Copy,
            bytes: copied.size,
            indexed,
        }))
    }

    /// Where a source kept in the trash went: the batch and path derive from the journal row, so
    /// recovery computes the same path again.
    fn kept_source(&self, run: &Run<'_>) -> Result<KeptSource, FsOpError> {
        let (id, created_at) = run.row()?;
        let batch = TrashBatch::for_journal(created_at, id);
        let trash_path = trash_rel(&batch, &run.job.src.rel)?;
        Ok(KeptSource { batch, trash_path })
    }

    /// Removes the source after the copy is in place, or keeps it in the trash when the setting
    /// says so (a move only; a restore always deletes the trashed copy).
    async fn retire_source(&self, run: &Run<'_>) -> Result<Retired, FsOpError> {
        let keep =
            run.job.mode == Mode::Move && run.rules.settings.keep_cross_volume_source_in_trash;
        let (root, rel, expected) = (
            Arc::clone(&run.src_root),
            run.job.src.rel.clone(),
            run.src_stat,
        );
        if !keep {
            return blocking(move || unlink_source(&root, &rel, &expected)).await;
        }
        let trash_path = self.kept_source(run)?.trash_path;
        let mover = Arc::clone(&self.mover);
        blocking(move || keep_source(&root, mover.as_ref(), &rel, &expected, &trash_path)).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use apm_core::hash::sha1_bytes;

    use super::*;
    use crate::fsops::backend::SystemMover;
    use crate::fsops::test_support::scratch;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    const PART: &str = ".apm-0000000000000abc.apm-part";

    struct Fixture {
        _dir: tempfile::TempDir,
        root: Arc<VolumeRoot>,
        reported: Arc<AtomicU64>,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let dir = scratch(label);
            let root = Arc::new(VolumeRoot::new(1, dir.path()).unwrap());
            Self {
                _dir: dir,
                root,
                reported: Arc::new(AtomicU64::new(0)),
            }
        }

        fn write(&self, name: &str, content: &[u8]) -> FileStat {
            let path = self.root.path().join(name);
            fs::write(&path, content).unwrap();
            FileStat::from_metadata(&fs::symlink_metadata(&path).unwrap())
        }

        fn stat(&self, name: &str) -> FileStat {
            FileStat::from_metadata(&fs::symlink_metadata(self.root.path().join(name)).unwrap())
        }

        fn job(&self, src: &str, expected: FileStat) -> CopyJob {
            let reported = Arc::clone(&self.reported);
            CopyJob {
                src_root: Arc::clone(&self.root),
                src: rel(src),
                dst_root: Arc::clone(&self.root),
                part: rel(PART),
                expected,
                cancel: CancellationToken::new(),
                on_bytes: Arc::new(move |bytes| {
                    reported.fetch_add(bytes, Ordering::SeqCst);
                }),
                free_space: Arc::new(crate::fsops::backend::SystemFreeSpace),
                failpoints: None,
            }
        }

        fn part_exists(&self) -> bool {
            fs::symlink_metadata(self.root.path().join(PART)).is_ok()
        }
    }

    struct Space(u64);

    impl FreeSpace for Space {
        fn available(&self, _: &std::path::Path) -> io::Result<u64> {
            Ok(self.0)
        }
    }

    #[test]
    fn part_names_have_the_exact_shape_recovery_looks_for() {
        let name = part_name();
        assert!(is_part_name(&name), "{name}");
        assert_eq!(name.len(), ".apm-".len() + 16 + ".apm-part".len());
        assert!(is_part_name(PART));
        for not_a_part in [
            "",
            "photo.apm-part",
            ".apm-0123456789abcdef",
            ".apm-0123456789ABCDEF.apm-part",
            ".apm-0123456789abcde.apm-part",
            ".apm-0123456789abcdeg.apm-part",
            "x.apm-0123456789abcdef.apm-part",
            ".apm-0123456789abcdef.apm-part.jpg",
        ] {
            assert!(!is_part_name(not_a_part), "{not_a_part}");
        }
        assert_ne!(part_name(), part_name());
    }

    #[test]
    fn a_copy_is_byte_identical_hashed_and_keeps_mtime_and_permissions() {
        let fx = Fixture::new("copy-ok");
        let content: Vec<u8> = (0..(2 * CHUNK_SIZE + 123))
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        fx.write("a.bin", &content);
        fs::set_permissions(
            fx.root.path().join("a.bin"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let stat = fx.stat("a.bin");
        let copied = copy_and_verify(&fx.job("a.bin", stat)).unwrap();

        assert_eq!(copied.hash, sha1_bytes(&content));
        assert_eq!(copied.size, content.len() as u64);
        assert_eq!(fx.reported.load(Ordering::SeqCst), content.len() as u64);
        assert_eq!(fs::read(fx.root.path().join(PART)).unwrap(), content);
        assert_eq!(copied.stat.size, stat.size);
        assert_eq!(copied.stat.mtime, stat.mtime);
        assert_eq!(copied.stat.permissions() & 0o777, 0o640);
        assert_ne!(copied.stat.ino, stat.ino);
    }

    #[test]
    fn an_empty_file_copies() {
        let fx = Fixture::new("copy-empty");
        let stat = fx.write("e.bin", b"");
        let copied = copy_and_verify(&fx.job("e.bin", stat)).unwrap();
        assert_eq!(copied.size, 0);
        assert_eq!(copied.hash, sha1_bytes(b""));
        assert!(fx.part_exists());
    }

    #[test]
    fn a_cancelled_copy_leaves_no_part_and_the_source_alone() {
        let fx = Fixture::new("copy-cancel");
        let stat = fx.write("a.bin", b"payload");
        let job = fx.job("a.bin", stat);
        job.cancel.cancel();
        assert_eq!(copy_and_verify(&job), Err(FsOpError::Cancelled));
        assert!(!fx.part_exists());
        assert_eq!(fs::read(fx.root.path().join("a.bin")).unwrap(), b"payload");
    }

    #[test]
    fn a_source_that_changed_is_refused_before_anything_is_created() {
        let fx = Fixture::new("copy-changed");
        let stat = fx.write("a.bin", b"payload");
        let mut stale = stat;
        stale.size += 1;
        assert_eq!(
            copy_and_verify(&fx.job("a.bin", stale)),
            Err(FsOpError::SourceChanged)
        );
        assert!(!fx.part_exists());
    }

    #[test]
    fn a_missing_source_is_source_missing() {
        let fx = Fixture::new("copy-missing");
        let stat = fx.write("a.bin", b"payload");
        fs::remove_file(fx.root.path().join("a.bin")).unwrap();
        assert_eq!(
            copy_and_verify(&fx.job("a.bin", stat)),
            Err(FsOpError::SourceMissing)
        );
    }

    #[test]
    fn a_full_destination_is_no_space_and_creates_nothing() {
        let fx = Fixture::new("copy-space");
        let stat = fx.write("a.bin", &[1_u8; 1000]);
        let mut job = fx.job("a.bin", stat);
        job.free_space = Arc::new(Space(1000));
        assert_eq!(copy_and_verify(&job), Err(FsOpError::NoSpace));
        assert!(!fx.part_exists());
        job.free_space = Arc::new(Space(1000 + SPACE_MARGIN));
        assert!(copy_and_verify(&job).is_ok());
    }

    #[test]
    fn a_corrupted_copy_fails_verification_and_is_deleted() {
        for content in [&b"some content"[..], &b""[..]] {
            let fx = Fixture::new("copy-corrupt");
            let stat = fx.write("a.bin", content);
            let mut job = fx.job("a.bin", stat);
            job.failpoints = Some(Failpoints::new(|step| {
                if step == Step::AfterCopy {
                    FailAction::Corrupt
                } else {
                    FailAction::Continue
                }
            }));
            assert_eq!(copy_and_verify(&job), Err(FsOpError::VerifyFailed));
            assert!(!fx.part_exists());
            assert_eq!(fs::read(fx.root.path().join("a.bin")).unwrap(), content);
        }
    }

    #[test]
    fn a_simulated_crash_leaves_the_part_behind_and_an_error_cleans_it() {
        for step in [
            Step::AfterPartCreate,
            Step::AfterCopy,
            Step::AfterFsync,
            Step::AfterVerify,
        ] {
            let fx = Fixture::new("copy-abort");
            let stat = fx.write("a.bin", b"payload");
            let mut job = fx.job("a.bin", stat);
            job.failpoints = Some(Failpoints::abort_at(step));
            assert_eq!(copy_and_verify(&job), Err(FsOpError::Aborted), "{step:?}");
            assert!(fx.part_exists(), "{step:?}");

            let fx = Fixture::new("copy-error");
            let stat = fx.write("a.bin", b"payload");
            let mut job = fx.job("a.bin", stat);
            job.failpoints = Some(Failpoints::new(move |reached| {
                if reached == step {
                    FailAction::Error
                } else {
                    FailAction::Continue
                }
            }));
            assert!(copy_and_verify(&job).is_err(), "{step:?}");
            assert!(!fx.part_exists(), "{step:?}");
        }
    }

    #[test]
    fn a_part_that_already_exists_is_never_touched() {
        let fx = Fixture::new("copy-exists");
        let stat = fx.write("a.bin", b"payload");
        fx.write(PART, b"somebody else's");
        assert!(copy_and_verify(&fx.job("a.bin", stat)).is_err());
        assert_eq!(
            fs::read(fx.root.path().join(PART)).unwrap(),
            b"somebody else's"
        );
    }

    #[test]
    fn discard_part_deletes_parts_only() {
        let fx = Fixture::new("discard");
        fx.write("photo.jpg", b"x");
        fx.write(PART, b"x");
        discard_part(&fx.root, &rel("photo.jpg"));
        assert!(fx.root.path().join("photo.jpg").exists());
        discard_part(&fx.root, &rel(PART));
        assert!(!fx.part_exists());
        // Already gone is fine.
        discard_part(&fx.root, &rel(PART));
        discard_part(&fx.root, &rel(&format!("missing/{PART}")));
    }

    #[test]
    fn unlink_source_only_removes_the_file_that_was_copied() {
        let fx = Fixture::new("unlink");
        let stat = fx.write("a.bin", b"payload");
        let mut other = stat;
        other.mtime_nanos += 1;
        assert_eq!(
            unlink_source(&fx.root, &rel("a.bin"), &other),
            Ok(Retired::Changed)
        );
        assert!(fx.root.path().join("a.bin").exists());
        assert_eq!(
            unlink_source(&fx.root, &rel("a.bin"), &stat),
            Ok(Retired::Removed)
        );
        assert!(!fx.root.path().join("a.bin").exists());
        assert_eq!(
            unlink_source(&fx.root, &rel("a.bin"), &stat),
            Ok(Retired::Removed)
        );
        assert_eq!(
            unlink_source(&fx.root, &rel("no/such/dir/a.bin"), &stat),
            Ok(Retired::Removed)
        );
    }

    #[test]
    fn unlink_source_never_follows_or_removes_a_link() {
        let fx = Fixture::new("unlink-link");
        let stat = fx.write("a.bin", b"payload");
        fs::remove_file(fx.root.path().join("a.bin")).unwrap();
        fs::write(fx.root.path().join("target.bin"), b"payload").unwrap();
        std::os::unix::fs::symlink("target.bin", fx.root.path().join("a.bin")).unwrap();
        assert_eq!(
            unlink_source(&fx.root, &rel("a.bin"), &stat),
            Ok(Retired::Changed)
        );
        assert!(fx.root.path().join("target.bin").exists());
        assert!(fs::symlink_metadata(fx.root.path().join("a.bin")).is_ok());
    }

    #[test]
    fn keep_source_renames_into_the_trash_with_the_original_layout() {
        let fx = Fixture::new("keep");
        fs::create_dir(fx.root.path().join("sub")).unwrap();
        let stat = fx.write("sub/a.bin", b"payload");
        let trash = rel(".apm-trash/20260101-000000-x1/sub/a.bin");
        assert_eq!(
            keep_source(&fx.root, &SystemMover, &rel("sub/a.bin"), &stat, &trash),
            Ok(Retired::Kept)
        );
        assert!(!fx.root.path().join("sub/a.bin").exists());
        assert_eq!(
            fs::read(
                fx.root
                    .path()
                    .join(".apm-trash/20260101-000000-x1/sub/a.bin")
            )
            .unwrap(),
            b"payload"
        );
        // Gone already: nothing to keep.
        assert_eq!(
            keep_source(&fx.root, &SystemMover, &rel("sub/a.bin"), &stat, &trash),
            Ok(Retired::Removed)
        );
    }

    #[test]
    fn keep_source_refuses_a_changed_source_and_never_overwrites_the_trash() {
        let fx = Fixture::new("keep-changed");
        let stat = fx.write("a.bin", b"payload");
        let trash = rel(".apm-trash/b/a.bin");
        let mut other = stat;
        other.size += 1;
        assert_eq!(
            keep_source(&fx.root, &SystemMover, &rel("a.bin"), &other, &trash),
            Ok(Retired::Changed)
        );
        assert!(fx.root.path().join("a.bin").exists());

        fs::create_dir_all(fx.root.path().join(".apm-trash/b")).unwrap();
        fs::write(fx.root.path().join(".apm-trash/b/a.bin"), b"old").unwrap();
        assert!(keep_source(&fx.root, &SystemMover, &rel("a.bin"), &stat, &trash).is_err());
        assert_eq!(fs::read(fx.root.path().join("a.bin")).unwrap(), b"payload");
        assert_eq!(
            fs::read(fx.root.path().join(".apm-trash/b/a.bin")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn rename_part_never_replaces() {
        let fx = Fixture::new("rename-part");
        fx.write(PART, b"new");
        fx.write("a.bin", b"old");
        assert!(matches!(
            rename_part(&fx.root, &SystemMover, &rel(PART), &rel("a.bin")),
            Ok(Renamed::Taken)
        ));
        assert_eq!(fs::read(fx.root.path().join("a.bin")).unwrap(), b"old");
        assert!(matches!(
            rename_part(&fx.root, &SystemMover, &rel(PART), &rel("b.bin")),
            Ok(Renamed::Placed)
        ));
        assert_eq!(fs::read(fx.root.path().join("b.bin")).unwrap(), b"new");
        // The part is gone now: a second rename is an i/o error, not a silent success.
        assert_eq!(
            rename_part(&fx.root, &SystemMover, &rel(PART), &rel("c.bin")).err(),
            Some(FsOpError::Io(io::ErrorKind::NotFound))
        );
    }

    #[test]
    fn syncing_a_folder_works_here() {
        let fx = Fixture::new("sync-dir");
        fs::create_dir(fx.root.path().join("d")).unwrap();
        sync_dir(&fx.root, &rel("d")).unwrap();
        sync_dir(&fx.root, &RelPath::root()).unwrap();
    }

    #[test]
    fn system_time_handles_negative_and_fractional_stamps() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(system_time(10, 500), epoch + Duration::new(10, 500));
        assert_eq!(
            system_time(-10, 500),
            epoch - Duration::from_secs(10) + Duration::from_nanos(500)
        );
        // Not representable on every platform; it only has to be handled without panicking.
        let _ = system_time(i64::MIN, 0);
        let _ = system_time(i64::MAX, 0);
    }
}
