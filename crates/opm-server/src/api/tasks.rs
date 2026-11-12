//! Task endpoints: `GET /api/tasks` (list) and `POST /api/tasks/{id}/cancel`.
//!
//! Owned by todo `building-task-engine`; the engine in `crate::tasks` does the work. A client
//! reads `GET /api/tasks` once, then follows `GET /api/events` for the changes.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use opm_core::dto::TaskDto;
use opm_core::paths::TaskId;

use super::extract::ApiPath;
use crate::error::AppResult;
use crate::state::AppState;

/// How many finished tasks the list carries next to the active ones.
const FINISHED_LIMIT: i64 = 100;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks", get(list))
        .route("/tasks/{id}/cancel", post(cancel))
}

/// The active tasks (running first, then queued) followed by the last finished ones, newest first.
async fn list(State(state): State<AppState>) -> AppResult<Json<Vec<TaskDto>>> {
    Ok(Json(state.tasks.list(FINISHED_LIMIT).await?))
}

/// Asks a task to stop and answers 202 with the task as it is now: a queued task is already
/// `cancelled`, a running one ends when its runner returns (the `task` events say when). Cancelling
/// a cancelled task answers the same; 404 for an unknown id, 409 for a task that ended otherwise.
async fn cancel(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<TaskId>,
) -> AppResult<(StatusCode, Json<TaskDto>)> {
    let task = state.tasks.cancel(id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
