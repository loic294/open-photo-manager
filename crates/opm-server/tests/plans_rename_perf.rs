//! Release-only scale checks for the rename planner.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::seed::Seed;
use opm_core::dto::{Filter, PlanKind, RenameRules, Scope, SideParams, SortKey, SortOrder};
use opm_core::paths::RelPath;
use opm_server::plans::overlay::{GhostView, PlanOverlay, Window};
use serde_json::json;

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn source(volume: i64, dir: &str) -> SideParams {
    SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: dir.to_owned(),
            recursive: true,
        },
        filter: Filter::default(),
    }
}

async fn insert_scale_files(app: &TestApp, volume: i64) {
    Seed::new(&app.state.db).dir(volume, "scale").await;
    sqlx::query(
        "WITH digits(d) AS (VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)), \
         numbers(n) AS (SELECT a.d + b.d * 10 + c.d * 100 + d.d * 1000 + 1 \
         FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d), \
         folders(n) AS (SELECT n FROM numbers WHERE n <= 5000) \
         INSERT INTO dirs (volume_id, path, parent_path, name) \
         SELECT ?, 'scale/dir' || printf('%04d', n), 'scale', 'dir' || printf('%04d', n) \
         FROM folders",
    )
    .bind(volume)
    .execute(app.state.db.write())
    .await
    .unwrap();
    sqlx::query(
        "WITH digits(d) AS (VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)), \
         numbers(n) AS (SELECT a.d + b.d * 10 + c.d * 100 + d.d * 1000 + e.d * 10000 + f.d * 100000 + 1 \
         FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e CROSS JOIN digits f) \
         INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, meta_state) \
         SELECT ?, 'scale/dir' || printf('%04d', ((n - 1) / 50) + 1), \
         printf('f%02d.jpg', (n - 1) % 50), printf('f%02d.jpg', (n - 1) % 50), \
         printf('f%02d', (n - 1) % 50), 'jpg', 'image', 1, 1700000000, 1 \
         FROM numbers WHERE n <= 250000",
    )
    .bind(volume)
    .execute(app.state.db.write())
    .await
    .unwrap();
    Seed::new(&app.state.db).dir(volume, "preview").await;
    sqlx::query(
        "WITH digits(d) AS (VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)), \
         numbers(n) AS (SELECT a.d + b.d * 10 + c.d * 100 + d.d * 1000 + e.d * 10000 + 1 \
         FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e) \
         INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, meta_state) \
         SELECT ?, 'preview', printf('p%05d.jpg', n), printf('p%05d.jpg', n), \
         printf('p%05d', n), 'jpg', 'image', 1, 1700000000, 1 \
         FROM numbers WHERE n <= 20000",
    )
    .bind(volume)
    .execute(app.state.db.write())
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "release scale benchmark; run explicitly with --release --ignored"]
async fn rename_scale_targets() {
    let app = TestApp::new().await;
    let (volume, _) = app.add_volume("photos").await;
    insert_scale_files(&app, volume).await;
    let params = json!({
        "source": source(volume, "scale"),
        "rules": RenameRules {
            pattern: "Renamed_{name}".to_owned(),
            ..RenameRules::default()
        },
        "conflict_policy": null
    });

    let mut builds_ms = Vec::new();
    let mut last_plan_id = 0_i64;
    for _ in 0..3 {
        let started = Instant::now();
        let (status, plan) = app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({
                    "kind": PlanKind::Rename,
                    "params": params,
                    "replaces": null
                })),
            )
            .await;
        builds_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(status, StatusCode::CREATED, "{plan}");
        assert_eq!(plan["stats"]["candidates"], 250000);
        assert_eq!(plan["stats"]["to_rename"], 250000);
        last_plan_id = plan["id"].as_i64().unwrap();
        if builds_ms.len() < 3 {
            app.state.plans.delete(last_plan_id).await.unwrap();
            app.state.plans.settle().await;
        }
    }

    let view = GhostView {
        volume_id: volume,
        dir: RelPath::new("scale/dir0001").unwrap(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let mut ghosts_ms = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let ghosts = app
            .state
            .plans
            .ghost_items(
                opm_core::dto::PlanRef {
                    plan_id: last_plan_id,
                    side: opm_core::dto::PlanSide::Comparison,
                },
                &view,
                Window { skip: 0, take: 50 },
            )
            .await
            .unwrap();
        ghosts_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(ghosts.total, 50);
        assert_eq!(ghosts.items.len(), 50);
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT file_id FROM move_items WHERE plan_id = ? ORDER BY file_id LIMIT 200",
    )
    .bind(last_plan_id)
    .fetch_all(app.state.db.read())
    .await
    .unwrap();
    let mut flags_ms = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let flags = app
            .state
            .plans
            .item_flags(
                opm_core::dto::PlanRef {
                    plan_id: last_plan_id,
                    side: opm_core::dto::PlanSide::Source,
                },
                &ids,
            )
            .await
            .unwrap();
        flags_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(flags.len(), ids.len());
    }

    let mut preview_ms = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let (status, preview) = app
            .call(
                Method::POST,
                "/api/rename/preview",
                Some(json!({
                    "rules": RenameRules {
                        pattern: "Renamed_{name}".to_owned(),
                        ..RenameRules::default()
                    },
                    "source": source(volume, "preview"),
                    "limit": 30
                })),
            )
            .await;
        preview_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["matched"], 20000);
        assert_eq!(preview["samples"].as_array().unwrap().len(), 30);
    }

    let build = median(&mut builds_ms);
    let ghosts = median(&mut ghosts_ms);
    let flags = median(&mut flags_ms);
    let preview = median(&mut preview_ms);
    eprintln!(
        "rename scale medians: build={build:.2}ms ghosts={ghosts:.2}ms flags={flags:.2}ms preview={preview:.2}ms"
    );
    assert!(build <= 5_000.0, "build {build:.2}ms exceeds 5000ms");
    assert!(ghosts <= 50.0, "ghost page {ghosts:.2}ms exceeds 50ms");
    assert!(flags <= 10.0, "flags {flags:.2}ms exceeds 10ms");
    assert!(preview <= 150.0, "preview {preview:.2}ms exceeds 150ms");
}
