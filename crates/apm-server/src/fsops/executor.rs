//! The move/copy engine.

use apm_core::dto::ConflictPolicy;
use apm_core::paths::FileId;

use super::{Expect, Loc};

/// One file to move or copy.
#[derive(Debug, Clone)]
pub struct MoveRequest {
    /// The index row of the source when the caller knows it (a hint: the engine looks the row up by
    /// location right before the disk step).
    pub file_id: Option<FileId>,
    pub src: Loc,
    pub dst: Loc,
    pub policy: ConflictPolicy,
    /// The size and mtime the plan saw; a mismatch is `source_changed` and nothing is touched.
    pub expect: Option<Expect>,
}

/// Why a file was left where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Source and destination are the same place.
    SameLocation,
    /// The destination already holds a file with the same content; the source is untouched.
    IdenticalAtDestination,
    /// The destination exists and the policy says skip (or the name may not change).
    DestinationExists,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved {
        /// The FINAL location (a conflict may have renamed the file).
        dst: Loc,
        renamed: bool,
        cross_device: bool,
        bytes: u64,
        /// The index row of the file at its new location (the same id for a move).
        file_id: FileId,
    },
    Skipped(SkipReason),
}
