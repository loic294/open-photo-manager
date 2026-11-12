//! Folders the user excluded from the index and from every file operation.
//!
//! Built from `SettingsDto::excluded_folders`. A path is excluded when it, or one of its ancestors,
//! is an excluded folder of the same volume. Unlike junk, an excluded folder still counts as content
//! when deciding whether its parent is empty: it is real data the user only wants left alone.

use std::collections::{HashMap, HashSet};

use crate::dto::ExcludedFolder;
use crate::paths::{RelPath, VolumeId};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exclusions {
    by_volume: HashMap<VolumeId, HashSet<String>>,
}

impl Exclusions {
    /// Entries whose path does not parse, or is the root, are ignored (validated settings have none).
    pub fn new(folders: &[ExcludedFolder]) -> Self {
        let mut by_volume: HashMap<VolumeId, HashSet<String>> = HashMap::new();
        for folder in folders {
            if let Ok(path) = RelPath::new(&folder.path)
                && !path.is_root()
            {
                by_volume
                    .entry(folder.volume_id)
                    .or_default()
                    .insert(path.as_str().to_owned());
            }
        }
        Self { by_volume }
    }

    pub fn is_empty(&self) -> bool {
        self.by_volume.is_empty()
    }

    /// True when `path` is excluded or holds an excluded folder somewhere below it.
    pub fn touches(&self, volume_id: VolumeId, path: &RelPath) -> bool {
        self.by_volume.get(&volume_id).is_some_and(|set| {
            covered(set, path)
                || set.iter().any(|excluded| {
                    RelPath::new(excluded).is_ok_and(|excluded| excluded.starts_with(path))
                })
        })
    }

    /// The excluded folders of one volume only.
    pub fn for_volume(&self, volume_id: VolumeId) -> VolumeExclusions {
        VolumeExclusions(self.by_volume.get(&volume_id).cloned().unwrap_or_default())
    }

    /// True when `path` or one of its ancestors is excluded on `volume_id`.
    pub fn contains(&self, volume_id: VolumeId, path: &RelPath) -> bool {
        self.by_volume
            .get(&volume_id)
            .is_some_and(|set| covered(set, path))
    }
}

/// The excluded folders of one volume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeExclusions(HashSet<String>);

impl VolumeExclusions {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when `path` or one of its ancestors is excluded.
    pub fn contains(&self, path: &RelPath) -> bool {
        covered(&self.0, path)
    }

    /// True when `path` itself is an excluded folder (its ancestors are not looked at).
    pub fn is_excluded_dir(&self, path: &RelPath) -> bool {
        self.0.contains(path.as_str())
    }
}

fn covered(set: &HashSet<String>, path: &RelPath) -> bool {
    !path.is_root()
        && !set.is_empty()
        && (set.contains(path.as_str())
            || path
                .ancestors()
                .any(|ancestor| set.contains(ancestor.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn set() -> Exclusions {
        Exclusions::new(&[
            ExcludedFolder {
                volume_id: 1,
                path: "Photos/2019".into(),
            },
            ExcludedFolder {
                volume_id: 2,
                path: "x".into(),
            },
        ])
    }

    #[test]
    fn a_folder_and_everything_below_it_is_excluded_on_its_volume_only() {
        let set = set();
        assert!(set.contains(1, &rel("Photos/2019")));
        assert!(set.contains(1, &rel("Photos/2019/a/b.jpg")));
        assert!(!set.contains(1, &rel("Photos/2019.old")));
        assert!(!set.contains(1, &rel("Photos")));
        assert!(!set.contains(1, &RelPath::root()));
        assert!(!set.contains(2, &rel("Photos/2019")));
        assert!(set.contains(2, &rel("x/y")));
    }

    #[test]
    fn a_parent_of_an_excluded_folder_touches_it() {
        let set = set();
        assert!(set.touches(1, &rel("Photos")));
        assert!(set.touches(1, &RelPath::root()));
        assert!(set.touches(1, &rel("Photos/2019/a")));
        assert!(!set.touches(1, &rel("Photos/2020")));
        assert!(!set.touches(3, &RelPath::root()));
    }

    #[test]
    fn a_volume_view_answers_the_same() {
        let one = set().for_volume(1);
        assert!(one.contains(&rel("Photos/2019/a")));
        assert!(one.is_excluded_dir(&rel("Photos/2019")));
        assert!(!one.is_excluded_dir(&rel("Photos/2019/a")));
        assert!(set().for_volume(9).is_empty());
    }
}
