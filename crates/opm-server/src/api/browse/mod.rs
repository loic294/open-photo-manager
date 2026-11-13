//! Browse endpoints, owned by todo `building-browse-api`:
//!
//! - `POST /api/browse/folders`: `BrowseRequest` -> `FoldersResponse`
//! - `POST /api/browse/items`: `ItemsRequest` -> `ItemsResponse`
//! - `POST /api/browse/ids`: `ItemsRequest` -> `IdsResponse` (offset and limit are ignored)
//! - `POST /api/facets`: `BrowseRequest` -> `Facets`
//!
//! Contract deviation from plan 5.12: these are `POST` with JSON bodies, not `GET` with query
//! strings, because filters do not fit in a query string.
//!
//! The logic lives in the `pub async fn`s of [`service`] (`folders`, `items`, `ids`, `facets`),
//! which take the database and a `&dyn PlanOverlay` explicitly so tests can drive them with a fake
//! provider; the handlers below only decode the request. Files, folders, counts and facets all come
//! from the query engine (`FileQuery`); this module adds validation, the volumes root and the plan
//! merge.
//!
//! # Requests
//!
//! * **Index only.** Browsing never stats, reads or lists the file system: a volume whose directory
//!   was deleted, or that is offline, is browsed from the index like any other.
//! * **`dir`** is relative to the volume root and canonical: `/`-separated, no empty or `.`
//!   segments (so no leading, trailing or doubled `/`), no `..`, no NUL, no backslash. Anything
//!   else is 400 `bad_request` ("invalid dir: ..."). Consequence: a folder whose name contains a
//!   backslash (legal on Linux) cannot be browsed.
//! * **`volume_id`**: an unknown id is 404 `not_found`; an offline volume is fine. `null` is only
//!   valid with `dir == ""` (the volumes root), otherwise 400 `bad_request`.
//! * **`filter`** is validated by the query engine (`opm_core::filter`); an invalid one is 400.
//! * Every 400 is decided before anything is read, then the volume is looked up (404).
//! * **`limit`**: 0 means `DEFAULT_PAGE_SIZE` (200) and anything above `MAX_PAGE_SIZE` (500) is
//!   clamped to 500. `offset` past the end gives an empty page whose `total` is still right.
//!   `ItemsResponse.offset` echoes the request.
//!
//! # The volumes root
//!
//! `volume_id = null` with `dir = ""` is the virtual top level of the file system view: the
//! volumes are its folders.
//!
//! * `folders`: one entry per volume in the database, online or not, sorted like folders (normalised
//!   name, then name, then id). `name` is the volume name, `path` is `""`, `count` is the assets of
//!   the volume that pass the filter and `total` the assets ignoring it (a volume without files has
//!   0 and 0), `matches` is `null` and the overlay is empty. `plan` is ignored and the provider is
//!   never called. The footer is `in_folder = 0`, `in_subfolders` = the sum of the counts. One
//!   grouped query counts every volume (`query::volumes::volume_counts`).
//! * `items` and `ids`: empty, `total = 0`.
//! * `facets`: the facets of every volume, recursively.
//!
//! # Folders inside a volume
//!
//! Rows come from `FileQuery::folders`, the footer from `FileQuery::footer_counts`. Empty folders
//! are listed. A directory that is not in the `dirs` table gets 404 `folder_not_found` so the UI can
//! walk up to the nearest ancestor that still exists, unless the plan creates it (see below). The
//! root of a known volume always exists. Only `folders` answers `folder_not_found`: `items`, `ids`
//! and `facets` of a missing directory are simply empty, and a directory that exists only in a plan
//! lists its ghosts. `facets` cover the subtree of `dir` and apply every filter dimension except
//! the one being counted.
//!
//! # Plans and the merge rules
//!
//! A request with a `plan` asks the [`PlanOverlay`](crate::plans::overlay::PlanOverlay) for the rest
//! of the listing; without a `plan` the provider is never called. A provider error is returned
//! unchanged (the default provider answers 404 `plan_not_found`), and the real listing is never
//! served without its overlay.
//!
//! * **Items.** Real items first, sorted by the request, then the ghost items (sorted by the same
//!   key and order) appended. `real_total` is the engine's count and `total = real_total +
//!   ghosts.total`. The window `[offset, offset + limit)` takes real rows from `offset` while
//!   `offset < real_total` and fills the rest with the ghost window `{ skip: max(offset -
//!   real_total, 0), take: limit - real rows }`, so a page may span the boundary or be only ghosts.
//!   `ghost_items` is always called, even with `take = 0`, because `total` needs it. Real items get
//!   their `matched`/`will_delete` flags from `item_flags` (called only when the page has real
//!   items); a file the provider does not mention keeps the default overlay.
//!   Rename comparison is the exception: `ghosts_only` lists and pages proposed names directly,
//!   with no real items preceding them. Folder and footer counts still describe real files.
//! * **Ids.** Real files only, in the order of `items`. Ghosts are never listed; `plan` is ignored.
//! * **Folders.** Real rows plus the provider's `ghost_folders`, sorted together by normalised name
//!   and then name. For a real folder, `count` and `total` add `ghost_count` and `ghost_total`, and
//!   `matches` and `overlay` come from `per_child` (keyed by folder name); a folder without an
//!   entry is left as it is. Ghost folders are trusted as returned.
//! * **Footer counts.** Real counts plus `ghosts`, so `in_folder` equals the `total` of the items
//!   endpoint for the same request, with or without a plan.
//! * **Missing directory.** If the real directory is not in the index but the provider says the
//!   plan creates it (`dir_planned`), the real queries are skipped (all zero) and only the ghosts are
//!   served. The provider is then asked with an empty `real_children`.
//! * **Facets** are about the real files only.
//!
//! The provider contract is in [`crate::plans::overlay`].
//!
//! # Timings
//!
//! Medians of 7 runs, one request each through the real router (routing, validation, the query
//! engine, JSON in both directions), on a library of 249,400 files in 20,145 directories over three
//! volumes: `photos` (20 years of events plus a 5,000-file `inbox`), `cards` (5,000 folders of 6
//! files) and `dump` (one directory of 50,000 files). Apple M1 Max, release build, measured while
//! other builds kept the machine at a load average of about 20 on 10 cores, so these are
//! pessimistic. `tests/browse_perf.rs` prints the whole table (`cargo test --release -p opm-server
//! --test browse_perf -- --ignored --nocapture`, with `CARGO_PROFILE_RELEASE_STRIP=none
//! CARGO_PROFILE_RELEASE_LTO=off`). No index was added for the volumes root.
//!
//! | request                                                  |     median (ms) |
//! |----------------------------------------------------------|----------------:|
//! | `GET /volumes`                                           |            0.03 |
//! | folders: volumes root (3 volumes)                        |              28 |
//! | folders: volumes root, one camera / selective filter     |         43 / 70 |
//! | folders: `photos` root (20 folders)                      |              41 |
//! | folders: `photos` root, one camera / selective filter    |         48 / 59 |
//! | folders: `cards` root (5,000 folders)                    |              25 |
//! | folders: `cards` root, selective filter                  |              29 |
//! | folders: `photos/2015` (12 folders)                      |               2 |
//! | items: `inbox` page of 200 (name / date / size)          | 2.3 / 2.9 / 2.9 |
//! | items: `dump/all` page of 200 (name / date / size)       |    12 / 13 / 12 |
//! | items: `dump/all` last page, offset 49,800 (name / date) |         33 / 56 |
//! | ids: `inbox` (4,600 ids)                                 |               3 |
//! | ids: `dump/all` (46,000 ids, name / date / size)         |    33 / 47 / 47 |
//! | ids: `dump/all`, one camera (14,000 ids)                 |              16 |
//! | facets: `photos/2015`                                    |               7 |
//! | facets: `dump/all` (50,000 files)                        |              39 |
//! | facets: `photos` root, one camera                        |             131 |
//! | facets: `photos` root (every file of the volume)         |      190 to 240 |
//! | facets: volumes root (every file of the library)         |      535 to 540 |
//!
//! Everything the pane asks for on a navigation (the folder list with its footer, an item page, the
//! ids of a selection) is far below the 200 ms target. The exception is the facets of a very large
//! scope, which scan every file of it (the cost documented in `query`): ask for them lazily, or for
//! the current folder, rather than at the volumes root on every navigation.

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use opm_core::dto::{
    BrowseRequest, Facets, FoldersResponse, IdsResponse, ItemsRequest, ItemsResponse,
};

use super::extract::ApiJson;
use crate::error::AppResult;
use crate::state::AppState;

mod merge;
mod request;
pub mod service;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/browse/folders", post(folders))
        .route("/browse/ids", post(ids))
        .route("/browse/items", post(items))
        .route("/facets", post(facets))
}

async fn facets(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<BrowseRequest>,
) -> AppResult<Json<Facets>> {
    service::facets(&state.db, &request).await.map(Json)
}

async fn folders(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<BrowseRequest>,
) -> AppResult<Json<FoldersResponse>> {
    service::folders(&state.db, &state.plans, &request)
        .await
        .map(Json)
}

async fn ids(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ItemsRequest>,
) -> AppResult<Json<IdsResponse>> {
    service::ids(&state.db, &request).await.map(Json)
}

async fn items(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ItemsRequest>,
) -> AppResult<Json<ItemsResponse>> {
    service::items(&state.db, &state.plans, &request)
        .await
        .map(Json)
}
