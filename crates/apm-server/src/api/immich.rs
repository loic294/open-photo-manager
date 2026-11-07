//! Immich endpoints. Owned by todo `building-immich-client`, except the sync
//! handler whose logic belongs to `syncing-immich-assets`:
//!
//! - `POST /api/immich/connect`, `DELETE /api/immich/connection`
//! - `GET /api/immich/libraries`, `PUT /api/immich/mappings`
//! - `POST /api/immich/sync`
//! - `GET /api/immich/status`, `POST /api/immich/exclude-trash`
//!
//! Never log or return stored secrets.

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-immich-client";
const SYNC_OWNER: &str = "syncing-immich-assets";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/immich/connect", post(connect))
        .route("/immich/connection", delete(disconnect))
        .route("/immich/exclude-trash", post(exclude_trash))
        .route("/immich/libraries", get(libraries))
        .route("/immich/mappings", put(mappings))
        .route("/immich/status", get(status))
        .route("/immich/sync", post(sync))
}

async fn connect() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn disconnect() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn exclude_trash() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn libraries() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn mappings() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn status() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn sync() -> AppResult<()> {
    Err(AppError::not_implemented(SYNC_OWNER))
}
