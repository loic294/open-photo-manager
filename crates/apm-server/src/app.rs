//! Router assembly: the JSON API under `/api`, the built web app for everything
//! else, and the security middleware stack around all of it.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;

use crate::api;
use crate::error::AppError;
use crate::security::{self, SecurityOptions};
use crate::state::AppState;

/// Builds the complete application router.
///
/// - `/api/*` is served by [`api::router`]; an unknown API path or method
///   answers with the JSON `ErrorBody`, never with the SPA.
/// - Every other path is served from `config.web_dir` by [`security::spa`],
///   falling back to `index.html` for client-side routes so they survive a page
///   reload (but never for a missing asset, and never below `/api`).
/// - [`security::layers`] wraps all of it: request ids and logging, security
///   headers, panic catching, the `Host` allow-list, CSRF, body limits and
///   timeouts.
pub fn router(state: AppState) -> Router {
    let web = security::spa::service(&state.config.web_dir);
    let options = SecurityOptions::from_config(&state.config);

    let api = api::router()
        .fallback(api_not_found)
        .method_not_allowed_fallback(api_method_not_allowed);

    Router::new()
        .nest("/api", api)
        // `nest` does not cover the bare `/api/`; keep it out of the SPA too.
        .route("/api/", any(api_not_found))
        .fallback_service(web)
        // The security stack goes last: a layer wraps everything registered
        // above it, so it covers `/api`, the SPA and the fallbacks.
        .layer(security::layers(options))
        .with_state(state)
}

async fn api_not_found() -> AppError {
    AppError::not_found("route_not_found", "no such API endpoint")
}

async fn api_method_not_allowed() -> AppError {
    AppError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "method not allowed for this endpoint",
    )
}
