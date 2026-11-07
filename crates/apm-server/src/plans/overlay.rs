//! Plan overlay: the seam between the browse API and the plans. It computes the `Overlay` flags
//! (matched, planned, will_delete), the ghost files and the folder match counts the browse API
//! adds to a listing.
//!
//! The interface in this file ([`PlanOverlay`] and its data types) is defined by todo
//! `building-browse-api`. Computing a real overlay belongs to todo `building-plan-engine`, which
//! keeps the trait and the types exactly as they are and replaces the implementation for
//! [`PlanService`] at the bottom of this file (today every plan is [`plan_not_found`]).
//!
//! # What the browse API asks
//!
//! A browse request that carries a `plan` ([`PlanRef`]: the plan and the side of it the pane
//! shows) lists real files from the index and asks this trait for the rest. Without a `plan` the
//! trait is never called, and the volumes root never calls it either. The three questions:
//!
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
//! * **Ghosts are not real.** A ghost never appears in an id list, a selection or a count of real
//!   files. Its `id` is the id of the real source file (thumbnails work) while `volume_id`, `dir`
//!   and `name` are the planned location, and `overlay.planned` is `true`.
//! * **No dedupe.** Ghost folders must not repeat a real child: the browse API trusts the provider
//!   and does not check. A provider only knows the plan and the index, never the file system.

use std::collections::HashMap;

use apm_core::dto::{
    FileItem, Filter, FolderEntry, FooterCounts, Overlay, PlanRef, SortKey, SortOrder,
};
use apm_core::paths::{FileId, PlanId, RelPath, VolumeId};
use futures_util::future::BoxFuture;

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

/// The "no plans exist yet" provider: every plan is unknown. `building-plan-engine` replaces it.
impl PlanOverlay for PlanService {
    fn item_flags<'a>(
        &'a self,
        plan: PlanRef,
        _ids: &'a [FileId],
    ) -> BoxFuture<'a, AppResult<HashMap<FileId, Overlay>>> {
        unknown_plan(plan)
    }

    fn ghost_items<'a>(
        &'a self,
        plan: PlanRef,
        _view: &'a GhostView,
        _window: Window,
    ) -> BoxFuture<'a, AppResult<GhostPage>> {
        unknown_plan(plan)
    }

    fn folder_overlay<'a>(
        &'a self,
        plan: PlanRef,
        _view: &'a GhostView,
        _real_children: &'a [String],
    ) -> BoxFuture<'a, AppResult<FolderOverlay>> {
        unknown_plan(plan)
    }
}

fn unknown_plan<'a, T: Send + 'a>(plan: PlanRef) -> BoxFuture<'a, AppResult<T>> {
    Box::pin(std::future::ready(Err(plan_not_found(plan.plan_id))))
}

#[cfg(test)]
mod tests {
    use apm_core::dto::PlanSide;
    use axum::http::StatusCode;

    use super::*;

    fn view() -> GhostView {
        GhostView {
            volume_id: 1,
            dir: RelPath::root(),
            filter: Filter::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        }
    }

    #[tokio::test]
    async fn the_default_provider_knows_no_plan() {
        let service = PlanService::new();
        let provider: &dyn PlanOverlay = &service;
        let plan = PlanRef {
            plan_id: 7,
            side: PlanSide::Comparison,
        };
        let view = view();
        let window = Window { skip: 0, take: 10 };

        let errors = [
            provider.item_flags(plan, &[1, 2]).await.err(),
            provider.ghost_items(plan, &view, window).await.err(),
            provider.folder_overlay(plan, &view, &[]).await.err(),
        ];
        for error in errors {
            let error = error.expect("every call fails");
            assert_eq!(error.status(), StatusCode::NOT_FOUND);
            assert_eq!(error.code(), "plan_not_found");
            assert_eq!(error.message(), "plan 7 not found");
        }
    }
}
