// Test-only helpers should fail immediately when setup or an API response is invalid.
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::fsops::{loc, request};
use opm_core::dto::{ConflictPolicy, StatisticsQuery};
use opm_server::fsops::{OpContext, TrashRequest};
use opm_server::indexer::{RunContext, ScanRequest};

async fn history(app: &TestApp) -> opm_core::dto::StatisticsResponse {
    app.state.statistics.refresh(true).await.unwrap();
    let (status, body) = app.call(Method::GET, "/api/statistics", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

fn activity(data: &opm_core::dto::StatisticsResponse, action: &str) -> i64 {
    data.activity
        .iter()
        .filter(|row| row.action == action)
        .map(|row| row.count)
        .sum()
}

#[tokio::test]
async fn records_baseline_new_changes_duplicates_and_restores_without_move_noise() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("one.jpg", b"one");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "added"), 0);
    assert_eq!(activity(&data, "baseline"), 1);
    tree.write("two.jpg", b"new file");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    tree.write("two.jpg", b"changed content");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    app.state
        .indexer
        .scan(
            &ScanRequest {
                force: true,
                ..Default::default()
            },
            &RunContext::noop(),
        )
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "added"), 1);
    assert_eq!(activity(&data, "modified"), 1);
    app.state
        .fsops
        .move_file(
            request(
                loc(volume, "two.jpg"),
                loc(volume, "renamed.jpg"),
                ConflictPolicy::Smart,
            ),
            &OpContext::detached(),
        )
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "added"), 1);
    assert_eq!(activity(&data, "removed"), 0);
    let op = OpContext::detached();
    app.state
        .fsops
        .trash_file(
            TrashRequest {
                file_id: None,
                loc: loc(volume, "renamed.jpg"),
                batch: op.batch().clone(),
                reason: "duplicate".into(),
                expect: None,
            },
            &op,
        )
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "duplicates"), 1);
    assert_eq!(activity(&data, "trashed"), 1);
    assert_eq!(activity(&data, "removed"), 1);
    assert_eq!(data.snapshots.last().unwrap().inventory.files, 1);
    let total: i64 = data
        .snapshots
        .last()
        .unwrap()
        .inventory
        .kinds
        .iter()
        .map(|kind| kind.files)
        .sum();
    assert_eq!(total, 1);
    assert!(!tree.root().join("renamed.jpg").exists());
    let trash: i64 = sqlx::query_scalar("SELECT id FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    app.state
        .fsops
        .restore(trash, ConflictPolicy::Smart, &op)
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "restored"), 1);
    assert_eq!(activity(&data, "added"), 1);
    assert_eq!(activity(&data, "duplicates"), 1);
    assert!(tree.root().join("renamed.jpg").exists());
    app.state
        .fsops
        .trash_file(
            TrashRequest {
                file_id: None,
                loc: loc(volume, "renamed.jpg"),
                batch: op.batch().clone(),
                reason: "duplicate_sidecar".into(),
                expect: None,
            },
            &op,
        )
        .await
        .unwrap();
    let trash: i64 = sqlx::query_scalar("SELECT id FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    app.state.fsops.purge(trash, &op).await.unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "duplicates"), 1);
    assert_eq!(activity(&data, "duplicate_sidecars"), 1);
    assert_eq!(activity(&data, "purged"), 1);
    assert_eq!(
        data.activity
            .iter()
            .filter(|row| row.action == "purged")
            .map(|row| row.bytes)
            .sum::<i64>(),
        15
    );
    app.state.begin_shutdown();
    app.state.shutdown().await;
}

#[tokio::test]
async fn folder_restore_is_not_a_new_import_and_copies_are_additions() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("folder/one.jpg", b"one");
    tree.write("folder/sub/two.jpg", b"second");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    let op = OpContext::detached();
    app.state
        .fsops
        .trash_file(
            TrashRequest {
                file_id: None,
                loc: loc(volume, "folder"),
                batch: op.batch().clone(),
                reason: "user".into(),
                expect: None,
            },
            &op,
        )
        .await
        .unwrap();
    let trash: i64 = sqlx::query_scalar("SELECT id FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(activity(&history(&app).await, "trashed"), 2);
    app.state
        .fsops
        .restore(trash, ConflictPolicy::Smart, &op)
        .await
        .unwrap();
    tree.write("folder/new-after-restore.txt", b"new");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "restored"), 2);
    assert_eq!(activity(&data, "added"), 1);
    app.state
        .fsops
        .copy_file(
            request(
                loc(volume, "folder/one.jpg"),
                loc(volume, "copy.jpg"),
                ConflictPolicy::Smart,
            ),
            &op,
        )
        .await
        .unwrap();
    let data = history(&app).await;
    assert_eq!(activity(&data, "added"), 2);
    assert_eq!(data.snapshots.last().unwrap().inventory.files, 4);
    assert!(tree.root().join("folder/sub/two.jpg").exists());
    assert!(tree.root().join("copy.jpg").exists());
    app.state.begin_shutdown();
    app.state.shutdown().await;
}

#[tokio::test]
async fn validates_ranges_and_filters_volumes_without_fabricating_history() {
    let app = TestApp::new().await;
    let (a, _) = app.add_volume("a").await;
    let (_b, _) = app.add_volume("b").await;
    for path in [
        "/api/statistics?from=2026-02-30",
        "/api/statistics?from=2020-01-01&to=2026-01-01",
        "/api/statistics?from=2026-11-13&to=2026-11-12",
    ] {
        assert_eq!(
            app.call(Method::GET, path, None).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let data = app
        .state
        .statistics
        .history(StatisticsQuery {
            volume_id: Some(a),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(data.snapshots.len(), 1);
    assert_eq!(data.snapshots[0].inventory.volume_id, a);
    let data = app
        .state
        .statistics
        .history(StatisticsQuery {
            from: Some("2000-01-01".into()),
            to: Some("2000-01-31".into()),
            volume_id: None,
        })
        .await
        .unwrap();
    assert!(data.snapshots.is_empty());
    assert!(data.activity.is_empty());
}

#[tokio::test]
async fn timezone_segments_survive_round_trips_and_service_restart() {
    let app = TestApp::new().await;
    let (_volume, tree) = app.add_volume("photos").await;
    tree.write("one.jpg", b"one");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    history(&app).await;
    for zone in ["Europe/Paris", "America/Los_Angeles"] {
        let mut settings = (*app.state.settings.get()).clone();
        settings.statistics_timezone = zone.into();
        app.state.settings.update(settings).await.unwrap();
        app.state.statistics.refresh(true).await.unwrap();
    }
    let today = chrono::Utc::now().date_naive();
    let query = StatisticsQuery {
        from: Some((today - chrono::Days::new(1)).to_string()),
        to: Some((today + chrono::Days::new(1)).to_string()),
        ..Default::default()
    };
    let before = app.state.statistics.history(query.clone()).await.unwrap();
    assert_eq!(before.segments.len(), 3);
    assert_eq!(
        before.snapshots.len(),
        3,
        "returning to a timezone must not overwrite its earlier segment"
    );
    assert_ne!(
        before.snapshots[0].segment_id,
        before.snapshots[2].segment_id
    );
    app.state.statistics.shutdown().await;
    let restarted = opm_server::statistics::StatisticsService::new(
        app.state.db.clone(),
        app.state.settings.clone(),
    )
    .await
    .unwrap();
    let after = restarted.history(query).await.unwrap();
    assert_eq!(before.activity, after.activity);
    assert_eq!(before.segments, after.segments);
    assert_eq!(after.snapshots.len(), 3);
    restarted.shutdown().await;
    app.state.begin_shutdown();
    app.state.shutdown().await;
}
