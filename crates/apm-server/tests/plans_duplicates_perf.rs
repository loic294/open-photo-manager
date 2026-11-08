//! Explicit scale gate: 200k files per side, 520k pairs, 200 folder children.
//! Run with optimized SQLite:
//! CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off
//! cargo test --release -p apm-server --test plans_duplicates_perf -- --ignored --nocapture
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use apm_core::dto::*;
use apm_core::paths::RelPath;
use apm_server::plans::overlay::{GhostView, PlanOverlay};
use common::{TestApp, seed::Seed};
use serde_json::json;

#[tokio::test]
#[ignore = "400,000-file duplicate build and folder timing"]
async fn duplicate_scale() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    for volume in [a, b] {
        for start in (0..200_000).step_by(2000) {
            sqlx::query(
                "WITH RECURSIVE seq(n) AS (SELECT ? UNION ALL SELECT n+1 FROM seq WHERE n < ?) \
                 INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime, capture_ts, hash) \
                 SELECT ?, printf('child%03d/copy%d', n % 200, n / 80000), \
                 printf('g%06d.jpg', n % 80000), printf('g%06d.jpg', n % 80000), printf('g%06d', n % 80000), \
                 'image', 1000, 1700000000, 1, CAST(printf('%020d', n % 80000) AS BLOB) FROM seq"
            ).bind(start).bind(start + 1999).bind(volume).execute(app.state.db.write()).await.unwrap();
        }
    }
    app.state.db.optimize().await.unwrap();
    let children: Vec<_> = (0..200).map(|n| format!("child{n:03}")).collect();
    for mode in MatchMode::ALL {
        sqlx::query("UPDATE files SET capture_ts = ? WHERE volume_id = ?")
            .bind(if mode == MatchMode::NameDifferentTime {
                2
            } else {
                1
            })
            .bind(b)
            .execute(app.state.db.write())
            .await
            .unwrap();
        let side = |volume| SideParams {
            scope: Scope {
                volume_id: Some(volume),
                dir: String::new(),
                recursive: true,
            },
            filter: Filter::default(),
        };
        let started = Instant::now();
        let plan = app
            .state
            .plans
            .create(CreatePlanRequest {
                kind: PlanKind::Duplicates,
                params: json!(DuplicateParams {
                    mode,
                    strict_prefix: false,
                    source: side(a),
                    comparison: side(b)
                }),
                replaces: None,
            })
            .await
            .unwrap();
        let build_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(plan.stats["pairs"], 520_000);
        let view = GhostView {
            volume_id: b,
            dir: RelPath::root(),
            filter: Filter::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        let reference = PlanRef {
            plan_id: plan.id,
            side: PlanSide::Comparison,
        };
        app.state
            .plans
            .folder_overlay(reference, &view, &children)
            .await
            .unwrap();
        let mut timings = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            let overlay = app
                .state
                .plans
                .folder_overlay(reference, &view, &children)
                .await
                .unwrap();
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(overlay.per_child.len(), 200);
            assert_eq!(
                overlay
                    .per_child
                    .values()
                    .map(|info| info.matches.unwrap())
                    .sum::<i64>(),
                200_000
            );
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "{mode:?}: build {build_ms:.1} ms, warm folder median {:.1} ms (520k pairs/200 children)",
            timings[3]
        );
        assert!(
            timings[3] < 100.0,
            "{mode:?} warm folder query exceeds 100 ms: {timings:?}"
        );
        app.state.plans.delete(plan.id).await.unwrap();
        app.state.plans.settle().await;
    }
    app.state.begin_shutdown();
}
