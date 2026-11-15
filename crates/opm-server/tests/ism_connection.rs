#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn info(instance: &str) -> Value {
    json!({
        "instance_id": instance,
        "instance_name": "Example sync",
        "version": "0.1.0",
        "api_version": 1,
        "token_kind": "opm",
        "actions": ["read", "manage"],
        "roots": [{
            "root_id": "photos", "name": "Photos",
            "immich_import_path": "/external/photos",
            "opm_prefix": "/volumes/photos", "available": true,
            "direct_writes": "blocked"
        }]
    })
}

async fn server(instance: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .and(header("authorization", "Bearer fixture-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info(instance)))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn connection_is_verified_encrypted_and_reused_without_returning_the_token() {
    let app = TestApp::new().await;
    let server = server("instance-1").await;
    let (status, body) = app
        .call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({"base_url": server.uri(), "token": "fixture-token"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["roots"][0]["direct_writes"], "blocked");
    let encrypted: String = sqlx::query_scalar("SELECT token_enc FROM ism_connection")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert!(!encrypted.contains("fixture-token"));
    let (status, body) = app.call(Method::GET, "/api/ism/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["connection"]["instance_id"], "instance-1");
    assert!(!body.to_string().contains("fixture-token"));
    let (status, body) = app.call(Method::POST, "/api/ism/test", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["instance_id"], "instance-1");
    let (status, body) = app.call(Method::DELETE, "/api/ism/connection", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["connection"].is_null());
}

#[tokio::test]
async fn bound_volumes_prevent_disconnect_or_switch_to_another_instance() {
    let app = TestApp::new().await;
    let first = server("instance-1").await;
    let second = server("instance-2").await;
    assert_eq!(
        app.call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({
                "base_url": first.uri(), "token": "fixture-token"
            }))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (volume, _) = app.add_volume("photos").await;
    sqlx::query("INSERT INTO ism_volume_roots (volume_id, root_id, root_name) VALUES (?, 'photos', 'Photos')")
        .bind(volume)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let (status, body) = app
        .call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({
                "base_url": second.uri(), "token": "fixture-token"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "ism_bindings_exist");
    assert_eq!(
        app.call(Method::DELETE, "/api/ism/connection", None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, body) = app.call(Method::GET, "/api/ism/status", None).await;
    assert_eq!(body["connection"]["instance_id"], "instance-1");
    assert_eq!(body["bound_volumes"], 1);
}

#[tokio::test]
async fn incompatible_token_or_redirect_never_replaces_a_connection() {
    let app = TestApp::new().await;
    let good = server("instance-1").await;
    app.call(
        Method::PUT,
        "/api/ism/connection",
        Some(json!({
            "base_url": good.uri(), "token": "fixture-token"
        })),
    )
    .await;
    let bad = MockServer::start().await;
    let mut invalid = info("instance-2");
    invalid["token_kind"] = json!("ila");
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(invalid))
        .mount(&bad)
        .await;
    let (status, body) = app
        .call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({
                "base_url": bad.uri(), "token": "fixture-token"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "ism_incompatible");
    let redirect = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", good.uri()))
        .mount(&redirect)
        .await;
    let (status, _) = app
        .call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({
                "base_url": redirect.uri(), "token": "fixture-token"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, body) = app.call(Method::GET, "/api/ism/status", None).await;
    assert_eq!(body["connection"]["instance_id"], "instance-1");
}
