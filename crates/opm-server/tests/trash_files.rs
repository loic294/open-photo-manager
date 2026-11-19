//! Deleting selected browse files: one resumable task, journaled moves and sidecars.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::seed::Seed;
use opm_core::dto::TaskState;
use serde_json::json;

#[tokio::test]
async fn trash_files_moves_selected_assets_and_unshared_sidecars() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    let photo = b"photo";
    let sidecar = b"xmp";
    let keep = b"keep";
    tree.write("trip/IMG_1.jpg", photo);
    tree.write("trip/IMG_1.xmp", sidecar);
    tree.write("trip/keep.jpg", keep);
    let seed = Seed::new(&app.state.db);
    let photo_id = seed
        .file(volume, "trip", "IMG_1.jpg")
        .size(photo.len() as i64)
        .insert()
        .await;
    let sidecar_id = seed
        .file(volume, "trip", "IMG_1.xmp")
        .size(sidecar.len() as i64)
        .insert()
        .await;
    seed.file(volume, "trip", "keep.jpg")
        .size(keep.len() as i64)
        .insert()
        .await;
    seed.sidecar(photo_id, sidecar_id, "exact").await;

    let (status, stored) = app
        .call(
            Method::POST,
            "/api/selections",
            Some(json!({"ids": [photo_id], "label": "Browse"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{stored}");
    let selection_id = stored["id"].as_i64().unwrap();

    let (status, queued) = app
        .call(
            Method::POST,
            "/api/trash/files",
            Some(json!({"selection_id": selection_id})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let task_id = queued["id"].as_i64().unwrap();
    let task = tokio::time::timeout(
        Duration::from_secs(10),
        app.state.tasks.wait_finished(task_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(task.title, "Trashed 1 item");

    assert!(!tree.path("trip/IMG_1.jpg").exists());
    assert!(!tree.path("trip/IMG_1.xmp").exists());
    assert!(tree.path("trip/keep.jpg").exists());
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT batch, orig_path, reason FROM trash_items WHERE volume_id = ? ORDER BY orig_path",
    )
    .bind(volume)
    .fetch_all(app.state.db.read())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, rows[1].0);
    assert_eq!(rows[0].1, "trip/IMG_1.jpg");
    assert_eq!(rows[0].2, "user");
    assert_eq!(rows[1].1, "trip/IMG_1.xmp");
    assert_eq!(rows[1].2, "user_sidecar");
    assert_eq!(
        std::fs::read(tree.path(&format!(".opm-trash/{}/trip/IMG_1.jpg", rows[0].0))).unwrap(),
        photo
    );
    assert_eq!(
        std::fs::read(tree.path(&format!(".opm-trash/{}/trip/IMG_1.xmp", rows[0].0))).unwrap(),
        sidecar
    );
    let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE volume_id = ?")
        .bind(volume)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(indexed, 1);
}

#[tokio::test]
async fn trash_files_refuses_an_unknown_or_empty_selection() {
    let app = TestApp::new().await;

    let (status, unknown) = app
        .call(
            Method::POST,
            "/api/trash/files",
            Some(json!({"selection_id": 999})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    assert_eq!(unknown["error"]["code"], "selection_not_found");

    let (status, stored) = app
        .call(
            Method::POST,
            "/api/selections",
            Some(json!({"ids": [], "label": "Empty"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{stored}");
    let (status, empty) = app
        .call(
            Method::POST,
            "/api/trash/files",
            Some(json!({"selection_id": stored["id"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{empty}");
    assert_eq!(empty["error"]["code"], "selection_empty");
}
