//! Batch rename is a restricted move plan: `move_plans` stores its frozen conflict policy and
//! name limit, while `move_items.seq` stores a folder-local `{count}`. It uses no second move
//! resolver, overlay query or executor. `names::Names` is the single rendering path for build,
//! preview and the execution-time `Revalidate` hook.
//!
//! The planner streams non-sidecar candidates in volume/folder/name order and retains at most one
//! folder for count assignment. `MAX_PLAN_ITEMS` applies to every in-scope non-sidecar candidate,
//! including files that do not match. Only changed names reach `Directory::resolve`; an unchanged
//! rendered name is `already_in_place`, while a case-only name change remains a move. Replace is
//! refused explicitly and a default Replace setting falls back to Smart.
//!
//! | Stats | Meaning |
//! |---|---|
//! | `candidates` | In-scope non-sidecar files |
//! | `matched` | Files selected by the matcher, including pending and failed |
//! | `to_rename` | Items with status `move` |
//! | `already_in_place` | Rendered name exactly equals the source name |
//! | `identical_at_destination` | Smart conflict policy found identical content |
//! | `skipped_by_policy` | Conflict policy left the item unchanged |
//! | `metadata_pending` | Matching files waiting for required metadata |
//! | `pattern_failed` | Matching names that could not be rendered safely |
//! | `reserved_destination` | Source or rendered paths rejected by the shared reserved/junk rules |
//! | `renamed_by_policy` | Movable names changed by conflict resolution |
//!
//! Warnings: `metadata_pending`, `date_from_file_time`, `pattern_failed`,
//! `reserved_destination`, `skipped_by_policy`, and `hash_unknown`. Execution reuses the move
//! safety skip codes `file_gone`, `stale_plan`, `stale_match`, `metadata_pending`,
//! `already_in_place`, `identical_at_destination`, `skipped_by_policy`, `sidecar_conflict`, and
//! `immich_move_refused`; the shared executor also reports `source_changed` and operation errors.
//!
//! The folder overlay is always empty because rename does not change directory membership. Ghost
//! cards still show the new names and retain the real file ids, while folders and footer counts
//! remain real-only. The preview samples the first matching folder, counts every matched file in
//! it, and holds at most 20,000 matching rows for exact counters. When truncated, later samples
//! are omitted and the preview's counter values may differ from a full plan.
//!
//! The comparison item listing shows only proposed-name ghosts; the source keeps real files.
//! Other v1 limits follow plan section 14.6:
//! `{ext}` can split RAW/JPEG counter sets; preview covers only the first matching folder; pending
//! metadata is not renamed; without `{count}` conflicts use the policy; regex uses Rust syntax;
//! rewrites affect stems only; the simple `-N` clean may remove a date-like tail; and `_N` is not
//! cleaned. Names freed by the same batch are not reused.

mod build;
mod execute;
mod names;
mod preview;

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use opm_core::dto::{MoveExecuteOptions, Overlay, PlanKind, PlanResult, PlanSide};
use opm_core::paths::FileId;
use serde_json::Value;

use super::moves::MovePlanner;
use super::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use super::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanDeps, PlanError, Planner};

pub(crate) use preview::preview;

pub(crate) struct RenamePlanner {
    moves: MovePlanner,
}

impl RenamePlanner {
    pub(crate) fn new(deps: &PlanDeps) -> Self {
        Self {
            moves: MovePlanner::new(deps),
        }
    }
}

fn options(value: Value) -> Result<MoveExecuteOptions, PlanError> {
    serde_json::from_value(value)
        .map_err(|error| PlanError::invalid_field("options", error.to_string()))
}

impl Planner for RenamePlanner {
    fn kind(&self) -> PlanKind {
        PlanKind::Rename
    }

    fn child_tables(&self) -> &'static [&'static str] {
        &[
            "plan_file_tracking",
            "plan_file_identities",
            "move_items",
            "move_plans",
        ]
    }

    fn build<'a>(
        &'a self,
        ctx: &'a BuildContext,
        params: Value,
    ) -> BoxFuture<'a, Result<Built, PlanError>> {
        Box::pin(build::build(ctx, params))
    }

    fn item_flags<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, Result<HashMap<FileId, Overlay>, PlanError>> {
        self.moves.item_flags(plan, side, ids)
    }

    fn ghost_items<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, Result<GhostPage, PlanError>> {
        self.moves.ghost_items(plan, side, view, window)
    }

    fn folder_overlay<'a>(
        &'a self,
        _plan: &'a Plan,
        _side: PlanSide,
        _view: &'a GhostView,
        _real_children: &'a [String],
    ) -> BoxFuture<'a, Result<FolderOverlay, PlanError>> {
        Box::pin(async { Ok(FolderOverlay::default()) })
    }

    fn involved_ids<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
    ) -> BoxFuture<'a, Result<Vec<FileId>, PlanError>> {
        self.moves.involved_ids(plan, side)
    }

    fn prepare_execute(&self, plan: &Plan, value: &Value) -> Result<ExecutePrep, PlanError> {
        options(value.clone())?;
        Ok(ExecutePrep::new(format!(
            "Renaming {} files",
            plan.stats.get("to_rename").copied().unwrap_or(0)
        ))
        .summary("Renamed", "file", "files"))
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        plan: &'a Plan,
        value: Value,
    ) -> BoxFuture<'a, Result<PlanResult, PlanError>> {
        Box::pin(async move {
            options(value)?;
            execute::execute(ctx, plan).await
        })
    }
}
