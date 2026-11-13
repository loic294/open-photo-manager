//! Duplicate plans share one pair stream with the move planner.
//!
//! Stats: source_candidates, comparison_candidates, pairs, source_matched, comparison_matched,
//! source_matched_bytes, comparison_matched_bytes, unmatchable_source, unmatchable_comparison.
//! Warnings: unhashed_files, no_capture_time, empty_stem, overlapping_scopes.
//! Limits: 2,000,000 pairs; 1,000,000 distinct matches per side. Narrow folders/filters on overflow.
//! Deletion always requires a live matching survivor OUTSIDE the entire effective deletion set.
//! Safety refusals are recorded as coded skips:
//! no_survivor, stale_match, file_gone, stale_index, or FsOpError::code().
//! The derived duplicate_matches location/live cache is synchronized on insertion, file relocation
//! and file deletion by SQLite triggers, in the same writer transaction. Snapshot ids remain for
//! file_gone execution details. Folder overlays scan this covering live cache (not every files row);
//! flags and hand-off ids still join files. This keeps 200-child overlays over 520k pairs below
//! 50 ms even with unoptimized SQLite on the development machine.
//! Tombstones never revive on file-id reuse, including restore. Deletes during a build are also
//! recorded per plan so later bulk insertion cannot resurrect an obsolete snapshot identity.

mod build;
mod execute;
mod files;
mod overlay;

use futures_util::future::BoxFuture;
use opm_core::dto::{
    DuplicateAction, DuplicateExecuteOptions, DuplicateFileParams, DuplicateParams, Overlay,
    PlanKind, PlanResult, PlanSide, Scope,
};
use opm_core::paths::FileId;
use serde_json::Value;
use std::collections::HashMap;

use super::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use super::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanDeps, PlanError, Planner};
use crate::db::Db;

pub(crate) use build::{OverlapPolicy, PairVisitor, candidate_pairs};
pub use files::groups;

pub const MAX_PAIRS: u64 = 2_000_000;
pub const MAX_MATCHED_PER_SIDE: usize = 1_000_000;
pub const MAX_FILE_VIEW_MATCHES: usize = 20_000;

fn parameters(value: Value) -> Result<(DuplicateParams, Option<DuplicateFileParams>), PlanError> {
    if value.get("matching").is_some() {
        let file: DuplicateFileParams =
            serde_json::from_value(value).map_err(|err| PlanError::invalid(err.to_string()))?;
        if file.excluded.len() > 100 {
            return Err(PlanError::invalid_field(
                "excluded",
                "at most 100 excluded folders",
            ));
        }
        if file.matching.comparison.scope
            != (Scope {
                volume_id: None,
                dir: String::new(),
                recursive: true,
            })
            || file.matching.comparison.filter != Default::default()
        {
            return Err(PlanError::invalid_field(
                "matching.comparison",
                "file view must search all volumes without a comparison filter",
            ));
        }
        Ok((file.matching.clone(), Some(file)))
    } else {
        serde_json::from_value(value)
            .map(|params| (params, None))
            .map_err(|err| PlanError::invalid(err.to_string()))
    }
}

#[derive(Clone)]
pub struct DuplicatesPlanner {
    db: Db,
    max_matched: usize,
}

impl DuplicatesPlanner {
    pub fn new(deps: &PlanDeps) -> Self {
        Self {
            db: deps.db.clone(),
            max_matched: MAX_MATCHED_PER_SIDE,
        }
    }
}

pub(super) fn side_name(side: PlanSide) -> &'static str {
    match side {
        PlanSide::Source => "source",
        PlanSide::Comparison => "comparison",
    }
}

fn options(value: &Value) -> Result<DuplicateExecuteOptions, PlanError> {
    let options: DuplicateExecuteOptions = serde_json::from_value(value.clone())
        .map_err(|err| PlanError::invalid_field("action", err.to_string()))?;
    if options.action == DuplicateAction::Move {
        return Err(PlanError::invalid_field(
            "action",
            "use POST /api/plans/{id}/selection to hand matched files to Asset move",
        ));
    }
    Ok(options)
}

impl Planner for DuplicatesPlanner {
    fn kind(&self) -> PlanKind {
        PlanKind::Duplicates
    }
    fn child_tables(&self) -> &'static [&'static str] {
        &[
            super::file_identity::CHILD_TABLES[0],
            super::file_identity::CHILD_TABLES[1],
            "duplicate_pairs",
            "duplicate_matches",
        ]
    }
    fn build<'a>(
        &'a self,
        ctx: &'a BuildContext,
        params: Value,
    ) -> BoxFuture<'a, Result<Built, PlanError>> {
        Box::pin(build::build(ctx, params, self.max_matched))
    }
    fn item_flags<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, Result<HashMap<FileId, Overlay>, PlanError>> {
        Box::pin(overlay::flags(&self.db, plan.id, side, ids))
    }
    fn ghost_items<'a>(
        &'a self,
        _plan: &'a Plan,
        _side: PlanSide,
        _view: &'a GhostView,
        _window: Window,
    ) -> BoxFuture<'a, Result<GhostPage, PlanError>> {
        Box::pin(async { Ok(GhostPage::default()) })
    }
    fn folder_overlay<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, Result<FolderOverlay, PlanError>> {
        Box::pin(overlay::folders(
            &self.db,
            plan.id,
            side,
            view,
            real_children,
        ))
    }
    fn involved_ids<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
    ) -> BoxFuture<'a, Result<Vec<FileId>, PlanError>> {
        Box::pin(overlay::ids(&self.db, plan.id, side))
    }
    fn prepare_execute(&self, plan: &Plan, value: &Value) -> Result<ExecutePrep, PlanError> {
        if parameters(plan.params()?)?.1.is_some() {
            execute::keepers(value)?;
            return Ok(ExecutePrep::new("Moving selected duplicates to the trash")
                .summary("Trashed", "file", "files"));
        }
        let side = if options(value)?.action == DuplicateAction::DeleteSource {
            "source"
        } else {
            "comparison"
        };
        Ok(
            ExecutePrep::new(format!("Moving {side} duplicates to the trash"))
                .summary("Trashed", "file", "files"),
        )
    }
    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        plan: &'a Plan,
        value: Value,
    ) -> BoxFuture<'a, Result<PlanResult, PlanError>> {
        Box::pin(execute::execute(ctx, plan, value))
    }
}
