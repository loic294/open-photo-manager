//! Immich endpoints. Owned by todo `building-immich-client`, except the sync
//! handler whose logic belongs to `syncing-immich-assets`:
//!
//! - `POST /api/immich/connect` (email and password in, the stored connection out; the password is
//!   used once and never kept), `DELETE /api/immich/connection`
//! - `GET /api/immich/status` (answered from the database; `?check=true` also asks Immich)
//! - `GET /api/immich/libraries`, `PUT /api/immich/mappings` (replaces the whole set)
//! - `POST /api/immich/exclude-trash`, `POST /api/immich/detect-prefix`
//! - `POST /api/immich/sync` (queues a sync and answers 202 with its task; the body `{"full":
//!   bool}` may be omitted)
//! - `GET /api/immich/outbox` (the queued file changes, paged), `POST /api/immich/outbox/retry`
//!   (the body `{"ids": [...]}` may be omitted: every change that needs attention or waits) and
//!   `POST /api/immich/outbox/dismiss`; the logic belongs to `reflecting-file-ops-in-immich`
//!   (`immich::outbox`)
//!
//! A request whose fields are invalid is answered with the detailed 422 `ValidationErrorBody`;
//! every other [`ImmichError`] becomes the usual `ErrorBody` (see `immich::error` for the table).
//! Never log or return stored secrets.

use apm_core::dto::{
    FieldError, ImmichConnectRequest, ImmichConnectionDto, ImmichDetectPrefixRequest,
    ImmichDetectPrefixResponse, ImmichDisconnectResult, ImmichExcludeTrashResult,
    ImmichLibrariesResponse, ImmichMappingsRequest, ImmichMappingsResponse, ImmichStatusDto,
    ImmichSyncRequest, OutboxDismissRequest, OutboxDismissResult, OutboxPage, OutboxRetryRequest,
    OutboxRetryResult, OutboxState, TaskDto, ValidationErrorBody,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::api::extract::{ApiJson, ApiQuery};
use crate::error::{AppError, AppResult};
use crate::immich::ImmichError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/immich/connect", post(connect))
        .route("/immich/connection", delete(disconnect))
        .route("/immich/detect-prefix", post(detect_prefix))
        .route("/immich/exclude-trash", post(exclude_trash))
        .route("/immich/libraries", get(libraries))
        .route("/immich/mappings", put(mappings))
        .route("/immich/outbox", get(outbox))
        .route("/immich/outbox/dismiss", post(outbox_dismiss))
        .route("/immich/outbox/retry", post(outbox_retry))
        .route("/immich/status", get(status))
        .route("/immich/sync", post(sync))
}

#[derive(Debug, Default, Deserialize)]
struct StatusQuery {
    /// Also ask Immich whether it answers and still accepts the key.
    #[serde(default)]
    check: bool,
}

/// How many queued changes a page holds when the request does not say.
const DEFAULT_OUTBOX_PAGE: i64 = 50;

#[derive(Debug, Deserialize)]
struct OutboxQuery {
    /// Only the changes in this state; every change when omitted.
    #[serde(default)]
    state: Option<OutboxState>,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_outbox_page")]
    limit: i64,
}

fn default_outbox_page() -> i64 {
    DEFAULT_OUTBOX_PAGE
}

async fn connect(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ImmichConnectRequest>,
) -> Result<Json<ImmichConnectionDto>, Rejection> {
    Ok(Json(state.immich.connect(request.into()).await?))
}

async fn disconnect(
    State(state): State<AppState>,
) -> Result<Json<ImmichDisconnectResult>, Rejection> {
    Ok(Json(state.immich.disconnect().await?))
}

async fn detect_prefix(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ImmichDetectPrefixRequest>,
) -> Result<Json<ImmichDetectPrefixResponse>, Rejection> {
    Ok(Json(state.immich.detect_prefix(request).await?))
}

async fn exclude_trash(
    State(state): State<AppState>,
) -> Result<Json<ImmichExcludeTrashResult>, Rejection> {
    Ok(Json(state.immich.exclude_trash().await?))
}

async fn libraries(
    State(state): State<AppState>,
) -> Result<Json<ImmichLibrariesResponse>, Rejection> {
    Ok(Json(state.immich.libraries().await?))
}

async fn mappings(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ImmichMappingsRequest>,
) -> Result<Json<ImmichMappingsResponse>, Rejection> {
    Ok(Json(state.immich.set_mappings(request.mappings).await?))
}

async fn status(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StatusQuery>,
) -> Result<Json<ImmichStatusDto>, Rejection> {
    Ok(Json(state.immich.status(query.check).await?))
}

/// One page of the queued file changes, oldest first; empty without a connection.
async fn outbox(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<OutboxQuery>,
) -> AppResult<Json<OutboxPage>> {
    let page = state
        .immich_outbox
        .list(query.state, query.offset, query.limit)
        .await?;
    Ok(Json(page))
}

/// Tries queued changes again (the named ones, or every one that needs attention or waits for its
/// next try; the body may be omitted) and starts working on them without the quiet period. 409
/// `immich_not_connected` without a connection.
async fn outbox_retry(
    State(state): State<AppState>,
    body: Bytes,
) -> AppResult<Json<OutboxRetryResult>> {
    let request: OutboxRetryRequest = optional_json(&body)?;
    let result = state.immich_outbox.retry(request.ids.as_deref()).await?;
    Ok(Json(result))
}

/// Forgets queued changes without touching Immich. 409 `immich_not_connected` without a
/// connection.
async fn outbox_dismiss(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<OutboxDismissRequest>,
) -> AppResult<Json<OutboxDismissResult>> {
    Ok(Json(state.immich_outbox.dismiss(&request.ids).await?))
}

/// Queues a sync of every mapped scope and answers 202 with its task. A request that arrives while
/// a sync waits for its turn is merged into it and gets that task (see `immich::sync`). The body
/// may be omitted or empty: an incremental sync. 409 `immich_not_connected`, 409
/// `immich_needs_reconnect` and 409 `immich_not_mapped` when there is nothing to sync with.
async fn sync(
    State(state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<TaskDto>)> {
    let request: ImmichSyncRequest = optional_json(&body)?;
    let submitted = state.immich_sync.submit(request.into()).await?;
    let task = state.tasks.get(submitted.task_id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// A JSON body that may be omitted or blank, which means the default value of the request.
fn optional_json<T: DeserializeOwned + Default>(body: &Bytes) -> AppResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|err| {
        let status = if err.is_data() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::BAD_REQUEST
        };
        AppError::new(status, "invalid_json", err.to_string())
    })
}

/// Why a request was refused: invalid fields get the detailed 422 body, everything else the usual
/// `ErrorBody`.
enum Rejection {
    Invalid(Vec<FieldError>),
    Failed(AppError),
}

impl From<ImmichError> for Rejection {
    fn from(err: ImmichError) -> Self {
        match err {
            ImmichError::Validation(fields) => Self::Invalid(fields),
            other => Self::Failed(other.into()),
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
