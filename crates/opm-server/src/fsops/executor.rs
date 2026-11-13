//! The move/copy engine: one pipeline for [`FsOps::move_file`], [`FsOps::copy_file`], the members of
//! a [`FsOps::move_group`] and a file [`FsOps::restore`].
//!
//! 1. Validate without touching anything: the destination path (names, trash, junk), the source (a
//!    regular file that is still what the caller expects) and the volumes (online, writable).
//! 2. Create the missing destination folders.
//! 3. Look at the destination and decide ([`FsOps::resolve_conflict`]): free, skip, a new name,
//!    replace (the existing file is trashed first) or "that is the very same file".
//! 4. Place the file: a no-replace RENAME when both ends are on one file system
//!    ([`rename_into_place`]), the verified copy of [`copy`](super::copy) for a copy and for a
//!    rename that turns out to cross file systems.
//! 5. One transaction updates the index and closes the journal row ([`apply`](super::apply)).
//!
//! Another writer can overtake every look at the disk, so no step trusts an earlier one: the
//! placing step itself never replaces anything, and when it finds the destination taken the engine
//! looks again and decides again (at most [`ATTEMPTS`] times).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirEntryExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use opm_core::conflict::{
    self, ConflictDecision, ContentInfo, decide, needs_hashes, sidecar_destination,
};
use opm_core::dto::ConflictPolicy;
use opm_core::hash::{self, Sha1Digest};
use opm_core::junk::JunkRules;
use opm_core::paths::{FileId, RelPath};
use opm_core::sidecar::LinkKind;
use tokio_util::sync::CancellationToken;

use super::apply::{
    Crossing, Indexed, Mode, Snap, TransferApply, file_at, load_snap, to_i64, touched_dirs,
};
use super::backend::Mover;
use super::context::OpContext;
use super::dirs::{free_name_with, in_trash, make_dirs, validate_file_path};
use super::error::FsOpError;
use super::failpoints::Step;
use super::journal::{Guard, NewEntry, Op};
use super::{Expect, FileStat, FsOps, Loc, Rules, blocking};
use crate::indexer::hash::{HashError, sha1_file};
use crate::security::{FsError, VolumeRoot};

/// How often the engine looks again at a destination that keeps changing underneath it.
pub(crate) const ATTEMPTS: usize = 8;

/// One file to move or copy.
#[derive(Debug, Clone)]
pub struct MoveRequest {
    /// The index row of the source when the caller knows it (a hint: the engine looks the row up by
    /// location right before the disk step).
    pub file_id: Option<FileId>,
    pub src: Loc,
    pub dst: Loc,
    pub policy: ConflictPolicy,
    /// The size and mtime the plan saw; a mismatch is `source_changed` and nothing is touched.
    pub expect: Option<Expect>,
}

/// Why a file was left where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Source and destination are the same place.
    SameLocation,
    /// The destination already holds a file with the same content; the source is untouched.
    IdenticalAtDestination,
    /// The destination exists and the policy says skip (or the name may not change).
    DestinationExists,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved {
        /// The FINAL location (a conflict may have renamed the file).
        dst: Loc,
        renamed: bool,
        /// The bytes had to be copied because the two file systems differ (always `false` for
        /// [`FsOps::copy_file`](super::FsOps::copy_file), which always copies).
        cross_device: bool,
        bytes: u64,
        /// The index row of the file at its new location (the same id for a move).
        file_id: FileId,
    },
    Skipped(SkipReason),
}

/// A sidecar that follows its owner to a name derived from the owner's final one.
#[derive(Debug, Clone)]
pub(crate) struct Companion {
    pub link: LinkKind,
    /// The owner's name before the move.
    pub old_owner: String,
    /// The sidecar's current name.
    pub sidecar: String,
}

impl Companion {
    /// The name the sidecar takes when its owner is called `new_owner`.
    pub(crate) fn destination(&self, new_owner: &str) -> String {
        sidecar_destination(self.link, &self.old_owner, new_owner, &self.sidecar)
    }

    /// How many bytes the sidecar's name adds to the owner's.
    fn tail(&self) -> usize {
        self.destination("").len()
    }
}

/// What a restore brings along from the trash item.
#[derive(Debug, Clone)]
pub(crate) struct RestoreInfo {
    pub item_id: i64,
    pub snap: Option<Snap>,
}

/// One file transfer, as the engine runs it.
#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub mode: Mode,
    /// For a restore: the location in the trash.
    pub src: Loc,
    pub dst: Loc,
    pub policy: ConflictPolicy,
    pub expect: Option<Expect>,
    /// Whether a taken destination may be answered with a new name. A sidecar never gets its own:
    /// its name follows its owner's.
    pub allow_rename: bool,
    /// The sidecars that follow this file: a new name must leave their names free too.
    pub companions: Vec<Companion>,
    pub crossing: Crossing,
    pub restore: Option<RestoreInfo>,
}

/// What a transfer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Transferred {
    Done {
        dst: Loc,
        renamed: bool,
        cross_device: bool,
        bytes: u64,
        indexed: Indexed,
    },
    Skipped(SkipReason),
}

#[derive(sqlx::FromRow)]
struct TransferReceipt {
    dst_volume_id: i64,
    dst_path: String,
    size: i64,
    result_file_id: Option<i64>,
    result_live: bool,
    result_mtime: Option<i64>,
}

impl Transferred {
    /// The outcome a caller of `move_file`/`copy_file` sees.
    pub(crate) fn into_outcome(self) -> Result<MoveOutcome, FsOpError> {
        match self {
            Self::Done {
                dst,
                renamed,
                cross_device,
                bytes,
                indexed,
            } => {
                let file_id = indexed
                    .file_id
                    .ok_or_else(|| FsOpError::internal("the transferred file has no index row"))?;
                Ok(MoveOutcome::Moved {
                    dst,
                    renamed,
                    cross_device,
                    bytes,
                    file_id,
                })
            }
            Self::Skipped(reason) => Ok(MoveOutcome::Skipped(reason)),
        }
    }
}

/// The state of one transfer while the engine works on it.
pub(crate) struct Run<'a> {
    pub job: &'a Job,
    pub cx: &'a OpContext,
    pub rules: &'a Rules,
    /// The longest name a conflict may pick for the file: the limit minus the longest tail of its
    /// companions.
    pub max: usize,
    pub src_root: Arc<VolumeRoot>,
    pub dst_root: Arc<VolumeRoot>,
    /// The source as it was when the operation started: the identity every later step checks.
    pub src_stat: FileStat,
    /// What the index knew of the source before anything moved.
    pub pre: Option<Snap>,
    /// `false` when the destination name is one the scanner ignores (only a restore may get there).
    pub indexed: bool,
    /// Where the file goes now (the planned destination, or the name a conflict picked).
    pub target: Loc,
    pub renamed: bool,
    /// The folders this operation created for the destination.
    pub created: Vec<RelPath>,
    /// The journal row, once there is one; it lives as long as the operation.
    pub guard: Option<Guard>,
    checked_target: Option<Loc>,
}

impl Run<'_> {
    /// The journal row's id and creation time.
    pub(crate) fn row(&self) -> Result<(i64, i64), FsOpError> {
        self.guard
            .as_ref()
            .map(|guard| (guard.id(), guard.created_at()))
            .ok_or_else(|| FsOpError::internal("the journal row is missing"))
    }
}

/// What one round of the engine concluded.
pub(crate) enum Flow {
    Done(Transferred),
    /// The destination changed under the engine's feet: look again.
    Retry,
}

/// What is at the destination.
pub(crate) enum Probe {
    Free,
    Regular(FileStat),
    /// A folder, a link or a special file.
    Other,
}

/// The answer to "what do we do about what is at the destination".
pub(crate) enum Resolution {
    /// Place the file at `run.target`.
    Free,
    Skip(SkipReason),
    /// Replace this existing file (it is trashed right before the new one takes its place).
    Replace(FileStat),
    /// Source and destination are the same file under names that differ in case only.
    CaseOnly,
    Retry,
}

/// `lstat` of the source with the checks every operation starts with.
fn stat_source(
    root: &VolumeRoot,
    rel: &RelPath,
    expect: Option<Expect>,
) -> Result<FileStat, FsOpError> {
    let meta = root.stat(rel).map_err(FsOpError::from_source)?;
    if !meta.is_file() {
        return Err(FsOpError::NotARegularFile);
    }
    let stat = FileStat::from_metadata(&meta);
    if expect.is_some_and(|expect| !stat.matches(&expect)) {
        return Err(FsOpError::SourceChanged);
    }
    Ok(stat)
}

/// Looks at the destination: nothing, a regular file, or something else.
fn probe_dest(root: &VolumeRoot, rel: &RelPath) -> Result<Probe, FsOpError> {
    match root.resolve_for_create(rel) {
        Ok(_) => Ok(Probe::Free),
        Err(FsError::AlreadyExists(_)) => match root.stat(rel) {
            Ok(meta) if meta.is_file() => Ok(Probe::Regular(FileStat::from_metadata(&meta))),
            Ok(_) | Err(FsError::SymlinkRejected(_)) => Ok(Probe::Other),
            // It vanished between the two looks.
            Err(FsError::NotFound(_)) => Ok(Probe::Free),
            Err(err) => Err(FsOpError::from_destination(err)),
        },
        Err(FsError::SymlinkRejected(at)) if &at == rel => Ok(Probe::Other),
        Err(err) => Err(FsOpError::from_destination(err)),
    }
}

/// The names (raw bytes) in `dir` whose entry is the inode `ino`. More than one means hard links;
/// exactly one is a single file, however a case-insensitive file system lets it be reached.
pub(crate) fn names_with_inode(
    root: &VolumeRoot,
    dir: &RelPath,
    ino: u64,
    candidates: &[String],
) -> Result<Vec<Vec<u8>>, FsOpError> {
    let path = root.resolve_dir(dir).map_err(FsOpError::from_destination)?;
    if ino == 0 {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&path)?
        .map(|entry| entry.map(|entry| (entry.file_name().as_bytes().to_vec(), entry.ino())))
        .collect::<Result<Vec<_>, _>>()?;
    candidate_matches(&entries, candidates, |name| {
        let rel = dir
            .join(name)
            .map_err(|_| FsOpError::DestinationInvalid("that name is invalid"))?;
        Ok(root.stat(&rel).is_ok_and(|meta| meta.ino() == ino))
    })
}

fn candidate_matches(
    entries: &[(Vec<u8>, u64)],
    candidates: &[String],
    mut is_expected_inode: impl FnMut(&str) -> Result<bool, FsOpError>,
) -> Result<Vec<Vec<u8>>, FsOpError> {
    let wanted: HashSet<_> = candidates
        .iter()
        .map(|name| opm_core::names::normalize_name(name))
        .collect();
    let mut matched = Vec::new();
    for (bytes, _readdir_inode) in entries {
        let Some(name) = std::str::from_utf8(bytes).ok() else {
            continue;
        };
        if wanted.contains(&opm_core::names::normalize_name(name)) && is_expected_inode(name)? {
            matched.push(bytes.clone());
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_lookup_stats_only_name_candidates_and_ignores_readdir_inode_values() {
        let entries = vec![
            (b"IMG_1.JPG".to_vec(), 0),
            (b"unrelated.jpg".to_vec(), 42),
            (b"img_1.jpg".to_vec(), 999),
        ];
        let candidates = vec!["img_1.jpg".to_owned()];
        let mut stats = Vec::new();
        let matches = candidate_matches(&entries, &candidates, |name| {
            stats.push(name.to_owned());
            Ok(true)
        })
        .unwrap();
        assert_eq!(matches, [b"IMG_1.JPG".to_vec(), b"img_1.jpg".to_vec()]);
        assert_eq!(stats, ["IMG_1.JPG", "img_1.jpg"]);
    }
}

/// The SHA-1 of a file through the shared hasher, refused when the file is not the one that was
/// looked at (`expected`) right before or right after the read.
pub(crate) fn hash_file(
    root: &VolumeRoot,
    rel: &RelPath,
    expected: &FileStat,
    cancel: &CancellationToken,
) -> Result<Sha1Digest, FsOpError> {
    let unchanged = || -> Result<(), FsOpError> {
        let meta = root.stat(rel).map_err(FsOpError::from_source)?;
        if meta.is_file() && FileStat::from_metadata(&meta).same_file(expected) {
            Ok(())
        } else {
            Err(FsOpError::SourceChanged)
        }
    };
    unchanged()?;
    let digest = sha1_file(root, rel, cancel, &mut |_| {}).map_err(|err| match err {
        HashError::Cancelled => FsOpError::Cancelled,
        HashError::Gone(err) => FsOpError::from_source(err),
        HashError::Io(kind) => FsOpError::Io(kind),
    })?;
    unchanged()?;
    Ok(digest)
}

/// How the rename step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    Done,
    /// Something is at the destination now: nothing was changed, look again.
    Raced,
    /// The two ends are on different file systems.
    CrossDevice,
}

/// The rename step: checks the source is still the file the operation started with, then renames
/// it without replacing anything. `case_only` is the one rename that may land on an existing
/// name: the destination is the very same entry under another case, and the caller proved it.
pub(crate) fn rename_into_place(
    src_root: &VolumeRoot,
    dst_root: &VolumeRoot,
    mover: &dyn Mover,
    src: &RelPath,
    dst: &RelPath,
    expected: &FileStat,
    case_only: bool,
) -> Result<Placed, FsOpError> {
    let from = src_root
        .resolve_existing(src)
        .map_err(FsOpError::from_source)?;
    let meta = fs::symlink_metadata(&from).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => FsOpError::SourceMissing,
        _ => FsOpError::from(err),
    })?;
    if !meta.is_file() {
        return Err(FsOpError::NotARegularFile);
    }
    if !FileStat::from_metadata(&meta).same_file(expected) {
        return Err(FsOpError::SourceChanged);
    }
    let to = if case_only {
        match dst_root.resolve_existing(dst) {
            Ok(path) => path,
            Err(FsError::NotFound(_) | FsError::NotADirectory(_) | FsError::SymlinkRejected(_)) => {
                return Ok(Placed::Raced);
            }
            Err(err) => return Err(FsOpError::from_destination(err)),
        }
    } else {
        match dst_root.resolve_for_create(dst) {
            Ok(path) => path,
            Err(FsError::AlreadyExists(_)) => return Ok(Placed::Raced),
            Err(FsError::SymlinkRejected(at)) if &at == dst => return Ok(Placed::Raced),
            Err(err) => return Err(FsOpError::from_destination(err)),
        }
    };
    if case_only && !is_entry_of(&to, expected) {
        return Ok(Placed::Raced);
    }
    let renamed = if case_only {
        mover.rename_same_inode(&from, &to)
    } else {
        mover.rename_no_replace(&from, &to)
    };
    match renamed {
        Ok(()) => Ok(Placed::Done),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(Placed::Raced),
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => Ok(Placed::CrossDevice),
        Err(err) if err.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(&from) {
            Err(gone) if gone.kind() == io::ErrorKind::NotFound => Err(FsOpError::SourceMissing),
            _ => Err(FsOpError::DestinationInvalid(
                "a parent directory is missing",
            )),
        },
        Err(err) => Err(FsOpError::from(err)),
    }
}

/// Whether the entry at `path` is the inode `expected` describes.
fn is_entry_of(path: &Path, expected: &FileStat) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        let stat = FileStat::from_metadata(&meta);
        stat.dev == expected.dev && stat.ino == expected.ino
    })
}

impl FsOps {
    pub(crate) async fn completed_bytes(
        &self,
        task_id: i64,
        op: &str,
        src: &Loc,
    ) -> Result<Option<u64>, FsOpError> {
        let size: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(j.size, 0) FROM fs_journal j JOIN tasks t ON t.id = j.task_id \
             WHERE j.task_id = ? AND t.resumable = 1 AND j.op = ? AND j.state = 'done' \
             AND j.src_volume_id = ? AND j.src_path = ? ORDER BY j.id DESC LIMIT 1",
        )
        .bind(task_id)
        .bind(op)
        .bind(src.volume_id)
        .bind(src.rel.as_str())
        .fetch_optional(self.db.read())
        .await?;
        size.map(|size| u64::try_from(size).map_err(|err| FsOpError::internal(err.to_string())))
            .transpose()
    }
    /// The journal commit is atomic with the index update, unlike a caller's next checkpoint.
    pub(crate) async fn completed_move(
        &self,
        task_id: i64,
        src: &Loc,
    ) -> Result<Option<MoveOutcome>, FsOpError> {
        let job = Job {
            mode: Mode::Move,
            src: src.clone(),
            dst: src.clone(),
            policy: ConflictPolicy::Skip,
            expect: None,
            allow_rename: true,
            companions: Vec::new(),
            crossing: Crossing::Keep,
            restore: None,
        };
        self.completed_transfer(&job, Some(task_id))
            .await?
            .map(Transferred::into_outcome)
            .transpose()
    }

    async fn completed_transfer(
        &self,
        job: &Job,
        task_id: Option<i64>,
    ) -> Result<Option<Transferred>, FsOpError> {
        let Some(task_id) = task_id else {
            return Ok(None);
        };
        let op = match job.mode {
            Mode::Move => "move",
            Mode::Copy => "copy",
            Mode::Restore => "restore",
        };
        let receipt: Option<TransferReceipt> = sqlx::query_as(
            "SELECT j.dst_volume_id, j.dst_path, COALESCE(j.size, 0) AS size, j.result_file_id, j.result_live, j.result_mtime FROM fs_journal j \
             JOIN tasks t ON t.id = j.task_id WHERE j.task_id = ? AND t.resumable = 1 \
             AND j.state = 'done' AND j.op = ? AND j.src_volume_id = ? AND j.src_path = ? \
             AND (? OR (j.dst_volume_id = ? AND j.dst_path = ?)) ORDER BY j.id DESC LIMIT 1",
        ).bind(task_id).bind(op).bind(job.src.volume_id).bind(job.src.rel.as_str())
            .bind(job.mode != Mode::Copy).bind(job.dst.volume_id).bind(job.dst.rel.as_str())
            .fetch_optional(self.db.read()).await?;
        let Some(TransferReceipt {
            dst_volume_id: volume,
            dst_path: path,
            size,
            result_file_id: recorded_id,
            result_live,
            result_mtime: mtime,
        }) = receipt
        else {
            return Ok(None);
        };
        if !result_live {
            return Err(FsOpError::SourceChanged);
        }
        let dst = Loc::new(
            volume,
            RelPath::new(&path).map_err(|err| FsOpError::internal(err.to_string()))?,
        );
        let file_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?",
        )
        .bind(dst.volume_id)
        .bind(dst.dir().as_str())
        .bind(dst.name())
        .fetch_optional(self.db.read())
        .await?;
        let file_id = file_id.ok_or(FsOpError::SourceChanged)?;
        if Some(file_id) != recorded_id {
            return Err(FsOpError::SourceChanged);
        }
        let stat = self.stat(&dst).await?;
        if size != to_i64(stat.size) || mtime != Some(stat.mtime) {
            return Err(FsOpError::SourceChanged);
        }
        if let Some(expect) = job.expect
            && (expect.size != stat.size || expect.mtime != stat.mtime)
        {
            return Err(FsOpError::SourceChanged);
        }
        Ok(Some(Transferred::Done {
            renamed: dst != job.dst,
            cross_device: dst.volume_id != job.src.volume_id,
            dst,
            bytes: u64::try_from(size).map_err(|err| FsOpError::internal(err.to_string()))?,
            indexed: super::apply::Indexed {
                file_id: Some(file_id),
                source_file_id: Some(file_id),
            },
        }))
    }
    /// Moves one file. On one file system that is an atomic no-replace rename; across file
    /// systems the file is copied, verified (size and SHA-1 read back from the destination) and
    /// only then deleted from the source (or kept in the trash when the setting says so).
    ///
    /// A taken destination is decided by the policy: `Smart` skips identical content and renames
    /// a different file, `Skip` skips, `Rename` renames, `Replace` trashes the existing file
    /// (reason `replaced`, in [`OpContext::batch`]) and takes its place. A rename is a number
    /// appended to the stem (`IMG_1.jpg` becomes `IMG_1_1.jpg`); the outcome carries the FINAL
    /// name.
    ///
    /// The file keeps its index row (id, tags, rating, thumbnails); only its location changes.
    /// A destination in a folder that does not exist creates the folders.
    ///
    /// # Errors
    /// `source_missing`, `source_changed` (the file is not what `expect` says, or changed while
    /// it was copied), `not_a_regular_file`, `symlink_rejected`, `volume_offline`,
    /// `volume_read_only`, `destination_invalid`, `name_too_long`, `no_space` and `verify_failed`
    /// (cross-device only), `cancelled`, `io_error`, `internal`.
    pub async fn move_file(
        &self,
        request: MoveRequest,
        cx: &OpContext,
    ) -> Result<MoveOutcome, FsOpError> {
        self.single(Mode::Move, request, cx).await
    }

    /// Copies one file; the source stays. The copy is always the verified streaming copy, and the
    /// destination gets a NEW index row cloned from the source's, with the content hash the copy
    /// computed. Conflicts and errors are the same as for [`FsOps::move_file`].
    pub async fn copy_file(
        &self,
        request: MoveRequest,
        cx: &OpContext,
    ) -> Result<MoveOutcome, FsOpError> {
        self.single(Mode::Copy, request, cx).await
    }

    async fn single(
        &self,
        mode: Mode,
        request: MoveRequest,
        cx: &OpContext,
    ) -> Result<MoveOutcome, FsOpError> {
        let job = Job {
            mode,
            src: request.src,
            dst: request.dst,
            policy: request.policy,
            expect: request.expect,
            allow_rename: true,
            companions: Vec::new(),
            crossing: Crossing::Delete,
            restore: None,
        };
        self.transfer(&job, cx).await?.into_outcome()
    }

    /// The size and modification time of a regular file, looked at without following links: what
    /// a caller passes back as [`Expect`].
    ///
    /// # Errors
    /// `source_missing`, `not_a_regular_file`, `symlink_rejected`, `volume_offline`.
    pub async fn stat(&self, loc: &Loc) -> Result<FileStat, FsOpError> {
        let root = self.volume(loc.volume_id)?;
        let rel = loc.rel.clone();
        blocking(move || stat_source(&root, &rel, None)).await
    }

    /// Runs one transfer: validate, make the destination folders, then place the file, looking
    /// again whenever the destination changed meanwhile.
    pub(crate) async fn transfer(
        &self,
        job: &Job,
        cx: &OpContext,
    ) -> Result<Transferred, FsOpError> {
        if let Some(done) = self.completed_transfer(job, cx.task_id).await? {
            return Ok(done);
        }
        if job.src == job.dst {
            return Ok(Transferred::Skipped(SkipReason::SameLocation));
        }
        cx.ensure_active()?;
        let rules = self.rules();
        let dst_root = self.writable_volume(job.dst.volume_id)?;
        let src_root = match job.mode {
            Mode::Copy => self.volume(job.src.volume_id)?,
            Mode::Move | Mode::Restore => self.writable_volume(job.src.volume_id)?,
        };
        let reserved = JunkRules::reserved_only();
        let judge = if job.mode == Mode::Restore {
            &reserved
        } else {
            &rules.junk
        };
        validate_file_path(&rules, judge, &job.dst.rel)?;
        rules.check_included(&job.src)?;
        rules.check_included(&job.dst)?;
        if job.mode != Mode::Restore && in_trash(&job.src.rel) {
            return Err(FsOpError::DestinationInvalid(
                "the source is inside the trash: use restore",
            ));
        }
        let src_stat = {
            let (root, rel, expect) = (Arc::clone(&src_root), job.src.rel.clone(), job.expect);
            blocking(move || stat_source(&root, &rel, expect)).await?
        };
        let created = {
            let (root, dir) = (Arc::clone(&dst_root), job.dst.dir());
            blocking(move || make_dirs(&root, &dir)).await?
        };
        let pre = match &job.restore {
            Some(info) => info.snap.clone(),
            None => {
                let mut conn = self.db.read().acquire().await?;
                load_snap(&mut conn, &job.src).await?.map(|(_, snap)| snap)
            }
        };
        let tail = job
            .companions
            .iter()
            .map(Companion::tail)
            .max()
            .unwrap_or(0);
        let mut run = Run {
            job,
            cx,
            rules: &rules,
            max: rules.name_max_len().saturating_sub(tail),
            src_root,
            dst_root,
            src_stat,
            pre,
            indexed: !rules.junk.is_junk_path(&job.dst.rel),
            target: job.dst.clone(),
            renamed: false,
            created,
            guard: None,
            checked_target: None,
        };
        for _ in 0..ATTEMPTS {
            if let Err(error) = cx.ensure_active() {
                if let (Some(guard), Some(target)) = (&self.move_guard, &run.checked_target) {
                    guard.settled(&job.src, target, false).await?;
                }
                return Err(error);
            }
            run.target = job.dst.clone();
            run.renamed = false;
            if let Flow::Done(done) = self.attempt(&mut run).await? {
                return Ok(done);
            }
        }
        if let (Some(guard), Some(target)) = (&self.move_guard, &run.checked_target) {
            guard.settled(&job.src, target, false).await?;
        }
        Err(FsOpError::DestinationInvalid(
            "the destination kept changing",
        ))
    }

    /// One round: look at the destination, decide, place.
    async fn attempt(&self, run: &mut Run<'_>) -> Result<Flow, FsOpError> {
        let probe = self.probe(run).await?;
        let resolution = self.resolve_conflict(run, probe, None).await?;
        if !matches!(resolution, Resolution::Skip(_) | Resolution::Retry)
            && let Err(error) = self.guard_run(run).await
        {
            if let Some(guard) = &self.move_guard {
                guard.settled(&run.job.src, &run.target, false).await?;
            }
            return Err(error);
        }
        let result = match resolution {
            Resolution::Skip(reason) => Ok(Flow::Done(Transferred::Skipped(reason))),
            Resolution::Retry => Ok(Flow::Retry),
            Resolution::Free => match run.job.mode {
                Mode::Copy => self.copy_path(run, None).await,
                Mode::Move | Mode::Restore => self.rename_path(run, false).await,
            },
            Resolution::CaseOnly => self.rename_path(run, true).await,
            Resolution::Replace(existing) => self.replace(run, existing).await,
        };
        if let Some(guard) = &self.move_guard {
            let identical = matches!(
                result,
                Ok(Flow::Done(Transferred::Skipped(
                    SkipReason::IdenticalAtDestination
                )))
            );
            // A simulated crash must preserve the intent for journal replay.
            if !matches!(result, Ok(Flow::Retry))
                && !result.as_ref().is_err_and(|error| error.is_abort())
            {
                guard.settled(&run.job.src, &run.target, identical).await?;
            }
        }
        result
    }

    /// Reuse the same successful preflight after replacement; an outage after
    /// trashing a victim must not produce a false "no file was moved" refusal.
    /// A late cross-device conflict picking a NEW name needs its own preflight.
    pub(crate) async fn guard_run(&self, run: &mut Run<'_>) -> Result<(), FsOpError> {
        if run.job.mode != Mode::Move
            || self.move_guard.is_none()
            || run.checked_target.as_ref() == Some(&run.target)
        {
            return Ok(());
        }
        if let (Some(guard), Some(previous)) = (&self.move_guard, &run.checked_target) {
            guard.settled(&run.job.src, previous, false).await?;
        }
        let sidecars = run
            .job
            .companions
            .iter()
            .map(|companion| {
                Ok((
                    run.job.src.with_name(&companion.sidecar)?,
                    run.target
                        .with_name(&companion.destination(run.target.name()))?,
                ))
            })
            .collect::<Result<Vec<_>, FsOpError>>()?;
        tokio::select! {
            () = run.cx.cancel.cancelled() => Err(FsOpError::Cancelled),
            result = self.guard_move(&run.job.src, &run.target, &sidecars) => result,
        }?;
        run.checked_target = Some(run.target.clone());
        Ok(())
    }

    /// Replaces the existing destination file. A rename trashes it first and then looks again
    /// (the name is free now, or somebody took it and the decision is made anew). A copy, or a
    /// move across file systems, copies and verifies FIRST and trashes the existing file only
    /// right before the final rename, so a failed copy replaces nothing.
    async fn replace(&self, run: &mut Run<'_>, existing: FileStat) -> Result<Flow, FsOpError> {
        let crosses = self.mover.crosses_devices(
            run.src_root.path(),
            run.src_stat.dev,
            run.dst_root.path(),
            existing.dev,
        );
        if run.job.mode == Mode::Copy || crosses {
            return self.copy_path(run, Some(existing)).await;
        }
        // Do not trash what is there for a source that is gone or different.
        self.recheck_source(run).await?;
        match self.trash_existing(run, existing).await {
            Ok(()) => Ok(Flow::Retry),
            // The destination changed since it was looked at.
            Err(
                FsOpError::SourceMissing | FsOpError::SourceChanged | FsOpError::NotARegularFile,
            ) => Ok(Flow::Retry),
            Err(err) => Err(err),
        }
    }

    /// The source must still be the file the operation started with.
    async fn recheck_source(&self, run: &Run<'_>) -> Result<(), FsOpError> {
        let (root, rel) = (Arc::clone(&run.src_root), run.job.src.rel.clone());
        let before = run.src_stat;
        blocking(move || {
            if stat_source(&root, &rel, None)?.same_file(&before) {
                Ok(())
            } else {
                Err(FsOpError::SourceChanged)
            }
        })
        .await
    }

    pub(crate) async fn probe(&self, run: &Run<'_>) -> Result<Probe, FsOpError> {
        let (root, rel) = (Arc::clone(&run.dst_root), run.target.rel.clone());
        blocking(move || probe_dest(&root, &rel)).await
    }

    /// Decides what to do about what is at `run.target`, possibly picking a new name (which
    /// changes `run.target`). `known` is the content hash of the incoming file when the caller
    /// has already computed it (a finished copy).
    pub(crate) async fn resolve_conflict(
        &self,
        run: &mut Run<'_>,
        probe: Probe,
        known: Option<Sha1Digest>,
    ) -> Result<Resolution, FsOpError> {
        match probe {
            Probe::Free => Ok(Resolution::Free),
            Probe::Other => match run.job.policy {
                ConflictPolicy::Skip => Ok(Resolution::Skip(SkipReason::DestinationExists)),
                ConflictPolicy::Smart | ConflictPolicy::Rename => self.rename_around(run).await,
                ConflictPolicy::Replace => Err(FsOpError::DestinationInvalid(
                    "a folder, link or special file is in the way",
                )),
            },
            Probe::Regular(existing) => self.resolve_regular(run, existing, known).await,
        }
    }

    /// Picks the first free numbered name for the PLANNED name, leaving the names of the
    /// companions free too; or skips when the name may not change.
    async fn rename_around(&self, run: &mut Run<'_>) -> Result<Resolution, FsOpError> {
        if !run.job.allow_rename {
            return Ok(Resolution::Skip(SkipReason::DestinationExists));
        }
        let (root, dir) = (Arc::clone(&run.dst_root), run.job.dst.dir());
        let (name, max) = (run.job.dst.name().to_owned(), run.max);
        let companions = run.job.companions.clone();
        let free = blocking(move || {
            free_name_with(&root, &dir, &name, max, &|candidate| {
                companions
                    .iter()
                    .map(|companion| companion.destination(candidate))
                    .collect()
            })
        })
        .await?;
        run.renamed = free != run.job.dst.name();
        run.target = run.job.dst.with_name(&free)?;
        Ok(Resolution::Free)
    }

    async fn resolve_regular(
        &self,
        run: &mut Run<'_>,
        existing: FileStat,
        known: Option<Sha1Digest>,
    ) -> Result<Resolution, FsOpError> {
        if existing.dev == run.src_stat.dev && existing.ino == run.src_stat.ino {
            return self.resolve_same_inode(run).await;
        }
        let policy = run.job.policy;
        let mut incoming = ContentInfo {
            size: run.src_stat.size,
            hash: known.or_else(|| {
                run.pre
                    .as_ref()
                    .filter(|snap| snap.content_matches(&run.src_stat))
                    .and_then(Snap::digest)
            }),
        };
        let mut present = ContentInfo {
            size: existing.size,
            hash: None,
        };
        if needs_hashes(policy, Some(present), incoming) {
            present.hash = self.indexed_hash(run, &existing).await?;
        }
        if needs_hashes(policy, Some(present), incoming) {
            if incoming.hash.is_none() {
                incoming.hash = Some(self.hash_source(run).await?);
            }
            if present.hash.is_none() {
                let (root, rel) = (Arc::clone(&run.dst_root), run.target.rel.clone());
                let cancel = run.cx.cancel.clone();
                let hashed = blocking(move || hash_file(&root, &rel, &existing, &cancel)).await;
                match hashed {
                    Ok(digest) => present.hash = Some(digest),
                    // The file at the destination vanished, changed or became something else
                    // while it was read: look again.
                    Err(
                        FsOpError::SourceMissing
                        | FsOpError::SourceChanged
                        | FsOpError::NotARegularFile
                        | FsOpError::SymlinkRejected,
                    ) => return Ok(Resolution::Retry),
                    Err(err) => return Err(err),
                }
            }
        }
        match decide(policy, Some(present), incoming) {
            ConflictDecision::Proceed => Ok(Resolution::Free),
            ConflictDecision::Skip(conflict::SkipReason::Identical) => {
                Ok(Resolution::Skip(SkipReason::IdenticalAtDestination))
            }
            ConflictDecision::Skip(conflict::SkipReason::Policy) => {
                Ok(Resolution::Skip(SkipReason::DestinationExists))
            }
            ConflictDecision::Rename => self.rename_around(run).await,
            ConflictDecision::ReplaceExisting => Ok(Resolution::Replace(existing)),
        }
    }

    /// The destination and the source are the same inode. A hard link elsewhere in the folder is
    /// identical content; one file reached under a name that differs in case (a case-insensitive
    /// file system) is the same place, or a case-only rename for a move within one folder.
    async fn resolve_same_inode(&self, run: &Run<'_>) -> Result<Resolution, FsOpError> {
        let (root, dir, ino, candidates) = (
            Arc::clone(&run.dst_root),
            run.target.dir(),
            run.src_stat.ino,
            vec![run.job.src.name().to_owned(), run.target.name().to_owned()],
        );
        let names = blocking(move || names_with_inode(&root, &dir, ino, &candidates)).await?;
        let exact = names
            .iter()
            .any(|name| name.as_slice() == run.target.name().as_bytes());
        if run.job.mode == Mode::Copy {
            return Ok(Resolution::Skip(if exact {
                SkipReason::IdenticalAtDestination
            } else {
                SkipReason::SameLocation
            }));
        }
        let own = run.job.src.name().as_bytes();
        let alone = names.len() == 1 && names.first().is_some_and(|name| name == own);
        let same_dir =
            run.job.src.volume_id == run.target.volume_id && run.job.src.dir() == run.target.dir();
        Ok(if !exact && alone && same_dir {
            Resolution::CaseOnly
        } else {
            Resolution::Skip(SkipReason::IdenticalAtDestination)
        })
    }

    /// The hash the index has for the file at the destination, when the row still describes the
    /// file on disk (the test the scanner itself uses to keep a hash).
    async fn indexed_hash(
        &self,
        run: &Run<'_>,
        existing: &FileStat,
    ) -> Result<Option<Sha1Digest>, FsOpError> {
        let mut conn = self.db.read().acquire().await?;
        let row = file_at(&mut conn, &run.target).await?;
        Ok(row
            .filter(|row| row.size == to_i64(existing.size) && row.mtime == existing.mtime)
            .and_then(|row| row.hash.as_deref().and_then(hash::from_slice)))
    }

    async fn hash_source(&self, run: &Run<'_>) -> Result<Sha1Digest, FsOpError> {
        let (root, rel) = (Arc::clone(&run.src_root), run.job.src.rel.clone());
        let (expected, cancel) = (run.src_stat, run.cx.cancel.clone());
        blocking(move || hash_file(&root, &rel, &expected, &cancel)).await
    }

    /// Opens the journal row of this operation (once), tells the watcher which folders are about
    /// to change, and fires the first failpoint. Hands back the row's id.
    pub(crate) async fn open_row(&self, run: &mut Run<'_>) -> Result<i64, FsOpError> {
        if let Some(guard) = &run.guard {
            return Ok(guard.id());
        }
        let op = match run.job.mode {
            Mode::Move => Op::Move,
            Mode::Copy => Op::Copy,
            Mode::Restore => Op::Restore,
        };
        let guard = self
            .journal
            .begin(NewEntry {
                task_id: run.cx.task_id,
                op,
                src: Some(&run.job.src),
                dst: Some(&run.target),
                size: Some(run.src_stat.size),
            })
            .await?;
        let id = guard.id();
        run.guard = Some(guard);
        let src = (run.job.mode == Mode::Move).then_some(&run.job.src);
        for (volume_id, dir) in touched_dirs(src, Some(&run.target), &run.created) {
            self.note_dirs(volume_id, std::slice::from_ref(&dir));
        }
        if let Err(err) = self.check(Step::AfterJournal) {
            return Err(self.fail_row(id, err).await);
        }
        Ok(id)
    }

    /// Marks the row failed (not for a simulated crash, which leaves it as a killed process
    /// would) and hands the error back.
    pub(crate) async fn fail_row(&self, id: i64, err: FsOpError) -> FsOpError {
        if !err.is_abort() {
            self.journal.fail(id, &err).await;
        }
        err
    }

    /// Moves the source to the target with a no-replace rename.
    async fn rename_path(&self, run: &mut Run<'_>, case_only: bool) -> Result<Flow, FsOpError> {
        let id = self.open_row(run).await?;
        let placed = {
            let (src_root, dst_root) = (Arc::clone(&run.src_root), Arc::clone(&run.dst_root));
            let mover = Arc::clone(&self.mover);
            let (src, dst) = (run.job.src.rel.clone(), run.target.rel.clone());
            let expected = run.src_stat;
            blocking(move || {
                rename_into_place(
                    &src_root,
                    &dst_root,
                    mover.as_ref(),
                    &src,
                    &dst,
                    &expected,
                    case_only,
                )
            })
            .await
        };
        match placed {
            Err(err) => return Err(self.fail_row(id, err).await),
            Ok(Placed::Raced) => {
                self.journal
                    .cancel(id, "the destination was taken meanwhile")
                    .await;
                run.guard = None;
                return Ok(Flow::Retry);
            }
            Ok(Placed::CrossDevice) => return self.copy_path(run, None).await,
            Ok(Placed::Done) => {}
        }
        // From here on the file is at its destination: nothing is rolled back, and an error
        // leaves the row pending for `reconcile` to finish.
        self.check(Step::AfterFinalRename)?;
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
                    dst_stat: &run.src_stat,
                    hash: None,
                    restore_item: run.job.restore.as_ref().map(|info| info.item_id),
                    kept: None,
                    crossing: run.job.crossing,
                    indexed: run.indexed,
                },
            )
            .await?;
        let src = (run.job.mode == Mode::Move).then_some(&run.job.src);
        self.announce(run.cx, &touched_dirs(src, Some(&run.target), &run.created));
        run.cx.report_bytes(run.src_stat.size);
        Ok(Flow::Done(Transferred::Done {
            dst: run.target.clone(),
            renamed: run.renamed,
            cross_device: false,
            bytes: run.src_stat.size,
            indexed,
        }))
    }
}
