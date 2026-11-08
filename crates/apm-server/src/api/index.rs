//! Index controls: `POST /api/index/rescan` queues an indexing task, `GET /api/index/status` says
//! what the scanner's mass-disappearance guard is holding.
//!
//! Owned by todo `building-index-controls`; [`IndexJobs`](crate::indexer::IndexJobs) does the work.
//! A client follows the task with `GET /api/events`, like any other task.

use apm_core::dto::{IndexStatusDto, RescanRequest, TaskDto};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use super::extract::ApiJson;
use crate::error::AppResult;
use crate::indexer::IndexOrigin;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/index/rescan", post(rescan))
        .route("/index/status", get(status))
}

/// Queues an index task for the scopes of the request and answers 202 with the task. A request
/// that arrives while a task of the same kind is still queued shares it (the answer is that task),
/// so a double click starts one job. An empty `targets` list means every online volume.
async fn rescan(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RescanRequest>,
) -> AppResult<(StatusCode, Json<TaskDto>)> {
    let request = state.index_jobs.scan_request(&request)?;
    let outcome = state.index_jobs.submit(request, IndexOrigin::User).await?;
    let task = state.tasks.get(outcome.task_id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// The removals the guard is holding back per volume, and the active index task if any.
async fn status(State(state): State<AppState>) -> AppResult<Json<IndexStatusDto>> {
    Ok(Json(state.index_jobs.status().await?))
}
