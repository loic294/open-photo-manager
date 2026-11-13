//! Cleanup against the real plan engine and disposable volumes only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::http::{Method, StatusCode};
use common::seed::Seed;
use common::{TestApp, Tree};
use opm_core::dto::{CreatePlanRequest, ExecutePlanRequest, PlanKind, PlanSide};
use opm_core::dto::{Filter, PlanRef, SortKey, SortOrder};
use opm_core::paths::RelPath;
use opm_server::fsops::{
    FailAction, Failpoints, FsOpError, Loc, OpContext, RmdirOutcome, Step, TrashBatch,
};
use opm_server::plans::cleanup::CleanupPlanner;
use opm_server::plans::overlay::{GhostView, PlanOverlay};
use opm_server::plans::{PlanDeps, PlanOptions, PlanService};
use serde_json::{Value, json};

async fn build(app: &TestApp, volume: i64, base: &str, recursive: bool) -> Value {
    let (status, plan) = app.call(Method::POST, "/api/plans", Some(json!({
        "kind": "cleanup", "params": {"scope": {"volume_id": volume, "dir": base, "recursive": recursive}},
        "replaces": null
    }))).await;
    assert_eq!(status, StatusCode::CREATED, "{plan}");
    plan
}

async fn paths(app: &TestApp, plan: &Value) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>("SELECT path FROM cleanup_dirs WHERE plan_id = ?")
        .bind(plan["id"].as_i64().unwrap())
        .fetch_all(app.state.db.read())
        .await
        .unwrap()
        .into_iter()
        .collect()
}

async fn run(app: &TestApp, plan: &Value, only: Option<i64>) -> Value {
    let id = plan["id"].as_i64().unwrap();
    let (status, task) = app
        .call(
            Method::POST,
            &format!("/api/plans/{id}/execute"),
            Some(json!({"options": {}, "only": only})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = app
        .state
        .tasks
        .wait_finished(task["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(task.items_done, task.items_total.unwrap());
    let (_, finished) = app
        .call(Method::GET, &format!("/api/plans/{id}"), None)
        .await;
    finished
}

async fn folder(app: &TestApp, volume: i64, tree: &Tree, path: &str) {
    tree.mkdir(path);
    Seed::new(&app.state.db).dir(volume, path).await;
}

#[tokio::test]
async fn build_counts_every_kind_and_never_the_base_or_prefix_siblings() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    for p in [
        "A/empty/deep",
        "A/junk",
        "A/system",
        "A_b/empty",
        "A (copy)",
        "A!x",
    ] {
        folder(&app, v, &tree, p).await;
    }
    tree.write("A/junk/.DS_Store", b"junk");
    tree.mkdir("A/system/#recycle");
    for (dir, name) in [
        ("A/sidecar", "orphan.xmp"),
        ("A/other", "notes.txt"),
        ("A/audio", "song.mp3"),
        ("A/image", "photo.jpg"),
        ("A/video", "film.mp4"),
    ] {
        seed.file(v, dir, name).insert().await;
    }
    let plan = build(&app, v, "A", true).await;
    assert_eq!(
        paths(&app, &plan).await,
        BTreeSet::from([
            "A/empty".into(),
            "A/empty/deep".into(),
            "A/junk".into(),
            "A/system".into()
        ])
    );
    assert_eq!(
        plan["stats"],
        json!({
            "folders_scanned": 9, "empty_folders": 4, "top_level_empty_folders": 3, "nonempty_folders": 5
        })
    );
    assert_eq!(plan["warnings"], json!([]));
    let root = build(&app, v, "", true).await;
    assert!(!paths(&app, &root).await.contains(""));
    let empty_base = build(&app, v, "A/empty", true).await;
    assert_eq!(
        paths(&app, &empty_base).await,
        BTreeSet::from(["A/empty/deep".into()])
    );
}

#[tokio::test]
async fn direct_children_mode_keeps_empty_branches_but_not_empty_leaves_of_nonempty_branches() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    for p in ["base/empty/deep/end", "base/full/empty"] {
        folder(&app, v, &tree, p).await;
    }
    Seed::new(&app.state.db)
        .file(v, "base/full", "notes.txt")
        .insert()
        .await;
    let plan = build(&app, v, "base", false).await;
    assert_eq!(
        paths(&app, &plan).await,
        BTreeSet::from([
            "base/empty".into(),
            "base/empty/deep".into(),
            "base/empty/deep/end".into()
        ])
    );
    assert_eq!(plan["stats"]["top_level_empty_folders"], 1);
    assert!(
        paths(&app, &build(&app, v, "base", true).await)
            .await
            .contains("base/full/empty")
    );
}

#[tokio::test]
async fn invalid_scopes_and_options_are_rejected_before_work() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    for (volume, dir) in [
        (None, ""),
        (Some(99999), ""),
        (Some(v), "missing"),
        (Some(v), ".opm-trash"),
        (Some(v), "@eaDir/a"),
        (Some(v), "#recycle/a"),
        (Some(v), "../escape"),
        (Some(v), "."),
    ] {
        let (status, body) = app.call(Method::POST, "/api/plans", Some(json!({
            "kind": "cleanup", "params": {"scope": {"volume_id": volume, "dir": dir, "recursive": true}},
            "replaces": null
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_plan_params");
    }
    let plan = build(&app, v, "", true).await;
    let id = plan["id"].as_i64().unwrap();
    let (status, _) = app
        .call(
            Method::POST,
            &format!("/api/plans/{id}/execute"),
            Some(json!({"options": {"recursive": true}, "only": null})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cleanup_dirs")
            .fetch_one(app.state.db.read())
            .await
            .unwrap()
            == 0
    );
}

#[tokio::test]
async fn empty_chains_delete_deepest_first_and_junk_restores_as_one_batch() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    for p in ["base/chain/deep/end", "base/junk/one", "base/second"] {
        folder(&app, v, &tree, p).await;
    }
    tree.write("base/junk/one/.DS_Store", b"junk1");
    tree.write("base/second/Thumbs.db", b"junk2");
    let plan = build(&app, v, "base", true).await;
    let finished = run(&app, &plan, None).await;
    assert_eq!(finished["result"]["done"], 6);
    assert_eq!(finished["result"]["failed"], 0);
    assert_eq!(finished["result"]["skipped"], 0);
    assert_eq!(finished["result"]["bytes"], 0);
    assert!(tree.path("base").is_dir());
    assert!(!tree.path("base/chain").exists());
    let batches: Vec<String> = sqlx::query_scalar("SELECT DISTINCT batch FROM trash_items")
        .fetch_all(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(
        build(&app, v, "base", true).await["stats"]["empty_folders"],
        0
    );
    let (status, task) = app.call(Method::POST, "/api/trash/restore", Some(json!({
        "target": {"kind": "batch", "volume_id": v, "batch": batches[0]}, "conflict_policy": null
    }))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = app
        .state
        .tasks
        .wait_finished(task["id"].as_i64().unwrap())
        .await
        .unwrap();
    assert!(task.error.is_none(), "{task:?}");
    assert_eq!(
        std::fs::read(tree.path("base/junk/one/.DS_Store")).unwrap(),
        b"junk1"
    );
    assert_eq!(
        std::fs::read(tree.path("base/second/Thumbs.db")).unwrap(),
        b"junk2"
    );
}

#[tokio::test]
async fn stale_index_new_content_links_system_dirs_and_gone_folders_are_safety_skips() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    for p in [
        "base/chain/deep",
        "base/link",
        "base/system",
        "base/gone",
        "base/empty",
    ] {
        folder(&app, v, &tree, p).await;
    }
    let plan = build(&app, v, "base", true).await;
    tree.write("base/chain/deep/new.txt", b"precious");
    std::os::unix::fs::symlink(tree.path("base/chain"), tree.path("base/link/symlink")).unwrap();
    tree.mkdir("base/system/#snapshot");
    tree.remove_dir("base/gone");
    let finished = run(&app, &plan, None).await;
    assert_eq!(finished["result"]["done"], 1);
    assert_eq!(finished["result"]["skipped"], 5);
    assert_eq!(finished["result"]["failed"], 0);
    let details = finished["result"]["failures"].as_array().unwrap();
    assert_eq!(details.len(), 5);
    assert_eq!(
        details
            .iter()
            .filter(|detail| detail["code"] == "not_empty")
            .count(),
        4
    );
    assert_eq!(
        details
            .iter()
            .filter(|detail| detail["code"] == "already_gone")
            .count(),
        1
    );
    assert!(details.iter().all(|detail| detail["file_id"].is_null()));
    assert!(tree.path("base").is_dir());
    assert!(tree.path("base/chain/deep/new.txt").is_file());
    assert!(tree.path("base/system/#snapshot").is_dir());
    assert!(tree.path("base/link/symlink").is_symlink());
}

#[tokio::test]
async fn file_selection_is_ignored_and_corrupt_paths_cannot_delete_the_base_or_escape() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    folder(&app, v, &tree, "base/empty").await;
    folder(&app, v, &tree, "outside").await;
    let seed = Seed::new(&app.state.db);
    let file = seed.file(v, "other", "one.jpg").insert().await;
    let (_, selection) = app
        .call(
            Method::POST,
            "/api/selections",
            Some(json!({
                "ids": [file], "label": null
            })),
        )
        .await;
    let plan = build(&app, v, "base", true).await;
    for path in ["base", "outside", "#recycle/corrupt"] {
        sqlx::query("INSERT INTO cleanup_dirs(plan_id,path,depth) VALUES (?,?,?)")
            .bind(plan["id"].as_i64().unwrap())
            .bind(path)
            .bind(RelPath::new(path).unwrap().depth() as i64)
            .execute(app.state.db.write())
            .await
            .unwrap();
    }
    let finished = run(&app, &plan, selection["id"].as_i64()).await;
    assert_eq!(finished["result"]["done"], 1);
    assert_eq!(finished["result"]["failed"], 2);
    assert_eq!(finished["result"]["skipped"], 1);
    let details = finished["result"]["failures"].as_array().unwrap();
    assert_eq!(details.len(), 3);
    assert_eq!(
        details
            .iter()
            .filter(|detail| detail["code"] == "invalid_plan_path")
            .count(),
        2
    );
    assert_eq!(
        details
            .iter()
            .filter(|detail| detail["code"] == "not_empty")
            .count(),
        1
    );
    assert!(tree.path("base").is_dir());
    assert!(tree.path("outside").is_dir());
    assert!(!tree.path("base/empty").exists());
}

#[tokio::test]
async fn comparison_only_overlay_counts_planned_descendants_without_ghosts_or_files() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    folder(&app, v, &tree, "base/empty/a/b").await;
    Seed::new(&app.state.db)
        .file(v, "base/full", "one.txt")
        .insert()
        .await;
    let plan = build(&app, v, "base", true).await;
    let view = GhostView {
        volume_id: v,
        dir: RelPath::new("base").unwrap(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let children = vec!["empty".to_owned(), "full".to_owned(), "outside".to_owned()];
    for side in [PlanSide::Source, PlanSide::Comparison] {
        let reference = PlanRef {
            plan_id: plan["id"].as_i64().unwrap(),
            side,
        };
        let overlay = app
            .state
            .plans
            .folder_overlay(reference, &view, &children)
            .await
            .unwrap();
        assert!(overlay.ghost_folders.is_empty());
        assert!(!overlay.dir_planned);
        assert_eq!(overlay.ghosts.in_folder, 0);
        assert_eq!(overlay.ghosts.in_subfolders, 0);
        assert!(
            app.state
                .plans
                .item_flags(reference, &[1])
                .await
                .unwrap()
                .is_empty()
        );
        if side == PlanSide::Source {
            assert!(overlay.per_child.is_empty());
        } else {
            assert_eq!(overlay.per_child.len(), 1);
            assert!(overlay.per_child["empty"].overlay.will_delete);
            assert_eq!(overlay.per_child["empty"].matches, Some(3));
        }
    }
}

#[tokio::test]
async fn index_warning_is_volume_specific_and_no_empty_result_is_normal() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    sqlx::query(
        "INSERT INTO tasks(kind,title,lane,state,params_json,created_at) \
        VALUES ('index','test','index','queued',?,0)",
    )
    .bind(json!({"targets": [{"volume_id": v, "dir": "", "recursive": true}]}).to_string())
    .execute(app.state.db.write())
    .await
    .unwrap();
    let plan = build(&app, v, "", true).await;
    assert_eq!(plan["warnings"][0]["code"], "index_running");
    assert_eq!(plan["stats"]["empty_folders"], 0);
}

#[tokio::test]
async fn fsops_protects_targets_and_descendants_even_without_default_junk_names() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    let mut settings = (*app.state.settings.get()).clone();
    settings.junk_names.retain(|name| !name.starts_with('#'));
    app.state.settings.update(settings).await.unwrap();
    let batch = TrashBatch::adhoc();
    let op = OpContext::detached();
    for path in [
        "#recycle",
        "#snapshot",
        "a/#ReCyClE/child",
        "#snapshot/child",
    ] {
        tree.mkdir(path);
        let rel = RelPath::new(path).unwrap();
        assert!(opm_server::fsops::is_protected_path(&rel));
        assert!(matches!(
            app.state
                .fsops
                .remove_dir_if_removable(Loc::new(v, rel), &batch, &op)
                .await,
            Err(FsOpError::NotEmpty)
        ));
        assert!(tree.path(path).is_dir());
    }
    for path in ["#recycle.old", "snapshot", "a/#snapshot_copy"] {
        tree.mkdir(path);
        let rel = RelPath::new(path).unwrap();
        assert!(!opm_server::fsops::is_protected_path(&rel));
        assert!(matches!(
            app.state
                .fsops
                .remove_dir_if_removable(Loc::new(v, rel), &batch, &op)
                .await,
            Ok(RmdirOutcome::Removed { .. })
        ));
    }
    let plan = build(&app, v, "", true).await;
    assert!(
        !paths(&app, &plan)
            .await
            .iter()
            .any(|p| opm_server::fsops::is_protected_path(&RelPath::new(p).unwrap()))
    );
}

fn deps(app: &TestApp) -> PlanDeps {
    PlanDeps {
        db: app.state.db.clone(),
        fsops: app.state.fsops.clone(),
        settings: app.state.settings.clone(),
        shutdown: app.state.tasks.shutdown_token(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    }
}

#[tokio::test]
async fn offline_and_read_only_volumes_are_invalid_bases_and_execution_records_offline_failures() {
    for (online, writable) in [(false, true), (true, false)] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        folder(&app, v, &tree, "base/empty").await;
        let plan = build(&app, v, "base", true).await;
        sqlx::query("UPDATE volumes SET online = ?, writable = ? WHERE id = ?")
            .bind(online)
            .bind(writable)
            .bind(v)
            .execute(app.state.db.write())
            .await
            .unwrap();
        let mut d = deps(&app);
        d.volumes = opm_server::indexer::VolumeRegistry::load(
            app.state.db.clone(),
            app.state.config.volumes_dir.clone(),
        )
        .await
        .unwrap();
        d.fsops = opm_server::fsops::FsOps::new(
            d.db.clone(),
            d.volumes.clone(),
            d.settings.clone(),
            app.state.indexer.clone(),
        );
        let service = PlanService::with_options(
            d.clone(),
            vec![Arc::new(CleanupPlanner::new(&d))],
            PlanOptions::default(),
        )
        .await
        .unwrap();
        assert!(
            service
                .create(CreatePlanRequest {
                    kind: PlanKind::Cleanup,
                    params: json!({"scope":{"volume_id":v,"dir":"base","recursive":true}}),
                    replaces: None
                })
                .await
                .is_err()
        );
        let task = service
            .execute(
                plan["id"].as_i64().unwrap(),
                ExecutePlanRequest {
                    options: json!({}),
                    only: None,
                },
            )
            .await
            .unwrap();
        app.state.tasks.wait_finished(task.id).await.unwrap();
        let result = service
            .get(plan["id"].as_i64().unwrap())
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(result.failed, 1);
        assert_eq!(
            result.failures[0].code,
            if online {
                "volume_read_only"
            } else {
                "volume_offline"
            }
        );
        assert!(tree.path("base/empty").is_dir());
        service.shutdown().await;
    }
}

#[tokio::test]
async fn permission_denial_is_a_failure_and_other_folders_still_delete() {
    use std::os::unix::fs::PermissionsExt;
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    for path in ["base/denied", "base/okay"] {
        folder(&app, v, &tree, path).await;
    }
    let plan = build(&app, v, "base", true).await;
    std::fs::set_permissions(
        tree.path("base/denied"),
        std::fs::Permissions::from_mode(0o0),
    )
    .unwrap();
    let finished = run(&app, &plan, None).await;
    std::fs::set_permissions(
        tree.path("base/denied"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(finished["result"]["failed"], 1);
    assert_eq!(finished["result"]["done"], 1);
    assert_eq!(finished["result"]["failures"][0]["code"], "io_error");
    assert!(!tree.path("base/okay").exists());
}

#[tokio::test]
async fn cancelled_build_and_item_cap_do_not_leave_materialized_rows() {
    let app = TestApp::new().await;
    let (v, tree) = app.add_volume("photos").await;
    folder(&app, v, &tree, "base/a/b/c").await;
    let d = deps(&app);
    let planner = CleanupPlanner::new(&d);
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let ctx = opm_server::plans::BuildContext {
        plan_id: 999,
        db: d.db.clone(),
        settings: d.settings.get(),
        volumes: d.volumes.clone(),
        cancel,
        now: 0,
        max_items: 2,
    };
    use opm_server::plans::Planner;
    assert!(matches!(
        planner
            .build(
                &ctx,
                json!({"scope":{"volume_id":v,"dir":"base","recursive":true}})
            )
            .await,
        Err(opm_server::plans::PlanError::Cancelled)
    ));
    let service = PlanService::with_options(
        d.clone(),
        vec![Arc::new(planner)],
        PlanOptions {
            max_items: 2,
            ..PlanOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(
        service
            .create(CreatePlanRequest {
                kind: PlanKind::Cleanup,
                params: json!({"scope":{"volume_id":v,"dir":"base","recursive":true}}),
                replaces: None
            })
            .await
            .is_err()
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM cleanup_dirs")
                .fetch_one(app.state.db.read())
                .await
                .unwrap();
            if rows == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    service.shutdown().await;
}

#[tokio::test]
async fn failures_continue_and_shutdown_resumes_the_original_cleanup_task() {
    for cancel in [false, true] {
        let app = TestApp::new().await;
        let (v, tree) = app.add_volume("photos").await;
        for path in ["base/a", "base/b", "base/c"] {
            folder(&app, v, &tree, path).await;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let mut d = deps(&app);
        let token = if cancel {
            app.state.tasks.shutdown_token()
        } else {
            tokio_util::sync::CancellationToken::new()
        };
        let c = calls.clone();
        let t = token.clone();
        d.fsops = d.fsops.with_failpoints(Failpoints::new(move |step| {
            if step == Step::AfterJournal && c.fetch_add(1, Ordering::SeqCst) == 0 {
                if cancel {
                    t.cancel();
                } else {
                    return FailAction::Error;
                }
            }
            FailAction::Continue
        }));
        // The task's parent token stops it synchronously at the first journaled operation.
        let service = PlanService::with_options(
            d.clone(),
            vec![Arc::new(CleanupPlanner::new(&d))],
            PlanOptions::default(),
        )
        .await
        .unwrap();
        let plan = service
            .create(CreatePlanRequest {
                kind: PlanKind::Cleanup,
                params: json!({"scope":{"volume_id":v,"dir":"base","recursive":true}}),
                replaces: None,
            })
            .await
            .unwrap();
        let task = service
            .execute(
                plan.id,
                ExecutePlanRequest {
                    options: json!({}),
                    only: None,
                },
            )
            .await
            .unwrap();
        if cancel {
            token.cancelled().await;
        }
        let task = app.state.tasks.wait_finished(task.id).await.unwrap();
        let finished = service.get(plan.id).await.unwrap();
        if cancel {
            assert_eq!(task.state, opm_core::dto::TaskState::Interrupted);
            assert!(finished.result.is_none());
            assert_eq!(finished.state, opm_core::dto::PlanState::Executing);
            service.shutdown().await;
            app.state.begin_shutdown();
            app.state.shutdown().await;
            let restarted = opm_server::state::AppState::new((*app.state.config).clone())
                .await
                .unwrap();
            let recovered = restarted.tasks.wait_finished(task.id).await.unwrap();
            assert_eq!(
                recovered.state,
                opm_core::dto::TaskState::Done,
                "{:?}",
                recovered.error
            );
            let result = restarted.plans.get(plan.id).await.unwrap().result.unwrap();
            assert_eq!((result.done, result.failed), (3, 0));
            for path in ["base/a", "base/b", "base/c"] {
                assert!(!tree.path(path).exists());
            }
            restarted.begin_shutdown();
            restarted.shutdown().await;
        } else {
            let result = finished.result.unwrap();
            assert_eq!((result.done, result.failed), (2, 1));
            assert_eq!(task.items_done, 3);
            assert!(tree.path("base/a").exists());
        }
        service.shutdown().await;
    }
}
