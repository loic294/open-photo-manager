//! Plan endpoints: `POST /api/plans` (create), `GET /api/plans/{id}` and
//! `POST /api/plans/{id}/execute`.
//!
//! Owned by todo `building-plan-engine`.

use apm_core::paths::PlanId;
use axum::Router;
use axum::routing::{get, post};

use super::extract::ApiPath;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-plan-engine";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/plans", post(create))
        .route("/plans/{id}", get(detail))
        .route("/plans/{id}/execute", post(execute))
}

async fn create() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn detail(ApiPath(_id): ApiPath<PlanId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn execute(ApiPath(_id): ApiPath<PlanId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
