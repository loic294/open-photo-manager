//! `GET` and `PUT /api/settings` over the real application state: the document, its validation,
//! and how it survives restarts and upgrades.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use opm_core::dto::{SettingsDto, ValidationErrorBody};
use opm_server::app;
use opm_server::config::Config;
use opm_server::state::AppState;
use serde_json::{Value, json};

async fn get(app: &TestApp) -> SettingsDto {
    let (status, body) = app.call(Method::GET, "/api/settings", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

async fn put(app: &TestApp, document: &Value) -> (StatusCode, Value) {
    app.call(Method::PUT, "/api/settings", Some(document.clone()))
        .await
}

fn as_json(settings: &SettingsDto) -> Value {
    serde_json::to_value(settings).unwrap()
}

/// The document in the `settings` table, if a user ever saved one.
async fn stored(app: &TestApp) -> Option<Value> {
    let text: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'app'")
        .fetch_optional(app.state.db.read())
        .await
        .unwrap();
    text.map(|text| serde_json::from_str(&text).unwrap())
}

/// Stops the application and starts a new one over the same data directory.
async fn restart(app: TestApp) -> TestApp {
    let TestApp { router, state, dir } = app;
    drop(router);
    state.shutdown().await;
    drop(state);
    let state = AppState::new(Config::for_tests(dir.path())).await.unwrap();
    let router = app::router(state.clone());
    TestApp { router, state, dir }
}

fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

#[tokio::test]
async fn a_new_installation_serves_the_defaults_and_stores_nothing() {
    let app = TestApp::new().await;

    let (status, body) = app.call(Method::GET, "/api/settings", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sorted_keys(&body),
        [
            "default_conflict_policy",
            "excluded_folders",
            "index",
            "junk_names",
            "junk_prefixes",
            "keep_cross_volume_source_in_trash",
            "name_max_len",
            "sidecar_extensions",
            "soft_excluded_folders",
            "statistics_timezone",
            "trash_retention_days",
        ]
    );
    assert_eq!(
        sorted_keys(&body["index"]),
        [
            "concurrency",
            "hash_concurrency",
            "rescan_interval_minutes",
            "thumbnail_concurrency",
            "watch_enabled",
        ]
    );
    assert_eq!(
        body["index"]["concurrency"],
        json!(1),
        "OPM_INDEX_CONCURRENCY"
    );
    assert_eq!(body["default_conflict_policy"], json!("smart"));
    assert_eq!(body["trash_retention_days"], Value::Null);
    assert_eq!(body["name_max_len"], json!(255));
    assert!(
        body["sidecar_extensions"]
            .as_array()
            .unwrap()
            .contains(&json!("xmp"))
    );
    assert_eq!(
        stored(&app).await,
        None,
        "nothing is stored until a user saves"
    );
}

#[tokio::test]
async fn put_replaces_the_settings_and_answers_with_the_normalised_document() {
    let app = TestApp::new().await;
    let mut changes = app.state.settings.subscribe();
    let mut document = as_json(&get(&app).await);
    document["sidecar_extensions"] = json!([".XMP", "xmp", " Dop "]);
    document["junk_names"] = json!(["Thumbs.db", "thumbs.DB", "  .DS_Store "]);
    document["junk_prefixes"] = json!(["._"]);
    document["name_max_len"] = json!(143);
    document["default_conflict_policy"] = json!("rename");
    document["keep_cross_volume_source_in_trash"] = json!(true);
    document["trash_retention_days"] = json!(30);
    document["index"]["concurrency"] = json!(3);
    document["index"]["watch_enabled"] = json!(false);

    let (status, saved) = put(&app, &document).await;

    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["sidecar_extensions"], json!(["xmp", "dop"]));
    assert_eq!(saved["junk_names"], json!(["Thumbs.db", ".DS_Store"]));
    assert_eq!(saved["name_max_len"], json!(143));
    assert_eq!(saved["default_conflict_policy"], json!("rename"));
    assert_eq!(saved["keep_cross_volume_source_in_trash"], json!(true));
    assert_eq!(saved["trash_retention_days"], json!(30));
    assert_eq!(saved["index"]["concurrency"], json!(3));
    assert_eq!(saved["index"]["watch_enabled"], json!(false));

    assert_eq!(as_json(&get(&app).await), saved, "GET after PUT");
    assert_eq!(stored(&app).await, Some(saved.clone()));
    assert_eq!(as_json(&app.state.settings.get()), saved);
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .expect("subscribers are told about the change")
        .unwrap();
    assert_eq!(as_json(&changes.borrow()), saved);

    // Saving the answer again is a no-op.
    let (status, again) = put(&app, &saved).await;
    assert_eq!((status, &again), (StatusCode::OK, &saved));
    assert!(!changes.has_changed().unwrap());
}

#[tokio::test]
async fn put_rejects_invalid_values_naming_every_field() {
    let app = TestApp::new().await;
    let before = get(&app).await;
    let mut document = as_json(&before);
    document["name_max_len"] = json!(3);
    document["index"]["concurrency"] = json!(0);
    document["index"]["rescan_interval_minutes"] = json!(2);
    document["sidecar_extensions"] = json!(["jpg", "not valid!"]);
    document["trash_retention_days"] = json!(0);

    let (status, body) = put(&app, &document).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error: ValidationErrorBody = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(error.error.code, "validation_failed");
    let mut fields: Vec<&str> = error
        .error
        .fields
        .iter()
        .map(|field| field.field.as_str())
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "index.concurrency",
            "index.rescan_interval_minutes",
            "name_max_len",
            "sidecar_extensions[1]",
            "trash_retention_days",
        ]
    );
    for field in &error.error.fields {
        assert!(!field.message.is_empty(), "{field:?}");
        assert!(
            error.error.message.contains(&field.field),
            "the one-line message names {}: {}",
            field.field,
            error.error.message
        );
    }
    assert_eq!(
        sorted_keys(&body["error"]),
        ["code", "fields", "message"],
        "a superset of the usual error body"
    );

    assert_eq!(get(&app).await, before, "nothing changed");
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn put_with_a_malformed_document_is_an_invalid_json_error() {
    let app = TestApp::new().await;
    let before = get(&app).await;
    let valid = as_json(&before);

    let mut cases = vec![json!({}), json!([]), json!("settings"), Value::Null];
    let mut set = |path: &[&str], value: Value| {
        let mut document = valid.clone();
        let (last, parents) = path.split_last().unwrap();
        let mut target = &mut document;
        for key in parents {
            target = &mut target[*key];
        }
        target[*last] = value;
        cases.push(document);
    };
    set(&["name_max_len"], json!(70_000));
    set(&["name_max_len"], json!("wide"));
    set(&["index", "concurrency"], json!(300));
    set(&["index", "concurrency"], json!(-1));
    set(&["default_conflict_policy"], json!("explode"));
    set(&["trash_retention_days"], json!("a week"));
    set(&["sidecar_extensions"], json!("xmp"));
    set(&["index"], json!(3));
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("junk_names");
    cases.push(missing);

    for case in cases {
        let (status, body) = put(&app, &case).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{case}: {body}");
        assert_eq!(body["error"]["code"], json!("invalid_json"), "{case}");
    }
    assert_eq!(get(&app).await, before);
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn unknown_fields_in_a_put_are_ignored() {
    let app = TestApp::new().await;
    let mut document = as_json(&get(&app).await);
    document["added_by_a_newer_client"] = json!(true);
    document["index"]["also_new"] = json!(1);

    let (status, saved) = put(&app, &document).await;

    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(sorted_keys(&saved).len(), 11);
    assert!(saved.get("added_by_a_newer_client").is_none());
    assert_eq!(sorted_keys(&stored(&app).await.unwrap()).len(), 11);
}

#[tokio::test]
async fn saved_settings_survive_a_restart() {
    let app = TestApp::new().await;
    let mut document = as_json(&get(&app).await);
    document["name_max_len"] = json!(143);
    document["junk_prefixes"] = json!(["._", "~$"]);
    document["trash_retention_days"] = json!(14);
    let (status, saved) = put(&app, &document).await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let app = restart(app).await;

    assert_eq!(as_json(&get(&app).await), saved);
    assert_eq!(as_json(&app.state.settings.get()), saved);
}

#[tokio::test]
async fn an_older_or_damaged_stored_document_never_stops_the_server_from_starting() {
    let app = TestApp::new().await;
    let defaults = get(&app).await;
    // What an older version wrote: it knew fewer settings, one value is no longer valid, and it
    // had a setting that is gone.
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('app', ?, 1)")
        .bind(
            r#"{"name_max_len": 143, "index": {"concurrency": 99, "watch_enabled": false}, "retired": 1}"#,
        )
        .execute(app.state.db.write())
        .await
        .unwrap();

    let app = restart(app).await;

    let settings = get(&app).await;
    assert_eq!(settings.name_max_len, 143);
    assert!(!settings.index.watch_enabled);
    assert_eq!(
        settings.index.concurrency, defaults.index.concurrency,
        "a stored value that is no longer valid falls back to its default"
    );
    assert_eq!(settings.sidecar_extensions, defaults.sidecar_extensions);
    assert_eq!(settings.junk_names, defaults.junk_names);
    assert_eq!(
        settings.default_conflict_policy,
        defaults.default_conflict_policy
    );

    // A document of the wrong shape (the table only accepts valid JSON) is ignored as a whole.
    sqlx::query("UPDATE settings SET value = '[1, 2]' WHERE key = 'app'")
        .execute(app.state.db.write())
        .await
        .unwrap();
    let app = restart(app).await;
    assert_eq!(get(&app).await, defaults);

    // Saving repairs the stored document.
    let (status, saved) = put(&app, &as_json(&defaults)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(stored(&app).await, Some(saved));
}

#[tokio::test]
async fn reading_needs_no_database_and_a_failing_database_is_an_internal_error() {
    let app = TestApp::new().await;
    let before = get(&app).await;
    app.state.db.close().await;

    assert_eq!(get(&app).await, before, "reads come from the snapshot");

    let (status, body) = put(&app, &as_json(&before)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], json!("internal"));
    assert_eq!(body["error"]["message"], json!("internal server error"));
    assert_eq!(get(&app).await, before);
}
