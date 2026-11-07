//! Media endpoints: `GET /api/files/{id}/thumb`, `/preview` and `/content`.
//!
//! Owned by todo `serving-media`. `content` must support `Range` requests.

use apm_core::paths::FileId;
use axum::Router;
use axum::routing::get;

use super::extract::ApiPath;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "serving-media";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/files/{id}/content", get(content))
        .route("/files/{id}/preview", get(preview))
        .route("/files/{id}/thumb", get(thumb))
}

async fn content(ApiPath(_id): ApiPath<FileId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn preview(ApiPath(_id): ApiPath<FileId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn thumb(ApiPath(_id): ApiPath<FileId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
