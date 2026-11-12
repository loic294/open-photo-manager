//! `rename` that never overwrites, on every platform and file system we meet.
//!
//! # Strategy
//! 1. Linux and macOS: the kernel's atomic no-replace rename (`renameat2` with
//!    `RENAME_NOREPLACE`; `renamex_np` with `RENAME_EXCL` on macOS). It either moves the file or
//!    fails with `EEXIST`; there is no window.
//! 2. When the file system does not implement that flag (`EINVAL`, `ENOSYS`, `ENOTSUP`: NFS,
//!    SMB, FAT, some FUSE mounts) and on other platforms: `link(2)` the new name (it never
//!    replaces) and `unlink` the old one. Between the two calls both names exist for the same
//!    inode; `reconcile` repairs that state after a crash.
//! 3. When the file system cannot hard-link either (`EPERM`, `ENOTSUP`: SMB, exFAT) and for
//!    folders (which cannot be hard-linked): look whether the destination exists, then plain
//!    `rename`. That leaves a window of a few microseconds in which another writer could create
//!    the destination and lose its file. It is the best that file system allows, and it only
//!    happens on file systems that support neither primitive.
//!
//! A rename across file systems fails with [`io::ErrorKind::CrossesDevices`] in every strategy.

use std::fs;
use std::io;
use std::path::Path;

use rustix::io::Errno;

/// Renames `from` to `to` without ever replacing `to`; see the module docs.
pub(crate) fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
            Ok(()) => return Ok(()),
            Err(errno) if flag_unsupported(errno) => {}
            Err(errno) => return Err(errno.into()),
        }
    }
    link_then_unlink(from, to)
}

/// The kernel or the file system does not know the no-replace flag.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn flag_unsupported(errno: Errno) -> bool {
    errno == Errno::INVAL
        || errno == Errno::NOSYS
        || errno == Errno::NOTSUP
        || errno == Errno::OPNOTSUPP
}

/// The file system cannot make a hard link (or the entry cannot be linked, like a folder).
fn link_unsupported(err: &io::Error) -> bool {
    err.raw_os_error()
        .map(Errno::from_raw_os_error)
        .is_some_and(|errno| {
            errno == Errno::PERM
                || errno == Errno::NOTSUP
                || errno == Errno::OPNOTSUPP
                || errno == Errno::NOSYS
        })
}

pub(crate) fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => {
            if let Err(err) = fs::remove_file(from) {
                // Put things back: only the old name may stay. If this fails too, both names
                // remain for one inode, which `reconcile` resolves.
                let _ = fs::remove_file(to);
                return Err(err);
            }
            Ok(())
        }
        Err(err) if link_unsupported(&err) => check_then_rename(from, to),
        Err(err) => Err(err),
    }
}

/// The last resort, see the module docs. The existence check uses `lstat`, so a dangling symbolic
/// link counts as existing.
fn check_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::fsops::test_support::scratch;

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn it_moves_a_file_to_a_free_name() {
        let dir = scratch("rename-free");
        write(&dir.path().join("a"), b"one");
        no_replace(&dir.path().join("a"), &dir.path().join("b")).unwrap();
        assert!(!dir.path().join("a").exists());
        assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"one");
    }

    #[test]
    fn it_never_replaces_a_file() {
        let dir = scratch("rename-exists");
        write(&dir.path().join("a"), b"one");
        write(&dir.path().join("b"), b"two");
        let err = no_replace(&dir.path().join("a"), &dir.path().join("b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dir.path().join("a")).unwrap(), b"one");
        assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"two");
    }

    #[test]
    fn it_never_replaces_a_directory_or_a_dangling_link() {
        let dir = scratch("rename-special");
        write(&dir.path().join("a"), b"one");
        fs::create_dir(dir.path().join("d")).unwrap();
        symlink("missing-target", dir.path().join("l")).unwrap();
        for name in ["d", "l"] {
            let err = no_replace(&dir.path().join("a"), &dir.path().join(name)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{name}");
        }
        assert_eq!(fs::read(dir.path().join("a")).unwrap(), b"one");
    }

    #[test]
    fn a_missing_source_is_not_found() {
        let dir = scratch("rename-missing");
        let err = no_replace(&dir.path().join("a"), &dir.path().join("b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!dir.path().join("b").exists());
    }

    #[test]
    fn it_moves_a_directory_to_a_free_name_and_refuses_an_existing_one() {
        let dir = scratch("rename-dir");
        fs::create_dir(dir.path().join("a")).unwrap();
        write(&dir.path().join("a/x"), b"x");
        fs::create_dir(dir.path().join("taken")).unwrap();
        let err = no_replace(&dir.path().join("a"), &dir.path().join("taken")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        no_replace(&dir.path().join("a"), &dir.path().join("b")).unwrap();
        assert_eq!(fs::read(dir.path().join("b/x")).unwrap(), b"x");
    }

    #[test]
    fn the_link_fallback_moves_and_refuses_like_the_kernel_primitive() {
        let dir = scratch("rename-link");
        write(&dir.path().join("a"), b"one");
        write(&dir.path().join("b"), b"two");
        let err = link_then_unlink(&dir.path().join("a"), &dir.path().join("b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dir.path().join("a")).unwrap(), b"one");
        link_then_unlink(&dir.path().join("a"), &dir.path().join("c")).unwrap();
        assert!(!dir.path().join("a").exists());
        assert_eq!(fs::read(dir.path().join("c")).unwrap(), b"one");
    }

    #[test]
    fn the_link_fallback_handles_directories_by_checking_first() {
        let dir = scratch("rename-link-dir");
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::create_dir(dir.path().join("taken")).unwrap();
        let err = link_then_unlink(&dir.path().join("a"), &dir.path().join("taken")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        link_then_unlink(&dir.path().join("a"), &dir.path().join("b")).unwrap();
        assert!(dir.path().join("b").is_dir() && !dir.path().join("a").exists());
    }

    #[test]
    fn a_racing_creator_is_never_overwritten() {
        // Many threads rename different sources to the same destination: exactly one wins and
        // nobody's bytes are lost.
        let dir = scratch("rename-race");
        let target = dir.path().join("target");
        let threads: Vec<_> = (0..16)
            .map(|index| {
                let source = dir.path().join(format!("src{index}"));
                write(&source, format!("payload {index}").as_bytes());
                let target = target.clone();
                std::thread::spawn(move || no_replace(&source, &target).is_ok())
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        let remaining = (0..16)
            .filter(|index| dir.path().join(format!("src{index}")).exists())
            .count();
        assert_eq!(remaining, 15, "the losers keep their files");
        assert!(fs::read_to_string(&target).unwrap().starts_with("payload "));
    }
}
