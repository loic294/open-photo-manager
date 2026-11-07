//! [`VolumeRoot`]: the ONLY way to turn a (volume, [`RelPath`]) pair into a
//! filesystem path.
//!
//! # Policy
//!
//! Symbolic links are never followed anywhere inside a volume. A link inside a
//! library is foreign content (the scanner skips them) and following one is how
//! a path escapes its volume. Every checked method therefore walks the path one
//! component at a time with `symlink_metadata` and fails with
//! [`FsError::SymlinkRejected`] when any component, the last one included, is a
//! link. This holds even for a link that points back into the same volume or
//! to nothing at all. The volume root itself may be a link (a bind mount): it is
//! canonicalised once in [`VolumeRoot::new`].
//!
//! The checked methods only look, they never create, change or remove anything.
//!
//! # Rules for callers
//!
//! - Every filesystem access under a volume takes a [`RelPath`] and gets its
//!   absolute path from here. Never glue strings onto a root yourself.
//! - Call a `resolve_*` method immediately before the operation and use the
//!   returned path once. Do not cache it, do not derive further paths from it
//!   with `..`, and do not keep using it after other work was done on the
//!   volume.
//! - Everything here is blocking filesystem I/O (one `lstat` per path
//!   component). A spun-down disk or a network share can make it slow, so async
//!   code that resolves many paths should run in `spawn_blocking`.
//! - Paths in error messages are always volume-relative: absolute host paths
//!   never reach a client.
//!
//! # Residual risk (TOCTOU)
//!
//! A process that can write inside the library can replace a directory with a
//! symlink between the check and the use of the returned path. This design does
//! not defend against that, since an attacker with write access to the photo
//! library already owns the library, and the app has no privileges beyond it.
//! [`VolumeRoot::open_read`] narrows the window for reads: the final component is
//! opened with `O_NOFOLLOW` and the opened file must be the same inode that the
//! walk validated. The upgrade path, if it ever matters, is to resolve through
//! directory file descriptors (`cap-std` / `openat2` with `RESOLVE_BENEATH`).

use std::fmt;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use apm_core::paths::{RelPath, VolumeId};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::error::AppError;

/// Why a volume path could not be resolved. The variants carry the
/// volume-relative path of the offending component (the volume root is empty).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    #[error("{} does not exist", Show(.0))]
    NotFound(RelPath),
    #[error("{} is or contains a symbolic link, which is never followed", Show(.0))]
    SymlinkRejected(RelPath),
    #[error("{} is not a folder", Show(.0))]
    NotADirectory(RelPath),
    #[error("{} is not a regular file", Show(.0))]
    NotAFile(RelPath),
    #[error("{} already exists", Show(.0))]
    AlreadyExists(RelPath),
    #[error("the path is outside the volume")]
    OutsideRoot,
    #[error("{} has an invalid or too long name", Show(.0))]
    InvalidFilename(RelPath),
    #[error("i/o error: {0}")]
    Io(io::ErrorKind),
}

/// Displays a relative path for humans: the root as "the volume root", others
/// quoted.
struct Show<'a>(&'a RelPath);

impl fmt::Display for Show<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_root() {
            f.write_str("the volume root")
        } else {
            write!(f, "'{}'", self.0)
        }
    }
}

impl FsError {
    /// The closest [`io::ErrorKind`], for callers that work with `io::Result`.
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::NotFound(_) => io::ErrorKind::NotFound,
            Self::SymlinkRejected(_) | Self::OutsideRoot => io::ErrorKind::PermissionDenied,
            Self::NotADirectory(_) => io::ErrorKind::NotADirectory,
            Self::NotAFile(_) => io::ErrorKind::InvalidInput,
            Self::AlreadyExists(_) => io::ErrorKind::AlreadyExists,
            Self::InvalidFilename(_) => io::ErrorKind::InvalidFilename,
            Self::Io(kind) => *kind,
        }
    }

    /// Classifies an OS error that happened while looking at `rel`.
    fn from_io(rel: &RelPath, err: &io::Error) -> Self {
        // `O_NOFOLLOW` on a link and a link loop both report ELOOP.
        if Errno::from_io_error(err) == Some(Errno::LOOP) {
            return Self::SymlinkRejected(rel.clone());
        }
        match err.kind() {
            io::ErrorKind::NotFound => Self::NotFound(rel.clone()),
            io::ErrorKind::AlreadyExists => Self::AlreadyExists(rel.clone()),
            io::ErrorKind::NotADirectory => Self::NotADirectory(rel.clone()),
            io::ErrorKind::IsADirectory => Self::NotAFile(rel.clone()),
            io::ErrorKind::InvalidFilename => Self::InvalidFilename(rel.clone()),
            kind => Self::Io(kind),
        }
    }
}

impl From<FsError> for io::Error {
    /// Keeps the typed error inside, so it can be recovered with
    /// `get_ref().and_then(|e| e.downcast_ref::<FsError>())`.
    fn from(err: FsError) -> Self {
        Self::new(err.kind(), err)
    }
}

impl From<FsError> for AppError {
    fn from(err: FsError) -> Self {
        let message = err.to_string();
        match err {
            FsError::NotFound(_) => Self::not_found("not_found", message),
            FsError::SymlinkRejected(_) => Self::forbidden("symlink_rejected", message),
            FsError::NotADirectory(_) => Self::bad_request("not_a_directory", message),
            FsError::NotAFile(_) => Self::bad_request("not_a_file", message),
            FsError::AlreadyExists(_) => Self::conflict("already_exists", message),
            FsError::OutsideRoot => Self::forbidden("outside_volume", message),
            FsError::InvalidFilename(_) => Self::bad_request("invalid_name", message),
            FsError::Io(io::ErrorKind::NotFound) => {
                Self::not_found("not_found", "resource not found")
            }
            FsError::Io(io::ErrorKind::PermissionDenied) => {
                Self::forbidden("forbidden", "permission denied")
            }
            FsError::Io(kind) => Self::internal(io::Error::from(kind)),
        }
    }
}

/// What may already exist at the final component of a path that is about to
/// be written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Existing {
    /// Nothing: the caller creates a new entry.
    Reject,
    /// A regular file: the caller replaces its content.
    RegularFile,
}

/// A mounted library root plus the rules for reaching files below it. See the
/// [module documentation](self) before using it.
///
/// Cheap to clone; share one per volume behind an `Arc`.
#[derive(Debug, Clone)]
pub struct VolumeRoot {
    id: VolumeId,
    root: PathBuf,
    /// The root as it was given, when that differs from the canonical path
    /// (a symlinked mount point). Watcher events may carry either prefix.
    configured: Option<PathBuf>,
}

impl VolumeRoot {
    /// Canonicalises `root` once (it may itself be a symlinked bind mount) and
    /// checks that it is a directory.
    pub fn new(id: VolumeId, root: impl Into<PathBuf>) -> Result<Self, FsError> {
        let configured = root.into();
        let at_root = RelPath::root();
        let canonical =
            fs::canonicalize(&configured).map_err(|err| FsError::from_io(&at_root, &err))?;
        let meta = fs::metadata(&canonical).map_err(|err| FsError::from_io(&at_root, &err))?;
        if !meta.is_dir() {
            return Err(FsError::NotADirectory(at_root));
        }
        let configured =
            (configured.is_absolute() && configured != canonical).then_some(configured);
        Ok(Self {
            id,
            root: canonical,
            configured,
        })
    }

    pub fn id(&self) -> VolumeId {
        self.id
    }

    /// The canonical absolute root of the volume.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Appends `rel` to the root. Purely lexical: no filesystem access, no
    /// symlink check. UNCHECKED: use it only for display, logging and paths
    /// already validated by one of the checked methods below.
    pub fn join(&self, rel: &RelPath) -> PathBuf {
        if rel.is_root() {
            self.root.clone()
        } else {
            self.root.join(rel.as_str())
        }
    }

    /// The absolute path of an existing entry (file, folder or anything else).
    /// Fails with [`FsError::SymlinkRejected`] if any component is a symbolic
    /// link and with [`FsError::NotFound`] (naming the first missing
    /// component) if it does not exist.
    pub fn resolve_existing(&self, rel: &RelPath) -> Result<PathBuf, FsError> {
        self.walk(rel).map(|(path, _)| path)
    }

    /// Like [`resolve_existing`](Self::resolve_existing), but the entry must be
    /// a folder.
    pub fn resolve_dir(&self, rel: &RelPath) -> Result<PathBuf, FsError> {
        let (path, meta) = self.walk(rel)?;
        if meta.is_dir() {
            Ok(path)
        } else {
            Err(FsError::NotADirectory(rel.clone()))
        }
    }

    /// The absolute path of an entry that is about to be CREATED. The parent
    /// must resolve as a folder by the rules of
    /// [`resolve_existing`](Self::resolve_existing); the final component must
    /// not exist ([`FsError::AlreadyExists`]) and must not be a link, not even
    /// a dangling one ([`FsError::SymlinkRejected`]). The root has no parent
    /// and counts as existing. Nothing is created.
    pub fn resolve_for_create(&self, rel: &RelPath) -> Result<PathBuf, FsError> {
        self.resolve_new(rel, Existing::Reject)
    }

    /// Like [`resolve_for_create`](Self::resolve_for_create), but the final
    /// component may also be an existing REGULAR file, for callers with
    /// overwrite semantics. An existing folder or special file is
    /// [`FsError::NotAFile`]. Nothing is created.
    pub fn resolve_for_replace(&self, rel: &RelPath) -> Result<PathBuf, FsError> {
        self.resolve_new(rel, Existing::RegularFile)
    }

    /// Metadata of an existing entry, with the same rules as
    /// [`resolve_existing`](Self::resolve_existing) (never follows a link).
    pub fn stat(&self, rel: &RelPath) -> Result<Metadata, FsError> {
        self.walk(rel).map(|(_, meta)| meta)
    }

    /// Opens a regular file for reading. Rules of
    /// [`resolve_existing`](Self::resolve_existing), plus: the final component is
    /// opened with `O_NOFOLLOW` (and without blocking on a FIFO), the opened
    /// file must be a regular file and the very inode the walk validated;
    /// otherwise the path counts as gone ([`FsError::NotFound`]).
    ///
    /// Returns [`FsError`] rather than a bare `io::Error`; it converts into one
    /// with `?`.
    pub fn open_read(&self, rel: &RelPath) -> Result<File, FsError> {
        let (path, walked) = self.walk(rel)?;
        if !walked.is_file() {
            return Err(FsError::NotAFile(rel.clone()));
        }
        let fd = rustix::fs::open(
            path.as_path(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|errno| FsError::from_io(rel, &io::Error::from(errno)))?;
        let file = File::from(fd);
        let opened = file.metadata().map_err(|err| FsError::from_io(rel, &err))?;
        if !opened.is_file() {
            return Err(FsError::NotAFile(rel.clone()));
        }
        if opened.dev() != walked.dev() || opened.ino() != walked.ino() {
            return Err(FsError::NotFound(rel.clone()));
        }
        // `NONBLOCK` only protected the `open`; hand out an ordinary file.
        let flags = rustix::fs::fcntl_getfl(&file)
            .map_err(|errno| FsError::Io(io::Error::from(errno).kind()))?;
        rustix::fs::fcntl_setfl(&file, flags - OFlags::NONBLOCK)
            .map_err(|errno| FsError::Io(io::Error::from(errno).kind()))?;
        Ok(file)
    }

    /// Maps an absolute path (a watcher event, a tool's output) back to a
    /// [`RelPath`]. Lexical, like [`join`](Self::join): `None` for a relative
    /// path, a path outside the root (also one that merely shares a name prefix
    /// with it), a name that is not UTF-8, or one containing `..` or NUL. The
    /// root may be given as configured or canonical. The result has NOT been
    /// checked against symlinks: pass it to a `resolve_*` method before use.
    pub fn rel_of(&self, path: &Path) -> Option<RelPath> {
        if !path.is_absolute() {
            return None;
        }
        let tail = path.strip_prefix(&self.root).ok().or_else(|| {
            self.configured
                .as_deref()
                .and_then(|configured| path.strip_prefix(configured).ok())
        })?;
        RelPath::new(tail.to_str()?).ok()
    }

    /// Walks `rel` below the root, one `lstat` per component. Returns the
    /// absolute path and the metadata of the last component.
    fn walk(&self, rel: &RelPath) -> Result<(PathBuf, Metadata), FsError> {
        let mut path = self.root.clone();
        let mut prefix = RelPath::root();
        let mut meta = lstat(&path, &prefix)?;
        for name in rel.components() {
            if !meta.is_dir() {
                return Err(FsError::NotADirectory(prefix));
            }
            path.push(name);
            prefix = prefix
                .join(name)
                .map_err(|_| FsError::InvalidFilename(rel.clone()))?;
            meta = lstat(&path, &prefix)?;
        }
        Ok((path, meta))
    }

    fn resolve_new(&self, rel: &RelPath, existing: Existing) -> Result<PathBuf, FsError> {
        let (Some(parent), Some(name)) = (rel.parent(), rel.file_name()) else {
            return Err(match existing {
                Existing::Reject => FsError::AlreadyExists(rel.clone()),
                Existing::RegularFile => FsError::NotAFile(rel.clone()),
            });
        };
        let (mut path, parent_meta) = self.walk(&parent)?;
        if !parent_meta.is_dir() {
            return Err(FsError::NotADirectory(parent));
        }
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => Err(FsError::SymlinkRejected(rel.clone())),
            Ok(meta) => match existing {
                Existing::Reject => Err(FsError::AlreadyExists(rel.clone())),
                Existing::RegularFile if meta.is_file() => Ok(path),
                Existing::RegularFile => Err(FsError::NotAFile(rel.clone())),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(err) => Err(FsError::from_io(rel, &err)),
        }
    }
}

/// `symlink_metadata` that turns a link into [`FsError::SymlinkRejected`].
fn lstat(path: &Path, rel: &RelPath) -> Result<Metadata, FsError> {
    let meta = fs::symlink_metadata(path).map_err(|err| FsError::from_io(rel, &err))?;
    if meta.file_type().is_symlink() {
        return Err(FsError::SymlinkRejected(rel.clone()));
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::Read;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use axum::http::StatusCode;

    use super::*;
    use crate::security::test_support::scratch_dir;

    /// A volume at `<scratch>/vol`, with an `outside` folder next to it that
    /// holds a secret.
    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        vol: VolumeRoot,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = scratch_dir();
            let base = dir.path().to_path_buf();
            fs::create_dir_all(base.join("vol")).unwrap();
            fs::create_dir_all(base.join("outside")).unwrap();
            fs::write(base.join("outside/secret.txt"), "top secret").unwrap();
            let vol = VolumeRoot::new(7, base.join("vol")).unwrap();
            Self {
                _dir: dir,
                base,
                vol,
            }
        }

        fn root(&self) -> PathBuf {
            self.base.join("vol")
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.root().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    fn rel(raw: &str) -> RelPath {
        RelPath::new(raw).unwrap()
    }

    #[test]
    fn new_canonicalises_and_requires_a_directory() {
        let fx = Fixture::new();
        assert_eq!(fx.vol.id(), 7);
        assert_eq!(fx.vol.path(), fx.root().canonicalize().unwrap());

        let missing = VolumeRoot::new(1, fx.base.join("nope")).unwrap_err();
        assert_eq!(missing, FsError::NotFound(RelPath::root()));

        fx.write("file.txt", "x");
        let not_dir = VolumeRoot::new(1, fx.root().join("file.txt")).unwrap_err();
        assert_eq!(not_dir, FsError::NotADirectory(RelPath::root()));
        let below_file = VolumeRoot::new(1, fx.root().join("file.txt/sub")).unwrap_err();
        assert!(
            matches!(below_file, FsError::NotADirectory(_) | FsError::NotFound(_)),
            "{below_file:?}"
        );

        // A dot-laden path is canonicalised.
        let dotted = VolumeRoot::new(1, fx.root().join("../vol/.")).unwrap();
        assert_eq!(dotted.path(), fx.vol.path());
    }

    #[test]
    fn the_root_itself_may_be_a_symlink() {
        let fx = Fixture::new();
        fx.write("a/photo.jpg", "x");
        let link = fx.base.join("mount");
        symlink(fx.root(), &link).unwrap();

        let vol = VolumeRoot::new(2, &link).unwrap();
        assert_eq!(vol.path(), fx.root().canonicalize().unwrap());
        assert_eq!(
            vol.resolve_existing(&rel("a/photo.jpg")).unwrap(),
            vol.path().join("a/photo.jpg")
        );
        // Events may arrive with either prefix.
        assert_eq!(
            vol.rel_of(&link.join("a/photo.jpg")),
            Some(rel("a/photo.jpg"))
        );
        assert_eq!(
            vol.rel_of(&vol.path().join("a/photo.jpg")),
            Some(rel("a/photo.jpg"))
        );
        assert_eq!(vol.rel_of(&link), Some(RelPath::root()));
    }

    #[test]
    fn resolves_normal_files_and_nested_folders() {
        let fx = Fixture::new();
        fx.write("photo.jpg", "x");
        fx.write("2024/Trip/day 1/IMG_0001.JPG", "y");

        let root = fx.vol.path().to_path_buf();
        assert_eq!(fx.vol.resolve_existing(&RelPath::root()).unwrap(), root);
        assert_eq!(
            fx.vol.resolve_existing(&rel("photo.jpg")).unwrap(),
            root.join("photo.jpg")
        );
        let deep = rel("2024/Trip/day 1/IMG_0001.JPG");
        assert_eq!(
            fx.vol.resolve_existing(&deep).unwrap(),
            root.join("2024/Trip/day 1/IMG_0001.JPG")
        );
        assert_eq!(
            fx.vol.resolve_dir(&rel("2024/Trip")).unwrap(),
            root.join("2024/Trip")
        );
        assert!(fx.vol.stat(&deep).unwrap().is_file());
        assert!(fx.vol.stat(&rel("2024")).unwrap().is_dir());
    }

    #[test]
    fn missing_entries_name_the_first_missing_component() {
        let fx = Fixture::new();
        fx.write("a/file.txt", "x");

        assert_eq!(
            fx.vol.resolve_existing(&rel("a/b/c/d.jpg")).unwrap_err(),
            FsError::NotFound(rel("a/b"))
        );
        assert_eq!(
            fx.vol.resolve_existing(&rel("nope.jpg")).unwrap_err(),
            FsError::NotFound(rel("nope.jpg"))
        );
        // Going through a file.
        assert_eq!(
            fx.vol.resolve_existing(&rel("a/file.txt/x")).unwrap_err(),
            FsError::NotADirectory(rel("a/file.txt"))
        );
        assert_eq!(
            fx.vol.resolve_dir(&rel("a/file.txt")).unwrap_err(),
            FsError::NotADirectory(rel("a/file.txt"))
        );
    }

    #[test]
    fn a_directory_symlink_pointing_outside_is_rejected() {
        let fx = Fixture::new();
        symlink(fx.base.join("outside"), fx.root().join("escape")).unwrap();

        for target in ["escape", "escape/secret.txt", "escape/missing.txt"] {
            assert_eq!(
                fx.vol.resolve_existing(&rel(target)).unwrap_err(),
                FsError::SymlinkRejected(rel("escape")),
                "{target}"
            );
        }
        assert_eq!(
            fx.vol.open_read(&rel("escape/secret.txt")).unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
        assert_eq!(
            fx.vol.stat(&rel("escape")).unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
        assert_eq!(
            fx.vol.resolve_dir(&rel("escape")).unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
    }

    #[test]
    fn a_nested_directory_symlink_is_rejected_at_its_own_component() {
        let fx = Fixture::new();
        fx.write("a/real.txt", "x");
        symlink(fx.base.join("outside"), fx.root().join("a/escape")).unwrap();
        assert_eq!(
            fx.vol
                .resolve_existing(&rel("a/escape/secret.txt"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("a/escape"))
        );
    }

    #[test]
    fn file_symlinks_are_rejected_even_inside_the_volume() {
        let fx = Fixture::new();
        fx.write("real.txt", "x");
        symlink(
            fx.root().join("real.txt"),
            fx.root().join("inside-link.txt"),
        )
        .unwrap();
        symlink(
            fx.base.join("outside/secret.txt"),
            fx.root().join("outside-link.txt"),
        )
        .unwrap();
        // A link to a folder of the same volume.
        fx.write("dir/child.txt", "x");
        symlink(fx.root().join("dir"), fx.root().join("dir-link")).unwrap();
        // A relative link.
        symlink("real.txt", fx.root().join("relative-link.txt")).unwrap();

        for name in [
            "inside-link.txt",
            "outside-link.txt",
            "dir-link",
            "relative-link.txt",
        ] {
            assert_eq!(
                fx.vol.resolve_existing(&rel(name)).unwrap_err(),
                FsError::SymlinkRejected(rel(name)),
                "{name}"
            );
            assert_eq!(
                fx.vol.open_read(&rel(name)).unwrap_err(),
                FsError::SymlinkRejected(rel(name)),
                "{name}"
            );
        }
        assert_eq!(
            fx.vol
                .resolve_existing(&rel("dir-link/child.txt"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("dir-link"))
        );
        assert!(fx.vol.resolve_existing(&rel("real.txt")).is_ok());
    }

    #[test]
    fn dangling_and_looping_symlinks_are_rejected_not_reported_missing() {
        let fx = Fixture::new();
        symlink(fx.base.join("does-not-exist"), fx.root().join("dangling")).unwrap();
        symlink(fx.root().join("loop-b"), fx.root().join("loop-a")).unwrap();
        symlink(fx.root().join("loop-a"), fx.root().join("loop-b")).unwrap();

        for name in ["dangling", "loop-a", "loop-b"] {
            assert_eq!(
                fx.vol.resolve_existing(&rel(name)).unwrap_err(),
                FsError::SymlinkRejected(rel(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn creating_under_a_symlinked_parent_is_rejected() {
        let fx = Fixture::new();
        symlink(fx.base.join("outside"), fx.root().join("escape")).unwrap();
        fx.write("dir/exists.txt", "x");

        assert_eq!(
            fx.vol
                .resolve_for_create(&rel("escape/new.txt"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
        assert_eq!(
            fx.vol
                .resolve_for_replace(&rel("escape/secret.txt"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
        // The link itself is not a valid final component either.
        assert_eq!(
            fx.vol.resolve_for_create(&rel("escape")).unwrap_err(),
            FsError::SymlinkRejected(rel("escape"))
        );
        // A dangling link at the destination must not be written through.
        symlink(
            fx.base.join("outside/new-target"),
            fx.root().join("dir/dangling"),
        )
        .unwrap();
        assert_eq!(
            fx.vol.resolve_for_create(&rel("dir/dangling")).unwrap_err(),
            FsError::SymlinkRejected(rel("dir/dangling"))
        );
        assert_eq!(
            fx.vol
                .resolve_for_replace(&rel("dir/dangling"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("dir/dangling"))
        );
        // A link to an existing regular file is rejected for replace as well.
        symlink(
            fx.base.join("outside/secret.txt"),
            fx.root().join("dir/to-secret"),
        )
        .unwrap();
        assert_eq!(
            fx.vol
                .resolve_for_replace(&rel("dir/to-secret"))
                .unwrap_err(),
            FsError::SymlinkRejected(rel("dir/to-secret"))
        );
        // Nothing was created or changed.
        assert!(!fx.base.join("outside/new-target").exists());
        assert_eq!(
            fs::read_to_string(fx.base.join("outside/secret.txt")).unwrap(),
            "top secret"
        );
    }

    #[test]
    fn resolve_for_create_rules() {
        let fx = Fixture::new();
        fx.write("dir/exists.txt", "x");
        fx.write("file.txt", "x");
        let root = fx.vol.path().to_path_buf();

        assert_eq!(
            fx.vol.resolve_for_create(&rel("dir/new.txt")).unwrap(),
            root.join("dir/new.txt")
        );
        assert_eq!(
            fx.vol.resolve_for_create(&rel("top-level.txt")).unwrap(),
            root.join("top-level.txt")
        );
        assert!(!root.join("dir/new.txt").exists(), "nothing is created");
        assert_eq!(
            fx.vol
                .resolve_for_create(&rel("dir/exists.txt"))
                .unwrap_err(),
            FsError::AlreadyExists(rel("dir/exists.txt"))
        );
        assert_eq!(
            fx.vol.resolve_for_create(&rel("dir")).unwrap_err(),
            FsError::AlreadyExists(rel("dir"))
        );
        assert_eq!(
            fx.vol.resolve_for_create(&RelPath::root()).unwrap_err(),
            FsError::AlreadyExists(RelPath::root())
        );
        assert_eq!(
            fx.vol
                .resolve_for_create(&rel("missing/new.txt"))
                .unwrap_err(),
            FsError::NotFound(rel("missing"))
        );
        assert_eq!(
            fx.vol
                .resolve_for_create(&rel("file.txt/new.txt"))
                .unwrap_err(),
            FsError::NotADirectory(rel("file.txt"))
        );
    }

    #[test]
    fn resolve_for_replace_allows_only_regular_files() {
        let fx = Fixture::new();
        fx.write("dir/exists.txt", "x");
        let root = fx.vol.path().to_path_buf();

        assert_eq!(
            fx.vol.resolve_for_replace(&rel("dir/exists.txt")).unwrap(),
            root.join("dir/exists.txt")
        );
        assert_eq!(
            fx.vol.resolve_for_replace(&rel("dir/new.txt")).unwrap(),
            root.join("dir/new.txt")
        );
        assert_eq!(
            fx.vol.resolve_for_replace(&rel("dir")).unwrap_err(),
            FsError::NotAFile(rel("dir"))
        );
        assert_eq!(
            fx.vol.resolve_for_replace(&RelPath::root()).unwrap_err(),
            FsError::NotAFile(RelPath::root())
        );
    }

    #[test]
    fn rel_of_maps_paths_back_and_rejects_everything_else() {
        let fx = Fixture::new();
        let root = fx.vol.path().to_path_buf();

        assert_eq!(fx.vol.rel_of(&root), Some(RelPath::root()));
        assert_eq!(
            fx.vol.rel_of(&root.join("2024/Trip/a.jpg")),
            Some(rel("2024/Trip/a.jpg"))
        );
        assert_eq!(
            fx.vol.rel_of(&root.join("a/./b//c/")),
            Some(rel("a/b/c")),
            "lexical normalisation"
        );
        // Outside the root.
        assert_eq!(fx.vol.rel_of(Path::new("/etc/passwd")), None);
        assert_eq!(fx.vol.rel_of(&fx.base.join("outside/secret.txt")), None);
        assert_eq!(fx.vol.rel_of(root.parent().unwrap()), None);
        // Shares a name prefix with the root without being below it.
        let mut sibling = root.clone().into_os_string();
        sibling.push("2");
        assert_eq!(fx.vol.rel_of(&PathBuf::from(sibling).join("x")), None);
        // Relative paths.
        assert_eq!(fx.vol.rel_of(Path::new("a/b")), None);
        assert_eq!(fx.vol.rel_of(Path::new("")), None);
        // `..` escapes.
        assert_eq!(fx.vol.rel_of(&root.join("../outside/secret.txt")), None);
        assert_eq!(fx.vol.rel_of(&root.join("a/../../etc")), None);
        // Not UTF-8.
        let mut bytes = root.clone().into_os_string().into_vec();
        bytes.extend_from_slice(b"/caf\xe9.jpg");
        assert_eq!(fx.vol.rel_of(Path::new(&OsString::from_vec(bytes))), None);
        // NUL.
        let mut with_nul = root.clone().into_os_string().into_vec();
        with_nul.extend_from_slice(b"/a\0b");
        assert_eq!(
            fx.vol.rel_of(Path::new(&OsString::from_vec(with_nul))),
            None
        );
    }

    #[test]
    fn join_is_lexical_and_touches_nothing() {
        let fx = Fixture::new();
        let root = fx.vol.path().to_path_buf();
        assert_eq!(fx.vol.join(&RelPath::root()), root);
        assert_eq!(fx.vol.join(&rel("a/b.jpg")), root.join("a/b.jpg"));
        assert!(!fx.vol.join(&rel("a/b.jpg")).exists());
        // Even a link is joined without complaint: it is the unchecked method.
        symlink(fx.base.join("outside"), fx.root().join("escape")).unwrap();
        assert_eq!(fx.vol.join(&rel("escape/x")), root.join("escape/x"));
    }

    #[test]
    fn open_read_reads_regular_files_and_refuses_the_rest() {
        let fx = Fixture::new();
        fx.write("dir/photo.jpg", "pixels");
        symlink(
            fx.base.join("outside/secret.txt"),
            fx.root().join("link.txt"),
        )
        .unwrap();

        let mut content = String::new();
        fx.vol
            .open_read(&rel("dir/photo.jpg"))
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "pixels");

        assert_eq!(
            fx.vol.open_read(&rel("dir")).unwrap_err(),
            FsError::NotAFile(rel("dir"))
        );
        assert_eq!(
            fx.vol.open_read(&RelPath::root()).unwrap_err(),
            FsError::NotAFile(RelPath::root())
        );
        assert_eq!(
            fx.vol.open_read(&rel("link.txt")).unwrap_err(),
            FsError::SymlinkRejected(rel("link.txt"))
        );
        assert_eq!(
            fx.vol.open_read(&rel("missing.jpg")).unwrap_err(),
            FsError::NotFound(rel("missing.jpg"))
        );
    }

    #[test]
    fn open_read_returns_an_ordinary_blocking_file() {
        let fx = Fixture::new();
        fx.write("a.txt", "x");
        let file = fx.vol.open_read(&rel("a.txt")).unwrap();
        let flags = rustix::fs::fcntl_getfl(&file).unwrap();
        assert!(!flags.contains(OFlags::NONBLOCK));
    }

    #[test]
    fn open_read_does_not_block_on_a_fifo() {
        let fx = Fixture::new();
        let status = Command::new("mkfifo").arg(fx.root().join("pipe")).status();
        if !status.is_ok_and(|status| status.success()) {
            eprintln!("skipping: mkfifo is not available");
            return;
        }
        assert_eq!(
            fx.vol.open_read(&rel("pipe")).unwrap_err(),
            FsError::NotAFile(rel("pipe"))
        );
        assert_eq!(
            fx.vol.resolve_for_replace(&rel("pipe")).unwrap_err(),
            FsError::NotAFile(rel("pipe"))
        );
    }

    #[test]
    fn very_long_names_are_an_invalid_name_not_a_crash() {
        let fx = Fixture::new();
        let long = "x".repeat(300);
        assert_eq!(
            fx.vol.resolve_existing(&rel(&long)).unwrap_err(),
            FsError::InvalidFilename(rel(&long))
        );
        assert_eq!(
            fx.vol
                .resolve_for_create(&rel(&format!("{long}/a")))
                .unwrap_err(),
            FsError::InvalidFilename(rel(&long))
        );
        assert_eq!(
            fx.vol.resolve_for_create(&rel(&long)).unwrap_err(),
            FsError::InvalidFilename(rel(&long))
        );
        // The longest legal name still works.
        let longest = "y".repeat(255);
        fx.write(&longest, "x");
        assert!(fx.vol.resolve_existing(&rel(&longest)).is_ok());
        // A deep path of existing folders may exceed PATH_MAX before it ends.
        let mut deep = String::new();
        let mut on_disk = fx.root();
        for _ in 0..40 {
            let part = "d".repeat(200);
            deep.push_str(&part);
            deep.push('/');
            on_disk.push(&part);
            if fs::create_dir(&on_disk).is_err() {
                break;
            }
        }
        deep.push_str("file.txt");
        let result = fx.vol.resolve_existing(&rel(&deep));
        assert!(
            matches!(
                result,
                Err(FsError::InvalidFilename(_) | FsError::NotFound(_))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn names_starting_with_a_dash_resolve_to_absolute_paths() {
        let fx = Fixture::new();
        fx.write("-rf", "x");
        fx.write("--help/-o", "x");
        for name in ["-rf", "--help", "--help/-o"] {
            let path = fx.vol.resolve_existing(&rel(name)).unwrap();
            assert!(path.is_absolute());
            assert!(path.starts_with(fx.vol.path()));
            assert_eq!(
                path.file_name().unwrap().as_bytes(),
                name.rsplit('/').next().unwrap().as_bytes()
            );
        }
        assert!(fx.vol.open_read(&rel("-rf")).is_ok());
    }

    #[test]
    fn names_are_never_silently_case_folded_or_normalised() {
        let fx = Fixture::new();
        fx.write("Photo.JPG", "x");
        let nfc = "caf\u{e9}.jpg";
        let nfd = "cafe\u{301}.jpg";
        fx.write(nfc, "x");

        // The filesystem may or may not be case-insensitive: the answer is
        // either "not found" or a path with exactly the requested bytes.
        for (requested, exact) in [
            ("photo.jpg", false),
            ("Photo.JPG", true),
            (nfd, false),
            (nfc, true),
        ] {
            match fx.vol.resolve_existing(&rel(requested)) {
                Ok(path) => assert!(
                    path.as_os_str().as_bytes().ends_with(requested.as_bytes()),
                    "{requested}: {path:?}"
                ),
                Err(err) => {
                    assert!(
                        !exact,
                        "{requested} exists with exactly these bytes: {err:?}"
                    );
                    assert_eq!(err, FsError::NotFound(rel(requested)), "{requested}");
                }
            }
        }
    }

    #[test]
    fn errors_never_contain_host_paths() {
        let fx = Fixture::new();
        fx.write("file.txt", "x");
        symlink(fx.base.join("outside"), fx.root().join("escape")).unwrap();
        let host = fx.base.to_string_lossy().into_owned();

        let errors = [
            fx.vol.resolve_existing(&rel("missing/a")).unwrap_err(),
            fx.vol
                .resolve_existing(&rel("escape/secret.txt"))
                .unwrap_err(),
            fx.vol.resolve_existing(&rel("file.txt/a")).unwrap_err(),
            fx.vol.resolve_for_create(&rel("file.txt")).unwrap_err(),
            fx.vol.open_read(&RelPath::root()).unwrap_err(),
            fx.vol.resolve_existing(&rel(&"n".repeat(300))).unwrap_err(),
            VolumeRoot::new(1, fx.base.join("nope")).unwrap_err(),
            FsError::OutsideRoot,
        ];
        for err in errors {
            let text = err.to_string();
            assert!(!text.contains(&host), "{text}");
            let app = AppError::from(err);
            assert!(!app.message().contains(&host), "{}", app.message());
        }
    }

    #[test]
    fn fs_errors_map_to_app_errors() {
        let cases = [
            (
                FsError::NotFound(rel("a")),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                FsError::SymlinkRejected(rel("a")),
                StatusCode::FORBIDDEN,
                "symlink_rejected",
            ),
            (
                FsError::NotADirectory(rel("a")),
                StatusCode::BAD_REQUEST,
                "not_a_directory",
            ),
            (
                FsError::NotAFile(rel("a")),
                StatusCode::BAD_REQUEST,
                "not_a_file",
            ),
            (
                FsError::AlreadyExists(rel("a")),
                StatusCode::CONFLICT,
                "already_exists",
            ),
            (
                FsError::OutsideRoot,
                StatusCode::FORBIDDEN,
                "outside_volume",
            ),
            (
                FsError::InvalidFilename(rel("a")),
                StatusCode::BAD_REQUEST,
                "invalid_name",
            ),
            (
                FsError::Io(io::ErrorKind::NotFound),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                FsError::Io(io::ErrorKind::PermissionDenied),
                StatusCode::FORBIDDEN,
                "forbidden",
            ),
            (
                FsError::Io(io::ErrorKind::TimedOut),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (err, status, code) in cases {
            let app = AppError::from(err.clone());
            assert_eq!(app.status(), status, "{err:?}");
            assert_eq!(app.code(), code, "{err:?}");
        }
        let message = AppError::from(FsError::NotFound(rel("2024/Trip")))
            .message()
            .to_owned();
        assert!(message.contains("2024/Trip"), "{message}");
        assert!(
            FsError::NotFound(RelPath::root())
                .to_string()
                .contains("volume root")
        );
    }

    #[test]
    fn fs_errors_convert_to_io_errors_keeping_the_typed_error() {
        let io_err = io::Error::from(FsError::SymlinkRejected(rel("a")));
        assert_eq!(io_err.kind(), io::ErrorKind::PermissionDenied);
        let inner = io_err
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<FsError>())
            .unwrap();
        assert_eq!(inner, &FsError::SymlinkRejected(rel("a")));
        assert_eq!(
            io::Error::from(FsError::NotFound(rel("a"))).kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            io::Error::from(FsError::Io(io::ErrorKind::TimedOut)).kind(),
            io::ErrorKind::TimedOut
        );
    }
}
