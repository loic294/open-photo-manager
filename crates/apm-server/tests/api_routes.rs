//! Contract tests for the HTTP surface: every endpoint is registered with its
//! method and path, errors are JSON, the SPA never shadows `/api`.
//!
//! The route table is deliberately independent of implementation status so
//! the owners of an endpoint never need to edit it: an endpoint may answer
//! with anything except "no such route", and a `501` must be a proper
//! `not_implemented` error.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};

/// Every endpoint of the API contract (plan 5.12; browse and facets are POST).
/// Paths use concrete ids and bodies are `{}`.
const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/health"),
    ("GET", "/api/settings"),
    ("PUT", "/api/settings"),
    ("GET", "/api/volumes"),
    ("POST", "/api/browse/folders"),
    ("POST", "/api/browse/items"),
    ("POST", "/api/browse/ids"),
    ("POST", "/api/facets"),
    ("GET", "/api/files/1"),
    ("GET", "/api/files/1/thumb"),
    ("GET", "/api/files/1/preview"),
    ("GET", "/api/files/1/content"),
    ("POST", "/api/selections"),
    ("GET", "/api/selections/1"),
    ("POST", "/api/plans"),
    ("GET", "/api/plans/1"),
    ("DELETE", "/api/plans/1"),
    ("POST", "/api/plans/1/execute"),
    ("POST", "/api/plans/1/selection"),
    ("POST", "/api/format/preview"),
    ("GET", "/api/format/tokens"),
    ("POST", "/api/rename/preview"),
    ("GET", "/api/rename/tokens"),
    ("GET", "/api/tasks"),
    ("POST", "/api/tasks/1/cancel"),
    ("GET", "/api/events"),
    ("POST", "/api/index/rescan"),
    ("GET", "/api/index/status"),
    ("GET", "/api/trash"),
    ("GET", "/api/trash/batches"),
    ("POST", "/api/trash/restore"),
    ("POST", "/api/trash/empty"),
    ("POST", "/api/trash/sync"),
    ("POST", "/api/immich/connect"),
    ("DELETE", "/api/immich/connection"),
    ("GET", "/api/immich/libraries"),
    ("PUT", "/api/immich/mappings"),
    ("POST", "/api/immich/sync"),
    ("GET", "/api/immich/status"),
    ("POST", "/api/immich/exclude-trash"),
    ("POST", "/api/immich/detect-prefix"),
    ("GET", "/api/immich/outbox"),
    ("POST", "/api/immich/outbox/retry"),
    ("POST", "/api/immich/outbox/dismiss"),
];

fn error_code(body: &Value) -> &Value {
    &body["error"]["code"]
}

#[tokio::test]
async fn every_endpoint_is_registered_with_its_method() {
    let app = TestApp::new().await;
    for &(method, path) in ROUTES {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let body = (method == Method::POST || method == Method::PUT).then(|| json!({}));
        let (status, body) = app.call(method.clone(), path, body).await;

        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} is registered with another method"
        );
        assert_ne!(
            error_code(&body),
            &json!("route_not_found"),
            "{method} {path} is not registered"
        );
        if status == StatusCode::NOT_IMPLEMENTED {
            assert_eq!(
                error_code(&body),
                &json!("not_implemented"),
                "{method} {path}"
            );
            assert!(body["error"]["message"].is_string(), "{method} {path}");
        }
    }
}

#[tokio::test]
async fn health_reports_ok_and_the_version() {
    let app = TestApp::new().await;
    let (status, body) = app.call(Method::GET, "/api/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION"), "warnings": [] })
    );
}

#[tokio::test]
async fn unknown_api_paths_return_the_json_404_even_when_the_spa_exists() {
    let app = TestApp::new().await;
    let web = &app.state.config.web_dir;
    std::fs::create_dir_all(web).unwrap();
    std::fs::write(web.join("index.html"), "<title>spa</title>").unwrap();

    for path in [
        "/api",
        "/api/",
        "/api/nope",
        "/api/health/extra",
        "/api/files/1/unknown",
    ] {
        let (status, body) = app.call(Method::GET, path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}");
        assert_eq!(error_code(&body), &json!("route_not_found"), "GET {path}");
    }
    let (status, body) = app.call(Method::POST, "/api/nope", Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), &json!("route_not_found"));
}

#[tokio::test]
async fn wrong_method_returns_the_json_405() {
    let app = TestApp::new().await;
    for (method, path) in [
        (Method::DELETE, "/api/health"),
        (Method::GET, "/api/browse/folders"),
        (Method::POST, "/api/volumes"),
    ] {
        let (status, body) = app.call(method.clone(), path, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        assert_eq!(
            error_code(&body),
            &json!("method_not_allowed"),
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn malformed_requests_get_json_errors() {
    let app = TestApp::new().await;

    let (status, body) = app.call(Method::GET, "/api/files/not-a-number", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), &json!("invalid_path"));

    let response = app
        .send(
            Method::POST,
            "/api/browse/folders",
            Some(&json!("not an object")),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_spa_is_served_for_everything_outside_api() {
    let app = TestApp::new().await;
    let web = &app.state.config.web_dir;
    std::fs::create_dir_all(web).unwrap();
    std::fs::write(web.join("index.html"), "<title>spa</title>").unwrap();
    std::fs::write(web.join("app.js"), "console.log('app')").unwrap();

    let (status, body) = app.get_text("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>spa</title>"));

    let (status, body) = app.get_text("/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("console.log('app')"));

    let (status, body) = app.get_text("/browse/2024/Trip").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>spa</title>"));
}

#[tokio::test]
async fn a_missing_web_dir_only_breaks_the_spa() {
    let app = TestApp::new().await;

    let (status, _) = app.get_text("/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app.call(Method::GET, "/api/health", None).await;
    assert_eq!(status, StatusCode::OK);
}
