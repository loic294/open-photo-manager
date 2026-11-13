//! `GET` and `PUT /api/settings`, and the excluded-folder list.
//!
//! Owned by todo `designing-database-schema` (typed settings service). Both endpoints carry the
//! complete `SettingsDto`; there is no partial update. A rejected `PUT` is answered with
//! 422 `validation_failed` and the per-field list of `ValidationErrorBody`.
//!
//! `PUT` keeps the stored `excluded_folders`: only `POST /api/settings/excluded-folders` (add) and
//! `/excluded-folders/remove` change them, so a stale Settings page never re-includes a folder.
//! Both queue a user index task of the folder: an excluded scope is purged from the index, an
//! included one is indexed again.
//!
//! The parallel `/soft-excluded-folders` add/remove endpoints manage a shared tool-only list.
//! They never queue index work or hide files. `PUT` preserves this list too.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use opm_core::consts::TRASH_DIR;
use opm_core::dto::{ExcludedFolder, SettingsDto, ValidationErrorBody};
use opm_core::paths::RelPath;

use crate::api::extract::ApiJson;
use crate::error::AppError;
use crate::fsops::is_protected_path;
use crate::indexer::{IndexOrigin, ScanRequest, ScanTarget};
use crate::settings::{SettingsError, ValidationError};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings", get(read).put(update))
        .route("/settings/excluded-folders", post(exclude))
        .route("/settings/excluded-folders/remove", post(include))
        .route("/settings/soft-excluded-folders", post(soft_exclude))
        .route("/settings/soft-excluded-folders/remove", post(soft_include))
}

async fn read(State(state): State<AppState>) -> Json<SettingsDto> {
    Json(state.settings.get().as_ref().clone())
}

/// Replaces the settings; answers with the normalised document that was stored.
async fn update(
    State(state): State<AppState>,
    ApiJson(proposed): ApiJson<SettingsDto>,
) -> Result<Json<SettingsDto>, Rejection> {
    let saved = state
        .settings
        .modify(move |current| {
            let mut proposed = proposed;
            proposed.excluded_folders = current.excluded_folders.clone();
            proposed.soft_excluded_folders = current.soft_excluded_folders.clone();
            Ok(proposed)
        })
        .await?;
    Ok(Json(saved.as_ref().clone()))
}

async fn soft_exclude(
    State(state): State<AppState>,
    ApiJson(folder): ApiJson<ExcludedFolder>,
) -> Result<Json<SettingsDto>, Rejection> {
    let path = excludable_path(&state, &folder)?;
    let entry = ExcludedFolder {
        volume_id: folder.volume_id,
        path: path.as_str().to_owned(),
    };
    let saved = state
        .settings
        .modify(move |current| {
            let mut next = current.clone();
            next.soft_excluded_folders.push(entry);
            Ok(next)
        })
        .await?;
    Ok(Json(saved.as_ref().clone()))
}

async fn soft_include(
    State(state): State<AppState>,
    ApiJson(folder): ApiJson<ExcludedFolder>,
) -> Result<Json<SettingsDto>, Rejection> {
    if !state.settings.get().soft_excluded_folders.contains(&folder) {
        return Err(
            AppError::not_found("exclusion_not_found", "this folder is not soft-excluded").into(),
        );
    }
    let saved = state
        .settings
        .modify(move |current| {
            let mut next = current.clone();
            next.soft_excluded_folders.retain(|entry| entry != &folder);
            Ok(next)
        })
        .await?;
    Ok(Json(saved.as_ref().clone()))
}

/// Excludes a folder: it leaves the index and every file operation refuses it.
async fn exclude(
    State(state): State<AppState>,
    ApiJson(folder): ApiJson<ExcludedFolder>,
) -> Result<Json<SettingsDto>, Rejection> {
    let path = excludable_path(&state, &folder)?;
    let entry = ExcludedFolder {
        volume_id: folder.volume_id,
        path: path.as_str().to_owned(),
    };
    let saved = state
        .settings
        .modify(move |current| {
            let mut next = current.clone();
            next.excluded_folders.push(entry);
            Ok(next)
        })
        .await?;
    reindex(&state, folder.volume_id, path).await?;
    Ok(Json(saved.as_ref().clone()))
}

/// Includes a folder again (removes the exact entry) and indexes it.
async fn include(
    State(state): State<AppState>,
    ApiJson(folder): ApiJson<ExcludedFolder>,
) -> Result<Json<SettingsDto>, Rejection> {
    let path = RelPath::new(&folder.path)
        .map_err(|_| AppError::bad_request("invalid_path", "the folder path is invalid"))?;
    if !state.settings.get().excluded_folders.contains(&folder) {
        return Err(
            AppError::not_found("exclusion_not_found", "this folder is not excluded").into(),
        );
    }
    let removed = folder.clone();
    let saved = state
        .settings
        .modify(move |current| {
            let mut next = current.clone();
            next.excluded_folders.retain(|entry| entry != &removed);
            Ok(next)
        })
        .await?;
    if state.volumes.volume(folder.volume_id).is_some() {
        reindex(&state, folder.volume_id, path).await?;
    }
    Ok(Json(saved.as_ref().clone()))
}

fn excludable_path(state: &AppState, folder: &ExcludedFolder) -> Result<RelPath, AppError> {
    if state.volumes.volume(folder.volume_id).is_none() {
        return Err(AppError::not_found(
            "volume_not_found",
            "the volume does not exist",
        ));
    }
    let path = RelPath::new(&folder.path)
        .map_err(|_| AppError::bad_request("invalid_path", "the folder path is invalid"))?;
    let first = path.components().next();
    let Some(first) = first else {
        return Err(AppError::bad_request(
            "invalid_path",
            "a whole volume cannot be excluded",
        ));
    };
    if first.eq_ignore_ascii_case(TRASH_DIR) || is_protected_path(&path) {
        return Err(AppError::bad_request(
            "invalid_path",
            "this folder is never indexed",
        ));
    }
    Ok(path)
}

async fn reindex(state: &AppState, volume_id: i64, dir: RelPath) -> Result<(), AppError> {
    let request = ScanRequest {
        targets: vec![ScanTarget::subtree(volume_id, dir)],
        force: false,
        confirm_removals: true,
    };
    state.index_jobs.submit(request, IndexOrigin::User).await?;
    Ok(())
}

/// Why a `PUT` was refused: invalid fields get the detailed 422 body, everything else the usual
/// `ErrorBody`.
enum Rejection {
    Invalid(ValidationError),
    Failed(AppError),
}

impl From<AppError> for Rejection {
    fn from(err: AppError) -> Self {
        Self::Failed(err)
    }
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
