//! Immich endpoints. Owned by todo `building-immich-client`, except the sync
//! handler whose logic belongs to `syncing-immich-assets`:
//!
//! - `POST /api/immich/connect` (email and password in, the stored connection out; the password is
//!   used once and never kept), `DELETE /api/immich/connection`
//! - `GET /api/immich/status` (answered from the database; `?check=true` also asks Immich)
//! - `GET /api/immich/libraries`, `PUT /api/immich/mappings` (replaces the whole set)
//! - `POST /api/immich/exclude-trash`, `POST /api/immich/detect-prefix`
//! - `POST /api/immich/sync`
//!
//! A request whose fields are invalid is answered with the detailed 422 `ValidationErrorBody`;
//! every other [`ImmichError`] becomes the usual `ErrorBody` (see `immich::error` for the table).
//! Never log or return stored secrets.

use apm_core::dto::{
    FieldError, ImmichConnectRequest, ImmichConnectionDto, ImmichDetectPrefixRequest,
    ImmichDetectPrefixResponse, ImmichDisconnectResult, ImmichExcludeTrashResult,
    ImmichLibrariesResponse, ImmichMappingsRequest, ImmichMappingsResponse, ImmichStatusDto,
    ValidationErrorBody,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::api::extract::{ApiJson, ApiQuery};
use crate::error::{AppError, AppResult};
use crate::immich::ImmichError;
use crate::state::AppState;

const SYNC_OWNER: &str = "syncing-immich-assets";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/immich/connect", post(connect))
        .route("/immich/connection", delete(disconnect))
        .route("/immich/detect-prefix", post(detect_prefix))
        .route("/immich/exclude-trash", post(exclude_trash))
        .route("/immich/libraries", get(libraries))
        .route("/immich/mappings", put(mappings))
        .route("/immich/status", get(status))
        .route("/immich/sync", post(sync))
}

#[derive(Debug, Default, Deserialize)]
struct StatusQuery {
    /// Also ask Immich whether it answers and still accepts the key.
    #[serde(default)]
    check: bool,
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

async fn sync() -> AppResult<()> {
    Err(AppError::not_implemented(SYNC_OWNER))
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
