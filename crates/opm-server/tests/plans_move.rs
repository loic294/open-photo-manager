//! Move plans over disposable indexed volumes; no real photo library is touched.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::seed::Seed;
use opm_core::dto::*;
use opm_core::hash::sha1_bytes;
use opm_core::paths::RelPath;
use opm_server::plans::overlay::{GhostView, PlanOverlay, Window};
use serde_json::json;

fn side(volume: i64, dir: &str) -> SideParams {
    SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: dir.into(),
            recursive: true,
        },
        filter: Filter::default(),
    }
}
fn params(kind: MoveType, src: i64, dst: i64) -> MoveParams {
    MoveParams {
        move_type: kind,
        source: side(src, "src"),
        destination: FolderRef {
            volume_id: dst,
            dir: "out".into(),
        },
        pattern: (kind == MoveType::Custom).then(|| "{YYYY}/{mm}".into()),
        conflict_policy: None,
        match_mode: None,
        strict_prefix: false,
    }
}
async fn ready(app: &TestApp) {
    sqlx::query("UPDATE files SET meta_state = 1")
        .execute(app.state.db.write())
        .await
        .unwrap();
}
async fn build(app: &TestApp, params: &MoveParams) -> PlanDto {
    let (status, body) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({"kind":"move", "params":params, "replaces":null})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_value(body).unwrap()
}
async fn names(app: &TestApp, plan: i64) -> Vec<(i64, String, String, String)> {
    sqlx::query_as("SELECT file_id, dst_dir, dst_name, status FROM move_items WHERE plan_id = ? ORDER BY src_dir, src_name")
        .bind(plan).fetch_all(app.state.db.read()).await.unwrap()
}
async fn run(app: &TestApp, id: i64, only: Option<i64>) -> (PlanResult, TaskDto) {
    let (status, body) = app
        .call(
            Method::POST,
            &format!("/api/plans/{id}/execute"),
            Some(json!({"options":{}, "only":only})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = body["id"].as_i64().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let task = app.state.tasks.get(task).await.unwrap();
            if matches!(
                task.state,
                TaskState::Done | TaskState::Failed | TaskState::Cancelled
            ) {
                return (app.state.plans.get(id).await.unwrap().result.unwrap(), task);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn immich_guard_refusal_is_a_coded_skip_before_replacement_and_untracked_moves_continue() {
    use futures_util::future::BoxFuture;
    use opm_server::fsops::{FsOpError, Loc, MoveGuard};
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::Arc;

    struct RefuseTracked;
    impl MoveGuard for RefuseTracked {
        fn check<'a>(
            &'a self,
            src: &'a Loc,
            dst: &'a Loc,
            sidecars: &'a [(Loc, Loc)],
        ) -> BoxFuture<'a, Result<(), FsOpError>> {
            Box::pin(async move {
                if src.rel.as_str() == "src/a.jpg" {
                    assert_eq!(dst.rel.as_str(), "out/a.jpg");
                    assert_eq!(sidecars.len(), 1);
                    assert_eq!(sidecars[0].1.rel.as_str(), "out/a.jpg.xmp");
                    return Err(FsOpError::ImmichMoveRefused(
                        "No file was moved. Restore the Immich database connection, then rebuild and retry.".into(),
                    ));
                }
                Ok(())
            })
        }
    }
    let mut app = TestApp::new().await;
    let fsops = app
        .state
        .fsops
        .clone()
        .with_move_guard(Arc::new(RefuseTracked));
    app.state.plans = PlanService::new(PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    })
    .await
    .unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/a.jpg", b"photo");
    tree.write("src/a.jpg.xmp", b"metadata");
    tree.write("src/b.jpg", b"other");
    tree.write("out/a.jpg", b"incumbent");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "a.jpg").size(5).insert().await;
    let s = seed.file(v, "src", "a.jpg.xmp").size(8).insert().await;
    seed.sidecar(a, s, "exact").await;
    seed.file(v, "src", "b.jpg").size(5).insert().await;
    seed.file(v, "out", "a.jpg").size(9).insert().await;
    ready(&app).await;
    let mut p = params(MoveType::Flat, v, v);
    p.conflict_policy = Some(ConflictPolicy::Replace);
    let plan = build(&app, &p).await;
    let (result, _) = run(&app, plan.id, None).await;
    assert_eq!((result.done, result.skipped, result.failed), (1, 1, 0));
    assert_eq!(result.failures[0].code, "immich_move_refused");
    assert!(result.failures[0].message.starts_with("No file was moved."));
    assert!(result.failures[0].message.contains("retry"));
    assert_eq!(std::fs::read(tree.path("out/a.jpg")).unwrap(), b"incumbent");
    assert_eq!(std::fs::read(tree.path("src/a.jpg")).unwrap(), b"photo");
    assert_eq!(
        std::fs::read(tree.path("src/a.jpg.xmp")).unwrap(),
        b"metadata"
    );
    assert_eq!(std::fs::read(tree.path("out/b.jpg")).unwrap(), b"other");
    let trash: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(trash, 0);
}

#[tokio::test]
async fn failed_shared_sidecar_copy_is_attention_with_the_actual_metadata_path() {
    use opm_server::fsops::{Mover, SystemMover};
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DenyFirstMetadata(AtomicBool);
    impl Mover for DenyFirstMetadata {
        fn rename_no_replace(
            &self,
            from: &std::path::Path,
            to: &std::path::Path,
        ) -> std::io::Result<()> {
            if to.file_name().is_some_and(|name| name == "one.xmp")
                && !self.0.swap(true, Ordering::SeqCst)
            {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            SystemMover.rename_no_replace(from, to)
        }
    }
    let mut app = TestApp::new().await;
    let fsops = app
        .state
        .fsops
        .clone()
        .with_mover(Arc::new(DenyFirstMetadata(AtomicBool::new(false))));
    app.state.plans = PlanService::new(PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    })
    .await
    .unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/one.CR3", b"photo");
    tree.write("src/one.JPG", b"jpeg");
    tree.write("src/one.xmp", b"metadata");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "one.CR3").size(5).insert().await;
    let b = seed.file(v, "src", "one.JPG").size(4).insert().await;
    let s = seed.file(v, "src", "one.xmp").size(8).insert().await;
    seed.sidecar(a, s, "stem").await;
    seed.sidecar(b, s, "stem").await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    let (result, _) = run(&app, plan.id, None).await;
    assert_eq!((result.done, result.failed), (2, 1), "{result:?}");
    assert_eq!(result.failures[0].file_id, Some(s));
    assert!(result.failures[0].path.ends_with("/src/one.xmp"));
    assert_eq!(result.failures[0].code, "io_error");
    assert!(tree.path("out/one.CR3").exists());
    assert!(tree.path("out/one.JPG").exists());
    assert_eq!(
        std::fs::read(tree.path("src/one.xmp")).unwrap(),
        b"metadata"
    );
    assert_eq!(
        std::fs::read(tree.path("out/one.xmp")).unwrap(),
        b"metadata"
    );
}

#[tokio::test]
async fn forced_cross_device_move_preserves_metadata_and_counts_copied_bytes_once() {
    use common::fsops::SplitMover;
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::Arc;

    let mut app = TestApp::new().await;
    let (src, source) = app.add_volume("source").await;
    let (dst, destination) = app.add_volume("destination").await;
    let fsops = app
        .state
        .fsops
        .clone()
        .with_mover(Arc::new(SplitMover::new(&[&source, &destination])));
    app.state.plans = PlanService::new(PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    })
    .await
    .unwrap();
    app.router = opm_server::app::router(app.state.clone());
    source.write("src/one.jpg", b"photo");
    source.write("src/one.jpg.xmp", b"metadata");
    let seed = Seed::new(&app.state.db);
    let owner = seed.file(src, "src", "one.jpg").size(5).insert().await;
    let metadata = seed.file(src, "src", "one.jpg.xmp").size(8).insert().await;
    seed.sidecar(owner, metadata, "exact").await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, src, dst)).await;
    let (result, task) = run(&app, plan.id, None).await;
    assert_eq!((result.done, result.failed, result.skipped), (1, 0, 0));
    assert_eq!(task.bytes_done, 13);
    assert_eq!(task.items_done, 1);
    assert!(!source.path("src/one.jpg").exists());
    assert!(!source.path("src/one.jpg.xmp").exists());
    assert_eq!(
        std::fs::read(destination.path("out/one.jpg")).unwrap(),
        b"photo"
    );
    assert_eq!(
        std::fs::read(destination.path("out/one.jpg.xmp")).unwrap(),
        b"metadata"
    );
    let links: Vec<(i64, i64)> = sqlx::query_as("SELECT file_id, sidecar_id FROM file_sidecars")
        .fetch_all(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(links, [(owner, metadata)]);
}

#[tokio::test]
async fn execute_uses_the_build_time_default_conflict_policy() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/one.jpg", b"photo");
    tree.write("out/one.jpg", b"other");
    let seed = Seed::new(&app.state.db);
    seed.file(v, "src", "one.jpg")
        .size(5)
        .hash(sha1_bytes(b"photo"))
        .insert()
        .await;
    seed.file(v, "out", "one.jpg")
        .size(5)
        .hash(sha1_bytes(b"other"))
        .insert()
        .await;
    ready(&app).await;
    let mut settings = (*app.state.settings.get()).clone();
    settings.default_conflict_policy = ConflictPolicy::Rename;
    app.state.settings.update(settings).await.unwrap();
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    assert_eq!(names(&app, plan.id).await[0].2, "one_1.jpg");
    let mut settings = (*app.state.settings.get()).clone();
    settings.default_conflict_policy = ConflictPolicy::Replace;
    app.state.settings.update(settings).await.unwrap();
    let (result, _) = run(&app, plan.id, None).await;
    assert_eq!((result.done, result.failed), (1, 0));
    assert_eq!(std::fs::read(tree.path("out/one.jpg")).unwrap(), b"other");
    assert_eq!(std::fs::read(tree.path("out/one_1.jpg")).unwrap(), b"photo");
    let trash: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(trash, 0);
}

#[tokio::test]
async fn overflow_removes_move_rows_and_shared_identity_registration() {
    use opm_server::plans::{PlanDeps, PlanOptions, PlanService};
    use std::sync::Arc;

    let mut app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        seed.file(v, "src", name).insert().await;
    }
    ready(&app).await;
    let deps = PlanDeps {
        db: app.state.db.clone(),
        fsops: app.state.fsops.clone(),
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    };
    let planner = Arc::new(opm_server::plans::moves::MovePlanner::new(&deps));
    app.state.plans = PlanService::with_options(
        deps,
        vec![planner],
        PlanOptions {
            max_items: 2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let error = app
        .state
        .plans
        .create(CreatePlanRequest {
            kind: PlanKind::Move,
            params: json!(params(MoveType::Flat, v, v)),
            replaces: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), "plan_too_large");
    app.state.plans.settle().await;
    for sql in [
        "SELECT COUNT(*) FROM move_items",
        "SELECT COUNT(*) FROM move_plans",
        "SELECT COUNT(*) FROM plan_file_tracking",
        "SELECT COUNT(*) FROM plan_file_identities",
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
        assert_eq!(count, 0, "{sql}");
    }
}

#[tokio::test]
async fn build_types_kinds_pending_and_cross_volume_stats() {
    let app = TestApp::new().await;
    let (src, _) = app.add_volume("source").await;
    let (dst, _) = app.add_volume("dest").await;
    let seed = Seed::new(&app.state.db);
    let image = seed
        .file(src, "src/event", "one.jpg")
        .capture(1_791_205_200)
        .size(10)
        .insert()
        .await;
    seed.file(src, "src/event", "notes.txt")
        .capture(1_791_205_200)
        .size(20)
        .insert()
        .await;
    seed.file(src, "src/event", "one.xmp").insert().await;
    ready(&app).await;
    let pending = seed.file(src, "src", "pending.jpg").insert().await;
    for (kind, dir) in [
        (MoveType::KeepSource, "out/event"),
        (MoveType::Flat, "out"),
        (MoveType::Custom, "out/2026/10"),
        (MoveType::KeepDestination, "out"),
    ] {
        let plan = build(&app, &params(kind, src, dst)).await;
        assert_eq!(plan.stats["candidates"], 3);
        assert_eq!(plan.stats["to_move"], 2);
        assert_eq!(plan.stats["bytes_to_move"], 30);
        assert_eq!(plan.stats["cross_volume_bytes"], 30);
        assert_eq!(plan.stats["metadata_pending"], 1);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.code == "cross_volume" && w.count == 2)
        );
        let rows = names(&app, plan.id).await;
        assert!(
            rows.iter()
                .any(|r| r.0 == image && r.1 == dir && r.3 == "move"),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.0 == pending && r.3 == "metadata_pending")
        );
    }
}

#[tokio::test]
async fn policy_matrix_exact_ghost_names_and_empty_folder_collisions() {
    for (policy, identical, expected, status) in [
        (
            ConflictPolicy::Smart,
            true,
            "one.jpg",
            "identical_at_destination",
        ),
        (ConflictPolicy::Smart, false, "one_1.jpg", "move"),
        (ConflictPolicy::Skip, false, "one.jpg", "skipped_by_policy"),
        (ConflictPolicy::Rename, true, "one_1.jpg", "move"),
        (ConflictPolicy::Replace, false, "one.jpg", "move"),
    ] {
        let app = TestApp::new().await;
        let (volume, _) = app.add_volume("photos").await;
        let seed = Seed::new(&app.state.db);
        seed.file(volume, "src", "one.jpg")
            .hash(sha1_bytes(b"a"))
            .insert()
            .await;
        let target = seed
            .file(volume, "out", "one.jpg")
            .hash(sha1_bytes(if identical { b"a" } else { b"b" }))
            .insert()
            .await;
        ready(&app).await;
        let mut p = params(MoveType::Flat, volume, volume);
        p.conflict_policy = Some(policy);
        let plan = build(&app, &p).await;
        assert_eq!(names(&app, plan.id).await[0].2, expected);
        assert_eq!(names(&app, plan.id).await[0].3, status);
        let view = GhostView {
            volume_id: volume,
            dir: RelPath::new("out").unwrap(),
            filter: Filter::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        let ghosts = app
            .state
            .plans
            .ghost_items(
                PlanRef {
                    plan_id: plan.id,
                    side: PlanSide::Comparison,
                },
                &view,
                Window { skip: 0, take: 20 },
            )
            .await
            .unwrap();
        assert_eq!(ghosts.total, i64::from(status == "move"));
        if status == "move" {
            assert_eq!(ghosts.items[0].name, expected);
            assert!(ghosts.items[0].overlay.planned);
        }
        let flags = app
            .state
            .plans
            .item_flags(
                PlanRef {
                    plan_id: plan.id,
                    side: PlanSide::Comparison,
                },
                &[target],
            )
            .await
            .unwrap();
        assert_eq!(
            flags.get(&target).is_some_and(|f| f.will_delete),
            policy == ConflictPolicy::Replace
        );
    }
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(v, "src/a", "ONE.jpg").insert().await;
    seed.file(v, "src/b", "one.jpg").insert().await;
    seed.dir(v, "out/one.jpg").await;
    seed.dir(v, "out/one_1.jpg").await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    let rows = names(&app, plan.id).await;
    assert_eq!(rows[0].2, "ONE_2.jpg");
    assert_eq!(rows[1].2, "one_3.jpg");
}

#[tokio::test]
async fn replace_never_trashes_a_source_or_reuses_a_freed_name() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(v, "src", "one.jpg").insert().await;
    seed.file(v, "src/a", "one.jpg").insert().await;
    ready(&app).await;
    let mut p = params(MoveType::Flat, v, v);
    p.destination.dir = "src".into();
    p.conflict_policy = Some(ConflictPolicy::Replace);
    let plan = build(&app, &p).await;
    let rows = names(&app, plan.id).await;
    assert_eq!(rows[0].3, "already_in_place");
    assert_eq!(rows[1].2, "one_1.jpg");
    assert_eq!(plan.stats["replaced"], 0);
}

#[tokio::test]
async fn normalized_unicode_collisions_preserve_byte_exact_source_and_avoid_arbitrary_replace() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    let source = seed.file(v, "src", "cafe\u{301}.jpg").insert().await;
    let a = seed.file(v, "out", "caf\u{e9}.jpg").insert().await;
    let b = seed.file(v, "out", "CAF\u{c9}.jpg").insert().await;
    ready(&app).await;
    let mut p = params(MoveType::Flat, v, v);
    p.conflict_policy = Some(ConflictPolicy::Replace);
    let plan = build(&app, &p).await;
    let rows = names(&app, plan.id).await;
    assert_eq!(rows[0].0, source);
    assert_eq!(rows[0].2, "cafe\u{301}_1.jpg");
    assert_eq!(rows[0].3, "move");
    assert_eq!(plan.stats["replaced"], 0);
    assert_eq!(plan.stats["renamed"], 1);
    let stored: String = sqlx::query_scalar("SELECT src_name FROM move_items WHERE plan_id = ?")
        .bind(plan.id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(stored, "cafe\u{301}.jpg");
    let flags = app
        .state
        .plans
        .item_flags(
            PlanRef {
                plan_id: plan.id,
                side: PlanSide::Comparison,
            },
            &[a, b],
        )
        .await
        .unwrap();
    assert!(flags.is_empty());
}

#[tokio::test]
async fn keep_destination_reuses_unoriented_matches_first_path_and_reports_missing() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    let owner = seed
        .file(v, "src", "one.jpg")
        .hash(sha1_bytes(b"a"))
        .insert()
        .await;
    seed.file(v, "src", "none.jpg").insert().await;
    let first = seed
        .file(v, "out/a", "other.jpg")
        .hash(sha1_bytes(b"a"))
        .insert()
        .await;
    seed.file(v, "out/z", "another.jpg")
        .hash(sha1_bytes(b"a"))
        .insert()
        .await;
    seed.file(v, "out.old", "ignore.jpg")
        .hash(sha1_bytes(b"a"))
        .insert()
        .await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::KeepDestination, v, v)).await;
    let rows = names(&app, plan.id).await;
    assert!(rows.iter().any(|r| r.0 == owner && r.1 == "out/a"));
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.code == "ambiguous_counterpart" && w.count == 1)
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.code == "no_counterpart" && w.count == 1)
    );
    let flags = app
        .state
        .plans
        .item_flags(
            PlanRef {
                plan_id: plan.id,
                side: PlanSide::Comparison,
            },
            &[first],
        )
        .await
        .unwrap();
    assert!(flags[&first].matched);
    // Both source and destination scopes include the same files; the higher-id source must
    // still receive the lower-id counterpart (Unoriented, not duplicate deletion orientation).
    let mut p = params(MoveType::KeepDestination, v, v);
    p.source = side(v, "out");
    let overlapping = build(&app, &p).await;
    assert_eq!(overlapping.stats["candidates"], 2);
    assert!(
        !overlapping
            .warnings
            .iter()
            .any(|w| w.code == "no_counterpart")
    );
}

#[tokio::test]
async fn overlay_filters_windows_nested_folders_and_handoff() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    let a = seed
        .file(v, "src/2019/Rome", "a.jpg")
        .rating(4)
        .insert()
        .await;
    let b = seed
        .file(v, "src/2019/Rome", "b.jpg")
        .rating(1)
        .insert()
        .await;
    seed.file(v, "src/2019.old", "sibling.jpg")
        .rating(4)
        .insert()
        .await;
    seed.dir(v, "out/2019").await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::KeepSource, v, v)).await;
    let mut view = GhostView {
        volume_id: v,
        dir: RelPath::new("out/2019").unwrap(),
        filter: Filter {
            ratings: vec![4],
            ..Filter::default()
        },
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let ref_ = PlanRef {
        plan_id: plan.id,
        side: PlanSide::Comparison,
    };
    let folders = app
        .state
        .plans
        .folder_overlay(ref_, &view, &[])
        .await
        .unwrap();
    assert!(folders.dir_planned);
    assert_eq!(folders.ghost_folders[0].path, "out/2019/Rome");
    assert_eq!(
        (
            folders.ghost_folders[0].count,
            folders.ghost_folders[0].total
        ),
        (1, 2)
    );
    assert_eq!(folders.ghosts.in_subfolders, 1);
    view.dir = RelPath::new("out/2019/Rome").unwrap();
    let ghosts = app
        .state
        .plans
        .ghost_items(ref_, &view, Window { skip: 0, take: 1 })
        .await
        .unwrap();
    assert_eq!(ghosts.total, 1);
    assert_eq!(ghosts.items[0].id, a);
    assert_eq!(ghosts.items[0].dir, "out/2019/Rome");
    let empty = app
        .state
        .plans
        .ghost_items(ref_, &view, Window { skip: 1, take: 1 })
        .await
        .unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.total, 1);
    let source = PlanRef {
        plan_id: plan.id,
        side: PlanSide::Source,
    };
    assert!(
        app.state
            .plans
            .ghost_items(source, &view, Window { skip: 0, take: 1 })
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let flags = app.state.plans.item_flags(source, &[a, b]).await.unwrap();
    assert!(flags[&a].matched && flags[&b].matched);
    let (status, selection) = app
        .call(
            Method::POST,
            &format!("/api/plans/{}/selection", plan.id),
            Some(json!({"side":"source"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(selection["item_count"], 3);
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(a)
        .execute(app.state.db.write())
        .await
        .unwrap();
    assert_eq!(
        app.state
            .plans
            .ghost_items(ref_, &view, Window { skip: 0, take: 1 })
            .await
            .unwrap()
            .total,
        0
    );
}

#[tokio::test]
async fn protected_source_and_destination_components_ignore_junk_settings_and_not_prefixes() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(v, "src/#SNAPSHOT", "protected.jpg")
        .insert()
        .await;
    seed.file(v, "src/#snapshot.old", "safe.jpg").insert().await;
    ready(&app).await;
    let mut settings = (*app.state.settings.get()).clone();
    settings.junk_names.clear();
    settings.junk_prefixes.clear();
    app.state.settings.update(settings).await.unwrap();
    for source in [false, true] {
        let mut p = params(MoveType::Flat, v, v);
        if source {
            p.source.scope.dir = "src/#ReCyClE".into();
        } else {
            p.destination.dir = "out/#SNAPSHOT/child".into();
        }
        let (status, _) = app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({"kind":"move", "params":p, "replaces":null})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let plan = build(&app, &params(MoveType::KeepSource, v, v)).await;
    assert_eq!(plan.stats["reserved_destination"], 1);
    assert_eq!(plan.stats["to_move"], 1);
    assert!(
        names(&app, plan.id)
            .await
            .iter()
            .any(|r| r.1 == "out/#snapshot.old" && r.3 == "move")
    );
    let mut custom = params(MoveType::Custom, v, v);
    custom.pattern = Some("#rEcYcLe/{filename}".into());
    let plan = build(&app, &custom).await;
    assert_eq!(plan.stats["reserved_destination"], 2);
    assert_eq!(plan.stats["to_move"], 0);
}

#[tokio::test]
async fn validation_and_reserved_generated_destinations() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(v, "src", "one.jpg").insert().await;
    ready(&app).await;
    for (field, mutate) in [
        ("source.scope", 0),
        ("pattern", 1),
        ("pattern", 2),
        ("match_mode", 3),
        ("destination", 4),
        ("destination", 5),
    ] {
        let mut p = params(MoveType::Flat, v, v);
        match mutate {
            0 => p.source.scope.volume_id = None,
            1 => p.pattern = Some("{YYYY}".into()),
            2 => {
                p.move_type = MoveType::Custom;
                p.pattern = Some("{yyyy}".into());
            }
            3 => p.match_mode = Some(MatchMode::Hash),
            4 => p.destination.dir = ".opm-trash".into(),
            _ => p.destination.volume_id = 98765,
        }
        let (status, body) = app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({"kind":"move","params":p,"replaces":null})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(field),
            "{body}"
        );
    }
    let mut p = params(MoveType::Custom, v, v);
    p.pattern = Some(".opm-trash/{filename}".into());
    let plan = build(&app, &p).await;
    assert_eq!(plan.stats["reserved_destination"], 1);
    assert_eq!(plan.stats["to_move"], 0);
}

#[tokio::test]
async fn executes_only_selection_preserves_ids_and_is_idempotent() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/a.jpg", b"photo");
    tree.write("src/b.jpg", b"photo");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "a.jpg").size(5).insert().await;
    let b = seed.file(v, "src", "b.jpg").size(5).insert().await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    let only = seed.selection(&[a]).await;
    let (result, task) = run(&app, plan.id, Some(only)).await;
    assert_eq!(
        (result.done, result.failed, result.skipped, result.bytes),
        (1, 0, 0, 5),
        "{result:?}"
    );
    assert_eq!(task.bytes_done, 5);
    assert!(!tree.path("src/a.jpg").exists());
    assert!(tree.path("src/b.jpg").exists());
    assert_eq!(std::fs::read(tree.path("out/a.jpg")).unwrap(), b"photo");
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM files ORDER BY id")
        .fetch_all(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(ids, [a, b]);
    let mut p = params(MoveType::Flat, v, v);
    p.source = side(v, "out");
    let replan = build(&app, &p).await;
    assert_eq!(replan.stats["already_in_place"], 1);
    assert_eq!(replan.stats["to_move"], 0);
}

#[tokio::test]
async fn stale_source_matrix_and_failure_continuation() {
    for change in [
        "gone",
        "index_path",
        "index_size",
        "capture",
        "disk_size",
        "disk_gone",
        "reused_id",
    ] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        tree.write("src/a.jpg", b"photo");
        tree.write("src/z.jpg", b"photo");
        let seed = Seed::new(&app.state.db);
        let good = seed
            .file(v, "src", "a.jpg")
            .size(5)
            .capture(1_791_205_200)
            .insert()
            .await;
        let bad = seed
            .file(v, "src", "z.jpg")
            .size(5)
            .capture(1_791_205_200)
            .insert()
            .await;
        ready(&app).await;
        let plan = build(&app, &params(MoveType::Custom, v, v)).await;
        match change {
            "gone" => {
                sqlx::query("DELETE FROM files WHERE id = ?")
                    .bind(bad)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "reused_id" => {
                sqlx::query("DELETE FROM files WHERE id = ?")
                    .bind(bad)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
                let replacement = seed
                    .file(v, "src", "new.jpg")
                    .size(5)
                    .capture(1_791_205_200)
                    .insert()
                    .await;
                assert_eq!(replacement, bad);
                tree.write("src/new.jpg", b"other");
                ready(&app).await;
            }
            "index_path" => {
                sqlx::query("UPDATE files SET dir_path = 'other' WHERE id = ?")
                    .bind(bad)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "index_size" => {
                sqlx::query("UPDATE files SET size = 9 WHERE id = ?")
                    .bind(bad)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "capture" => {
                sqlx::query("UPDATE files SET capture_ts = 0 WHERE id = ?")
                    .bind(bad)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "disk_size" => tree.write("src/z.jpg", b"longer photo"),
            _ => tree.remove("src/z.jpg"),
        }
        let (result, _) = run(&app, plan.id, None).await;
        assert_eq!(result.done, 1, "{change}: {result:?}");
        assert_eq!(result.failures.len(), 1, "{change}: {result:?}");
        assert_eq!(result.failures[0].file_id, Some(bad));
        assert!(tree.path("out/2026/10/a.jpg").exists());
        assert!(!tree.path("out/2026/10/z.jpg").exists());
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT dir_path FROM files WHERE id = ?")
                .bind(good)
                .fetch_one(app.state.db.read())
                .await
                .unwrap(),
            "out/2026/10"
        );
        if change.starts_with("disk") {
            assert_eq!(result.failed, 1);
        } else {
            assert_eq!(result.skipped, 1);
        }
    }
}

#[tokio::test]
async fn smart_hashes_disk_on_demand_and_replace_trashes_one_batch() {
    for identical in [true, false] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        tree.write("src/one.jpg", b"photo");
        tree.write("out/one.jpg", if identical { b"photo" } else { b"other" });
        let seed = Seed::new(&app.state.db);
        seed.file(v, "src", "one.jpg").size(5).insert().await;
        seed.file(v, "out", "one.jpg").size(5).insert().await;
        ready(&app).await;
        let plan = build(&app, &params(MoveType::Flat, v, v)).await;
        assert_eq!(plan.stats["identical_at_destination"], u64::from(identical));
        if identical {
            let (r, _) = run(&app, plan.id, None).await;
            assert_eq!(r.done, 0);
            assert!(tree.path("src/one.jpg").exists());
        } else {
            let mut p = params(MoveType::Flat, v, v);
            p.conflict_policy = Some(ConflictPolicy::Replace);
            let plan = build(&app, &p).await;
            let (r, _) = run(&app, plan.id, None).await;
            assert_eq!((r.done, r.failed), (1, 0), "{r:?}");
            assert_eq!(std::fs::read(tree.path("out/one.jpg")).unwrap(), b"photo");
            let batches: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT batch) FROM trash_items")
                .fetch_one(app.state.db.read())
                .await
                .unwrap();
            assert_eq!(batches, 1);
        }
    }
}

#[tokio::test]
async fn sidecars_exact_shared_carriers_selection_and_failed_owner() {
    for case in [
        "exact",
        "one_dir",
        "different_dirs",
        "some_stay",
        "owner_fails",
        "final_fails",
        "cr3_renamed",
    ] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        let exact = case == "exact";
        tree.write("src/one.CR3", b"photo");
        tree.write("src/one.JPG", b"jpeg");
        let sc = if exact { "one.CR3.xmp" } else { "one.xmp" };
        tree.write(&format!("src/{sc}"), b"metadata");
        let seed = Seed::new(&app.state.db);
        let a = seed.file(v, "src", "one.CR3").size(5).insert().await;
        let b = seed.file(v, "src", "one.JPG").size(4).insert().await;
        let s = seed.file(v, "src", sc).size(8).insert().await;
        seed.sidecar(a, s, if exact { "exact" } else { "stem" })
            .await;
        if !exact {
            seed.sidecar(b, s, "stem").await;
        }
        ready(&app).await;
        let mut p = params(MoveType::Flat, v, v);
        if case == "different_dirs" {
            p.move_type = MoveType::Custom;
            p.pattern = Some("{ext}".into());
        }
        if case == "cr3_renamed" {
            tree.write("out/one.CR3", b"old");
            seed.file(v, "out", "one.CR3").size(3).insert().await;
            ready(&app).await;
        }
        let plan = build(&app, &p).await;
        let only = if case == "some_stay" {
            Some(seed.selection(&[a]).await)
        } else {
            None
        };
        if case == "owner_fails" {
            tree.write("src/one.CR3", b"changed photo");
        }
        if case == "final_fails" {
            tree.write("src/one.JPG", b"changed jpeg");
        }
        let (result, _) = run(&app, plan.id, only).await;
        assert_eq!(
            result.failed,
            u64::from(matches!(case, "owner_fails" | "final_fails")),
            "{case}: {result:?}"
        );
        match case {
            "different_dirs" => {
                assert_eq!(
                    std::fs::read(tree.path("out/CR3/one.xmp")).unwrap(),
                    b"metadata"
                );
                assert_eq!(
                    std::fs::read(tree.path("out/JPG/one.xmp")).unwrap(),
                    b"metadata"
                );
                assert!(!tree.path("src/one.xmp").exists());
                let original: String =
                    sqlx::query_scalar("SELECT dir_path FROM files WHERE id = ?")
                        .bind(s)
                        .fetch_one(app.state.db.read())
                        .await
                        .unwrap();
                assert_eq!(original, "out/JPG");
            }
            "some_stay" | "owner_fails" | "final_fails" | "one_dir" => {
                assert_eq!(
                    std::fs::read(tree.path("src/one.xmp")).unwrap(),
                    b"metadata"
                );
                if case == "one_dir" {
                    assert_eq!(result.skipped, 0);
                    assert!(
                        result
                            .note
                            .as_deref()
                            .unwrap()
                            .contains("shared_sidecar_retained")
                    );
                    let copies: i64 =
                        sqlx::query_scalar("SELECT COUNT(*) FROM fs_journal WHERE op = 'copy'")
                            .fetch_one(app.state.db.read())
                            .await
                            .unwrap();
                    assert_eq!(copies, 1);
                }
            }
            "cr3_renamed" => {
                assert_eq!(
                    std::fs::read(tree.path("out/one_1.xmp")).unwrap(),
                    b"metadata"
                );
                assert_eq!(
                    std::fs::read(tree.path("out/one.xmp")).unwrap(),
                    b"metadata"
                );
                assert!(!tree.path("src/one.xmp").exists());
                assert_eq!(
                    std::fs::read(tree.path("out/one.xmp")).unwrap(),
                    b"metadata"
                );
            }
            _ => {
                assert_eq!(
                    std::fs::read(tree.path(&format!("out/{sc}"))).unwrap(),
                    b"metadata"
                );
                assert!(!tree.path(&format!("src/{sc}")).exists());
            }
        }
        let owners_without_sidecar: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_sidecars l JOIN files f ON f.id = l.file_id JOIN files s ON s.id = l.sidecar_id WHERE f.dir_path <> s.dir_path OR f.volume_id <> s.volume_id")
                    .fetch_one(app.state.db.read()).await.unwrap();
        assert_eq!(owners_without_sidecar, 0);
    }
}

#[tokio::test]
async fn sidecar_collisions_number_owner_in_preview_and_on_disk() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/one.CR3", b"photo");
    tree.write("src/one.CR3.xmp", b"metadata");
    tree.write("out/one.CR3", b"old");
    tree.write("out/one_1.CR3.xmp", b"occupied");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "one.CR3").size(5).insert().await;
    let s = seed.file(v, "src", "one.CR3.xmp").size(8).insert().await;
    seed.sidecar(a, s, "exact").await;
    seed.file(v, "out", "one.CR3").size(3).insert().await;
    seed.file(v, "out", "one_1.CR3.xmp").size(8).insert().await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    assert_eq!(names(&app, plan.id).await[0].2, "one_2.CR3");
    let (result, _) = run(&app, plan.id, None).await;
    assert_eq!((result.done, result.failed), (1, 0), "{result:?}");
    assert_eq!(std::fs::read(tree.path("out/one_2.CR3")).unwrap(), b"photo");
    assert_eq!(
        std::fs::read(tree.path("out/one_2.CR3.xmp")).unwrap(),
        b"metadata"
    );
    assert_eq!(
        std::fs::read(tree.path("out/one_1.CR3.xmp")).unwrap(),
        b"occupied"
    );
}

#[tokio::test]
async fn live_counterpart_revalidation_refuses_gone_or_changed_match() {
    for change in ["gone", "hash", "dir"] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        tree.write("src/one.jpg", b"photo");
        let seed = Seed::new(&app.state.db);
        seed.file(v, "src", "one.jpg")
            .size(5)
            .hash(sha1_bytes(b"photo"))
            .insert()
            .await;
        let c = seed
            .file(v, "out/a", "counterpart.jpg")
            .size(5)
            .hash(sha1_bytes(b"photo"))
            .insert()
            .await;
        ready(&app).await;
        let plan = build(&app, &params(MoveType::KeepDestination, v, v)).await;
        let statement = match change {
            "gone" => "DELETE FROM files WHERE id = ?",
            "hash" => "UPDATE files SET hash = NULL WHERE id = ?",
            _ => "UPDATE files SET dir_path = 'out/b' WHERE id = ?",
        };
        sqlx::query(statement)
            .bind(c)
            .execute(app.state.db.write())
            .await
            .unwrap();
        let (result, _) = run(&app, plan.id, None).await;
        assert_eq!(
            (result.done, result.skipped, result.failed),
            (0, 1, 0),
            "{result:?}"
        );
        assert_eq!(
            result.failures[0].code,
            if change == "dir" {
                "stale_plan"
            } else {
                "stale_match"
            }
        );
        assert!(tree.path("src/one.jpg").exists());
    }
}

#[tokio::test]
async fn cancellation_during_final_copy_matches_terminal_task_and_preserves_completed_work() {
    use opm_server::fsops::{FailAction, Failpoints, Step};
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    for completed in [0, 1] {
        let mut app = TestApp::new().await;
        let (src, source) = app.add_volume("source").await;
        let (dst, destination) = app.add_volume("destination").await;
        if completed == 1 {
            source.write("src/a.jpg", b"first");
        }
        source.write("src/z.jpg", b"last");
        let seed = Seed::new(&app.state.db);
        if completed == 1 {
            seed.file(src, "src", "a.jpg").size(5).insert().await;
        }
        seed.file(src, "src", "z.jpg").size(4).insert().await;
        ready(&app).await;

        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let (release, blocked) = mpsc::channel();
        let blocked = Mutex::new(blocked);
        let copies = AtomicUsize::new(0);
        let points = Failpoints::new(move |step| {
            if step == Step::AfterPartCreate && copies.fetch_add(1, Ordering::SeqCst) == completed {
                notify.notify_one();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            FailAction::Continue
        });
        let fsops = app
            .state
            .fsops
            .clone()
            .with_mover(Arc::new(common::fsops::SplitMover::new(&[
                &source,
                &destination,
            ])))
            .with_failpoints(points);
        app.state.plans = PlanService::new(PlanDeps {
            db: app.state.db.clone(),
            fsops,
            settings: app.state.settings.clone(),
            shutdown: app.state.shutdown.clone(),
            tasks: app.state.tasks.clone(),
            volumes: app.state.volumes.clone(),
        })
        .await
        .unwrap();
        app.router = opm_server::app::router(app.state.clone());
        let plan = build(&app, &params(MoveType::Flat, src, dst)).await;
        let (status, body) = app
            .call(
                Method::POST,
                &format!("/api/plans/{}/execute", plan.id),
                Some(json!({"options":{}, "only":null})),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let id = body["id"].as_i64().unwrap();
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        assert_eq!(
            app.state.tasks.get(id).await.unwrap().state,
            TaskState::Running
        );
        assert!(app.state.plans.get(plan.id).await.unwrap().result.is_none());
        app.state.tasks.cancel(id).await.unwrap();
        release.send(()).unwrap();
        let task = tokio::time::timeout(Duration::from_secs(10), app.state.tasks.wait_finished(id))
            .await
            .unwrap()
            .unwrap();
        let terminal = app.state.plans.get(plan.id).await.unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
        assert_eq!(terminal.task_id, Some(id));
        let result = terminal.result.unwrap();
        assert!(result.cancelled, "{result:?}");
        assert_eq!(
            (result.done, result.bytes, result.failed, result.skipped),
            (completed as u64, completed as u64 * 5, 0, 0)
        );
        assert!(result.failures.is_empty());
        assert_eq!(std::fs::read(source.path("src/z.jpg")).unwrap(), b"last");
        assert!(!destination.path("out/z.jpg").exists());
        if completed == 1 {
            assert!(!source.path("src/a.jpg").exists());
            assert_eq!(
                std::fs::read(destination.path("out/a.jpg")).unwrap(),
                b"first"
            );
        }
        let journal: String = sqlx::query_scalar(
            "SELECT state FROM fs_journal WHERE task_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
        assert_eq!(journal, "failed");
        assert!(
            std::fs::read_dir(destination.path("out"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".opm-part"))
        );
    }
}

#[tokio::test]
async fn cancellation_after_first_move_keeps_partial_result_and_finishes_exact_sidecar() {
    use opm_server::fsops::{AppliedOp, OpHook};
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::Arc;
    let mut app = TestApp::new().await;
    struct CancelOnApply(opm_server::tasks::TaskEngine);
    impl OpHook for CancelOnApply {
        fn on_applied<'a>(
            &'a self,
            tx: &'a mut sqlx::SqliteConnection,
            _op: &'a AppliedOp,
        ) -> futures_util::future::BoxFuture<'a, Result<(), sqlx::Error>> {
            Box::pin(async move {
                let task: i64 =
                    sqlx::query_scalar("SELECT task_id FROM fs_journal ORDER BY id DESC LIMIT 1")
                        .fetch_one(tx)
                        .await?;
                self.0.cancel(task).await.unwrap();
                Ok(())
            })
        }
    }
    let fsops = app
        .state
        .fsops
        .clone()
        .with_hook(Arc::new(CancelOnApply(app.state.tasks.clone())));
    app.state.plans = PlanService::new(PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    })
    .await
    .unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/a.jpg", b"photo");
    tree.write("src/a.jpg.xmp", b"metadata");
    tree.write("src/b.jpg", b"other");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "a.jpg").size(5).insert().await;
    let s = seed.file(v, "src", "a.jpg.xmp").size(8).insert().await;
    seed.sidecar(a, s, "exact").await;
    seed.file(v, "src", "b.jpg").size(5).insert().await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    let (result, task) = run(&app, plan.id, None).await;
    assert!(result.cancelled);
    assert_eq!((result.done, result.failed, task.items_done), (1, 0, 1));
    assert!(tree.path("out/a.jpg.xmp").exists());
    assert!(tree.path("src/b.jpg").exists());
}

#[tokio::test]
async fn reused_counterpart_and_replacement_ids_fail_closed_with_identical_index_facts() {
    for kind in [MoveType::KeepDestination, MoveType::Flat] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        let name = if kind == MoveType::KeepDestination {
            "counterpart.jpg"
        } else {
            "one.jpg"
        };
        tree.write("src/one.jpg", b"photo");
        tree.write(&format!("out/{name}"), b"other");
        let seed = Seed::new(&app.state.db);
        seed.file(v, "src", "one.jpg")
            .size(5)
            .hash(sha1_bytes(b"photo"))
            .insert()
            .await;
        let hash = sha1_bytes(if kind == MoveType::KeepDestination {
            b"photo"
        } else {
            b"other"
        });
        let target = seed.file(v, "out", name).size(5).hash(hash).insert().await;
        ready(&app).await;
        let mut p = params(kind, v, v);
        p.conflict_policy = Some(ConflictPolicy::Replace);
        let plan = build(&app, &p).await;
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(target)
            .execute(app.state.db.write())
            .await
            .unwrap();
        let new = seed.file(v, "out", name).size(5).hash(hash).insert().await;
        assert_eq!(new, target);
        ready(&app).await;
        let reference = PlanRef {
            plan_id: plan.id,
            side: PlanSide::Comparison,
        };
        let mut view = GhostView {
            volume_id: v,
            dir: RelPath::new("out").unwrap(),
            filter: Default::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        for filtered in [false, true] {
            if filtered {
                view.filter.ratings = vec![0];
            }
            let ghosts = app
                .state
                .plans
                .ghost_items(reference, &view, Window { skip: 0, take: 200 })
                .await
                .unwrap();
            assert_eq!(ghosts.total, 0);
            assert!(ghosts.items.is_empty());
        }
        let flags = app
            .state
            .plans
            .item_flags(
                PlanRef {
                    plan_id: plan.id,
                    side: PlanSide::Comparison,
                },
                &[new],
            )
            .await
            .unwrap();
        assert!(flags.is_empty());
        let (result, _) = run(&app, plan.id, None).await;
        assert_eq!((result.done, result.skipped, result.failed), (0, 1, 0));
        assert_eq!(
            result.failures[0].code,
            if kind == MoveType::KeepDestination {
                "stale_match"
            } else {
                "stale_plan"
            }
        );
        assert_eq!(std::fs::read(tree.path("src/one.jpg")).unwrap(), b"photo");
        assert_eq!(
            std::fs::read(tree.path(&format!("out/{name}"))).unwrap(),
            b"other"
        );
    }
}

#[tokio::test]
async fn deleted_source_incarnation_is_not_revived_by_identical_index_facts() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/one.jpg", b"photo");
    let seed = Seed::new(&app.state.db);
    let old = seed
        .file(v, "src", "one.jpg")
        .size(5)
        .capture(1_791_205_200)
        .hash(sha1_bytes(b"photo"))
        .insert()
        .await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(old)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let new = seed
        .file(v, "src", "one.jpg")
        .size(5)
        .capture(1_791_205_200)
        .hash(sha1_bytes(b"photo"))
        .insert()
        .await;
    assert_eq!(new, old);
    ready(&app).await;
    tree.write("src/one.jpg", b"other");
    let (result, _) = run(&app, plan.id, None).await;
    assert_eq!(
        (result.done, result.skipped, result.failed),
        (0, 1, 0),
        "{result:?}"
    );
    assert_eq!(result.failures[0].code, "stale_plan");
    assert_eq!(std::fs::read(tree.path("src/one.jpg")).unwrap(), b"other");
    assert!(!tree.path("out/one.jpg").exists());
    let view = GhostView {
        volume_id: v,
        dir: RelPath::new("out").unwrap(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    // A fresh ready plan is required for overlay assertions: executed plans are already hidden
    // by the generic engine, independently of the identity provider.
    let fresh = build(&app, &params(MoveType::Flat, v, v)).await;
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(new)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let revived = seed
        .file(v, "src", "one.jpg")
        .size(5)
        .capture(1_791_205_200)
        .hash(sha1_bytes(b"photo"))
        .insert()
        .await;
    assert_eq!(revived, new);
    ready(&app).await;
    let source = PlanRef {
        plan_id: fresh.id,
        side: PlanSide::Source,
    };
    assert!(
        app.state
            .plans
            .item_flags(source, &[revived])
            .await
            .unwrap()
            .is_empty()
    );
    let comparison = PlanRef {
        plan_id: fresh.id,
        side: PlanSide::Comparison,
    };
    assert_eq!(
        app.state
            .plans
            .ghost_items(comparison, &view, Window { skip: 0, take: 20 })
            .await
            .unwrap()
            .total,
        0
    );
    assert!(
        !app.state
            .plans
            .folder_overlay(comparison, &view, &[])
            .await
            .unwrap()
            .dir_planned
    );
}

#[tokio::test]
async fn cancelled_shared_carrier_keeps_metadata_durable_across_recovery() {
    use opm_server::fsops::{AppliedOp, FsOps, OpHook};
    use opm_server::plans::{PlanDeps, PlanService};
    use std::sync::Arc;
    let mut app = TestApp::new().await;
    struct CancelOnOwner(opm_server::tasks::TaskEngine);
    impl OpHook for CancelOnOwner {
        fn on_applied<'a>(
            &'a self,
            tx: &'a mut sqlx::SqliteConnection,
            _op: &'a AppliedOp,
        ) -> futures_util::future::BoxFuture<'a, Result<(), sqlx::Error>> {
            Box::pin(async move {
                let task: i64 =
                    sqlx::query_scalar("SELECT task_id FROM fs_journal ORDER BY id DESC LIMIT 1")
                        .fetch_one(tx)
                        .await?;
                self.0.cancel(task).await.unwrap();
                Ok(())
            })
        }
    }
    let fsops = app
        .state
        .fsops
        .clone()
        .with_hook(Arc::new(CancelOnOwner(app.state.tasks.clone())));
    app.state.plans = PlanService::new(PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    })
    .await
    .unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (v, tree) = app.add_volume("photos").await;
    tree.write("src/one.CR3", b"photo");
    tree.write("src/one.JPG", b"jpeg");
    tree.write("src/one.xmp", b"metadata");
    let seed = Seed::new(&app.state.db);
    let a = seed.file(v, "src", "one.CR3").size(5).insert().await;
    let b = seed.file(v, "src", "one.JPG").size(4).insert().await;
    let s = seed.file(v, "src", "one.xmp").size(8).insert().await;
    seed.sidecar(a, s, "stem").await;
    seed.sidecar(b, s, "stem").await;
    ready(&app).await;
    let plan = build(&app, &params(MoveType::Flat, v, v)).await;
    let (result, _) = run(&app, plan.id, None).await;
    assert!(result.cancelled);
    assert_eq!((result.done, result.failed), (1, 0));
    assert_eq!(
        std::fs::read(tree.path("out/one.xmp")).unwrap(),
        b"metadata"
    );
    assert_eq!(
        std::fs::read(tree.path("src/one.xmp")).unwrap(),
        b"metadata"
    );
    // New executor, no planner's carrier map: recovery relies solely on journaled copies.
    let fresh = FsOps::new(
        app.state.db.clone(),
        app.state.volumes.clone(),
        app.state.settings.clone(),
        app.state.indexer.clone(),
    );
    fresh.reconcile().await.unwrap();
    let links: Vec<(i64, String)> = sqlx::query_as("SELECT l.file_id, f.dir_path FROM file_sidecars l JOIN files f ON f.id = l.sidecar_id ORDER BY l.file_id")
        .fetch_all(app.state.db.read()).await.unwrap();
    assert_eq!(links, [(a, "out".into()), (b, "src".into())]);
    assert!(tree.path("out/one.CR3").exists());
    assert!(tree.path("src/one.JPG").exists());
}
