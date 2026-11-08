//! A short-lived memory of the directories this process changed itself.
//!
//! The file watcher (a later todo) sees our own renames as file-system events. It asks
//! [`SelfWrites::is_recent`] to tell them from outside changes and does not rescan what the file
//! operations already indexed. The directories are noted BEFORE the disk step (the events can
//! arrive before the operation returns) and again after the index transaction committed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use apm_core::paths::{RelPath, VolumeId};

/// Entries older than this are forgotten when the map is full.
const RETENTION: Duration = Duration::from_secs(5 * 60);
/// The map never grows beyond this (the memory is a hint, losing it only costs a rescan).
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Default)]
pub struct SelfWrites {
    dirs: Arc<Mutex<HashMap<(VolumeId, String), Instant>>>,
}

impl SelfWrites {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers that this process just changed (or is changing) `dir`.
    pub fn note(&self, volume_id: VolumeId, dir: &RelPath) {
        let now = Instant::now();
        let mut dirs = self.lock();
        if dirs.len() >= MAX_ENTRIES {
            dirs.retain(|_, noted| now.duration_since(*noted) < RETENTION);
            if dirs.len() >= MAX_ENTRIES {
                dirs.clear();
            }
        }
        dirs.insert((volume_id, dir.as_str().to_owned()), now);
    }

    /// Whether `dir` was noted less than `within` ago.
    pub fn is_recent(&self, volume_id: VolumeId, dir: &RelPath, within: Duration) -> bool {
        self.lock()
            .get(&(volume_id, dir.as_str().to_owned()))
            .is_some_and(|noted| noted.elapsed() <= within)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(VolumeId, String), Instant>> {
        self.dirs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    #[test]
    fn a_noted_directory_is_recent_for_its_volume_only() {
        let writes = SelfWrites::new();
        writes.note(1, &rel("a/b"));
        let long = Duration::from_secs(60);
        assert!(writes.is_recent(1, &rel("a/b"), long));
        assert!(!writes.is_recent(2, &rel("a/b"), long));
        assert!(!writes.is_recent(1, &rel("a"), long));
    }

    #[test]
    fn an_old_note_is_not_recent() {
        let writes = SelfWrites::new();
        writes.note(1, &rel(""));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!writes.is_recent(1, &rel(""), Duration::from_millis(1)));
        assert!(writes.is_recent(1, &rel(""), Duration::from_secs(60)));
    }

    #[test]
    fn the_memory_is_bounded() {
        let writes = SelfWrites::new();
        for index in 0..(MAX_ENTRIES + 50) {
            writes.note(1, &rel(&format!("d{index}")));
        }
        assert!(writes.lock().len() <= MAX_ENTRIES);
        assert!(writes.is_recent(
            1,
            &rel(&format!("d{}", MAX_ENTRIES + 49)),
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn clones_share_the_memory() {
        let writes = SelfWrites::new();
        let clone = writes.clone();
        clone.note(3, &rel("x"));
        assert!(writes.is_recent(3, &rel("x"), Duration::from_secs(60)));
    }
}
