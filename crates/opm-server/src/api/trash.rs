//! Trash endpoints, thin over `TrashService` (todo `building-trash-management`):
//!
//! - `GET /api/trash`: one page of the items (`volume_id`, `batch`, `q`, `sort`, `order`, `offset`,
//!   `limit` in the query string) with the summary of the whole trash.
//! - `GET /api/trash/batches`: the deletions (`volume_id`, `offset`, `limit`) with the same summary.
//! - `POST /api/trash/restore`: puts items back (`TrashRestoreRequest`); answers 202 with the task.
//! - `POST /api/trash/empty`: deletes for good what the user was looking at (`TrashEmptyRequest`:
//!   everything with `id <= up_to_id`); answers 202 with the task. The only permanent delete.
//! - `POST /api/trash/sync`: makes the rows agree with `.opm-trash` on disk; answers 202.
//!
//! The two GET queries are read as `key=value` pairs and parsed by the DTO (`from_pairs`), so a bad
//! value is a 422 `ValidationErrorBody` that names every rejected field. Invalid fields of a body
//! are answered the same way; everything else is the usual `ErrorBody` (404 `batch_not_found`, 409
//! `volume_offline`, 409 `trash_already_empty`, ...). The GETs never change anything.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use opm_core::dto::{
    FieldError, TaskDto, TrashBatchesQuery, TrashBatchesResponse, TrashEmptyRequest,
    TrashListQuery, TrashListResponse, TrashRestoreRequest, ValidationErrorBody,
};
use opm_core::paths::TaskId;

use crate::api::extract::{ApiJson, ApiQuery};
use crate::error::AppError;
use crate::state::AppState;
use crate::trash::TrashError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trash", get(list))
        .route("/trash/batches", get(batches))
        .route("/trash/empty", post(empty))
        .route("/trash/restore", post(restore))
        .route("/trash/sync", post(sync))
}

/// The pairs of a query string, borrowed the way the DTO parsers take them.
fn borrowed(pairs: &[(String, String)]) -> impl Iterator<Item = (&str, &str)> {
    pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
}

async fn list(
    State(state): State<AppState>,
    ApiQuery(pairs): ApiQuery<Vec<(String, String)>>,
) -> Result<Json<TrashListResponse>, Rejection> {
    let query = TrashListQuery::from_pairs(borrowed(&pairs))?;
    Ok(Json(state.trash.list(query).await?))
}

async fn batches(
    State(state): State<AppState>,
    ApiQuery(pairs): ApiQuery<Vec<(String, String)>>,
) -> Result<Json<TrashBatchesResponse>, Rejection> {
    let query = TrashBatchesQuery::from_pairs(borrowed(&pairs))?;
    Ok(Json(state.trash.batches(query).await?))
}

async fn restore(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<TrashRestoreRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.restore(request).await?;
    accepted(&state, id).await
}

async fn empty(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<TrashEmptyRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.empty(request).await?;
    accepted(&state, id).await
}

async fn sync(State(state): State<AppState>) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.sync().await?;
    accepted(&state, id).await
}

/// 202 with the task a request queued: the client follows it on the event stream.
async fn accepted(state: &AppState, id: TaskId) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let task = state.tasks.get(id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// Why a request was refused: invalid fields get the detailed 422 body, everything else the usual
/// `ErrorBody`.
enum Rejection {
    Invalid(Vec<FieldError>),
    Failed(AppError),
}

impl From<Vec<FieldError>> for Rejection {
    fn from(fields: Vec<FieldError>) -> Self {
        Self::Invalid(fields)
    }
}

impl From<AppError> for Rejection {
    fn from(err: AppError) -> Self {
        Self::Failed(err)
    }
}

impl From<TrashError> for Rejection {
    fn from(err: TrashError) -> Self {
        match err {
            TrashError::Validation(fields) => Self::Invalid(fields),
            TrashError::Failed(err) => Self::Failed(err),
        }
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid(fields) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ValidationErrorBody::new(fields)),
            )
                .into_response(),
            Self::Failed(err) => err.into_response(),
        }
    }
}
