//! `GET /api/volumes`: the volumes known to the index.
//!
//! Owned by todo `building-browse-api`.

use apm_core::dto::VolumeDto;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::db::volumes;
use crate::error::AppResult;
use crate::query::volumes::volume_sort_key;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/volumes", get(list))
}

/// Every volume in the database, online or not (`online` says which), in the order of the volumes
/// root of the file system view. It reads the table and nothing else: the registry is not
/// refreshed and nothing is written.
async fn list(State(state): State<AppState>) -> AppResult<Json<Vec<VolumeDto>>> {
    let mut volumes = volumes::list(&state.db).await?;
    volumes.sort_by_cached_key(|volume| volume_sort_key(&volume.name, volume.id));
    Ok(Json(
        volumes
            .into_iter()
            .map(|volume| VolumeDto {
                id: volume.id,
                name: volume.name,
                writable: volume.writable,
                online: volume.online,
            })
            .collect(),
    ))
}
