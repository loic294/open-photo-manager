//! `GET /api/health`: liveness probe (used by the Docker healthcheck and the
//! web app's connection check). Implemented; no owner needed.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use opm_core::dto::HealthResponse;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        warnings: crate::hostfs::warnings::for_state(&state.volumes, &state.settings.get()),
    })
}
