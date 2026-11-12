//! File detail: `GET /api/files/{id}` -> `FileDetail`.
//!
//! Owned by todo `building-browse-api`. Thumbnails, previews and original
//! content are in `api/media.rs`.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use opm_core::dto::FileDetail;
use opm_core::paths::FileId;

use super::extract::ApiPath;
use crate::error::AppResult;
use crate::query;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/files/{id}", get(detail))
}

/// The details of one indexed file; an unknown id is 404 `not_found`, a malformed one 400
/// `invalid_path`.
async fn detail(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<FileId>,
) -> AppResult<Json<FileDetail>> {
    Ok(Json(query::file_detail(&state.db, id).await?))
}
