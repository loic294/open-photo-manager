//! Where a file stands: the paths Immich knows it by, and what its volume proves about a place.
//!
//! The probe is what keeps the outbox from ever deleting an asset whose file is still there. It is
//! deliberately stingy: only the answer "there is nothing at that path" counts as absent; a volume
//! that is offline, a link in the way or any I/O error is "unknown", and unknown never leads to a
//! delete.

use apm_core::paths::RelPath;

use super::decide::Disk;
use super::merge::Place;
use crate::immich::paths::PathMap;
use crate::indexer::VolumeRegistry;
use crate::security::{FsError, VolumeRoot};

/// The path of `place` inside its volume; `None` for a name the volume cannot hold.
pub(super) fn rel_of(place: &Place) -> Option<RelPath> {
    RelPath::new(&place.dir).ok()?.join(&place.name).ok()
}

/// The path Immich knows `place` by (`originalPath`); `None` for a volume outside every mapping.
pub(super) fn place_path(map: &PathMap, place: &Place) -> Option<String> {
    map.to_immich(place.volume_id, &rel_of(place)?)
}

/// What `root` says about `rel`: nothing is read, only looked at (`stat` never follows a link).
pub(super) fn probe_root(root: &VolumeRoot, rel: &RelPath) -> Disk {
    match root.stat(rel) {
        Ok(_) => Disk::Present,
        Err(FsError::NotFound(_)) => Disk::Absent,
        Err(_) => Disk::Unknown,
    }
}

/// What the volume of `place` says about it. A volume that is offline or unknown, a name the volume
/// cannot hold and a failed lookup are all [`Disk::Unknown`].
pub(super) async fn probe(volumes: &VolumeRegistry, place: &Place) -> Disk {
    let Ok(root) = volumes.get(place.volume_id) else {
        return Disk::Unknown;
    };
    let Some(rel) = rel_of(place) else {
        return Disk::Unknown;
    };
    tokio::task::spawn_blocking(move || probe_root(&root, &rel))
        .await
        .unwrap_or(Disk::Unknown)
}

/// Direct path writes need the exact directory-entry bytes and a regular file,
/// not a case-insensitive alias or a directory. API deletion deliberately keeps
/// the more conservative stat-only probe above.
pub(super) async fn probe_exact_file(volumes: &VolumeRegistry, place: &Place) -> Disk {
    let Ok(root) = volumes.get(place.volume_id) else {
        return Disk::Unknown;
    };
    let Some(rel) = rel_of(place) else {
        return Disk::Unknown;
    };
    tokio::task::spawn_blocking(move || {
        let mut parent = RelPath::root();
        for name in rel.components() {
            let directory = match root.resolve_dir(&parent) {
                Ok(path) => path,
                Err(FsError::NotFound(_)) => return Disk::Absent,
                Err(_) => return Disk::Unknown,
            };
            let Ok(entries) = std::fs::read_dir(directory) else {
                return Disk::Unknown;
            };
            let mut found = false;
            for entry in entries {
                let Ok(entry) = entry else {
                    return Disk::Unknown;
                };
                if entry.file_name() == std::ffi::OsStr::new(name) {
                    found = true;
                    break;
                }
            }
            if !found {
                return Disk::Absent;
            }
            let Ok(next) = parent.join(name) else {
                return Disk::Unknown;
            };
            parent = next;
        }
        match root.stat(&rel) {
            Ok(metadata) if metadata.is_file() => Disk::Present,
            Err(FsError::NotFound(_)) => Disk::Absent,
            Ok(_) | Err(_) => Disk::Unknown,
        }
    })
    .await
    .unwrap_or(Disk::Unknown)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use apm_core::dto::ImmichMappingEntry;

    use super::*;
    use crate::immich::test_support::scratch_dir;

    fn map() -> PathMap {
        PathMap::new(&[ImmichMappingEntry {
            volume_id: 1,
            library_id: Some("lib".to_owned()),
            path_prefix: "/mnt/photos".to_owned(),
        }])
        .unwrap()
    }

    #[test]
    fn a_place_is_a_path_inside_its_volume_and_a_path_in_immich() {
        let place = Place::new(1, "2019/Trip", "IMG 1.jpg");
        assert_eq!(
            rel_of(&place).unwrap().as_str(),
            "2019/Trip/IMG 1.jpg",
            "the exact names"
        );
        assert_eq!(
            place_path(&map(), &place).as_deref(),
            Some("/mnt/photos/2019/Trip/IMG 1.jpg")
        );
        let at_root = Place::new(1, "", "a.jpg");
        assert_eq!(rel_of(&at_root).unwrap().as_str(), "a.jpg");
        assert_eq!(
            place_path(&map(), &at_root).as_deref(),
            Some("/mnt/photos/a.jpg")
        );
    }

    #[test]
    fn a_volume_outside_the_mapping_has_no_path_in_immich() {
        assert_eq!(place_path(&map(), &Place::new(2, "", "a.jpg")), None);
    }

    #[test]
    fn a_name_the_volume_cannot_hold_has_no_path() {
        assert!(rel_of(&Place::new(1, "", "a/b.jpg")).is_none());
        assert!(rel_of(&Place::new(1, "", "..")).is_none());
        assert!(rel_of(&Place::new(1, "../x", "a.jpg")).is_none());
        assert_eq!(place_path(&map(), &Place::new(1, "", "a/b.jpg")), None);
    }

    #[test]
    fn only_a_missing_path_is_absent() {
        let dir = scratch_dir();
        let root = VolumeRoot::new(1, dir.path()).unwrap();
        fs::create_dir_all(dir.path().join("there")).unwrap();
        fs::write(dir.path().join("there/a.jpg"), b"x").unwrap();
        fs::write(dir.path().join("plain"), b"x").unwrap();

        let rel = |text: &str| RelPath::new(text).unwrap();
        assert_eq!(probe_root(&root, &rel("there/a.jpg")), Disk::Present);
        assert_eq!(probe_root(&root, &rel("there")), Disk::Present);
        assert_eq!(probe_root(&root, &rel("there/b.jpg")), Disk::Absent);
        assert_eq!(probe_root(&root, &rel("missing/b.jpg")), Disk::Absent);
        // A file where a folder should be: the path cannot be read, which proves nothing.
        assert_eq!(probe_root(&root, &rel("plain/b.jpg")), Disk::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_way_is_unknown_not_absent() {
        let dir = scratch_dir();
        let root = VolumeRoot::new(1, dir.path()).unwrap();
        let outside = scratch_dir();
        fs::write(outside.path().join("a.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let rel = RelPath::new("link/a.jpg").unwrap();
        assert_eq!(probe_root(&root, &rel), Disk::Unknown);
        let gone = RelPath::new("link/b.jpg").unwrap();
        assert_eq!(probe_root(&root, &gone), Disk::Unknown);
    }
}
