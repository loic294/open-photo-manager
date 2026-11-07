//! The decisions of a scan that need no database and no file system: what changed in one directory
//! and when the mass-disappearance guard holds removals. Pure functions, unit tested.

use std::collections::{HashMap, HashSet};

use apm_core::dto::FileKind;
use apm_core::paths::FileId;

use super::walk::DirListing;

/// A scan that would remove at least this many files of a volume ...
pub(crate) const GUARD_MIN_REMOVED: u64 = 50;
/// ... and more than one in this many of the volume's files (20%) is held back instead.
pub(crate) const GUARD_FRACTION_DENOMINATOR: u64 = 5;

/// Whether removing `removed` of the `total` file rows a volume had before the scan must wait for
/// the user's confirmation. Losing a fifth of a library at once, and at least 50 files (so tiny
/// volumes are never blocked), looks like an unmounted or emptied share, not like deleted photos.
pub(crate) fn guard_trips(removed: u64, total: u64) -> bool {
    removed >= GUARD_MIN_REMOVED && removed.saturating_mul(GUARD_FRACTION_DENOMINATOR) > total
}

/// A `files` row as far as the diff needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredFile {
    pub id: FileId,
    pub name: String,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: i64,
}

/// What differs between a directory on disk and its stored rows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DirDiff<'a> {
    /// One entry per listed file: the stored id, `None` for a file that has no row yet.
    pub ids: Vec<Option<FileId>>,
    /// Indexes into the listing's `files` of the files to write: new ones and changed ones.
    pub writes: Vec<usize>,
    pub added: u64,
    pub changed: u64,
    pub unchanged: u64,
    /// Stored rows whose file is not on disk (any more).
    pub removed: Vec<&'a StoredFile>,
    /// A file appeared or changed kind, or the scan is forced: the sidecar links of the directory
    /// must be recomputed. (A vanished file cannot change the links of the others.)
    pub relink: bool,
    /// Stored sidecars whose size, modification time or kind changed: their owners must be
    /// extracted again.
    pub changed_sidecars: Vec<FileId>,
}

/// Compares a directory listing with its stored rows. Names are compared byte for byte (the
/// index stores names as the disk reports them); a file is changed when its size, modification
/// time (whole seconds) or kind differs, or always when `force` is set.
pub(crate) fn diff_dir<'a>(
    listing: &DirListing,
    stored: &'a [StoredFile],
    force: bool,
) -> DirDiff<'a> {
    let by_name: HashMap<&str, &StoredFile> =
        stored.iter().map(|row| (row.name.as_str(), row)).collect();

    let mut diff = DirDiff {
        ids: Vec::with_capacity(listing.files.len()),
        writes: Vec::new(),
        added: 0,
        changed: 0,
        unchanged: 0,
        removed: Vec::new(),
        relink: force,
        changed_sidecars: Vec::new(),
    };
    for (index, entry) in listing.files.iter().enumerate() {
        let Some(row) = by_name.get(entry.name.as_str()) else {
            diff.ids.push(None);
            diff.writes.push(index);
            diff.added += 1;
            diff.relink = true;
            continue;
        };
        diff.ids.push(Some(row.id));
        let kind_changed = row.kind != entry.kind;
        if kind_changed || row.size != entry.size || row.mtime != entry.mtime || force {
            diff.writes.push(index);
            diff.changed += 1;
            diff.relink |= kind_changed;
            if entry.kind == FileKind::Sidecar || row.kind == FileKind::Sidecar {
                diff.changed_sidecars.push(row.id);
            }
        } else {
            diff.unchanged += 1;
        }
    }

    if stored.len() > by_name.len() || stored.len() > listing.files.len() - diff.added as usize {
        let listed: HashSet<&str> = listing
            .files
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        diff.removed = stored
            .iter()
            .filter(|row| !listed.contains(row.name.as_str()))
            .collect();
    }
    diff
}

#[cfg(test)]
mod tests {
    use apm_core::paths::RelPath;

    use super::*;
    use crate::indexer::walk::FileEntry;

    fn entry(name: &str, kind: FileKind, size: i64, mtime: i64) -> FileEntry {
        FileEntry {
            name: name.to_owned(),
            kind,
            size,
            mtime,
            btime: None,
        }
    }

    fn listing(files: Vec<FileEntry>) -> DirListing {
        DirListing {
            dir: RelPath::root(),
            mtime: Some(1),
            files,
            subdirs: Vec::new(),
            skipped_invalid: 0,
            error: None,
        }
    }

    fn row(id: FileId, name: &str, kind: FileKind, size: i64, mtime: i64) -> StoredFile {
        StoredFile {
            id,
            name: name.to_owned(),
            kind,
            size,
            mtime,
        }
    }

    #[test]
    fn everything_is_new_in_a_directory_without_rows() {
        let listing = listing(vec![
            entry("a.jpg", FileKind::Image, 1, 1),
            entry("b.mp4", FileKind::Video, 2, 2),
        ]);
        let diff = diff_dir(&listing, &[], false);
        assert_eq!(diff.ids, [None, None]);
        assert_eq!(diff.writes, [0, 1]);
        assert_eq!((diff.added, diff.changed, diff.unchanged), (2, 0, 0));
        assert!(diff.removed.is_empty());
        assert!(diff.relink);
    }

    #[test]
    fn an_unchanged_directory_writes_nothing() {
        let listing = listing(vec![
            entry("a.jpg", FileKind::Image, 1, 10),
            entry("a.xmp", FileKind::Sidecar, 5, 11),
        ]);
        let stored = [
            row(7, "a.xmp", FileKind::Sidecar, 5, 11),
            row(3, "a.jpg", FileKind::Image, 1, 10),
        ];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(
            diff.ids,
            [Some(3), Some(7)],
            "rows are matched by name, not by position"
        );
        assert!(diff.writes.is_empty());
        assert_eq!((diff.added, diff.changed, diff.unchanged), (0, 0, 2));
        assert!(diff.removed.is_empty());
        assert!(!diff.relink);
        assert!(diff.changed_sidecars.is_empty());
    }

    #[test]
    fn a_different_size_or_mtime_is_a_change_that_keeps_the_id() {
        let listing = listing(vec![
            entry("size.jpg", FileKind::Image, 2, 10),
            entry("time.jpg", FileKind::Image, 1, 11),
            entry("same.jpg", FileKind::Image, 1, 10),
        ]);
        let stored = [
            row(1, "same.jpg", FileKind::Image, 1, 10),
            row(2, "size.jpg", FileKind::Image, 1, 10),
            row(3, "time.jpg", FileKind::Image, 1, 10),
        ];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(diff.ids, [Some(2), Some(3), Some(1)]);
        assert_eq!(diff.writes, [0, 1]);
        assert_eq!((diff.added, diff.changed, diff.unchanged), (0, 2, 1));
        assert!(!diff.relink, "a content change does not alter the links");
    }

    #[test]
    fn a_new_kind_is_a_change_and_relinks() {
        let listing = listing(vec![entry("a.xmp", FileKind::Sidecar, 1, 1)]);
        let stored = [row(4, "a.xmp", FileKind::Other, 1, 1)];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(diff.writes, [0]);
        assert!(diff.relink);
        assert_eq!(diff.changed_sidecars, [4], "a file that became a sidecar");

        let listing = listing_of_one("a.xmp", FileKind::Other);
        let stored = [row(4, "a.xmp", FileKind::Sidecar, 1, 1)];
        let diff = diff_dir(&listing, &stored, false);
        assert!(diff.relink);
        assert_eq!(
            diff.changed_sidecars,
            [4],
            "a sidecar that stopped being one"
        );
    }

    fn listing_of_one(name: &str, kind: FileKind) -> DirListing {
        listing(vec![entry(name, kind, 1, 1)])
    }

    #[test]
    fn only_changed_sidecars_are_listed_for_their_owners() {
        let listing = listing(vec![
            entry("a.jpg", FileKind::Image, 9, 1),
            entry("a.xmp", FileKind::Sidecar, 6, 1),
        ]);
        let stored = [
            row(1, "a.jpg", FileKind::Image, 1, 1),
            row(2, "a.xmp", FileKind::Sidecar, 5, 1),
        ];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(diff.changed, 2);
        assert_eq!(diff.changed_sidecars, [2]);
    }

    #[test]
    fn vanished_rows_are_reported_and_do_not_relink() {
        let listing = listing(vec![entry("keep.jpg", FileKind::Image, 1, 1)]);
        let stored = [
            row(1, "gone.jpg", FileKind::Image, 1, 1),
            row(2, "keep.jpg", FileKind::Image, 1, 1),
            row(3, "gone.xmp", FileKind::Sidecar, 1, 1),
        ];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(
            diff.removed.iter().map(|row| row.id).collect::<Vec<_>>(),
            [1, 3]
        );
        assert!(diff.writes.is_empty());
        assert!(!diff.relink);
    }

    #[test]
    fn a_vanished_directory_content_is_all_removed() {
        let listing = listing(Vec::new());
        let stored = [row(1, "a.jpg", FileKind::Image, 1, 1)];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.ids.is_empty());
    }

    #[test]
    fn a_rename_is_a_removal_plus_an_addition() {
        let listing = listing(vec![entry("new.jpg", FileKind::Image, 1, 1)]);
        let stored = [row(1, "old.jpg", FileKind::Image, 1, 1)];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!(diff.ids, [None]);
        assert_eq!((diff.added, diff.removed.len()), (1, 1));
    }

    #[test]
    fn names_are_compared_byte_for_byte() {
        let nfc = "caf\u{e9}.jpg";
        let nfd = "cafe\u{301}.jpg";
        let listing = listing(vec![entry(nfd, FileKind::Image, 1, 1)]);
        let stored = [row(1, nfc, FileKind::Image, 1, 1)];
        let diff = diff_dir(&listing, &stored, false);
        assert_eq!((diff.added, diff.removed.len()), (1, 1));

        let same = [row(1, nfd, FileKind::Image, 1, 1)];
        let diff = diff_dir(&listing, &same, false);
        assert_eq!((diff.added, diff.unchanged, diff.removed.len()), (0, 1, 0));
    }

    #[test]
    fn force_rewrites_every_file_and_relinks() {
        let listing = listing(vec![
            entry("a.jpg", FileKind::Image, 1, 1),
            entry("a.xmp", FileKind::Sidecar, 1, 1),
        ]);
        let stored = [
            row(1, "a.jpg", FileKind::Image, 1, 1),
            row(2, "a.xmp", FileKind::Sidecar, 1, 1),
        ];
        let diff = diff_dir(&listing, &stored, true);
        assert_eq!(diff.writes, [0, 1]);
        assert_eq!((diff.added, diff.changed, diff.unchanged), (0, 2, 0));
        assert!(diff.relink);
        assert_eq!(diff.changed_sidecars, [2]);
    }

    #[test]
    fn the_guard_needs_both_a_fifth_and_fifty_files() {
        assert!(!guard_trips(0, 0));
        assert!(!guard_trips(49, 49), "everything, but a tiny volume");
        assert!(guard_trips(50, 50));
        assert!(guard_trips(50, 100));
        assert!(!guard_trips(50, 250), "exactly 20% is not more than 20%");
        assert!(guard_trips(51, 250));
        assert!(!guard_trips(999, 10_000), "under a fifth of a big volume");
        assert!(guard_trips(2001, 10_000));
        assert!(!guard_trips(2000, 10_000));
    }

    #[test]
    fn the_guard_does_not_overflow() {
        assert!(guard_trips(u64::MAX, u64::MAX - 1));
    }
}
