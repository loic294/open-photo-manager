//! Move plans resolve exact names before execution and share their destination rules with
//! live formatting previews. Only FsOps mutates volume files.
//!
//! Smart hashing is bounded by 1 GiB and 20 seconds per build. Unknown content is renamed.
//! Snapshot source facts revalidate location, size, mtime and derived destinations before execution.
//!
//! Release scale (200k sources, n=3; all values min/median/max milliseconds):
//! | Type | Build | Ghost page + total | Flags | Preview |
//! | --- | --- | --- | --- | --- |
//! | KeepSource | 3055.465/3151.374/3182.421 | 4.716/5.048/5.280 | .837/.927/1.215 | .224/.243/.811 |
//! | KeepDestination | 5020.779/5042.105/5043.381 | 5.159/5.174/5.204 | .980/1.005/1.637 | .197/.198/.234 |
//! | Flat | 2352.146/2372.602/2373.937 | 7.917/8.008/9.536 | .522/.604/.793 | .205/.242/.252 |
//! | Custom | 3307.751/3314.792/3319.150 | 4.839/4.917/5.147 | .832/1.464/1.653 | .215/.233/.236 |
//!
//! The KeepDestination build misses the UNCHANGED 5000 ms gate by 0.84% at the quiet
//! host floor (idle 71.2% before, 67.46% after; load 18.08/22.58/26.04 before,
//! 36.81/27.65/27.71 after). Landing with this documented miss was explicitly approved.
//! All other gates pass: ghosts 50 ms, flags 10 ms, preview 50 ms, other builds 5000 ms.
//! Residual cost includes shared matching, counterpart projections and identity tracking;
//! their individual contributions are not yet profiled. Next tuning lever: profile those
//! phases before optimizing projection/track batches, preserving the matching and identity seams.
//! Exact unfiltered counts use the covering move range minus distinct dead dependencies;
//! build completion verifies identity membership. Pages and filtered counts retain live joins.
//! Debug diagnostic build medians were 11257.868/14785.409/8605.799/13002.953 ms
//! in the same type order; debug mode reports timings without enforcing release thresholds.

mod build;
mod conflicts;
mod dest;
mod execute;
mod format;
mod overlay;
mod params;
mod sidecars;

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use opm_core::dto::{MoveExecuteOptions, Overlay, PlanKind, PlanResult, PlanSide};
use opm_core::paths::FileId;
use serde_json::Value;

use super::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use super::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanDeps, PlanError, Planner};
use crate::db::Db;

pub(crate) use build::source_statement;
pub(crate) use conflicts::{Directory, HashBudget, Resolved};
pub(crate) use dest::{COLUMNS, Source, Wanted};
pub(crate) use execute::{Item, Revalidate, Stored, run};
pub use format::{preview, tokens};
pub(crate) use params::{reserved, source_query, validate_volume};
pub(crate) use sidecars::{Link, links};

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
        let p: opm_core::dto::MoveParams = plan.params()?;
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
