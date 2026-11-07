//! `GET` and `PUT /api/settings`.
//!
//! Owned by todo `designing-database-schema` (typed settings service).

use axum::Router;
use axum::routing::get;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "designing-database-schema";

pub fn routes() -> Router<AppState> {
    Router::new().route("/settings", get(read).put(update))
}

async fn read() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn update() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
