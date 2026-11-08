//! Trash, restore and purge.

use apm_core::paths::{FileId, RelPath};

use super::executor::SkipReason;
use super::{Expect, Loc, TrashBatch};

/// One file or folder to trash.
#[derive(Debug, Clone)]
pub struct TrashRequest {
    pub file_id: Option<FileId>,
    pub loc: Loc,
    pub expect: Option<Expect>,
    pub batch: TrashBatch,
    /// Free text kept in `trash_items.reason` (`user`, `replaced`, `junk`, `moved`, ...).
    pub reason: String,
}

/// A trashed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashOutcome {
    pub trash_item_id: i64,
    /// Where the item is now, relative to the volume root (below `.apm-trash/`).
    pub trash_path: RelPath,
    pub is_dir: bool,
    pub bytes: u64,
    /// How many `files` rows were removed from the index.
    pub files_removed: u64,
}

/// A restored item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored {
        /// Where the item went (a conflict may have renamed it).
        dst: Loc,
        renamed: bool,
        /// The new row of a restored file; `None` for a folder (rescanned instead).
        file_id: Option<FileId>,
        bytes: u64,
    },
    Skipped(SkipReason),
}

/// A purged item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub bytes: u64,
    pub files: u64,
}
