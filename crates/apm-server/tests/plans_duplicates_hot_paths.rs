//! Compare the real files table write costs with/without cache triggers, on identical rows.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use apm_core::dto::*;
use apm_server::db::Db;
use apm_server::plans::overlay::PlanOverlay;
use common::{TestApp, seed::Seed};
use serde_json::json;
use std::time::Instant;

const UPSERT: &str = "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, btime, meta_state, hash_state, thumb_state, seen_gen) \
    SELECT volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size + 1, mtime + 1, btime, meta_state, hash_state, thumb_state, seen_gen + 1 FROM files WHERE volume_id = ? \
    ON CONFLICT(volume_id, dir_path, name) DO UPDATE SET kind = excluded.kind, size = excluded.size, mtime = excluded.mtime, btime = excluded.btime, hash = NULL, \
    meta_state = excluded.meta_state, hash_state = excluded.hash_state, thumb_state = excluded.thumb_state, seen_gen = excluded.seen_gen";
const DELETE: &str = "DELETE FROM files WHERE volume_id = ?";
const DELETE_FOLDER: &str = "DELETE FROM files WHERE volume_id = ? AND dir_path = 'p0'";

async fn median(db: &Db, volume: i64, sql: &'static str) -> f64 {
    let mut times = Vec::new();
    for _ in 0..7 {
        let mut tx = db.begin_write().await.unwrap();
        let start = Instant::now();
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(volume)
            .execute(&mut *tx)
            .await
            .unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        tx.rollback().await.unwrap();
    }
    times.sort_by(f64::total_cmp);
    times[3]
}

#[tokio::test]
#[ignore = "200k-file cache hot-path benchmark"]
async fn duplicate_hot_paths() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    seed.file(a, "", "survivor.jpg")
        .hash([0; 20])
        .insert()
        .await;
    for start in (0..200_000).step_by(2000) {
        sqlx::query("WITH RECURSIVE seq(n) AS (SELECT ? UNION ALL SELECT n+1 FROM seq WHERE n < ?) \
            INSERT INTO files(volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime, hash) \
            SELECT ?, printf('p%d', n / 50000), printf('g%06d.jpg', n), printf('g%06d.jpg', n), printf('g%06d', n), 'image', 1000, 1700000000, zeroblob(20) FROM seq")
            .bind(start).bind(start + 1999).bind(b).execute(app.state.db.write()).await.unwrap();
    }
    let triggers: Vec<(String, String)> = sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'files' AND name LIKE 'duplicate_matches_%'")
        .fetch_all(app.state.db.read()).await.unwrap();
    for (name, _) in &triggers {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER {name}")))
            .execute(app.state.db.write())
            .await
            .unwrap();
    }
    let baseline_upsert = median(&app.state.db, b, UPSERT).await;
    let baseline_delete = median(&app.state.db, b, DELETE).await;
    for (_, ddl) in triggers {
        sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(app.state.db.write())
            .await
            .unwrap();
    }
    let empty_upsert = median(&app.state.db, b, UPSERT).await;
    let empty_delete = median(&app.state.db, b, DELETE).await;
    let side = |volume| SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: String::new(),
            recursive: true,
        },
        filter: Filter::default(),
    };
    let plan = app
        .state
        .plans
        .create(CreatePlanRequest {
            kind: PlanKind::Duplicates,
            params: json!(DuplicateParams {
                mode: MatchMode::Hash,
                strict_prefix: false,
                source: side(a),
                comparison: side(b)
            }),
            replaces: None,
        })
        .await
        .unwrap();
    assert_eq!(plan.stats["comparison_matched"], 200_000);
    let populated_upsert = median(&app.state.db, b, UPSERT).await;
    let populated_delete = median(&app.state.db, b, DELETE).await;
    let populated_folder = median(&app.state.db, b, DELETE_FOLDER).await;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM files WHERE volume_id = ? LIMIT 200")
        .bind(b)
        .fetch_all(app.state.db.read())
        .await
        .unwrap();
    let start = Instant::now();
    let flags = app
        .state
        .plans
        .item_flags(
            PlanRef {
                plan_id: plan.id,
                side: PlanSide::Comparison,
            },
            &ids,
        )
        .await
        .unwrap();
    let page_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(flags.len(), 200);
    eprintln!(
        "200k upsert ms baseline={baseline_upsert:.1} empty={empty_upsert:.1} populated={populated_upsert:.1}; empty overhead {:.2}%",
        100.0 * (empty_upsert / baseline_upsert - 1.0)
    );
    eprintln!(
        "200k delete ms baseline={baseline_delete:.1} empty={empty_delete:.1} populated={populated_delete:.1}; empty overhead {:.2}%",
        100.0 * (empty_delete / baseline_delete - 1.0)
    );
    eprintln!(
        "50k matched-folder delete={populated_folder:.1} ms; first 200-item overlay page={page_ms:.2} ms"
    );
    app.state.begin_shutdown();
}
