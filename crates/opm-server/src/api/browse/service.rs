//! The four browse service functions. The HTTP handlers in the parent module only decode the
//! request and call these, so tests can drive them with a fake [`PlanOverlay`].
//!
//! Every function validates first (a 400 never depends on the database), then looks up the volume
//! (404 `not_found`), then reads the index through [`FileQuery`]. The merge rules, limits and
//! timings are in the module docs of [`super`].

use std::collections::HashMap;

use opm_core::dto::{
    BrowseRequest, Facets, Filter, FolderEntry, FoldersResponse, FooterCounts, IdsResponse,
    ItemsRequest, ItemsResponse, PlanRef,
};
use opm_core::paths::{FileId, VolumeId};

use super::merge;
use super::request::{self, Place};
use crate::db::Db;
use crate::error::AppResult;
use crate::plans::overlay::{FolderOverlay, GhostView, PlanOverlay, Window};
use crate::query::volumes::{self, VolumeCount};
use crate::query::{FileQuery, QueryScope};

/// `POST /api/browse/folders`: the subfolders of a directory with their counts, and the footer
/// counts of the directory.
///
/// At the volumes root (`volume_id` null, `dir` empty) the folders are the volumes. Inside a
/// volume, a directory that is not in the index is a 404 `folder_not_found` unless the plan
/// creates it.
pub async fn folders(
    db: &Db,
    plans: &dyn PlanOverlay,
    req: &BrowseRequest,
) -> AppResult<FoldersResponse> {
    match Place::parse(req.volume_id, &req.dir)? {
        Place::VolumesRoot => volumes_root(db, &req.filter).await,
        Place::Dir { volume_id, dir } => {
            let query = FileQuery::new(listing_scope(volume_id, dir), &req.filter)?;
            request::require_volume(db, volume_id).await?;
            directory_folders(db, plans, &query, volume_id, req.plan).await
        }
    }
}

/// `POST /api/browse/items`: one page of the files directly in a directory, real files first and
/// then the ghosts of the plan. Rename comparison lists only proposed-name ghosts.
///
/// A directory that is not in the index lists nothing (only [`folders`] reports
/// `folder_not_found`). The volumes root has no files.
pub async fn items(
    db: &Db,
    plans: &dyn PlanOverlay,
    req: &ItemsRequest,
) -> AppResult<ItemsResponse> {
    let browse = &req.browse;
    let Place::Dir { volume_id, dir } = Place::parse(browse.volume_id, &browse.dir)? else {
        FileQuery::new(volumes::all_volumes(), &browse.filter)?;
        return Ok(ItemsResponse {
            items: Vec::new(),
            total: 0,
            offset: req.offset,
        });
    };
    let query =
        FileQuery::new(listing_scope(volume_id, dir), &browse.filter)?.sorted(req.sort, req.order);
    request::require_volume(db, volume_id).await?;

    let offset = i64::from(req.offset);
    let limit = request::page_limit(req.limit);
    if let Some(plan) = browse.plan
        && plans.ghosts_only(plan).await?
    {
        let ghosts = plans
            .ghost_items(
                plan,
                &ghost_view(&query, volume_id),
                Window {
                    skip: offset,
                    take: limit,
                },
            )
            .await?;
        return Ok(ItemsResponse {
            items: ghosts.items,
            total: ghosts.total,
            offset: req.offset,
        });
    }
    let (real_total, real) = tokio::try_join!(query.count(db), query.items(db, offset, limit))?;
    let Some(plan) = browse.plan else {
        return Ok(ItemsResponse {
            items: real,
            total: real_total,
            offset: req.offset,
        });
    };

    let window = merge::ghost_window(offset, limit, real_total, real.len());
    let view = ghost_view(&query, volume_id);
    let real_ids: Vec<FileId> = real.iter().map(|item| item.id).collect();
    let (flags, ghosts) = tokio::try_join!(
        async {
            if real_ids.is_empty() {
                Ok(HashMap::new())
            } else {
                plans.item_flags(plan, &real_ids).await
            }
        },
        plans.ghost_items(plan, &view, window),
    )?;
    let total = real_total + ghosts.total;
    Ok(ItemsResponse {
        items: merge::items(real, &flags, ghosts, window.take),
        total,
        offset: req.offset,
    })
}

/// `POST /api/browse/ids`: the ids of every real file the items endpoint lists for the same
/// request (same order, no paging, at most `MAX_IDS`). Ghosts are never listed and `plan` is
/// ignored: an id list stands for files that exist.
pub async fn ids(db: &Db, req: &ItemsRequest) -> AppResult<IdsResponse> {
    let browse = &req.browse;
    let Place::Dir { volume_id, dir } = Place::parse(browse.volume_id, &browse.dir)? else {
        FileQuery::new(volumes::all_volumes(), &browse.filter)?;
        return Ok(IdsResponse::default());
    };
    let query =
        FileQuery::new(listing_scope(volume_id, dir), &browse.filter)?.sorted(req.sort, req.order);
    request::require_volume(db, volume_id).await?;
    Ok(IdsResponse {
        ids: query.ids(db).await?,
    })
}

/// `POST /api/facets`: the facet values and counts of the real files in the subtree of a
/// directory (every volume at the volumes root). `plan` is ignored.
pub async fn facets(db: &Db, req: &BrowseRequest) -> AppResult<Facets> {
    let query = match Place::parse(req.volume_id, &req.dir)? {
        Place::VolumesRoot => FileQuery::new(volumes::all_volumes(), &req.filter)?,
        Place::Dir { volume_id, dir } => {
            let scope = QueryScope {
                recursive: true,
                ..listing_scope(volume_id, dir)
            };
            let query = FileQuery::new(scope, &req.filter)?;
            request::require_volume(db, volume_id).await?;
            query
        }
    };
    Ok(query.facets(db).await?)
}

/// The files directly in `dir` of one volume, which is what a pane lists.
fn listing_scope(volume_id: VolumeId, dir: opm_core::paths::RelPath) -> QueryScope {
    QueryScope {
        volume_id: Some(volume_id),
        dir,
        recursive: false,
    }
}

/// What the plan is asked about: the listed directory, in the request's canonical filter and order
/// (the default order for the folder list).
fn ghost_view(query: &FileQuery, volume_id: VolumeId) -> GhostView {
    let (sort, order) = query.sort();
    GhostView {
        volume_id,
        dir: query.scope().dir.clone(),
        filter: query.filter().clone(),
        sort,
        order,
    }
}

/// The volumes as folders, each with the assets in it. No plan applies at this level.
async fn volumes_root(db: &Db, filter: &Filter) -> AppResult<FoldersResponse> {
    let volumes = volumes::volume_counts(db, filter).await?;
    let in_subfolders = volumes.iter().map(|volume| volume.count).sum();
    Ok(FoldersResponse {
        folders: volumes.into_iter().map(volume_folder).collect(),
        counts: FooterCounts {
            in_folder: 0,
            in_subfolders,
        },
    })
}

fn volume_folder(volume: VolumeCount) -> FolderEntry {
    FolderEntry {
        name: volume.name,
        path: String::new(),
        volume_id: volume.id,
        count: volume.count,
        total: volume.total,
        ..FolderEntry::default()
    }
}

/// The folder list of a directory of a known volume, with the plan merged in.
async fn directory_folders(
    db: &Db,
    plans: &dyn PlanOverlay,
    query: &FileQuery,
    volume_id: VolumeId,
    plan: Option<PlanRef>,
) -> AppResult<FoldersResponse> {
    let dir = &query.scope().dir;
    let indexed = dir.is_root() || volumes::dir_exists(db, volume_id, dir).await?;
    if !indexed && plan.is_none() {
        return Err(request::folder_not_found(volume_id, dir));
    }
    let (rows, real_counts) = if indexed {
        tokio::try_join!(query.folders(db), query.footer_counts(db))?
    } else {
        (Vec::new(), FooterCounts::default())
    };
    let overlay = match plan {
        None => FolderOverlay::default(),
        Some(plan) => {
            let real_children: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
            let overlay = plans
                .folder_overlay(plan, &ghost_view(query, volume_id), &real_children)
                .await?;
            if !indexed && !overlay.dir_planned {
                return Err(request::folder_not_found(volume_id, dir));
            }
            overlay
        }
    };
    Ok(merge::folders(rows, real_counts, overlay))
}
