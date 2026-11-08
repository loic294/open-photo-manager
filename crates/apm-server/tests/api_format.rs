#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::*;
use apm_core::pattern;
use axum::http::{Method, StatusCode};
use common::{TestApp, seed::Seed};
use serde_json::json;

#[tokio::test]
async fn samples_use_indexed_sidecar_dates_filename_tokens_and_pending_skips() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    let id = seed
        .file(v, "event", "a.jpg")
        .capture(1_791_205_200)
        .capture_src(CaptureSource::Xmp)
        .insert()
        .await;
    sqlx::query("UPDATE files SET meta_state = 1 WHERE id = ?")
        .bind(id)
        .execute(app.state.db.write())
        .await
        .unwrap();
    seed.file(v, "event", "b.jpg").insert().await;
    seed.file(v, "event", "a.xmp").insert().await;
    let source = SideParams {
        scope: Scope {
            volume_id: Some(v),
            dir: "event".into(),
            recursive: true,
        },
        filter: Filter::default(),
    };
    let (status, body) = app
        .call(
            Method::POST,
            "/api/format/preview",
            Some(json!({"pattern":"{YYYY}/{mm}/{name}_edit.{ext}","source":source,"limit":null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview: FormatPreviewResponse = serde_json::from_value(body).unwrap();
    assert!(preview.error.is_none());
    assert!(preview.has_filename_token && !preview.appends_filename);
    assert_eq!(preview.samples.len(), 2);
    assert_eq!(preview.samples[0].source, "photos/event/a.jpg");
    assert_eq!(
        preview.samples[0].destination.as_deref(),
        Some("2026/10/a_edit.jpg")
    );
    assert_eq!(preview.samples[0].capture_src, Some(CaptureSource::Xmp));
    assert_eq!(preview.samples[1].skip.as_deref(), Some("metadata_pending"));
    let (status, body) = app
        .call(
            Method::POST,
            "/api/format/preview",
            Some(json!({"pattern":".apm-trash/{filename}","source":source,"limit":1})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["samples"][0]["skip"], "reserved_destination");
    assert!(body["samples"][0]["destination"].is_null());
}

#[tokio::test]
async fn live_typing_errors_have_spans_and_catalogue_is_formatter_owned() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    for (text, code, suggestion) in [
        ("{yyyy}/{mm}", "unknown_token", Some("{YYYY}")),
        ("../{filename}", "parent_component", None),
    ] {
        let (status, body) = app.call(Method::POST, "/api/format/preview", Some(json!({
            "pattern":text,"source":{"scope":{"volume_id":v,"dir":"","recursive":true},"filter":{}},"limit":null
        }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["error"]["code"], code);
        assert_eq!(body["error"]["suggestion"].as_str(), suggestion);
        assert_eq!(body["error"]["start"], 0);
        assert!(body["error"]["end"].as_u64().unwrap() > 0);
        assert_eq!(body["samples"], json!([]));
    }
    let (status, body) = app.call(Method::GET, "/api/format/tokens", None).await;
    assert_eq!(status, StatusCode::OK);
    let response: FormatTokensResponse = serde_json::from_value(body).unwrap();
    assert_eq!(response.tokens.len(), pattern::describe().len());
    for (actual, expected) in response.tokens.iter().zip(pattern::describe()) {
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.example, expected.example);
    }
    assert_eq!(response.examples[0].result, pattern::examples()[0].result);
    assert_eq!(
        response.sample.file_name,
        pattern::sample_context().file_name
    );
}

#[tokio::test]
async fn malformed_requests_limits_and_csrf_are_errors() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    for body in [
        json!({}),
        json!({"pattern":"{YYYY}","source":{"scope":{"volume_id":v,"dir":"","recursive":true},"filter":{}},"limit":21}),
    ] {
        let (status, _) = app
            .call(Method::POST, "/api/format/preview", Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/format/preview")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        app.router.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
