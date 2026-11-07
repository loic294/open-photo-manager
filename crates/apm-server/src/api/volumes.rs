//! `GET /api/volumes`: the volumes found under the volumes directory.
//!
//! Owned by todo `building-browse-api`.

use apm_core::dto::VolumeDto;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-browse-api";

pub fn routes() -> Router<AppState> {
    Router::new().route("/volumes", get(list))
}

async fn list() -> AppResult<Json<Vec<VolumeDto>>> {
    Err(AppError::not_implemented(OWNER))
}
