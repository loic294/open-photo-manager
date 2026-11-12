//! Change reporting: how the indexer (and the file operations) tell the UI which directories changed.
//!
//! The contract, which every writer of `files` / `dirs` follows:
//!
//! * [`ChangeSink::dirs_changed`] is called AFTER the transaction that made the change has
//!   committed, never before: the UI refetches the directories as soon as it hears about them and
//!   must find the new state.
//! * The list holds every directory whose VISIBLE LISTING changed: the directory of each inserted,
//!   updated or removed file, and for an inserted or removed sub-directory both the directory
//!   itself and its parent (a folder list shows sub-folders and counts). The paths are sorted and
//!   without duplicates within one call.
//! * Nothing is reported for a change nobody can see (a directory's modification time, a
//!   re-stamped generation), so a rescan of an unchanged tree reports nothing.
//!
//! The server adapts a sink to `fs_change` events (`ServerEvent::FsChange`); the sink is kept as a
//! trait so the indexer and the file operations do not depend on the task engine.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use opm_core::paths::{RelPath, VolumeId};

/// Receives the directories whose listing changed.
pub trait ChangeSink: Send + Sync {
    fn dirs_changed(&self, volume_id: VolumeId, dirs: &[RelPath]);
}

/// Discards every report.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopChanges;

impl ChangeSink for NoopChanges {
    fn dirs_changed(&self, _volume_id: VolumeId, _dirs: &[RelPath]) {}
}

/// One call of [`ChangeSink::dirs_changed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeCall {
    pub volume_id: VolumeId,
    pub dirs: Vec<RelPath>,
}

/// Keeps every report, for tests.
#[derive(Debug, Default)]
pub struct RecordingChanges {
    calls: Mutex<Vec<ChangeCall>>,
}

impl RecordingChanges {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<ChangeCall> {
        self.lock().clone()
    }

    /// The directories reported for `volume_id` so far, as strings (`""` is the volume root).
    pub fn dirs(&self, volume_id: VolumeId) -> BTreeSet<String> {
        self.lock()
            .iter()
            .filter(|call| call.volume_id == volume_id)
            .flat_map(|call| call.dirs.iter().map(|dir| dir.as_str().to_owned()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Forgets everything reported so far.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ChangeCall>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ChangeSink for RecordingChanges {
    fn dirs_changed(&self, volume_id: VolumeId, dirs: &[RelPath]) {
        self.lock().push(ChangeCall {
            volume_id,
            dirs: dirs.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    #[test]
    fn the_recording_sink_keeps_calls_in_order() {
        let sink = RecordingChanges::new();
        assert!(sink.is_empty());
        sink.dirs_changed(1, &[rel("a"), rel("a/b")]);
        sink.dirs_changed(2, &[rel("")]);
        sink.dirs_changed(1, &[rel("a")]);

        let calls = sink.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].volume_id, 2);
        assert_eq!(
            sink.dirs(1).into_iter().collect::<Vec<_>>(),
            ["a", "a/b"],
            "the union for one volume"
        );
        assert_eq!(sink.dirs(2).into_iter().collect::<Vec<_>>(), [""]);

        sink.clear();
        assert!(sink.is_empty());
    }

    #[test]
    fn the_noop_sink_accepts_anything() {
        NoopChanges.dirs_changed(1, &[rel("x")]);
    }
}
