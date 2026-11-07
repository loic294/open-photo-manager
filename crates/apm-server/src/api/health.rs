//! `GET /api/health`: liveness probe (used by the Docker healthcheck and the
//! web app's connection check). Implemented; no owner needed.

use apm_core::dto::HealthResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}
