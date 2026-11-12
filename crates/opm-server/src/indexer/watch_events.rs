//! Pure event classification and lossless per-directory debounce.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use notify::Event;
use notify::event::{AccessKind, AccessMode, ModifyKind};
use opm_core::exclusions::Exclusions;
use opm_core::junk::JunkRules;
use opm_core::paths::{RelPath, VolumeId};

use super::ScanTarget;
use crate::db::volumes::PROBE_PREFIX;
use crate::fsops::SelfWrites;
use crate::security::VolumeRoot;

pub const QUIET: Duration = Duration::from_secs(5);
pub const MAX_WAIT: Duration = Duration::from_secs(60);
pub const TARGET_LIMIT: usize = 2000;
const DIRTY_LIMIT: usize = 8192;

pub(crate) fn relevant(event: &Event) -> bool {
    match event.kind {
        notify::EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        notify::EventKind::Access(_) => false,
        notify::EventKind::Create(_)
        | notify::EventKind::Remove(_)
        | notify::EventKind::Modify(_) => true,
        notify::EventKind::Any | notify::EventKind::Other => true,
    }
}

pub(crate) fn invalidates_watches(event: &Event) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Remove(_) | notify::EventKind::Modify(ModifyKind::Name(_))
    )
}

pub(crate) fn map_path(
    root: &VolumeRoot,
    path: &std::path::Path,
    junk: &JunkRules,
) -> Option<RelPath> {
    root.rel_of(path).filter(|rel| {
        !junk.is_junk_path(rel)
            && !rel.file_name().is_some_and(|name| junk.is_junk_name(name))
            && !rel.components().any(|name| name.starts_with(PROBE_PREFIX))
    })
}

#[derive(Debug)]
pub(crate) struct Mark {
    pub volume_id: VolumeId,
    pub dir: RelPath,
    pub candidates: BTreeSet<RelPath>,
    pub full: bool,
    first: Instant,
    last: Instant,
}

#[derive(Default)]
pub(crate) struct Dirty {
    marks: BTreeMap<(VolumeId, RelPath), Mark>,
}

impl Dirty {
    pub fn len(&self) -> usize {
        self.marks.len()
    }
    pub fn clear(&mut self) {
        self.marks.clear();
    }

    pub fn filter(&mut self, junk: &JunkRules, excluded: &Exclusions) {
        self.marks.retain(|(volume_id, dir), mark| {
            mark.candidates.retain(|path| {
                !junk.is_junk_path(path)
                    && !path.file_name().is_some_and(|name| junk.is_junk_name(name))
                    && !excluded.contains(*volume_id, path)
            });
            !junk.is_junk_path(dir) && !excluded.contains(*volume_id, dir)
        });
    }

    pub fn mark(&mut self, volume_id: VolumeId, path: RelPath, now: Instant) {
        let dir = path.parent().unwrap_or_else(RelPath::root);
        if self.marks.contains_key(&(volume_id, RelPath::root()))
            && self.marks[&(volume_id, RelPath::root())].full
        {
            return;
        }
        if self.marks.len() >= DIRTY_LIMIT {
            self.full(volume_id, now);
            return;
        }
        let mark = self
            .marks
            .entry((volume_id, dir.clone()))
            .or_insert_with(|| Mark {
                volume_id,
                dir,
                candidates: BTreeSet::new(),
                full: false,
                first: now,
                last: now,
            });
        mark.last = now;
        mark.candidates.insert(path);
        if mark.candidates.len() > TARGET_LIMIT {
            self.full(volume_id, now);
        }
    }

    pub fn full(&mut self, volume_id: VolumeId, now: Instant) {
        let first = self
            .marks
            .values()
            .filter(|m| m.volume_id == volume_id)
            .map(|m| m.first)
            .min()
            .unwrap_or(now);
        self.marks.retain(|(id, _), _| *id != volume_id);
        self.marks.insert(
            (volume_id, RelPath::root()),
            Mark {
                volume_id,
                dir: RelPath::root(),
                candidates: BTreeSet::new(),
                full: true,
                first,
                last: now,
            },
        );
    }

    /// Removes only ready marks. Events admitted after this drain create a new batch.
    pub fn drain(&mut self, now: Instant, quiet: Duration, max_wait: Duration) -> Vec<Mark> {
        let keys: Vec<_> = self
            .marks
            .iter()
            .filter(|(_, mark)| {
                now.duration_since(mark.last) >= quiet || now.duration_since(mark.first) >= max_wait
            })
            .map(|(key, _)| key.clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| self.marks.remove(&key))
            .collect()
    }

    pub fn restore(&mut self, marks: Vec<Mark>) {
        for mark in marks {
            self.marks.insert((mark.volume_id, mark.dir.clone()), mark);
        }
    }
}

pub(crate) fn assemble(
    mut targets: Vec<ScanTarget>,
    writes: &SelfWrites,
    window: Duration,
) -> Vec<ScanTarget> {
    targets.retain(|target| !writes.is_recent(target.volume_id, &target.dir, window));
    targets.sort_by(|a, b| a.dir.cmp(&b.dir).then(b.recursive.cmp(&a.recursive)));
    targets.dedup();
    let recursive: Vec<_> = targets
        .iter()
        .filter(|t| t.recursive)
        .map(|t| t.dir.clone())
        .collect();
    targets.retain(|t| {
        !recursive
            .iter()
            .any(|root| (t.dir != *root || !t.recursive) && t.dir.starts_with(root))
    });
    if targets.len() > TARGET_LIMIT {
        let id = targets[0].volume_id;
        return if writes.is_recent(id, &RelPath::root(), window) {
            Vec::new()
        } else {
            vec![ScanTarget::volume(id)]
        };
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;
    use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode};

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    #[test]
    fn event_classification_and_rename_purge() {
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Create(CreateKind::Folder),
            EventKind::Remove(RemoveKind::File),
            EventKind::Remove(RemoveKind::Folder),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
        ] {
            assert!(relevant(&Event::new(kind)));
        }
        for kind in [
            AccessKind::Open(AccessMode::Read),
            AccessKind::Read,
            AccessKind::Close(AccessMode::Read),
            AccessKind::Close(AccessMode::Any),
        ] {
            assert!(!relevant(&Event::new(EventKind::Access(kind))));
        }
        assert!(invalidates_watches(&Event::new(EventKind::Remove(
            RemoveKind::Any
        ))));
        assert!(invalidates_watches(&Event::new(EventKind::Modify(
            ModifyKind::Name(RenameMode::Both)
        ))));
        assert!(!invalidates_watches(&Event::new(EventKind::Create(
            CreateKind::Folder
        ))));
    }

    #[test]
    fn mapping_drops_junk_reserved_paths_and_outside_roots() {
        let scratch = crate::indexer::test_support::scratch_dir();
        let root = VolumeRoot::new(1, scratch.path()).unwrap();
        let junk = JunkRules::default();
        for path in [
            "@eaDir/pic.jpg",
            "#recycle/pic.jpg",
            "#snapshot/pic.jpg",
            ".opm-trash/batch/pic.jpg",
            "pic.opm-part",
            ".DS_Store",
            "._pic",
        ] {
            assert!(
                map_path(&root, &root.path().join(path), &junk).is_none(),
                "{path}"
            );
        }
        assert!(map_path(&root, &root.path().join("../outside"), &junk).is_none());
        assert_eq!(
            map_path(&root, &root.path().join("Album/pic.jpg"), &junk),
            Some(rel("Album/pic.jpg"))
        );
    }

    #[test]
    fn quiet_directories_do_not_wait_for_busy_ones_and_drain_is_atomic() {
        let now = Instant::now();
        let mut dirty = Dirty::default();
        dirty.mark(1, rel("quiet/a"), now);
        dirty.mark(1, rel("busy/a"), now);
        dirty.mark(1, rel("busy/b"), now + Duration::from_secs(4));
        let batch = dirty.drain(now + QUIET, QUIET, MAX_WAIT);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].dir, rel("quiet"));
        dirty.mark(1, rel("quiet/next"), now + QUIET);
        assert_eq!(dirty.len(), 2);
        let batch = dirty.drain(now + QUIET * 2, QUIET, MAX_WAIT);
        assert_eq!(batch.len(), 2);
        assert!(
            batch
                .iter()
                .find(|m| m.dir == rel("quiet"))
                .unwrap()
                .candidates
                .contains(&rel("quiet/next"))
        );
    }

    #[test]
    fn max_wait_slices_continuous_import_and_overflow_preserves_first_deadline() {
        let now = Instant::now();
        let mut dirty = Dirty::default();
        dirty.mark(1, rel("busy/a"), now);
        for sec in 1..60 {
            dirty.mark(1, rel("busy/a"), now + Duration::from_secs(sec));
        }
        assert!(
            dirty
                .drain(now + MAX_WAIT - Duration::from_secs(1), QUIET, MAX_WAIT)
                .is_empty()
        );
        dirty.full(1, now + MAX_WAIT - Duration::from_secs(1));
        let batch = dirty.drain(now + MAX_WAIT, QUIET, MAX_WAIT);
        assert_eq!(batch.len(), 1);
        assert!(batch[0].full);
    }

    #[test]
    fn failed_submission_restores_marks_and_storms_are_bounded() {
        let now = Instant::now();
        let mut dirty = Dirty::default();
        for n in 0..TARGET_LIMIT + 1 {
            dirty.mark(1, rel(&format!("album/{n}")), now);
        }
        assert_eq!(dirty.len(), 1);
        let batch = dirty.drain(now + QUIET, QUIET, MAX_WAIT);
        assert!(batch[0].full);
        dirty.restore(batch);
        assert_eq!(dirty.len(), 1);
    }

    #[test]
    fn targets_merge_recursive_roots_without_matching_siblings_and_suppress_self_writes() {
        let writes = SelfWrites::new();
        let targets = vec![
            ScanTarget::dir(1, rel("a")),
            ScanTarget::subtree(1, rel("a")),
            ScanTarget::dir(1, rel("a/b")),
            ScanTarget::dir(1, rel("a.old")),
            ScanTarget::subtree(1, rel("a/b")),
        ];
        assert_eq!(
            assemble(targets, &writes, QUIET),
            vec![
                ScanTarget::subtree(1, rel("a")),
                ScanTarget::dir(1, rel("a.old"))
            ]
        );
        writes.note(1, &rel("a"));
        assert!(assemble(vec![ScanTarget::subtree(1, rel("a"))], &writes, QUIET).is_empty());
        let targets = (0..TARGET_LIMIT + 1)
            .map(|n| ScanTarget::dir(1, rel(&format!("d{n}"))))
            .collect();
        assert_eq!(
            assemble(targets, &writes, QUIET),
            vec![ScanTarget::volume(1)]
        );
    }
}
