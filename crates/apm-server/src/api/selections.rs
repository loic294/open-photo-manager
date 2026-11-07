//! Selection endpoints: `POST /api/selections` (store an id set) and
//! `GET /api/selections/{id}`.
//!
//! Owned by todo `building-plan-engine`.

use apm_core::paths::SelectionId;
use axum::Router;
use axum::routing::{get, post};

use super::extract::ApiPath;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-plan-engine";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/selections", post(create))
        .route("/selections/{id}", get(detail))
}

async fn create() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn detail(ApiPath(_id): ApiPath<SelectionId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
