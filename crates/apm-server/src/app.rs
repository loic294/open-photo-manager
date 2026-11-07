//! Router assembly: the JSON API under `/api`, the built web app for everything
//! else.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::api;
use crate::error::AppError;
use crate::state::AppState;

/// Builds the complete application router.
///
/// - `/api/*` is served by [`api::router`]; an unknown API path or method
///   answers with the JSON `ErrorBody`, never with the SPA.
/// - Every other path is served from `config.web_dir`, falling back to
///   `index.html` so client-side routes survive a page reload.
pub fn router(state: AppState) -> Router {
    let index = state.config.web_dir.join("index.html");
    let web = ServeDir::new(&state.config.web_dir).fallback(ServeFile::new(index));

    let api = api::router()
        .fallback(api_not_found)
        .method_not_allowed_fallback(api_method_not_allowed);

    Router::new()
        .nest("/api", api)
        // `nest` does not cover the bare `/api/`; keep it out of the SPA too.
        .route("/api/", any(api_not_found))
        .fallback_service(web)
        // ---- Insertion point: security layer (todo `securing-api-baseline`) ----
        // Add the layer here, e.g.
        //     .layer(axum::middleware::from_fn_with_state(state.clone(), crate::security::guard))
        // A layer wraps everything registered above it, so it covers `/api`, the
        // SPA and the fallbacks. Keep `TraceLayer` below it so rejected requests
        // are still logged.
        // ------------------------------------------------------------------------
        .layer(TraceLayer::new_for_http())
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
