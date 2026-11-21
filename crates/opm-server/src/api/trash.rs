//! Trash endpoints, thin over `TrashService` (todo `building-trash-management`):
//!
//! - `GET /api/trash`: one page of the items (`volume_id`, `batch`, `q`, `sort`, `order`, `offset`,
//!   `limit` in the query string) with the summary of the whole trash.
//! - `GET /api/trash/batches`: the deletions (`volume_id`, `offset`, `limit`) with the same summary.
//! - `POST /api/trash/files`: puts the files of a selection in the trash; answers 202 with the task.
//! - `POST /api/trash/restore`: puts items back (`TrashRestoreRequest`); answers 202 with the task.
//! - `POST /api/trash/empty`: deletes for good what the user was looking at (`TrashEmptyRequest`:
//!   everything with `id <= up_to_id`); answers 202 with the task. The only permanent delete.
//! - `POST /api/trash/sync`: makes the rows agree with `.opm-trash` on disk; answers 202.
//! - `GET /api/trash/ism?volume_id=`: the sync manager trash of a bound volume (`IsmTrashDto`).
//! - `POST /api/trash/ism/restore`: restores ISM trash items (`IsmTrashRestoreRequest`); 202.
//! - `POST /api/trash/ism/purge`: deletes ISM trash items for good (`IsmTrashPurgeRequest`, with
//!   `confirm: true` and a token with the `purge` grant); 202.
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
    FieldError, IsmTrashDto, IsmTrashPurgeRequest, IsmTrashRestoreRequest, TaskDto,
    TrashBatchesQuery, TrashBatchesResponse, TrashEmptyRequest, TrashFilesRequest, TrashListQuery,
    TrashListResponse, TrashRestoreRequest, ValidationErrorBody,
};
use opm_core::paths::TaskId;

use crate::api::extract::{ApiJson, ApiLocalOnly, ApiQuery};
use crate::error::AppError;
use crate::state::AppState;
use crate::trash::TrashError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trash", get(list))
        .route("/trash/batches", get(batches))
        .route("/trash/empty", post(empty))
        .route("/trash/files", post(files))
        .route("/trash/restore", post(restore))
        .route("/trash/sync", post(sync))
        .route("/trash/ism", get(ism_list))
        .route("/trash/ism/restore", post(ism_restore))
        .route("/trash/ism/purge", post(ism_purge))
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
    ApiLocalOnly(local_only): ApiLocalOnly,
    ApiJson(request): ApiJson<TrashRestoreRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.restore_with_mode(request, local_only).await?;
    accepted(&state, id).await
}

async fn empty(
    State(state): State<AppState>,
    ApiLocalOnly(local_only): ApiLocalOnly,
    ApiJson(request): ApiJson<TrashEmptyRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.empty_with_mode(request, local_only).await?;
    accepted(&state, id).await
}

async fn files(
    State(state): State<AppState>,
    ApiLocalOnly(local_only): ApiLocalOnly,
    ApiJson(request): ApiJson<TrashFilesRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let file_ids = state.plans.selection_file_ids(request.selection_id).await?;
    let id = state
        .trash
        .trash_files_with_mode(request.selection_id, file_ids, local_only)
        .await?;
    accepted(&state, id).await
}

async fn sync(
    State(state): State<AppState>,
    ApiLocalOnly(local_only): ApiLocalOnly,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    let id = state.trash.sync_with_mode(local_only).await?;
    accepted(&state, id).await
}

#[derive(serde::Deserialize)]
struct IsmTrashQuery {
    volume_id: opm_core::paths::VolumeId,
}

async fn ism_list(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<IsmTrashQuery>,
) -> Result<Json<IsmTrashDto>, Rejection> {
    Ok(Json(state.trash.ism_list(query.volume_id).await?))
}

async fn ism_restore(
    State(state): State<AppState>,
    ApiLocalOnly(local_only): ApiLocalOnly,
    ApiJson(request): ApiJson<IsmTrashRestoreRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    refuse_local_only(local_only)?;
    let id = state.trash.ism_restore(request).await?;
    accepted(&state, id).await
}

async fn ism_purge(
    State(state): State<AppState>,
    ApiLocalOnly(local_only): ApiLocalOnly,
    ApiJson(request): ApiJson<IsmTrashPurgeRequest>,
) -> Result<(StatusCode, Json<TaskDto>), Rejection> {
    refuse_local_only(local_only)?;
    let id = state.trash.ism_purge(request).await?;
    accepted(&state, id).await
}

fn refuse_local_only(local_only: bool) -> Result<(), Rejection> {
    if local_only {
        return Err(AppError::conflict(
            "local_only_remote_trash",
            "The sync manager trash cannot be changed in local-only mode.",
        )
        .into());
    }
    Ok(())
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
