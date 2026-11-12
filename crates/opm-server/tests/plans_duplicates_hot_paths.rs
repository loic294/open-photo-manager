//! Compare the real files table write costs with/without cache triggers, on identical rows.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{TestApp, seed::Seed};
use opm_core::dto::*;
use opm_server::db::Db;
use opm_server::plans::overlay::PlanOverlay;
use serde_json::json;
use sqlx::Row;
use std::time::Instant;

const UPSERT: &str = "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, btime, meta_state, hash_state, thumb_state, seen_gen) \
    SELECT volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size + 1, mtime + 1, btime, meta_state, hash_state, thumb_state, seen_gen + 1 FROM files WHERE volume_id = ? \
    ON CONFLICT(volume_id, dir_path, name) DO UPDATE SET kind = excluded.kind, size = excluded.size, mtime = excluded.mtime, btime = excluded.btime, hash = NULL, \
    meta_state = excluded.meta_state, hash_state = excluded.hash_state, thumb_state = excluded.thumb_state, seen_gen = excluded.seen_gen RETURNING id, dir_path, name";
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

async fn empty_cache_comparison(
    db: &Db,
    volume: i64,
    triggers: &[(String, String)],
) -> (f64, f64, f64, f64) {
    let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    // Alternate order so changing machine load does not bias all baseline runs.
    for round in 0..8 {
        for enabled in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            for (name, ddl) in triggers {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP TRIGGER IF EXISTS {name}"
                )))
                .execute(db.write())
                .await
                .unwrap();
                if enabled {
                    sqlx::query(sqlx::AssertSqlSafe(ddl.clone()))
                        .execute(db.write())
                        .await
                        .unwrap();
                }
            }
            for (operation, sql) in [UPSERT, DELETE].into_iter().enumerate() {
                let mut tx = db.begin_write().await.unwrap();
                let start = Instant::now();
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .bind(volume)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                if round > 0 {
                    samples[usize::from(enabled) * 2 + operation].push(ms);
                }
                tx.rollback().await.unwrap();
            }
        }
    }
    for (name, ddl) in triggers {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER IF EXISTS {name}"
        )))
        .execute(db.write())
        .await
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(ddl.clone()))
            .execute(db.write())
            .await
            .unwrap();
    }
    for times in &mut samples {
        times.sort_by(f64::total_cmp);
    }
    for (label, times) in [
        "baseline upsert",
        "baseline delete",
        "empty upsert",
        "empty delete",
    ]
    .into_iter()
    .zip(&samples)
    {
        eprintln!(
            "{label}: median {:.1} ms; spread {:.1}..{:.1} ms",
            times[3], times[0], times[6]
        );
    }
    (samples[0][3], samples[1][3], samples[2][3], samples[3][3])
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
    let triggers: Vec<(String, String)> = sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'files' AND (name LIKE 'duplicate_matches_%' OR name = 'plan_file_identity_deleted')")
        .fetch_all(app.state.db.read()).await.unwrap();
    let programs: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN {UPSERT}")))
        .bind(b)
        .fetch_all(app.state.db.read())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|row| row.get::<Option<String>, _>("p4"))
        .collect();
    for (name, _) in &triggers {
        assert!(
            !programs.iter().any(|program| program.contains(name)),
            "scanner upsert must not execute {name}"
        );
    }
    eprintln!("EXPLAIN confirms scanner upsert executes none of the added trigger programs");
    let (baseline_upsert, baseline_delete, empty_upsert, empty_delete) =
        empty_cache_comparison(&app.state.db, b, &triggers).await;
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
