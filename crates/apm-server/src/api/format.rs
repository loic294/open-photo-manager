//! `POST /api/format/preview`: previews the destination names a move pattern
//! produces for sample files.
//!
//! Owned by todo `implementing-move-planner`, which also defines the request and
//! response DTOs in `apm_core::dto::plans` and switches the handler to
//! `ApiJson<Request>`.

use axum::Router;
use axum::routing::post;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "implementing-move-planner";

pub fn routes() -> Router<AppState> {
    Router::new().route("/format/preview", post(preview))
}

async fn preview() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
