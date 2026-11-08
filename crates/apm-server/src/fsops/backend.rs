//! The two seams between the file operations and the real disk, so tests can inject what a
//! temporary directory cannot produce: a rename that crosses devices and a disk that is full.
//!
//! [`SystemMover`] and [`SystemFreeSpace`] are what production uses. Every method here BLOCKS:
//! call them from `spawn_blocking`.

use std::io;
use std::path::Path;

use super::rename;

/// Renames files. Implementations must never replace an existing destination in
/// [`rename_no_replace`](Mover::rename_no_replace).
pub trait Mover: Send + Sync {
    /// Renames `from` to `to`, failing with [`io::ErrorKind::AlreadyExists`] when `to` exists
    /// (any kind of entry, symbolic links included) and with [`io::ErrorKind::CrossesDevices`]
    /// when the two are on different file systems. Never overwrites: this is what keeps a racing
    /// writer from losing a file.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// A plain `rename(2)`. Only used to change the CASE of a name on a case-insensitive file
    /// system, where `to` is the same inode as `from`; the caller proved that first.
    fn rename_same_inode(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    /// Whether a rename from a file under the volume root `from_root` (on device `from_dev`) to
    /// one under `to_root` (on `to_dev`) cannot work. The real answer is that the devices
    /// differ; a test mover overrides it to make two directories of one file system look like
    /// two file systems, consistently with [`rename_no_replace`](Mover::rename_no_replace).
    fn crosses_devices(
        &self,
        _from_root: &Path,
        from_dev: u64,
        _to_root: &Path,
        to_dev: u64,
    ) -> bool {
        from_dev != to_dev
    }
}

/// The real rename: `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS,
/// and a hard-link-then-unlink fallback where the file system lacks it, see [`rename`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemMover;

impl Mover for SystemMover {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename::no_replace(from, to)
    }
}

/// Reports how much room a destination has.
pub trait FreeSpace: Send + Sync {
    /// Bytes an unprivileged writer can still allocate on the file system holding `dir`.
    fn available(&self, dir: &Path) -> io::Result<u64>;
}

/// `statvfs`: `f_bavail * f_frsize`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemFreeSpace;

impl FreeSpace for SystemFreeSpace {
    fn available(&self, dir: &Path) -> io::Result<u64> {
        let stat = rustix::fs::statvfs(dir)?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::test_support::scratch;

    #[test]
    fn the_system_reports_some_free_space() {
        let dir = scratch("space");
        assert!(SystemFreeSpace.available(dir.path()).unwrap() > 0);
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = scratch("space-missing");
        assert!(SystemFreeSpace.available(&dir.path().join("nope")).is_err());
    }
}
