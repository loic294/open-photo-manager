//! Live membership parity and hot-write transaction invariants.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::*;
use apm_core::hash::sha1_bytes;
use apm_core::paths::RelPath;
use apm_server::fsops::{Expect, Loc, MoveRequest, OpContext};
use apm_server::indexer::{RunContext, ScanRequest};
use apm_server::plans::overlay::{GhostView, PlanOverlay};
use common::{TestApp, seed::Seed};
use serde_json::json;

async fn build(app: &TestApp, a: i64, b: i64) -> PlanDto {
    let side = |volume| SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: String::new(),
            recursive: true,
        },
        filter: Filter::default(),
    };
    app.state
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
        .unwrap()
}

async fn parity(app: &TestApp, plan: i64, volume: i64) {
    let children = [
        "p0".to_owned(),
        "p1".to_owned(),
        "p2".to_owned(),
        "moved".to_owned(),
    ];
    let view = GhostView {
        volume_id: volume,
        dir: RelPath::root(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let overlay = app
        .state
        .plans
        .folder_overlay(
            PlanRef {
                plan_id: plan,
                side: PlanSide::Comparison,
            },
            &view,
            &children,
        )
        .await
        .unwrap();
    for child in children {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT p.comparison_id) FROM duplicate_pairs p JOIN files f ON f.id = p.comparison_id \
             WHERE p.plan_id = ? AND f.volume_id = ? AND (f.dir_path = ? OR substr(f.dir_path, 1, length(?) + 1) = ? || '/')"
        ).bind(plan).bind(volume).bind(&child).bind(&child).bind(&child).fetch_one(app.state.db.read()).await.unwrap();
        assert_eq!(overlay.per_child[&child].matches, Some(count), "{child}");
    }
}

#[tokio::test]
async fn random_build_move_delete_rebuild_and_rollback_counts_equal_live_pair_join() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    seed.file(a, "", "one.jpg")
        .hash(sha1_bytes(b"1"))
        .insert()
        .await;
    let mut ids = Vec::new();
    for i in 0..36 {
        ids.push(
            seed.file(b, &format!("p{}", i % 3), &format!("{i}.jpg"))
                .hash(sha1_bytes(b"1"))
                .insert()
                .await,
        );
    }
    let mut plan = build(&app, a, b).await;
    let mut random = 98765_u64;
    for step in 0..40 {
        parity(&app, plan.id, b).await;
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let id = ids[(random % ids.len() as u64) as usize];
        let mut tx = app.state.db.begin_write().await.unwrap();
        if step % 3 == 0 {
            sqlx::query("DELETE FROM files WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE files SET dir_path = ? WHERE id = ?")
                .bind(format!("p{}", random % 3))
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        if step % 4 == 0 {
            tx.rollback().await.unwrap();
        } else {
            tx.commit().await.unwrap();
        }
        if step % 7 == 0 {
            plan = build(&app, a, b).await;
        }
    }
    parity(&app, plan.id, b).await;
    sqlx::query("DELETE FROM volumes WHERE id = ?")
        .bind(b)
        .execute(app.state.db.write())
        .await
        .unwrap();
    parity(&app, plan.id, b).await;
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplicate_matches WHERE plan_id = ? AND side = 'comparison' AND live = 1").bind(plan.id).fetch_one(app.state.db.read()).await.unwrap();
    assert_eq!(live, 0, "volume FK cascade marks all memberships vanished");
    app.state.begin_shutdown();
}

#[tokio::test]
async fn scanner_and_fsops_finalizing_transactions_keep_cache_in_step() {
    let app = TestApp::new().await;
    let (a, ta) = app.add_volume("a").await;
    let (b, tb) = app.add_volume("b").await;
    ta.write("one.jpg", b"same");
    tb.write("p0/one.jpg", b"same");
    let seed = Seed::new(&app.state.db);
    seed.file(a, "", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let c = seed
        .file(b, "p0", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let plan = build(&app, a, b).await;
    parity(&app, plan.id, b).await;
    tb.write("p0/one.jpg", b"edited");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    parity(&app, plan.id, b).await;
    let context = OpContext::detached();
    app.state
        .fsops
        .move_file(
            MoveRequest {
                file_id: Some(c),
                src: Loc::new(b, RelPath::new("p0/one.jpg").unwrap()),
                dst: Loc::new(b, RelPath::new("moved/one.jpg").unwrap()),
                expect: Some(Expect {
                    size: 6,
                    mtime: common::BASE_MTIME,
                }),
                policy: ConflictPolicy::Smart,
            },
            &context,
        )
        .await
        .unwrap();
    parity(&app, plan.id, b).await;
    let trashed = app
        .state
        .fsops
        .trash_file(
            apm_server::fsops::TrashRequest {
                file_id: Some(c),
                loc: Loc::new(b, RelPath::new("moved/one.jpg").unwrap()),
                expect: Some(Expect {
                    size: 6,
                    mtime: common::BASE_MTIME,
                }),
                batch: context.batch().clone(),
                reason: "test".to_owned(),
            },
            &context,
        )
        .await
        .unwrap();
    parity(&app, plan.id, b).await;
    app.state
        .fsops
        .restore(trashed.trash_item_id, ConflictPolicy::Smart, &context)
        .await
        .unwrap();
    let flags = app
        .state
        .plans
        .item_flags(
            PlanRef {
                plan_id: plan.id,
                side: PlanSide::Comparison,
            },
            &[c],
        )
        .await
        .unwrap();
    assert!(
        flags.is_empty(),
        "a reused restore id cannot resurrect an old snapshot membership"
    );
    tb.mkdir("empty");
    app.state
        .fsops
        .remove_dir_if_removable(
            Loc::new(b, RelPath::new("empty").unwrap()),
            context.batch(),
            &context,
        )
        .await
        .unwrap();
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplicate_matches WHERE plan_id = ? AND side = 'comparison' AND live = 1").bind(plan.id).fetch_one(app.state.db.read()).await.unwrap();
    assert_eq!(live, 0);
    app.state.begin_shutdown();
}

#[test]
fn production_never_replaces_files_rows() {
    fn check(dir: &std::path::Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check(&path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).unwrap().to_ascii_lowercase();
                assert!(
                    !text.contains("insert or replace into files"),
                    "{}",
                    path.display()
                );
            }
        }
    }
    check(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}

#[tokio::test]
async fn deletion_before_membership_bulk_write_is_also_tombstoned() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let v = seed.volume("v").await;
    let file = seed.file(v, "p0", "one.jpg").insert().await;
    let plan: i64 = sqlx::query_scalar("INSERT INTO plans(kind,state,params_json,created_at,expires_at) VALUES ('duplicates','building','{}',0,9999999999) RETURNING id")
        .fetch_one(app.state.db.write()).await.unwrap();
    sqlx::query("INSERT INTO plan_file_tracking(plan_id) VALUES (?)")
        .bind(plan)
        .execute(app.state.db.write())
        .await
        .unwrap();
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(file)
        .execute(app.state.db.write())
        .await
        .unwrap();
    sqlx::query("INSERT INTO files(id,volume_id,dir_path,name,name_norm,stem_norm,kind,size,mtime) VALUES (?,?,'p0','one.jpg','one.jpg','one','image',1000,1700000000)")
        .bind(file).bind(v).execute(app.state.db.write()).await.unwrap();
    sqlx::query("INSERT INTO duplicate_matches(plan_id,side,file_id,volume_id,dir_path) VALUES (?,'comparison',?,?,'p0')")
        .bind(plan).bind(file).bind(v).execute(app.state.db.write()).await.unwrap();
    let live: i64 = sqlx::query_scalar("SELECT live FROM duplicate_matches WHERE plan_id = ?")
        .bind(plan)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(
        live, 0,
        "bulk insertion cannot revive an id deleted while the plan was building"
    );
    app.state.begin_shutdown();
}
