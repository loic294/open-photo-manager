//! Move plans resolve exact names before execution and share their destination rules with
//! live formatting previews. Only FsOps mutates volume files.
//!
//! Smart hashing is bounded by 1 GiB and 20 seconds per build. Unknown content is renamed.
//! Snapshot source facts revalidate location, size, mtime and derived destinations before execution.

mod build;
mod conflicts;
mod dest;
mod execute;
mod format;
mod overlay;
mod params;
mod sidecars;

use std::collections::HashMap;

use apm_core::dto::{MoveExecuteOptions, Overlay, PlanKind, PlanResult, PlanSide};
use apm_core::paths::FileId;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use super::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanDeps, PlanError, Planner};
use crate::db::Db;

pub use format::{preview, tokens};

#[derive(Clone)]
pub struct MovePlanner {
    db: Db,
}

impl MovePlanner {
    pub fn new(deps: &PlanDeps) -> Self {
        Self {
            db: deps.db.clone(),
        }
    }
}

fn options(value: Value) -> Result<MoveExecuteOptions, PlanError> {
    serde_json::from_value(value)
        .map_err(|err| PlanError::invalid_field("options", err.to_string()))
}

impl Planner for MovePlanner {
    fn kind(&self) -> PlanKind {
        PlanKind::Move
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
        Box::pin(overlay::flags(&self.db, plan.id, side, ids))
    }
    fn ghost_items<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, Result<GhostPage, PlanError>> {
        Box::pin(overlay::items(&self.db, plan.id, side, view, window))
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
        options(value.clone())?;
        let p: apm_core::dto::MoveParams = plan.params()?;
        Ok(ExecutePrep::new(format!(
            "Moving {} files to {}",
            plan.stats.get("to_move").copied().unwrap_or(0),
            p.destination.dir
        ))
        .summary("Moved", "file", "files"))
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
