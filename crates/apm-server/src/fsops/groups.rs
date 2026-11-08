//! Moving an owner file together with its sidecars.

use apm_core::paths::FileId;
use apm_core::sidecar::LinkKind;

use super::error::FsOpError;
use super::executor::{MoveOutcome, MoveRequest, SkipReason};
use super::Loc;

/// What to do with one sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarAction {
    Move,
    /// The sidecar is shared with an owner that stays behind: copy it, keep the original.
    Copy,
}

#[derive(Debug, Clone)]
pub struct SidecarItem {
    pub file_id: Option<FileId>,
    pub src: Loc,
    pub kind: LinkKind,
    pub action: SidecarAction,
}

#[derive(Debug, Clone)]
pub struct MoveGroup {
    pub owner: MoveRequest,
    pub sidecars: Vec<SidecarItem>,
}

/// Why a sidecar was left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarSkip {
    /// The destination already holds identical content.
    AlreadyPresent,
    /// The derived name is taken by something else; a sidecar never gets its own new name.
    SidecarConflict,
    SameLocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarOutcome {
    Moved { dst: Loc, file_id: FileId },
    Copied { dst: Loc, file_id: FileId },
    Skipped(SidecarSkip),
    /// The owner failed or was skipped.
    NotAttempted,
    Failed(FsOpError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupOutcome {
    pub owner: MoveOutcome,
    /// One entry per `MoveGroup::sidecars`, in the same order.
    pub sidecars: Vec<SidecarOutcome>,
}

impl From<SkipReason> for SidecarSkip {
    fn from(reason: SkipReason) -> Self {
        match reason {
            SkipReason::SameLocation => Self::SameLocation,
            SkipReason::IdenticalAtDestination => Self::AlreadyPresent,
            SkipReason::DestinationExists => Self::SidecarConflict,
        }
    }
}
