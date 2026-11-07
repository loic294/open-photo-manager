//! Trash endpoints: `GET /api/trash`, `POST /api/trash/restore` and
//! `POST /api/trash/empty`.
//!
//! Owned by todo `building-trash-management`.

use axum::Router;
use axum::routing::{get, post};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-trash-management";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trash", get(list))
        .route("/trash/empty", post(empty))
        .route("/trash/restore", post(restore))
}

async fn empty() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn list() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}

async fn restore() -> AppResult<()> {
    Err(AppError::not_implemented(OWNER))
}
