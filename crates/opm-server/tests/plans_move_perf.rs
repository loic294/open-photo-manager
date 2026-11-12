//! Opt-in 200k-source scale gate. All data is synthetic and index-only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::{TestApp, seed::Seed};
use opm_core::dto::*;
use opm_core::hash::sha1_bytes;
use opm_core::paths::RelPath;
use opm_server::plans::overlay::{GhostView, PlanOverlay, Window};
use serde_json::json;
use sqlx::{QueryBuilder, Sqlite};

const FILES: i64 = 200_000;

#[tokio::test]
#[ignore = "200k-source scale gate; run with --release --ignored --nocapture"]
async fn move_build_and_overlay_scale() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    for dir in ["src/0", "src/1", "out/0", "out/1"] {
        seed.dir(v, dir).await;
    }
    let mut tx = app.state.db.begin_write().await.unwrap();
    for start in (0..FILES).step_by(1000) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "INSERT INTO files (volume_id,dir_path,name,name_norm,stem_norm,ext,kind,size,mtime,capture_ts,capture_src,meta_state,hash) ",
        );
        qb.push_values(start..start + 1000, |mut b, i| {
            let name = format!("photo{i:06}.jpg");
            b.push_bind(v)
                .push_bind(format!("src/{}", i / 100_000))
                .push_bind(name.clone())
                .push_bind(name)
                .push_bind(format!("photo{i:06}"))
                .push_bind("jpg")
                .push_bind("image")
                .push_bind(1000_i64)
                .push_bind(1_700_000_000_i64)
                .push_bind(1_791_205_200_i64)
                .push_bind("exif")
                .push_bind(1_i64)
                .push_bind(sha1_bytes(&i.to_le_bytes()).to_vec());
        });
        qb.build().execute(&mut *tx).await.unwrap();
        let mut qb = QueryBuilder::<Sqlite>::new(
            "INSERT INTO files (volume_id,dir_path,name,name_norm,stem_norm,ext,kind,size,mtime,capture_ts,capture_src,meta_state,hash) ",
        );
        qb.push_values(start..start + 1000, |mut b, i| {
            let name = format!("match{i:06}.jpg");
            b.push_bind(v)
                .push_bind(format!("out/{}", i / 100_000))
                .push_bind(name.clone())
                .push_bind(name)
                .push_bind(format!("match{i:06}"))
                .push_bind("jpg")
                .push_bind("image")
                .push_bind(1000_i64)
                .push_bind(1_700_000_000_i64)
                .push_bind(1_791_205_200_i64)
                .push_bind("exif")
                .push_bind(1_i64)
                .push_bind(sha1_bytes(&i.to_le_bytes()).to_vec());
        });
        qb.build().execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE")
        .execute(app.state.db.write())
        .await
        .unwrap();
    let mut measurements = Vec::new();
    for kind in [
        MoveType::KeepSource,
        MoveType::KeepDestination,
        MoveType::Flat,
        MoveType::Custom,
    ] {
        for _ in 0..3 {
            let p = MoveParams {
                move_type: kind,
                source: SideParams {
                    scope: Scope {
                        volume_id: Some(v),
                        dir: "src".into(),
                        recursive: true,
                    },
                    filter: Filter::default(),
                },
                destination: FolderRef {
                    volume_id: v,
                    dir: "out".into(),
                },
                pattern: (kind == MoveType::Custom).then(|| "{YYYY}/{mm}/{folder}".into()),
                conflict_policy: None,
                match_mode: None,
                strict_prefix: false,
            };
            let start = Instant::now();
            let (status, body) = app
                .call(
                    Method::POST,
                    "/api/plans",
                    Some(json!({"kind":"move","params":p,"replaces":null})),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            let build = start.elapsed();
            let plan: PlanDto = serde_json::from_value(body).unwrap();
            assert_eq!(plan.stats["to_move"], FILES as u64);
            let dir = match kind {
                MoveType::KeepSource | MoveType::KeepDestination => "out/0",
                MoveType::Flat => "out",
                MoveType::Custom => "out/2026/10/0",
            };
            let view = GhostView {
                volume_id: v,
                dir: RelPath::new(dir).unwrap(),
                filter: Filter::default(),
                sort: SortKey::Name,
                order: SortOrder::Asc,
            };
            let ref_ = PlanRef {
                plan_id: plan.id,
                side: PlanSide::Comparison,
            };
            // Warm once; measure the actual page plus exact total, not a count-only proxy.
            app.state
                .plans
                .ghost_items(ref_, &view, Window { skip: 0, take: 200 })
                .await
                .unwrap();
            let start = Instant::now();
            let page = app
                .state
                .plans
                .ghost_items(ref_, &view, Window { skip: 0, take: 200 })
                .await
                .unwrap();
            let ghosts = start.elapsed();
            assert_eq!(page.items.len(), 200);
            assert_eq!(
                page.total,
                if kind == MoveType::Flat {
                    FILES
                } else {
                    FILES / 2
                }
            );
            let ids: Vec<_> = page.items.iter().map(|i| i.id).collect();
            let start = Instant::now();
            let flags = app
                .state
                .plans
                .item_flags(
                    PlanRef {
                        plan_id: plan.id,
                        side: PlanSide::Source,
                    },
                    &ids,
                )
                .await
                .unwrap();
            let flags_time = start.elapsed();
            assert_eq!(flags.len(), 200);
            let start = Instant::now();
            let (status, body) = app
                .call(
                    Method::POST,
                    "/api/format/preview",
                    Some(json!({"pattern":"{YYYY}/{mm}","source":p.source,"limit":5})),
                )
                .await;
            let preview = start.elapsed();
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["samples"].as_array().unwrap().len(), 5);
            println!(
                "{kind:?}: build={build:?} ghosts={ghosts:?} flags={flags_time:?} preview={preview:?}"
            );
            measurements.push((kind, [build, ghosts, flags_time, preview]));
            let (status, body) = app
                .call(Method::DELETE, &format!("/api/plans/{}", plan.id), None)
                .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            app.state.plans.settle().await;
        }
    }
    println!("type | metric | n | min ms | median ms | max ms | limit ms");
    let mut failures = Vec::new();
    for kind in [
        MoveType::KeepSource,
        MoveType::KeepDestination,
        MoveType::Flat,
        MoveType::Custom,
    ] {
        for (index, (label, limit)) in [
            ("build", Duration::from_secs(5)),
            ("ghosts", Duration::from_millis(50)),
            ("flags", Duration::from_millis(10)),
            ("preview", Duration::from_millis(50)),
        ]
        .into_iter()
        .enumerate()
        {
            let mut samples: Vec<_> = measurements
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, times)| times[index])
                .collect();
            samples.sort();
            assert_eq!(samples.len(), 3);
            println!(
                "{kind:?} | {label} | 3 | {:.3} | {:.3} | {:.3} | {:.0}",
                samples[0].as_secs_f64() * 1000.,
                samples[1].as_secs_f64() * 1000.,
                samples[2].as_secs_f64() * 1000.,
                limit.as_secs_f64() * 1000.
            );
            if !cfg!(debug_assertions) && samples[2] >= limit {
                failures.push(format!(
                    "{kind:?} {label}: max {:?} >= {limit:?}",
                    samples[2]
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
