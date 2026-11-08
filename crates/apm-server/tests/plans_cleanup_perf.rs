//! Synthetic scale checks; no volume files are created or removed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{CreatePlanRequest, Filter, PlanKind, PlanRef, PlanSide, SortKey, SortOrder};
use apm_core::paths::RelPath;
use apm_server::plans::overlay::{GhostView, PlanOverlay};
use common::TestApp;
use serde_json::json;
use std::time::Instant;

#[tokio::test]
#[ignore = "200k directories and 1M files; run explicitly with --ignored --nocapture"]
async fn cleanup_scale() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let mut tx = app.state.db.begin_write().await.unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<999) \
         INSERT INTO dirs(volume_id,path,parent_path,name) \
         SELECT ?,printf('b%04d',i),'',printf('b%04d',i) FROM n",
    )
    .bind(v)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<198999) \
         INSERT INTO dirs(volume_id,path,parent_path,name) SELECT ?, \
         printf('b%04d/d%03d',i/199,i%199),printf('b%04d',i/199),printf('d%03d',i%199) FROM n",
    )
    .bind(v)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<999999) \
         INSERT INTO files(volume_id,dir_path,name,name_norm,stem_norm,ext,kind,size,mtime) \
         SELECT ?,'b0000/d000', \
         printf('f%07d.txt',i),printf('f%07d.txt',i),printf('f%07d',i),'txt','other',1,1 FROM n",
    )
    .bind(v)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let started = Instant::now();
    let plan = app
        .state
        .plans
        .create(CreatePlanRequest {
            kind: PlanKind::Cleanup,
            params: json!({"scope":{"volume_id":v,"dir":"","recursive":true}}),
            replaces: None,
        })
        .await
        .unwrap();
    let build = started.elapsed();
    assert_eq!(plan.stats["folders_scanned"], 200_000);
    assert_eq!(plan.stats["empty_folders"], 199_998);
    let reference = PlanRef {
        plan_id: plan.id,
        side: PlanSide::Comparison,
    };
    let mut view = GhostView {
        volume_id: v,
        dir: RelPath::root(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let children: Vec<_> = (0..1000).map(|i| format!("b{i:04}")).collect();
    let started = Instant::now();
    let overlay = app
        .state
        .plans
        .folder_overlay(reference, &view, &children)
        .await
        .unwrap();
    let root = started.elapsed();
    assert_eq!(overlay.per_child.len(), 999);
    view.dir = RelPath::new("b0999").unwrap();
    let children: Vec<_> = (0..199).map(|i| format!("d{i:03}")).collect();
    let started = Instant::now();
    let overlay = app
        .state
        .plans
        .folder_overlay(reference, &view, &children)
        .await
        .unwrap();
    let typical = started.elapsed();
    assert_eq!(overlay.per_child.len(), 199);
    println!(
        "cleanup: build={build:?}, root_overlay={root:?}, typical_overlay={typical:?}, \
        folders={}, planned={}; bounded buffers: 2000 insertion rows, 1000 stat rows, \
        1000 execution rows, 500 membership binds; no file/ancestor set",
        plan.stats["folders_scanned"], plan.stats["empty_folders"]
    );
    // The acceptance timings are for optimized builds; debug runs still report all measurements.
    if !cfg!(debug_assertions) {
        assert!(build.as_secs_f64() < 3.0, "build {build:?}");
        assert!(root.as_millis() < 150, "root overlay {root:?}");
        assert!(typical.as_millis() < 20, "typical overlay {typical:?}");
    }
}
