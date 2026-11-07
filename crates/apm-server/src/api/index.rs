//! `POST /api/index/rescan`: queues an indexing task for the given scopes.
//!
//! Owned by todo `building-index-controls`.

use apm_core::dto::RescanRequest;
use axum::Router;
use axum::routing::post;

use super::extract::ApiJson;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-index-controls";

pub fn routes() -> Router<AppState> {
    Router::new().route("/index/rescan", post(rescan))
}

async fn rescan(ApiJson(_request): ApiJson<RescanRequest>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
