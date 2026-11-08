//! Stat snapshots of files, as the file operations compare them.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::UNIX_EPOCH;

/// What the caller believes the source looks like: the `size` and `mtime` (epoch seconds) the
/// index holds. A source that differs is not touched (`source_changed`), which is what protects a
/// plan computed from an index that has gone stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub size: u64,
    pub mtime: i64,
}

/// A file's `lstat`, in the units the index stores. Never follows a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    /// Modification time in epoch seconds, like `files.mtime`.
    pub mtime: i64,
    /// The sub-second part of the modification time (identity checks need it).
    pub mtime_nanos: i64,
    /// The permission bits and file type (`st_mode`).
    pub mode: u32,
    pub dev: u64,
    pub ino: u64,
    /// Birth time in epoch seconds, when the platform and the file system report it.
    pub btime: Option<i64>,
}

impl FileStat {
    pub fn from_metadata(meta: &Metadata) -> Self {
        let btime = meta
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok());
        Self {
            size: meta.len(),
            mtime: meta.mtime(),
            mtime_nanos: meta.mtime_nsec(),
            mode: meta.mode(),
            dev: meta.dev(),
            ino: meta.ino(),
            btime,
        }
    }

    /// Whether the file still has the size and modification second the caller expects.
    pub fn matches(&self, expect: &Expect) -> bool {
        self.size == expect.size && self.mtime == expect.mtime
    }

    /// Whether `other` is the same file in the same state: same inode, size and modification
    /// time down to the nanosecond.
    pub fn same_file(&self, other: &Self) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.size == other.size
            && self.mtime == other.mtime
            && self.mtime_nanos == other.mtime_nanos
    }

    /// The permission bits, without the file type.
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(size: u64, mtime: i64) -> FileStat {
        FileStat {
            size,
            mtime,
            mtime_nanos: 5,
            mode: 0o100_644,
            dev: 1,
            ino: 2,
            btime: None,
        }
    }

    #[test]
    fn matches_compares_size_and_second() {
        let current = stat(10, 100);
        assert!(current.matches(&Expect {
            size: 10,
            mtime: 100
        }));
        assert!(!current.matches(&Expect {
            size: 11,
            mtime: 100
        }));
        assert!(!current.matches(&Expect {
            size: 10,
            mtime: 101
        }));
    }

    #[test]
    fn same_file_needs_the_inode_and_the_nanoseconds() {
        let current = stat(10, 100);
        assert!(current.same_file(&stat(10, 100)));
        let mut other = stat(10, 100);
        other.mtime_nanos = 6;
        assert!(!current.same_file(&other));
        other = stat(10, 100);
        other.ino = 3;
        assert!(!current.same_file(&other));
    }

    #[test]
    fn permissions_drop_the_file_type() {
        assert_eq!(stat(1, 1).permissions(), 0o644);
    }
}
