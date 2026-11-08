//! Crash recovery.

use tokio_util::sync::CancellationToken;

use super::{FsOps, Loc};

impl FsOps {
    pub async fn reconcile_at_startup(&self, _shutdown: CancellationToken) {}
}

/// What `FsOps::reconcile` found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Pending journal rows looked at.
    pub examined: usize,
    /// Rows finished (the index now matches the disk).
    pub completed: usize,
    /// Rows given up on without changing the disk.
    pub abandoned: usize,
    /// Rows left for a later run (offline volume, or still running in this process).
    pub still_pending: usize,
    /// Leftover part files deleted.
    pub parts_removed: usize,
    /// Stray part files swept.
    pub stray_parts_swept: usize,
    /// Old journal rows deleted.
    pub pruned: u64,
    /// Folders whose index needs a rescan (hand them to the indexer).
    pub rescan: Vec<Loc>,
    /// Rows that could not be processed, with the reason.
    pub errors: Vec<String>,
}
