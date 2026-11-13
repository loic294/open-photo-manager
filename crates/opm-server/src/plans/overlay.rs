//! Plan overlay: the seam between the browse API and the plans. It computes the `Overlay` flags
//! (matched, planned, will_delete), the ghost files and the folder match counts the browse API
//! adds to a listing.
//!
//! The interface in this file ([`PlanOverlay`] and its data types) is defined by todo
//! `building-browse-api`. Computing a real overlay belongs to todo `building-plan-engine`: the
//! trait and the types stay exactly as they are, and `impl PlanOverlay for PlanService` at the
//! bottom of this file hands each question to the `Planner` of the plan's kind.
//!
//! # What the browse API asks
//!
//! A browse request that carries a `plan` ([`PlanRef`]: the plan and the side of it the pane
//! shows) normally lists real files from the index and asks this trait for the rest. Without a
//! `plan` the trait is never called, and the volumes root never calls it either. The questions:
//!
//! * [`PlanOverlay::ghosts_only`]: whether the listing replaces real files with proposed names
//!   (rename comparison), rather than appending ghosts.
//! * [`PlanOverlay::item_flags`]: for the real files of one page, which ones have a match on the
//!   other side of the plan (`matched`) or are removed by it (`will_delete`).
//! * [`PlanOverlay::ghost_items`]: the files that will exist DIRECTLY in the listed directory once
//!   the plan runs (a page of them, plus how many there are). They are shown after the real files.
//! * [`PlanOverlay::folder_overlay`]: plan data for the folder list of a directory: match counts
//!   and flags of the real subfolders, the subfolders that do not exist yet, and the number of
//!   ghost files in and below the directory (for the footer).
//!
//! `side` is only forwarded: the browse API never interprets it.
//!
//! # Contract for implementers
//!
//! * **Errors are final.** The browse API returns a provider error unchanged and never serves the
//!   real listing without its overlay. An unknown plan MUST be [`plan_not_found`] (404), so the
//!   client can drop the plan from the pane.
//! * **Filters.** `GhostView::filter` is already validated and in canonical form
//!   (`FileQuery::filter`). It applies to the attributes of the SOURCE file a ghost derives from
//!   (rating, flag, tags, camera, selection), exactly like it applies to real files.
//!   `FileQuery::new(QueryScope { volume_id: None, dir: RelPath::root(), recursive: true }, filter)`
//!   followed by `push_where(qb, "f")` turns it into one SQL condition over the `files` table.
//! * **Kinds.** Ghosts are assets: like the real listing, they never include sidecars.
//! * **Order.** Ghosts are sorted like `FileQuery::items` sorts real files: by `view.sort` in
//!   `view.order`, files without a capture time last in both directions for `date`, and ties by
//!   normalised name and then id, ascending whatever the order. `window` applies after sorting.
//! * **Counts agree.** For the same plan, directory and filter, `GhostPage::total` equals
//!   `FolderOverlay::ghosts.in_folder`, and `ghosts.in_subfolders` counts the ghost files strictly
//!   below the directory. The browse API relies on it so that the footer count equals the `total`
//!   of the items endpoint.
//!   A planner that never changes what a folder holds (batch rename) deliberately returns the
//!   default folder overlay instead: folder rows and footer counts remain real-only, while the
//!   comparison items endpoint shows only the rename ghost cards.
//! * **Ghosts are not real.** A ghost never appears in an id list, a selection or a count of real
//!   files. Its `id` is the id of the real source file (thumbnails work) while `volume_id`, `dir`
//!   and `name` are the planned location, and `overlay.planned` is `true`.
//! * **No dedupe.** Ghost folders must not repeat a real child: the browse API trusts the provider
//!   and does not check. A provider only knows the plan and the index, never the file system.

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use opm_core::dto::{
    FileItem, Filter, FolderEntry, FooterCounts, Overlay, PlanKind, PlanRef, PlanSide, SortKey,
    SortOrder,
};
use opm_core::paths::{FileId, PlanId, RelPath, VolumeId};

use super::PlanService;
use crate::error::{AppError, AppResult};

/// The directory a ghost listing is about, with the filter and the order of the real listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhostView {
    pub volume_id: VolumeId,
    /// Volume-relative directory, `""` for the volume root.
    pub dir: RelPath,
    /// The request's filter in canonical form.
    pub filter: Filter,
    pub sort: SortKey,
    pub order: SortOrder,
}

/// A slice of the sorted ghost list: skip `skip` ghosts, then return at most `take`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Ghosts to skip, never negative.
    pub skip: i64,
    /// Ghosts to return, never negative. 0 asks for the total only: the page stays empty.
    pub take: i64,
}

/// A window of ghost files and the size of the whole list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GhostPage {
    /// Ghosts of the whole view that pass the filter, whatever the window says.
    pub total: i64,
    /// The ghosts inside the window, complete `FileItem`s with `overlay.planned = true`.
    pub items: Vec<FileItem>,
}

/// What a plan says about one real subfolder of the listed directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderPlanInfo {
    /// Files in or below the folder that match the plan (`FolderEntry::matches`); `None` when the
    /// plan has no notion of a match count.
    pub matches: Option<i64>,
    /// Flags of the folder row, for example `will_delete` for an empty folder the plan removes.
    pub overlay: Overlay,
    /// Ghost files in or below the folder that pass the filter; added to the folder's `count`.
    pub ghost_count: i64,
    /// Ghost files in or below the folder, whatever the filter says; added to its `total`.
    pub ghost_total: i64,
}

/// Plan data for the folder list of one directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderOverlay {
    /// The plan's data about the real subfolders, keyed by folder NAME (the last path component,
    /// as listed in `real_children`). A folder the plan does not touch has no entry, which means
    /// `FolderPlanInfo::default()`.
    pub per_child: HashMap<String, FolderPlanInfo>,
    /// Subfolders of the directory that do not exist yet: `overlay.planned = true`, and
    /// `count`/`total` count the ghost files below them.
    pub ghost_folders: Vec<FolderEntry>,
    /// Ghost files that pass the filter, directly in the directory (`in_folder`) and strictly
    /// below it (`in_subfolders`).
    pub ghosts: FooterCounts,
    /// The plan creates the directory or places files in or below it, so listing it is legitimate
    /// even though it is not (yet) in the index.
    pub dir_planned: bool,
}

/// What the browse API needs from the plan engine. See the module docs for the contract.
///
/// Methods return boxed futures so the trait stays dyn-compatible: the browse functions take a
/// `&dyn PlanOverlay`, which lets tests drive them with a fake.
pub trait PlanOverlay: Send + Sync {
    /// Rename comparison previews list proposed names instead of appending them to real files.
    fn ghosts_only(&self, _plan: PlanRef) -> BoxFuture<'_, AppResult<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Flags of the REAL files `ids` (one page of a listing): `matched` and `will_delete`.
    /// Files without flags are simply absent from the map (`Overlay::default()`); `planned` is
    /// never set on a real file.
    fn item_flags<'a>(
        &'a self,
        plan: PlanRef,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, AppResult<HashMap<FileId, Overlay>>>;

    /// The ghost files directly in `view.dir`, sorted by `view.sort` and `view.order`, restricted
    /// to `window`; `total` is the size of the whole sorted list (see the module docs for what a
    /// ghost is). `window.take == 0` asks for the total only.
    fn ghost_items<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, AppResult<GhostPage>>;

    /// The plan's view of the folder list of `view.dir` (`view.sort` and `view.order` are not
    /// relevant here), given the names of the real subfolders the browse API found there
    /// (`real_children`, empty when the directory is not in the index at all).
    fn folder_overlay<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, AppResult<FolderOverlay>>;
}

/// The 404 for a plan that does not exist (or is gone: expired, cancelled, deleted). Shared so
/// every provider answers an unknown plan the same way.
pub fn plan_not_found(plan_id: PlanId) -> AppError {
    AppError::not_found("plan_not_found", format!("plan {plan_id} not found"))
}

/// The overlay of the plan engine: each call finds the plan and hands the question to the
/// [`Planner`](super::Planner) of its kind (see `PlanService::overlay_target`).
///
/// An unknown, expired or still-building plan is [`plan_not_found`]. A plan that was already
/// executed has nothing left to show: its overlays are empty, and the planners are only asked
/// about plans that are `ready` or `executing`.
impl PlanOverlay for PlanService {
    fn ghosts_only(&self, plan: PlanRef) -> BoxFuture<'_, AppResult<bool>> {
        Box::pin(async move {
            if plan.side != PlanSide::Comparison {
                return Ok(false);
            }
            Ok(self
                .overlay_target(plan.plan_id)
                .await?
                .is_some_and(|target| target.plan.kind == PlanKind::Rename))
        })
    }

    fn item_flags<'a>(
        &'a self,
        plan: PlanRef,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, AppResult<HashMap<FileId, Overlay>>> {
        Box::pin(async move {
            let Some(target) = self.overlay_target(plan.plan_id).await? else {
                return Ok(HashMap::new());
            };
            Ok(target
                .planner
                .item_flags(&target.plan, plan.side, ids)
                .await?)
        })
    }

    fn ghost_items<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, AppResult<GhostPage>> {
        Box::pin(async move {
            let Some(target) = self.overlay_target(plan.plan_id).await? else {
                return Ok(GhostPage::default());
            };
            Ok(target
                .planner
                .ghost_items(&target.plan, plan.side, view, window)
                .await?)
        })
    }

    fn folder_overlay<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, AppResult<FolderOverlay>> {
        Box::pin(async move {
            let Some(target) = self.overlay_target(plan.plan_id).await? else {
                return Ok(FolderOverlay::default());
            };
            Ok(target
                .planner
                .folder_overlay(&target.plan, plan.side, view, real_children)
                .await?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_pages_and_folder_overlays_default_to_nothing() {
        assert_eq!(GhostPage::default().total, 0);
        assert!(GhostPage::default().items.is_empty());
        let overlay = FolderOverlay::default();
        assert!(overlay.per_child.is_empty() && overlay.ghost_folders.is_empty());
        assert!(!overlay.dir_planned);
    }

    #[test]
    fn an_unknown_plan_is_a_404() {
        let err = plan_not_found(7);
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.code(), "plan_not_found");
        assert_eq!(err.message(), "plan 7 not found");
    }
}
