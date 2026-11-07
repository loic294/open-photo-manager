//! Task endpoints: `GET /api/tasks` (list) and `POST /api/tasks/{id}/cancel`.
//!
//! Owned by todo `building-task-engine`.

use apm_core::dto::TaskDto;
use apm_core::paths::TaskId;
use axum::routing::{get, post};
use axum::{Json, Router};

use super::extract::ApiPath;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-task-engine";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks", get(list))
        .route("/tasks/{id}/cancel", post(cancel))
}

async fn cancel(ApiPath(_id): ApiPath<TaskId>) -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn list() -> AppResult<Json<Vec<TaskDto>>> {
    Err(AppError::not_implemented(OWNER))
}
