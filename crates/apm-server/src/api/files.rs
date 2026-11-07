//! File detail: `GET /api/files/{id}` -> `FileDetail`.
//!
//! Owned by todo `building-browse-api`. Thumbnails, previews and original
//! content are in `api/media.rs`.

use apm_core::dto::FileDetail;
use apm_core::paths::FileId;
use axum::routing::get;
use axum::{Json, Router};

use super::extract::ApiPath;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-browse-api";

pub fn routes() -> Router<AppState> {
    Router::new().route("/files/{id}", get(detail))
}

async fn detail(ApiPath(_id): ApiPath<FileId>) -> AppResult<Json<FileDetail>> {
    Err(AppError::not_implemented(OWNER))
}
