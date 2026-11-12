//! The on-disk rendition cache.
//!
//! ```text
//! {data_dir}/cache/{thumb-sm|thumb-lg|preview}/{id mod 256, 2 hex digits}/{id}-{mtime}-{size}.jpg
//! {data_dir}/cache/tmp/            partial renders; emptied at startup
//! ```
//!
//! The name carries the row's `mtime` and `size`, so a changed file simply misses; the sweep
//! removes what no row refers to any more. Entries are written to `cache/tmp` and renamed, so a
//! reader never sees a partial file. A cache that cannot be written (disk full, read-only) never
//! fails a request: the caller serves the bytes and the failure is logged once.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use opm_core::paths::FileId;

/// A kind of cached picture; it names the directory and fixes the size and quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// 320 px grid thumbnail.
    ThumbSm,
    /// 800 px thumbnail.
    ThumbLg,
    /// 2560 px viewer picture.
    Preview,
}

impl Variant {
    pub const ALL: [Self; 3] = [Self::ThumbSm, Self::ThumbLg, Self::Preview];

    pub const fn dir(self) -> &'static str {
        match self {
            Self::ThumbSm => "thumb-sm",
            Self::ThumbLg => "thumb-lg",
            Self::Preview => "preview",
        }
    }

    pub const fn max_edge(self) -> u32 {
        match self {
            Self::ThumbSm => 320,
            Self::ThumbLg => 800,
            Self::Preview => 2560,
        }
    }

    pub const fn quality(self) -> u8 {
        match self {
            Self::ThumbSm => 80,
            Self::ThumbLg => 82,
            Self::Preview => 85,
        }
    }

    pub const fn is_thumbnail(self) -> bool {
        !matches!(self, Self::Preview)
    }
}

/// Identifies one rendition of one version of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub variant: Variant,
    pub id: FileId,
    pub mtime: i64,
    pub size: i64,
}

impl CacheKey {
    /// The strong `ETag` of this rendition.
    pub fn etag(&self) -> String {
        format!(
            "\"{}-{}-{}-{}\"",
            self.variant.dir(),
            self.id,
            self.mtime,
            self.size
        )
    }

    fn file_name(&self) -> String {
        format!("{}-{}-{}.jpg", self.id, self.mtime, self.size)
    }
}

fn shard(id: FileId) -> String {
    format!("{:02x}", id.rem_euclid(256))
}

#[derive(Debug)]
pub(super) struct Cache {
    root: PathBuf,
    tmp: PathBuf,
}

static PART: AtomicU64 = AtomicU64::new(0);

impl Cache {
    /// Opens the cache below `data_dir` and empties its `tmp` directory (stale partial renders).
    pub(super) fn open(data_dir: &Path) -> Self {
        let root = data_dir.join("cache");
        let tmp = root.join("tmp");
        let _ = fs::remove_dir_all(&tmp);
        if let Err(err) = fs::create_dir_all(&tmp) {
            tracing::warn!(error = %err, "cannot create the media cache directory");
        }
        Self { root, tmp }
    }

    pub(super) fn tmp_dir(&self) -> &Path {
        &self.tmp
    }

    pub(super) fn path(&self, key: &CacheKey) -> PathBuf {
        self.root
            .join(key.variant.dir())
            .join(shard(key.id))
            .join(key.file_name())
    }

    pub(super) fn read(&self, key: &CacheKey) -> Option<Vec<u8>> {
        fs::read(self.path(key))
            .ok()
            .filter(|bytes| !bytes.is_empty())
    }

    /// Writes the entry atomically.
    pub(super) fn write(&self, key: &CacheKey, bytes: &[u8]) -> io::Result<()> {
        let target = self.path(key);
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::create_dir_all(&self.tmp)?;
        let part = self.tmp.join(format!(
            "{}-{}.part",
            std::process::id(),
            PART.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = fs::File::create(&part)?;
            file.write_all(bytes)?;
            file.flush()?;
            fs::rename(&part, &target)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&part);
        }
        written
    }

    /// The entries of the shards `from..from + count` (modulo 256), as `(path, key parts)`.
    pub(super) fn entries(&self, from: usize, count: usize) -> Vec<(PathBuf, Entry)> {
        let mut found = Vec::new();
        for offset in 0..count {
            let shard = format!("{:02x}", (from + offset) % 256);
            for variant in Variant::ALL {
                let dir = self.root.join(variant.dir()).join(&shard);
                let Ok(read) = fs::read_dir(&dir) else {
                    continue;
                };
                for entry in read.flatten() {
                    let path = entry.path();
                    let parsed = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(parse_name);
                    found.push((path, Entry { parsed }));
                }
            }
        }
        found
    }
}

/// A file found in the cache; `parsed` is `None` for a name that is not an entry's.
#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    pub(super) parsed: Option<(FileId, i64, i64)>,
}

fn parse_name(name: &str) -> Option<(FileId, i64, i64)> {
    let stem = name.strip_suffix(".jpg")?;
    let mut parts = stem.splitn(3, '-');
    let id = parts.next()?.parse().ok()?;
    let mtime = parts.next()?.parse().ok()?;
    let size = parts.next()?.parse().ok()?;
    Some((id, mtime, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_back_into_their_key() {
        assert_eq!(
            parse_name("12-1700000000-345.jpg"),
            Some((12, 1_700_000_000, 345))
        );
        assert_eq!(parse_name("12-1700000000.jpg"), None);
        assert_eq!(parse_name("x-1-2.jpg"), None);
        assert_eq!(parse_name("12-1-2.png"), None);
    }

    #[test]
    fn the_layout_shards_by_id() {
        let key = CacheKey {
            variant: Variant::ThumbSm,
            id: 258,
            mtime: 5,
            size: 7,
        };
        let cache = Cache {
            root: PathBuf::from("/c"),
            tmp: PathBuf::from("/c/tmp"),
        };
        assert_eq!(
            cache.path(&key),
            PathBuf::from("/c/thumb-sm/02/258-5-7.jpg")
        );
        assert_eq!(key.etag(), "\"thumb-sm-258-5-7\"");
    }
}
