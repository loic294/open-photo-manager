//! Moving an owner file together with its sidecars ([`FsOps::move_group`]).
//!
//! The owner moves first, with the engine's full conflict handling. When it has landed, every
//! sidecar follows to the name derived from the owner's FINAL name (`opm_core::conflict::
//! sidecar_destination`), in the owner's destination folder. A sidecar never gets a name of its own:
//! when its derived name is taken it is skipped (`SidecarConflict`), except identical content (it
//! is already there) and the `Replace` policy (the existing file goes to the trash).
//!
//! * Each member is its own transfer: its own journal row and its own index transaction, so a
//!   crash between two members leaves every file somewhere complete and reconcilable; there is no
//!   group-level atomicity. A sidecar that fails is reported as `Failed` and the group goes on.
//! * A group whose owner moved is finished even when the context is cancelled meanwhile: the
//!   sidecars are tiny and leaving them behind would orphan them.
//! * The link rows are repaired once at the end ([`FsOps::apply_group_fixup`]): links that now
//!   cross folders are dropped and every destination folder is linked again with the scanner's own
//!   function. A failure there only logs: the next scan and the startup sweep repair the links.

use std::collections::{BTreeSet, HashSet};

use opm_core::dto::ConflictPolicy;
use opm_core::paths::FileId;
use opm_core::sidecar::LinkKind;

use super::apply::{Crossing, Mode};
use super::context::OpContext;
use super::dirs::validate_file_path;
use super::error::FsOpError;
use super::executor::{Companion, Job, MoveOutcome, MoveRequest, SkipReason, Transferred};
use super::{FsOps, Loc};

/// What to do with one sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarAction {
    Move,
    /// The sidecar is shared with an owner that stays behind: copy it, keep the original.
    Copy,
}

#[derive(Debug, Clone)]
pub struct SidecarItem {
    /// The index row of the sidecar when the caller knows it (a hint, ignored).
    pub file_id: Option<FileId>,
    /// Where the sidecar is now: the owner's folder.
    pub src: Loc,
    pub kind: LinkKind,
    pub action: SidecarAction,
}

#[derive(Debug, Clone)]
pub struct MoveGroup {
    /// The owner is always MOVED; its `policy` also rules the sidecars' conflicts.
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
    Moved {
        dst: Loc,
        file_id: FileId,
    },
    Copied {
        dst: Loc,
        file_id: FileId,
    },
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

impl FsOps {
    /// Moves the owner, then its sidecars to the names derived from the owner's final name.
    ///
    /// # Errors
    /// The owner's errors (see [`FsOps::move_file`]): then nothing was moved. A malformed group
    /// (a sidecar that is not in the owner's folder, listed twice, or whose derived name is not
    /// valid) is refused before anything is touched: `destination_invalid`, or `name_too_long`
    /// when a derived name is too long. A sidecar's own failure is reported in its
    /// [`SidecarOutcome::Failed`], not here.
    pub async fn move_group(
        &self,
        group: MoveGroup,
        cx: &OpContext,
    ) -> Result<GroupOutcome, FsOpError> {
        let companions = self.check_group(&group)?;
        let owner_job = Job {
            mode: Mode::Move,
            src: group.owner.src.clone(),
            dst: group.owner.dst.clone(),
            policy: group.owner.policy,
            expect: group.owner.expect,
            allow_rename: true,
            companions: companions.clone(),
            crossing: Crossing::Keep,
            restore: None,
        };
        let owner = self.transfer(&owner_job, cx).await?.into_outcome()?;
        let MoveOutcome::Moved { dst, file_id, .. } = &owner else {
            return Ok(GroupOutcome {
                owner,
                sidecars: vec![SidecarOutcome::NotAttempted; group.sidecars.len()],
            });
        };
        let owner_dst = dst.clone();
        let mut placed = vec![*file_id];

        let finishing = cx.without_cancel();
        let mut sidecars = Vec::with_capacity(group.sidecars.len());
        for (item, companion) in group.sidecars.iter().zip(&companions) {
            let outcome = self
                .move_sidecar(item, companion, &owner_dst, group.owner.policy, &finishing)
                .await;
            match outcome {
                // A simulated crash stops everything, exactly like a dead process would.
                Err(err) if err.is_abort() => return Err(err),
                Err(err) => sidecars.push(SidecarOutcome::Failed(err)),
                Ok(outcome) => {
                    if let SidecarOutcome::Moved { file_id, .. }
                    | SidecarOutcome::Copied { file_id, .. } = &outcome
                    {
                        placed.push(*file_id);
                    }
                    sidecars.push(outcome);
                }
            }
        }

        let dest_dirs = BTreeSet::from([(owner_dst.volume_id, owner_dst.dir())]);
        if let Err(err) = self
            .apply_group_fixup(&self.rules(), &placed, &dest_dirs)
            .await
        {
            tracing::warn!(error = %err, "could not repair the sidecar links of a moved group");
        }
        Ok(GroupOutcome { owner, sidecars })
    }

    /// Checks a group without touching anything and derives how each sidecar follows its owner.
    fn check_group(&self, group: &MoveGroup) -> Result<Vec<Companion>, FsOpError> {
        let rules = self.rules();
        let owner = &group.owner;
        rules.check_included(&owner.src)?;
        rules.check_included(&owner.dst)?;
        let mut listed = HashSet::new();
        let mut companions = Vec::with_capacity(group.sidecars.len());
        for item in &group.sidecars {
            if item.src.volume_id != owner.src.volume_id || item.src.dir() != owner.src.dir() {
                return Err(FsOpError::DestinationInvalid(
                    "a sidecar must sit next to its owner",
                ));
            }
            if item.src == owner.src || !listed.insert(&item.src) {
                return Err(FsOpError::DestinationInvalid(
                    "a file may appear once in a group",
                ));
            }
            let companion = Companion {
                link: item.kind,
                old_owner: owner.src.name().to_owned(),
                sidecar: item.src.name().to_owned(),
            };
            let planned = owner
                .dst
                .with_name(&companion.destination(owner.dst.name()))?;
            validate_file_path(&rules, &rules.junk, &planned.rel)?;
            companions.push(companion);
        }
        Ok(companions)
    }

    async fn move_sidecar(
        &self,
        item: &SidecarItem,
        companion: &Companion,
        owner_dst: &Loc,
        policy: ConflictPolicy,
        cx: &OpContext,
    ) -> Result<SidecarOutcome, FsOpError> {
        let dst = owner_dst.with_name(&companion.destination(owner_dst.name()))?;
        let job = Job {
            mode: match item.action {
                SidecarAction::Move => Mode::Move,
                SidecarAction::Copy => Mode::Copy,
            },
            src: item.src.clone(),
            dst,
            policy,
            expect: None,
            allow_rename: false,
            companions: Vec::new(),
            crossing: Crossing::Keep,
            restore: None,
        };
        match self.transfer(&job, cx).await? {
            Transferred::Skipped(reason) => Ok(SidecarOutcome::Skipped(reason.into())),
            Transferred::Done { dst, indexed, .. } => {
                let file_id = indexed
                    .file_id
                    .ok_or_else(|| FsOpError::internal("the sidecar has no index row"))?;
                Ok(match item.action {
                    SidecarAction::Move => SidecarOutcome::Moved { dst, file_id },
                    SidecarAction::Copy => SidecarOutcome::Copied { dst, file_id },
                })
            }
        }
    }
}
