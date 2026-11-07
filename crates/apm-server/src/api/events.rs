//! `GET /api/events`: server-sent events carrying `ServerEvent` JSON
//! (task progress and filesystem changes; indexing is a task of kind `index`).
//!
//! Owned by todo `building-task-engine`. The stream must end when
//! `AppState::begin_shutdown` is called, otherwise graceful shutdown waits for
//! it.

use axum::Router;
use axum::routing::get;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-task-engine";

pub fn routes() -> Router<AppState> {
    Router::new().route("/events", get(events))
}

async fn events() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
