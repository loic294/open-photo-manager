//! ISM connection endpoints.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use opm_core::dto::{
    IsmBindingRequest, IsmConnectRequest, IsmInfoDto, IsmStatusDto, IsmSyncFolderRequest, TaskDto,
};

use crate::api::extract::ApiJson;
use crate::error::AppResult;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ism/bindings", put(stage_binding))
        .route("/ism/connection", put(connect).delete(disconnect))
        .route("/ism/status", get(status))
        .route("/ism/sync", post(sync_folder))
        .route("/ism/test", post(test))
}

async fn connect(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<IsmConnectRequest>,
) -> AppResult<Json<IsmInfoDto>> {
    Ok(Json(state.ism.connect(input).await?))
}

async fn disconnect(State(state): State<AppState>) -> AppResult<Json<IsmStatusDto>> {
    Ok(Json(state.ism.disconnect().await?))
}

async fn status(State(state): State<AppState>) -> AppResult<Json<IsmStatusDto>> {
    Ok(Json(state.ism.status().await?))
}

async fn test(State(state): State<AppState>) -> AppResult<Json<IsmInfoDto>> {
    Ok(Json(state.ism.test().await?))
}

async fn stage_binding(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<IsmBindingRequest>,
) -> AppResult<Json<IsmStatusDto>> {
    Ok(Json(state.ism.stage_binding(input).await?))
}

async fn sync_folder(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<IsmSyncFolderRequest>,
) -> AppResult<(StatusCode, Json<TaskDto>)> {
    let task_id = state.ism.sync_folder(input).await?;
    let task = state.tasks.get(task_id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
