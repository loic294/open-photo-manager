//! `POST /api/format/preview`: previews the destination names a move pattern
//! produces for sample files.
//! The read-only token catalogue and samples both use the core formatter.

use apm_core::dto::{FormatPreviewRequest, FormatPreviewResponse, FormatTokensResponse};
use axum::routing::{get, post};
use axum::{Json, Router, extract::State};

use super::extract::ApiJson;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/format/preview", post(preview))
        .route("/format/tokens", get(tokens))
}

async fn preview(
    State(state): State<AppState>,
    request: Result<ApiJson<FormatPreviewRequest>, AppError>,
) -> AppResult<Json<FormatPreviewResponse>> {
    let ApiJson(request) =
        request.map_err(|e| AppError::bad_request("invalid_json", e.to_string()))?;
    Ok(Json(crate::plans::moves::preview(&state, request).await?))
}

async fn tokens() -> Json<FormatTokensResponse> {
    Json(crate::plans::moves::tokens())
}
