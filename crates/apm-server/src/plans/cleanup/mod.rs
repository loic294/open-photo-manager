//! Absolute index-based emptiness, previewed as folders and rechecked through FsOps.
//!
//! The current source folder is a protected base. Filters and file selections never restrict
//! emptiness: every indexed kind, including orphan sidecars, prevents removal. Nonrecursive mode
//! includes wholly empty direct-child branches and their descendants; recursive mode includes
//! all empty folders strictly below base. The execution client sends `only: null`.
//!
//! The million-folder examination cap retains the generic TooLarge error; users must select a
//! deeper source folder. Plan rows use rowid STRICT storage because generic chunked collection
//! requires rowid. Overlay memory is bounded by visible child groups, not the plan size.
//!
//! Execution is deepest first, with live FsOps rechecks. Unknown files, symlinks, new content and
//! protected system paths survive; only recognized junk is trashed. Removed folders themselves
//! are not restorable. Folder-result bytes stay zero: RmdirOutcome reports a junk entry count.
//! The inverse UI toggle uses existing in-memory per-tool state, not a new persisted preference.
//!
//! Scale gate: `CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off
//! cargo test --release -p apm-server --test plans_cleanup_perf -- --ignored --nocapture`.

mod build;
mod execute;
mod overlay;

use std::collections::HashMap;

use apm_core::dto::{Overlay, PlanKind, PlanSide};
use apm_core::paths::FileId;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use super::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanDeps, PlanError, Planner};

pub struct CleanupPlanner {
    db: crate::db::Db,
}

impl CleanupPlanner {
    pub fn new(deps: &PlanDeps) -> Self {
        Self {
            db: deps.db.clone(),
        }
    }
}

impl Planner for CleanupPlanner {
    fn kind(&self) -> PlanKind {
        PlanKind::Cleanup
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
        _plan: &'a Plan,
        _side: PlanSide,
        _ids: &'a [FileId],
    ) -> BoxFuture<'a, Result<HashMap<FileId, Overlay>, PlanError>> {
        Box::pin(async { Ok(HashMap::new()) })
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
        Box::pin(overlay::folders(&self.db, plan, side, view, real_children))
    }

    fn involved_ids<'a>(
        &'a self,
        _plan: &'a Plan,
        _side: PlanSide,
    ) -> BoxFuture<'a, Result<Vec<FileId>, PlanError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn prepare_execute(&self, plan: &Plan, options: &Value) -> Result<ExecutePrep, PlanError> {
        execute::prepare(plan, options)
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        plan: &'a Plan,
        options: Value,
    ) -> BoxFuture<'a, Result<apm_core::dto::PlanResult, PlanError>> {
        Box::pin(execute::execute(ctx, plan, options))
    }

    fn child_tables(&self) -> &'static [&'static str] {
        &["cleanup_dirs"]
    }
}
