#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use opm_core::dto::TaskState;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
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

#[tokio::test]
async fn a_bound_folder_can_request_a_targeted_immich_sync() {
    let app = TestApp::new().await;
    let remote = MockServer::start().await;
    let (volume, _) = app.add_volume("photos").await;
    let discovery = info("instance-1");
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
        .mount(&remote)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/instances/instance-1/immich/sync"))
        .and(header("authorization", "Bearer fixture-token"))
        .and(body_json(
            json!({"root_id": "photos", "path": "library/trip"}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&remote)
        .await;

    assert_eq!(
        app.call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({"base_url": remote.uri(), "token": "fixture-token"})),
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query(
        "INSERT INTO ism_volume_roots (volume_id, root_id, root_name, root_prefix)
         VALUES (?, 'photos', 'Photos', 'library')",
    )
    .bind(volume)
    .execute(app.state.db.write())
    .await
    .unwrap();

    let (status, task) = app
        .call(
            Method::POST,
            "/api/ism/sync",
            Some(json!({"volume_id": volume, "dir": "trip"})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(task["kind"], "immich_sync");
    let id = task["id"].as_i64().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if app.state.tasks.get(id).await.unwrap().state == TaskState::Done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn shared_rules_are_normalized_cached_and_do_not_replace_local_rules() {
    let app = TestApp::new().await;
    let remote = MockServer::start().await;
    let mut discovery = info("instance-1");
    discovery["capabilities"] = json!(["shared_file_settings"]);
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
        .mount(&remote)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/client/settings"))
        .and(header("authorization", "Bearer fixture-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": {
                "sidecar_extensions": [".XMP", "xmp", "JSON"],
                "junk_names": ["ignored"], "junk_prefixes": ["._"], "name_max_len": 143
            },
            "revision": 4, "origin_instance_id": "primary",
            "primary": {"instance_id": "primary", "name": "Primary NAS"}
        })))
        .mount(&remote)
        .await;

    let (_, mut local) = app.call(Method::GET, "/api/settings", None).await;
    local["name_max_len"] = json!(199);
    assert_eq!(
        app.call(Method::PUT, "/api/settings", Some(local)).await.0,
        StatusCode::OK
    );
    app.state.ism.start_background(app.state.shutdown.clone());
    let mut settings = app.state.settings.subscribe();
    assert_eq!(
        app.call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({"base_url": remote.uri(), "token": "fixture-token"})),
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        settings.wait_for(|doc| {
            doc.file_settings_source
                .as_ref()
                .is_some_and(|source| source.ready)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let (_, mut effective) = app.call(Method::GET, "/api/settings", None).await;
    assert_eq!(
        effective["file_settings_source"]["primary_name"],
        "Primary NAS"
    );
    assert_eq!(effective["sidecar_extensions"], json!(["xmp", "json"]));
    assert_eq!(effective["name_max_len"], 143);
    effective["name_max_len"] = json!(18);
    effective["keep_cross_volume_source_in_trash"] = json!(true);
    let (status, saved) = app
        .call(Method::PUT, "/api/settings", Some(effective))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["name_max_len"], 143);
    assert_eq!(saved["keep_cross_volume_source_in_trash"], true);

    drop(remote);
    let reloaded =
        opm_server::settings::SettingsService::load(app.state.db.clone(), &app.state.config)
            .await
            .unwrap();
    assert_eq!(reloaded.get().name_max_len, 143);
    assert_eq!(
        app.call(Method::DELETE, "/api/ism/connection", None)
            .await
            .0,
        StatusCode::OK
    );
    let (_, local) = app.call(Method::GET, "/api/settings", None).await;
    assert_eq!(local["name_max_len"], 199);
    assert_eq!(local["keep_cross_volume_source_in_trash"], true);
    assert!(local.get("file_settings_source").is_none());
}

#[tokio::test]
async fn paginated_assets_are_mapped_into_the_bound_volume() {
    let app = TestApp::new().await;
    let remote = MockServer::start().await;
    let (volume, _) = app.add_volume("photos").await;
    let mut discovery = info("instance-1");
    discovery["capabilities"] = json!(["immich_assets"]);
    Mock::given(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
        .mount(&remote)
        .await;
    Mock::given(path("/api/v1/assets"))
        .and(header("authorization", "Bearer fixture-token"))
        .and(query_param("path", "library"))
        .and(query_param("root_id", "photos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "root_id": "photos", "linked": true,
            "assets": [{"asset_id": "a", "path": "library/a.jpg"}],
            "next": "library/a.jpg"
        })))
        .with_priority(2)
        .mount(&remote)
        .await;
    Mock::given(path("/api/v1/assets"))
        .and(query_param("after", "library/a.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "root_id": "photos", "linked": true,
            "assets": [{"asset_id": "b", "path": "library/trip/b.jpg"},
                       {"asset_id": "c", "path": "library2/c.jpg"}],
            "next": null
        })))
        .with_priority(1)
        .mount(&remote)
        .await;
    assert_eq!(
        app.call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({"base_url": remote.uri(), "token": "fixture-token"})),
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("INSERT INTO ism_volume_roots VALUES (?, 'photos', 'Photos', 'library')")
        .bind(volume)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let mut events = app.state.tasks.subscribe();
    app.state.ism.start_background(app.state.shutdown.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !matches!(events.recv().await.unwrap(),
            opm_core::dto::ServerEvent::ImmichAssetsChanged { volume_id } if volume_id == volume)
        {
        }
    })
    .await
    .unwrap();
    let paths: Vec<(String, String)> = sqlx::query_as(
        "SELECT dir_path, name FROM ism_assets WHERE volume_id = ? ORDER BY dir_path, name",
    )
    .bind(volume)
    .fetch_all(app.state.db.read())
    .await
    .unwrap();
    assert_eq!(
        paths,
        [("".into(), "a.jpg".into()), ("trip".into(), "b.jpg".into())]
    );
    assert!(app.state.settings.get().file_settings_source.is_none());
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
