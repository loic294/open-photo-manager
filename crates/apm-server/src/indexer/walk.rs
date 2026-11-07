//! The walker: lists a directory tree on a blocking thread, one directory at a time, and streams
//! the listings to the scanner through a bounded channel (so memory stays flat and the database
//! work overlaps the disk work).
//!
//! # What is listed
//! Regular files and sub-directories, nothing else:
//! - Symbolic links are never indexed and never followed (files, directories, loops and dangling
//!   links alike). `DirEntry::file_type` does not follow links, and a directory is `lstat`-ed again
//!   right before it is read, so a link swapped in after its parent was listed is not followed
//!   either. Child paths are derived from the (checked) path of the parent and a name that came
//!   from `read_dir`; they never contain `..` or a separator.
//! - Special files (sockets, pipes, devices) are skipped.
//! - Junk is skipped by the effective settings ([`JunkRules`]: `.DS_Store`, `@eaDir`, `._*`, the
//!   built-in `.apm-trash` folder and `*.apm-part` partial files, ...). A junk directory is not
//!   descended into and gets no row. The probe files that volume discovery creates
//!   ([`PROBE_PREFIX`]) are skipped too.
//! - A name that is not UTF-8 cannot be a [`RelPath`] component: it is skipped and counted in
//!   [`DirListing::skipped_invalid`].
//!
//! Names are reported exactly as the file system returns them (macOS stores NFD, nothing here
//! normalises). Entries are sorted by name, so the order is deterministic.
//!
//! # Failure and completeness
//! A directory that cannot be listed produces a [`DirListing`] with `error` set instead of
//! partial contents: the scanner then leaves everything below it alone. [`WalkEnd`] says how the
//! walk ended; only [`WalkEnd::Complete`] allows the scanner to conclude that what it did not see
//! is gone.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use apm_core::dto::FileKind;
use apm_core::junk::JunkRules;
use apm_core::paths::RelPath;
use apm_core::sidecar::SidecarMatcher;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::volumes::PROBE_PREFIX;
use crate::security::{FsError, VolumeRoot};

/// What to walk.
pub(crate) struct WalkSpec {
    pub root: Arc<VolumeRoot>,
    /// The directory to start at (the volume root for a whole-volume scan).
    pub start: RelPath,
    /// Descend into sub-directories. When false only `start` is listed, its sub-directories are
    /// reported by name in [`DirListing::subdirs`].
    pub recursive: bool,
    pub junk: JunkRules,
    pub sidecars: SidecarMatcher,
}

/// One file as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileEntry {
    /// Exactly as on disk.
    pub name: String,
    /// The `files.kind` under the current sidecar settings.
    pub kind: FileKind,
    pub size: i64,
    /// Modification time, epoch seconds.
    pub mtime: i64,
    /// Birth time, epoch seconds, when the platform exposes it.
    pub btime: Option<i64>,
}

/// One directory as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirListing {
    pub dir: RelPath,
    /// The directory's own modification time, epoch seconds.
    pub mtime: Option<i64>,
    /// Sorted by name.
    pub files: Vec<FileEntry>,
    /// Names of the sub-directories (junk and links left out), sorted. Listed whether or not the
    /// walk descends into them.
    pub subdirs: Vec<String>,
    /// Entries skipped because their name is not valid UTF-8.
    pub skipped_invalid: u64,
    /// Why the directory could not be listed. `NotFound` means it vanished (or turned into
    /// something else) after its parent was listed; the other kinds mean it exists but cannot be
    /// read. When set, `files` and `subdirs` are empty: a partial listing is never reported.
    pub error: Option<io::ErrorKind>,
}

/// How a walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkEnd {
    /// Every directory in scope was listed (some may carry an `error`).
    Complete,
    Cancelled,
    /// The scanner stopped listening (it failed or was dropped).
    ReceiverGone,
    /// The volume root cannot be read: the volume is effectively offline, nothing may be
    /// concluded about its files.
    RootUnavailable(io::ErrorKind),
    /// The volume is readable but the start directory does not exist (anymore), is not a
    /// directory, or is (behind) a symbolic link.
    StartGone,
    /// The start directory exists but cannot be read.
    StartUnreadable(io::ErrorKind),
}

/// Walks `spec` on the current (blocking) thread, sending one [`DirListing`] per directory,
/// parents before their children. Checks `cancel` between directories.
pub(crate) fn walk(
    spec: &WalkSpec,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<DirListing>,
) -> WalkEnd {
    if let Err(err) = spec.root.resolve_dir(&RelPath::root()) {
        return WalkEnd::RootUnavailable(err.kind());
    }
    let start = match spec.root.resolve_dir(&spec.start) {
        Ok(path) => path,
        Err(FsError::NotFound(_) | FsError::NotADirectory(_) | FsError::SymlinkRejected(_)) => {
            return WalkEnd::StartGone;
        }
        Err(err) => return WalkEnd::StartUnreadable(err.kind()),
    };

    let mut stack = vec![(spec.start.clone(), start)];
    let mut first = true;
    while let Some((dir, abs)) = stack.pop() {
        if cancel.is_cancelled() {
            return WalkEnd::Cancelled;
        }
        let listing = list_dir(spec, &dir, &abs);
        if first {
            first = false;
            if let Some(kind) = listing.error {
                return start_failure(&spec.start, kind);
            }
        }
        if spec.recursive && listing.error.is_none() {
            for name in listing.subdirs.iter().rev() {
                if let Ok(child) = dir.join(name) {
                    stack.push((child, abs.join(name)));
                }
            }
        }
        if tx.blocking_send(listing).is_err() {
            return WalkEnd::ReceiverGone;
        }
    }
    WalkEnd::Complete
}

fn start_failure(start: &RelPath, kind: io::ErrorKind) -> WalkEnd {
    if start.is_root() {
        WalkEnd::RootUnavailable(kind)
    } else if matches!(kind, io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) {
        WalkEnd::StartGone
    } else {
        WalkEnd::StartUnreadable(kind)
    }
}

fn failed(dir: &RelPath, kind: io::ErrorKind) -> DirListing {
    DirListing {
        dir: dir.clone(),
        mtime: None,
        files: Vec::new(),
        subdirs: Vec::new(),
        skipped_invalid: 0,
        error: Some(kind),
    }
}

/// Lists one directory. `abs` is the absolute path derived from the checked path of its parent.
fn list_dir(spec: &WalkSpec, dir: &RelPath, abs: &Path) -> DirListing {
    let mtime = match fs::symlink_metadata(abs) {
        Ok(meta) if meta.is_dir() => meta.mtime(),
        Ok(_) => return failed(dir, io::ErrorKind::NotFound),
        Err(err) => return failed(dir, err.kind()),
    };
    let entries = match fs::read_dir(abs) {
        Ok(entries) => entries,
        Err(err) => return failed(dir, err.kind()),
    };

    let mut files = Vec::new();
    let mut subdirs = Vec::new();
    let mut skipped_invalid = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return failed(dir, err.kind()),
        };
        let Ok(name) = entry.file_name().into_string() else {
            skipped_invalid += 1;
            continue;
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return failed(dir, err.kind()),
        };
        if file_type.is_dir() {
            if spec.junk.is_junk_dir(&name) {
                continue;
            }
            if dir.join(&name).is_err() {
                skipped_invalid += 1;
                continue;
            }
            subdirs.push(name);
        } else if file_type.is_file() {
            if spec.junk.is_junk_name(&name) || name.starts_with(PROBE_PREFIX) {
                continue;
            }
            // `DirEntry::metadata` does not follow links.
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return failed(dir, err.kind()),
            };
            if !meta.is_file() {
                continue;
            }
            files.push(FileEntry {
                kind: spec.sidecars.file_kind(&name),
                size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
                mtime: meta.mtime(),
                btime: meta.created().ok().and_then(epoch_secs),
                name,
            });
        }
    }
    files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    subdirs.sort_unstable();
    DirListing {
        dir: dir.clone(),
        mtime: Some(mtime),
        files,
        subdirs,
        skipped_invalid,
        error: None,
    }
}

fn epoch_secs(time: SystemTime) -> Option<i64> {
    let secs = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::indexer::test_support::{scratch_dir, touch};

    struct Walked {
        listings: Vec<DirListing>,
        end: WalkEnd,
    }

    async fn run(root: &Path, start: &str, recursive: bool) -> Walked {
        run_with(root, start, recursive, CancellationToken::new(), 64).await
    }

    async fn run_with(
        root: &Path,
        start: &str,
        recursive: bool,
        cancel: CancellationToken,
        capacity: usize,
    ) -> Walked {
        let spec = WalkSpec {
            root: Arc::new(VolumeRoot::new(1, root).unwrap()),
            start: RelPath::new(start).unwrap(),
            recursive,
            junk: JunkRules::default(),
            sidecars: SidecarMatcher::default(),
        };
        let (tx, mut rx) = mpsc::channel(capacity);
        let handle = tokio::task::spawn_blocking(move || walk(&spec, &cancel, &tx));
        let mut listings = Vec::new();
        while let Some(listing) = rx.recv().await {
            listings.push(listing);
        }
        Walked {
            listings,
            end: handle.await.unwrap(),
        }
    }

    fn names(listing: &DirListing) -> Vec<&str> {
        listing
            .files
            .iter()
            .map(|file| file.name.as_str())
            .collect()
    }

    fn dirs(walked: &Walked) -> Vec<&str> {
        walked
            .listings
            .iter()
            .map(|listing| listing.dir.as_str())
            .collect()
    }

    #[tokio::test]
    async fn lists_files_and_directories_depth_first_and_sorted() {
        let tmp = scratch_dir();
        touch(tmp.path(), "b.jpg", b"bb");
        touch(tmp.path(), "a.jpg", b"a");
        touch(tmp.path(), "2024/Trip/x.cr3", b"xxx");
        touch(tmp.path(), "2024/y.mp4", b"y");
        touch(tmp.path(), "2023/z.txt", b"z");
        fs::create_dir_all(tmp.path().join("empty")).unwrap();

        let walked = run(tmp.path(), "", true).await;

        assert_eq!(walked.end, WalkEnd::Complete);
        assert_eq!(
            dirs(&walked),
            ["", "2023", "2024", "2024/Trip", "empty"],
            "parents before children, siblings in name order"
        );
        let root = &walked.listings[0];
        assert_eq!(names(root), ["a.jpg", "b.jpg"]);
        assert_eq!(root.subdirs, ["2023", "2024", "empty"]);
        assert_eq!(root.files[0].kind, FileKind::Image);
        assert_eq!(root.files[1].size, 2);
        assert!(root.mtime.is_some());
        assert_eq!(names(&walked.listings[2]), ["y.mp4"]);
        assert_eq!(walked.listings[2].files[0].kind, FileKind::Video);
        assert_eq!(walked.listings[3].files[0].kind, FileKind::Image);
        assert_eq!(walked.listings[1].files[0].kind, FileKind::Other);
        assert!(walked.listings[4].files.is_empty());
        assert!(walked.listings.iter().all(|l| l.error.is_none()));
    }

    #[tokio::test]
    async fn a_non_recursive_walk_lists_one_directory_and_names_its_subdirectories() {
        let tmp = scratch_dir();
        touch(tmp.path(), "top.jpg", b"t");
        touch(tmp.path(), "sub/deep.jpg", b"d");

        let walked = run(tmp.path(), "", false).await;

        assert_eq!(walked.end, WalkEnd::Complete);
        assert_eq!(dirs(&walked), [""]);
        assert_eq!(walked.listings[0].subdirs, ["sub"]);
        assert_eq!(names(&walked.listings[0]), ["top.jpg"]);
    }

    #[tokio::test]
    async fn starts_at_a_sub_directory() {
        let tmp = scratch_dir();
        touch(tmp.path(), "top.jpg", b"t");
        touch(tmp.path(), "a/b/c.jpg", b"c");
        touch(tmp.path(), "a/d.jpg", b"d");

        let walked = run(tmp.path(), "a", true).await;

        assert_eq!(walked.end, WalkEnd::Complete);
        assert_eq!(dirs(&walked), ["a", "a/b"]);
    }

    #[tokio::test]
    async fn links_are_never_indexed_or_followed() {
        let tmp = scratch_dir();
        let outside = scratch_dir();
        touch(outside.path(), "secret.jpg", b"s");
        touch(tmp.path(), "real.jpg", b"r");
        touch(tmp.path(), "dir/inner.jpg", b"i");
        symlink(
            outside.path().join("secret.jpg"),
            tmp.path().join("link.jpg"),
        )
        .unwrap();
        symlink(outside.path(), tmp.path().join("out")).unwrap();
        symlink(tmp.path(), tmp.path().join("dir/loop")).unwrap();
        symlink("missing", tmp.path().join("dangling")).unwrap();

        let walked = run(tmp.path(), "", true).await;

        assert_eq!(walked.end, WalkEnd::Complete);
        assert_eq!(dirs(&walked), ["", "dir"]);
        assert_eq!(names(&walked.listings[0]), ["real.jpg"]);
        assert_eq!(walked.listings[0].subdirs, ["dir"]);
        assert_eq!(walked.listings[1].subdirs, Vec::<String>::new());
        assert_eq!(names(&walked.listings[1]), ["inner.jpg"]);
    }

    #[tokio::test]
    async fn junk_partial_files_trash_and_probes_are_skipped() {
        let tmp = scratch_dir();
        touch(tmp.path(), "keep.jpg", b"k");
        touch(tmp.path(), ".DS_Store", b"j");
        touch(tmp.path(), "._keep.jpg", b"j");
        touch(tmp.path(), "movie.mp4.apm-part", b"p");
        touch(tmp.path(), ".apm-probe-1-2", b"p");
        touch(tmp.path(), ".apm-trash/20260101/gone.jpg", b"t");
        touch(tmp.path(), "@eaDir/keep.jpg/SYNOPHOTO_THUMB_M.jpg", b"t");

        let walked = run(tmp.path(), "", true).await;

        assert_eq!(walked.end, WalkEnd::Complete);
        assert_eq!(dirs(&walked), [""]);
        assert_eq!(names(&walked.listings[0]), ["keep.jpg"]);
        assert!(walked.listings[0].subdirs.is_empty());
    }

    #[tokio::test]
    async fn names_are_reported_exactly_as_the_file_system_returns_them() {
        let tmp = scratch_dir();
        let nfd = "Cafe\u{301}.jpg";
        touch(tmp.path(), nfd, b"c");

        let walked = run(tmp.path(), "", true).await;

        let found = names(&walked.listings[0]);
        assert_eq!(found.len(), 1);
        assert!(
            found[0] == nfd || found[0] == "Caf\u{e9}.jpg",
            "unexpected name {found:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn names_that_are_not_utf8_are_skipped_and_counted() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let tmp = scratch_dir();
        touch(tmp.path(), "ok.jpg", b"k");
        fs::write(tmp.path().join(OsStr::from_bytes(b"bad\xff.jpg")), b"x").unwrap();

        let walked = run(tmp.path(), "", true).await;

        assert_eq!(names(&walked.listings[0]), ["ok.jpg"]);
        assert_eq!(walked.listings[0].skipped_invalid, 1);
    }

    #[tokio::test]
    async fn a_missing_start_is_gone_and_a_missing_root_is_unavailable() {
        let tmp = scratch_dir();
        touch(tmp.path(), "a.jpg", b"a");
        touch(tmp.path(), "file.txt", b"f");

        assert_eq!(run(tmp.path(), "nope", true).await.end, WalkEnd::StartGone);
        assert_eq!(
            run(tmp.path(), "file.txt", true).await.end,
            WalkEnd::StartGone,
            "a file is not a directory"
        );
        let target = scratch_dir();
        symlink(target.path(), tmp.path().join("linked")).unwrap();
        assert_eq!(
            run(tmp.path(), "linked", true).await.end,
            WalkEnd::StartGone,
            "a link is never walked"
        );

        let root = tmp.path().to_path_buf();
        let spec_root = Arc::new(VolumeRoot::new(1, &root).unwrap());
        drop(tmp);
        let spec = WalkSpec {
            root: spec_root,
            start: RelPath::root(),
            recursive: true,
            junk: JunkRules::default(),
            sidecars: SidecarMatcher::default(),
        };
        let (tx, mut rx) = mpsc::channel(4);
        let end = tokio::task::spawn_blocking(move || walk(&spec, &CancellationToken::new(), &tx))
            .await
            .unwrap();
        assert!(rx.recv().await.is_none());
        assert!(matches!(end, WalkEnd::RootUnavailable(_)), "{end:?}");
    }

    #[tokio::test]
    async fn an_unreadable_directory_is_reported_not_skipped() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = scratch_dir();
        touch(tmp.path(), "open/a.jpg", b"a");
        touch(tmp.path(), "locked/b.jpg", b"b");
        let locked = tmp.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = fs::read_dir(&locked).is_ok();

        let walked = run(tmp.path(), "", true).await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(walked.end, WalkEnd::Complete);
        if readable_anyway {
            return; // running as root: permissions do not apply
        }
        assert_eq!(dirs(&walked), ["", "locked", "open"]);
        let locked = &walked.listings[1];
        assert_eq!(locked.error, Some(io::ErrorKind::PermissionDenied));
        assert!(locked.files.is_empty() && locked.subdirs.is_empty());
    }

    #[tokio::test]
    async fn cancelling_stops_the_walk() {
        let tmp = scratch_dir();
        for index in 0..50 {
            touch(tmp.path(), &format!("d{index:02}/f.jpg"), b"f");
        }
        let cancel = CancellationToken::new();
        cancel.cancel();

        let walked = run_with(tmp.path(), "", true, cancel, 4).await;

        assert_eq!(walked.end, WalkEnd::Cancelled);
        assert!(walked.listings.is_empty());
    }

    #[tokio::test]
    async fn a_dropped_receiver_stops_the_walk() {
        let tmp = scratch_dir();
        for index in 0..20 {
            touch(tmp.path(), &format!("d{index:02}/f.jpg"), b"f");
        }
        let spec = WalkSpec {
            root: Arc::new(VolumeRoot::new(1, tmp.path()).unwrap()),
            start: RelPath::root(),
            recursive: true,
            junk: JunkRules::default(),
            sidecars: SidecarMatcher::default(),
        };
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let end = tokio::task::spawn_blocking(move || walk(&spec, &CancellationToken::new(), &tx))
            .await
            .unwrap();
        assert_eq!(end, WalkEnd::ReceiverGone);
    }
}
