//! Batch-rename preview and the rename-specific token catalogue.

use apm_core::dto::{FormatTokensResponse, RenamePreviewRequest, RenamePreviewResponse};
use axum::routing::{get, post};
use axum::{Json, Router, extract::State};

use super::extract::ApiJson;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/rename/preview", post(preview))
        .route("/rename/tokens", get(tokens))
}

async fn preview(
    State(state): State<AppState>,
    request: Result<ApiJson<RenamePreviewRequest>, AppError>,
) -> AppResult<Json<RenamePreviewResponse>> {
    let ApiJson(request) =
        request.map_err(|error| AppError::bad_request("invalid_json", error.to_string()))?;
    Ok(Json(crate::plans::rename::preview(&state, request).await?))
}

async fn tokens() -> Json<FormatTokensResponse> {
    Json(apm_core::rename::tokens())
}
