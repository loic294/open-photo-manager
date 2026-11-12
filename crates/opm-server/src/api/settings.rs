//! `GET` and `PUT /api/settings`.
//!
//! Owned by todo `designing-database-schema` (typed settings service). Both endpoints carry the
//! complete `SettingsDto`; there is no partial update. A rejected `PUT` is answered with
//! 422 `validation_failed` and the per-field list of `ValidationErrorBody`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use opm_core::dto::{SettingsDto, ValidationErrorBody};

use crate::api::extract::ApiJson;
use crate::error::AppError;
use crate::settings::{SettingsError, ValidationError};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/settings", get(read).put(update))
}

async fn read(State(state): State<AppState>) -> Json<SettingsDto> {
    Json(state.settings.get().as_ref().clone())
}

/// Replaces the settings; answers with the normalised document that was stored.
async fn update(
    State(state): State<AppState>,
    ApiJson(proposed): ApiJson<SettingsDto>,
) -> Result<Json<SettingsDto>, Rejection> {
    let saved = state.settings.update(proposed).await?;
    Ok(Json(saved.as_ref().clone()))
}

/// Why a `PUT` was refused: invalid fields get the detailed 422 body, everything else the usual
/// `ErrorBody`.
enum Rejection {
    Invalid(ValidationError),
    Failed(AppError),
}

impl From<SettingsError> for Rejection {
    fn from(err: SettingsError) -> Self {
        match err {
            SettingsError::Invalid(err) => Self::Invalid(err),
            other => Self::Failed(other.into()),
        }
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid(err) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ValidationErrorBody::new(err.fields)),
            )
                .into_response(),
            Self::Failed(err) => err.into_response(),
        }
    }
}
