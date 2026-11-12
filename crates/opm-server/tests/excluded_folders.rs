//! Excluded folders: `POST /api/settings/excluded-folders` drops a folder from the index and keeps
//! it out of every file operation, `PUT /api/settings` preserves the list, and
//! `/excluded-folders/remove` indexes the folder again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::browse::{error_code, folders, items, names};
use common::fsops::{loc, request as move_request};
use common::{TestApp, Tree};
use opm_core::dto::common::ConflictPolicy;
use opm_core::paths::VolumeId;
use opm_server::fsops::{FsOpError, OpContext, TrashBatch, TrashRequest};
use opm_server::indexer::{Embedded, FakeExtractor, IndexJobs, SchedulerOptions};
use serde_json::{Value, json};

const PATIENCE: Duration = Duration::from_secs(30);

async fn eventually<F, Fut>(what: &str, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + PATIENCE;
    while !probe().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn lab() -> (TestApp, VolumeId, Tree) {
    let mut app = TestApp::new().await;
    let state = &app.state;
    let extractor = Arc::new(FakeExtractor::new(|_| Ok(Embedded::default())));
    app.state.index_jobs = IndexJobs::with_options(
        state.indexer.with_extractor(extractor),
        state.tasks.clone(),
        state.fsops.clone(),
        state.shutdown.clone(),
        SchedulerOptions::default(),
    );
    app.router = opm_server::app::router(app.state.clone());
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("2024/keep.jpg", b"keep");
    tree.write("2024/private/secret.jpg", b"secret");
    tree.write("2024/private/deep/more.jpg", b"more");
    let (code, body) = app
        .call(
            Method::POST,
            "/api/index/rescan",
            Some(json!({"targets": []})),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let task = body["id"].as_i64().unwrap();
    app.state.tasks.wait_finished(task).await.unwrap();
    (app, volume, tree)
}

async fn indexed_dir(app: &TestApp, volume: VolumeId, dir: &str) -> bool {
    let (code, body) = folders(app, json!({"volume_id": volume, "dir": dir})).await;
    match code {
        StatusCode::OK => true,
        StatusCode::NOT_FOUND => {
            assert_eq!(error_code(&body), "folder_not_found", "{body}");
            false
        }
        other => panic!("{other}: {body}"),
    }
}

async fn subfolders(app: &TestApp, volume: VolumeId, dir: &str) -> Vec<String> {
    let (code, body) = folders(app, json!({"volume_id": volume, "dir": dir})).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    names(&body, "folders")
}

async fn exclude(app: &TestApp, volume: VolumeId, path: &str) -> (StatusCode, Value) {
    app.call(
        Method::POST,
        "/api/settings/excluded-folders",
        Some(json!({"volume_id": volume, "path": path})),
    )
    .await
}

#[tokio::test]
async fn an_excluded_folder_leaves_the_index_and_comes_back_when_included() {
    let (app, volume, tree) = lab().await;
    assert!(indexed_dir(&app, volume, "2024/private").await);

    let (code, body) = exclude(&app, volume, "2024/private").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(
        body["excluded_folders"],
        json!([{"volume_id": volume, "path": "2024/private"}])
    );
    eventually("the folder to leave the index", || async {
        !indexed_dir(&app, volume, "2024/private").await
    })
    .await;
    assert_eq!(subfolders(&app, volume, "2024").await, Vec::<String>::new());
    let (_, listing) = items(
        &app,
        json!({"volume_id": volume, "dir": "2024", "recursive": true}),
    )
    .await;
    assert_eq!(names(&listing, "items"), vec!["keep.jpg"]);
    assert!(
        tree.path("2024/private/secret.jpg").exists(),
        "files stay on disk"
    );

    // A rescan of the whole volume does not bring it back.
    let (code, body) = app
        .call(
            Method::POST,
            "/api/index/rescan",
            Some(json!({"targets": []})),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    app.state
        .tasks
        .wait_finished(body["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert!(!indexed_dir(&app, volume, "2024/private").await);

    let (code, body) = app
        .call(
            Method::POST,
            "/api/settings/excluded-folders/remove",
            Some(json!({"volume_id": volume, "path": "2024/private"})),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["excluded_folders"], json!([]));
    eventually("the folder to be indexed again", || async {
        indexed_dir(&app, volume, "2024/private/deep").await
    })
    .await;
}

#[tokio::test]
async fn file_operations_refuse_excluded_folders() {
    let (app, volume, _tree) = lab().await;
    let (code, body) = exclude(&app, volume, "2024/private").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let ops = &app.state.fsops;
    let cx = OpContext::detached();

    let into = move_request(
        loc(volume, "2024/keep.jpg"),
        loc(volume, "2024/private/keep.jpg"),
        ConflictPolicy::Skip,
    );
    let err = ops.move_file(into, &cx).await.unwrap_err();
    assert!(matches!(err, FsOpError::FolderExcluded), "{err:?}");

    let out = move_request(
        loc(volume, "2024/private/secret.jpg"),
        loc(volume, "2024/secret.jpg"),
        ConflictPolicy::Skip,
    );
    let err = ops.move_file(out, &cx).await.unwrap_err();
    assert!(matches!(err, FsOpError::FolderExcluded), "{err:?}");

    let trash = TrashRequest {
        file_id: None,
        loc: loc(volume, "2024/private/secret.jpg"),
        expect: None,
        batch: TrashBatch::adhoc(),
        reason: "user".to_owned(),
    };
    let err = ops.trash_file(trash, &cx).await.unwrap_err();
    assert!(matches!(err, FsOpError::FolderExcluded), "{err:?}");
    assert_eq!(err.code(), "folder_excluded");
}

#[tokio::test]
async fn put_settings_keeps_the_excluded_folders() {
    let (app, volume, _tree) = lab().await;
    let (_, stale) = app.call(Method::GET, "/api/settings", None).await;
    let (code, body) = exclude(&app, volume, "2024/private").await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let (code, saved) = app.call(Method::PUT, "/api/settings", Some(stale)).await;
    assert_eq!(code, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["excluded_folders"],
        json!([{"volume_id": volume, "path": "2024/private"}])
    );
}

#[tokio::test]
async fn invalid_exclusions_are_refused() {
    let (app, volume, _tree) = lab().await;
    for (volume, path, status) in [
        (volume, "", StatusCode::BAD_REQUEST),
        (volume, "../x", StatusCode::BAD_REQUEST),
        (volume, ".opm-trash/batch", StatusCode::BAD_REQUEST),
        (volume, "#recycle", StatusCode::BAD_REQUEST),
        (volume + 99, "2024", StatusCode::NOT_FOUND),
    ] {
        let (code, body) = exclude(&app, volume, path).await;
        assert_eq!(code, status, "{path}: {body}");
    }
    let (code, body) = app
        .call(
            Method::POST,
            "/api/settings/excluded-folders/remove",
            Some(json!({"volume_id": volume, "path": "2024"})),
        )
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "exclusion_not_found");
}
