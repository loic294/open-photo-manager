//! Real duplicate plans, index-backed overlays and journaled trash over scratch volumes only.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::seed::Seed;
use opm_core::dto::*;
use opm_core::hash::sha1_bytes;
use opm_core::matching::{MatchFacts, MatchOptions, Overlap, Pair, is_match, is_valid_pair};
use opm_core::paths::RelPath;
use opm_server::fsops::{AppliedOp, FsOps, Mover, OpHook, SystemMover};
use opm_server::plans::overlay::{GhostView, PlanOverlay};
use opm_server::plans::{PlanDeps, PlanOptions, PlanService};
use serde_json::json;

fn side(volume: i64, dir: &str) -> SideParams {
    SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: dir.to_owned(),
            recursive: true,
        },
        filter: Filter::default(),
    }
}

fn params(mode: MatchMode, source: SideParams, comparison: SideParams) -> DuplicateParams {
    DuplicateParams {
        mode,
        strict_prefix: false,
        source,
        comparison,
    }
}

async fn build(app: &TestApp, params: &DuplicateParams) -> PlanDto {
    let (status, value) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "duplicates", "params": params, "replaces": null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    serde_json::from_value(value).unwrap()
}

async fn pairs(app: &TestApp, id: i64) -> BTreeSet<Pair> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT source_id, comparison_id FROM duplicate_pairs WHERE plan_id = ?",
    )
    .bind(id)
    .fetch_all(app.state.db.read())
    .await
    .unwrap()
    .into_iter()
    .map(|(s, c)| Pair::new(s, c))
    .collect()
}

async fn run(
    app: &TestApp,
    id: i64,
    action: DuplicateAction,
    only: Option<i64>,
) -> (PlanResult, TaskDto) {
    run_options(app, id, json!({"action": action}), only).await
}

async fn run_options(
    app: &TestApp,
    id: i64,
    options: serde_json::Value,
    only: Option<i64>,
) -> (PlanResult, TaskDto) {
    let (status, task) = app
        .call(
            Method::POST,
            &format!("/api/plans/{id}/execute"),
            Some(json!({
                "options": options, "only": only,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task_id = task["id"].as_i64().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = app.state.tasks.get(task_id).await.unwrap();
            if matches!(
                task.state,
                TaskState::Done | TaskState::Failed | TaskState::Cancelled
            ) {
                let plan = app.state.plans.get(id).await.unwrap();
                return (plan.result.unwrap(), task);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

async fn file_plan(
    app: &TestApp,
    mode: MatchMode,
    source: SideParams,
    excluded: Vec<Scope>,
) -> PlanDto {
    let (status, body) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "duplicates", "replaces": null, "params": DuplicateFileParams {
                    matching: DuplicateParams {
                        mode, strict_prefix: false, source,
                        comparison: SideParams {
                            scope: Scope { volume_id: None, dir: String::new(), recursive: true },
                            filter: Filter::default(),
                        },
                    },
                    excluded,
                },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_value(body).unwrap()
}

#[tokio::test]
async fn file_view_groups_find_all_volumes_and_apply_boundary_safe_exclusions_for_every_mode() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("source").await;
    let b = seed.volume("other").await;
    let source = seed
        .file(a, "selected", "one.jpg")
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let outside = seed
        .file(b, "elsewhere", "one.jpg")
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .tags(["trip"])
        .rating(4)
        .insert()
        .await;
    let sibling = seed
        .file(b, "excluded.old", "one.jpg")
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    seed.file(b, "excluded/sub", "one.jpg")
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    seed.file(b, "elsewhere", "unrelated.jpg")
        .hash(sha1_bytes(b"unique"))
        .insert()
        .await;
    sqlx::query("UPDATE files SET has_description = 1, btime = 10 WHERE id = ?")
        .bind(outside)
        .execute(app.state.db.write())
        .await
        .unwrap();
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
        let plan = file_plan(
            &app,
            mode,
            side(a, "selected"),
            vec![Scope {
                volume_id: Some(b),
                dir: "excluded".to_owned(),
                recursive: true,
            }],
        )
        .await;
        let (status, body) = app
            .call(
                Method::GET,
                &format!("/api/plans/{}/duplicate-files", plan.id),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let groups: DuplicateFileGroups = serde_json::from_value(body).unwrap();
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(
            groups.groups[0]
                .files
                .iter()
                .map(|file| file.item.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([source, outside, sibling])
        );
        let metadata = groups.groups[0]
            .files
            .iter()
            .find(|file| file.item.id == outside)
            .unwrap();
        assert_eq!(metadata.tags, ["trip"]);
        assert_eq!(metadata.btime, Some(10));
        assert!(metadata.has_description);
        assert_eq!(plan.stats["comparison_candidates"], 4);
    }
    let plan = file_plan(
        &app,
        MatchMode::Hash,
        side(a, "selected"),
        vec![Scope {
            volume_id: Some(a),
            dir: "selected".to_owned(),
            recursive: true,
        }],
    )
    .await;
    assert_eq!(plan.stats["pairs"], 0);
    app.state.begin_shutdown();
}

#[tokio::test]
async fn file_view_can_keep_an_outside_copy_and_trash_source_and_other_copies_in_one_batch() {
    let app = TestApp::new().await;
    let (a, tree) = app.add_volume("source").await;
    let (b, backup) = app.add_volume("backup").await;
    let seed = Seed::new(&app.state.db);
    tree.write("selected/one.jpg", b"same");
    backup.write("keep/one.jpg", b"same");
    backup.write("remove/one.jpg", b"same");
    for (volume, dir) in [(a, "selected"), (b, "keep"), (b, "remove")] {
        seed.file(volume, dir, "one.jpg")
            .size(4)
            .hash(sha1_bytes(b"same"))
            .insert()
            .await;
    }
    let keep: i64 =
        sqlx::query_scalar("SELECT id FROM files WHERE volume_id = ? AND dir_path = 'keep'")
            .bind(b)
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
    let plan = file_plan(&app, MatchMode::Hash, side(a, "selected"), vec![]).await;
    let (result, task) = run_options(&app, plan.id, json!({"keep": [keep]}), None).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!((result.done, result.skipped, result.failed), (2, 0, 0));
    assert!(!tree.path("selected/one.jpg").exists());
    assert!(!backup.path("remove/one.jpg").exists());
    assert!(backup.path("keep/one.jpg").exists());
    let batches: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT batch) FROM trash_items")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(batches, 1);
    app.state.begin_shutdown();
}

#[tokio::test]
async fn file_view_rechecks_direct_survivors_and_identity_instead_of_trusting_client_choices() {
    for stale in [false, true] {
        let app = TestApp::new().await;
        let (volume, tree) = app.add_volume("photos").await;
        let seed = Seed::new(&app.state.db);
        let mut ids = Vec::new();
        for (dir, capture) in [("a", 1), ("b", 2), ("c", 1)] {
            tree.write(&format!("{dir}/one.jpg"), b"same");
            ids.push(
                seed.file(volume, dir, "one.jpg")
                    .capture(capture)
                    .size(4)
                    .hash(sha1_bytes(b"same"))
                    .insert()
                    .await,
            );
        }
        let mode = if stale {
            MatchMode::Hash
        } else {
            MatchMode::NameDifferentTime
        };
        let plan = file_plan(&app, mode, side(volume, ""), vec![]).await;
        if stale {
            sqlx::query("DELETE FROM files WHERE id = ?")
                .bind(ids[0])
                .execute(app.state.db.write())
                .await
                .unwrap();
            seed.file(volume, "replacement", "one.jpg")
                .size(4)
                .hash(sha1_bytes(b"same"))
                .insert()
                .await;
            let (status, _) = app
                .call(
                    Method::GET,
                    &format!("/api/plans/{}/duplicate-files", plan.id),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let (result, _) = run_options(&app, plan.id, json!({"keep": [ids[0]]}), None).await;
        if stale {
            assert_eq!(result.done, 0);
            assert_eq!(result.skipped, 2);
        } else {
            assert_eq!(result.done, 1);
            assert_eq!(result.skipped, 1);
            assert_eq!(result.failures[0].code, "no_survivor");
        }
        assert!(tree.path("c/one.jpg").exists());
        app.state.begin_shutdown();
    }
}

#[tokio::test]
async fn file_view_rejects_empty_keepers_and_folder_plans_do_not_expose_file_groups() {
    let app = TestApp::new().await;
    let (volume, _, _, _, folder) = disk_pair(&app, MatchMode::Hash).await;
    let (status, _) = app
        .call(
            Method::GET,
            &format!("/api/plans/{}/duplicate-files", folder.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let plan = file_plan(&app, MatchMode::Hash, side(volume, ""), vec![]).await;
    let (status, _) = app
        .call(
            Method::POST,
            &format!("/api/plans/{}/execute", plan.id),
            Some(json!({
                "options": {"keep": []}, "only": null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.state.plans.get(plan.id).await.unwrap().state,
        PlanState::Ready
    );
    app.state.begin_shutdown();
}

#[tokio::test]
async fn modes_stats_warnings_kinds_and_independent_filters() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    let s = seed
        .file(a, "src/nested", "IMG_0001.CR3")
        .hash(sha1_bytes(b"same"))
        .capture(1)
        .rating(4)
        .insert()
        .await;
    let same = seed
        .file(b, "cmp", "img_0001.cr3")
        .hash(sha1_bytes(b"same"))
        .capture(1)
        .rating(3)
        .insert()
        .await;
    let different = seed
        .file(b, "cmp/sub", "IMG_0001.CR3")
        .capture(2)
        .rating(3)
        .insert()
        .await;
    let edited = seed
        .file(b, "cmp/sub", "IMG_0001_edit.JPG")
        .rating(3)
        .insert()
        .await;
    let twelve = seed
        .file(b, "cmp", "IMG_00012.JPG")
        .rating(3)
        .insert()
        .await;
    seed.file(b, "cmp", "IMG_0001.XMP")
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    seed.file(b, "cmp", "IMG_0001.txt")
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    seed.file(b, "cmp.old", "IMG_0001.CR3")
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    seed.file(b, "cmp", "ignored.jpg")
        .hash(sha1_bytes(b"same"))
        .rating(1)
        .insert()
        .await;
    let mut source = side(a, "src");
    source.filter.ratings = vec![4];
    let mut comparison = side(b, "cmp");
    comparison.filter.ratings = vec![3];
    for (mode, expected) in [
        (MatchMode::Hash, vec![same]),
        (MatchMode::NameAndTime, vec![same]),
        (MatchMode::NameDifferentTime, vec![different]),
        (MatchMode::NamePrefix, vec![same, different, edited]),
    ] {
        let plan = build(&app, &params(mode, source.clone(), comparison.clone())).await;
        assert_eq!(
            pairs(&app, plan.id).await,
            expected.into_iter().map(|id| Pair::new(s, id)).collect()
        );
        assert_eq!(plan.stats["source_candidates"], 1);
        assert_eq!(plan.stats["comparison_candidates"], 4);
        assert_eq!(plan.stats["source_matched_bytes"], 1000);
        if mode == MatchMode::Hash {
            assert_eq!(plan.stats["unmatchable_comparison"], 3);
            assert!(
                plan.warnings
                    .iter()
                    .any(|w| w.code == "unhashed_files" && w.count == 3)
            );
        }
    }
    let mut strict = params(MatchMode::NamePrefix, source, comparison);
    strict.strict_prefix = true;
    assert!(
        pairs(&app, build(&app, &strict).await.id)
            .await
            .contains(&Pair::new(s, twelve))
    );
    app.state.begin_shutdown();
}

/// Deterministic randomized collision-heavy libraries, using the domain matcher as oracle.
#[tokio::test]
async fn stored_pairs_equal_reference_for_random_scopes_filters_and_every_mode() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("property").await;
    let names = [
        "IMG_1.JPG",
        "img_1.jpg",
        "IMG_12.jpg",
        "IMG_1_edit.CR3",
        "\u{e9}.jpg",
        "e\u{301}.jpg",
        ".jpg",
    ];
    let mut random = 0xabcde_u64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let mut files = Vec::new();
    for index in 0..36 {
        let name = names[(next() % names.len() as u64) as usize].to_owned();
        let dir = format!("set{}/item{index}", next() % 3);
        let time = match next() % 3 {
            0 => None,
            n => Some(n as i64),
        };
        let hash = match next() % 3 {
            0 => None,
            n => Some(sha1_bytes(&[n as u8])),
        };
        let rating = (next() % 3) as u8;
        let kind = match next() % 5 {
            0 => FileKind::Other,
            1 => FileKind::Sidecar,
            _ => FileKind::Image,
        };
        let mut file = seed.file(volume, &dir, &name).rating(rating).kind(kind);
        if let Some(time) = time {
            file = file.capture(time);
        }
        if let Some(hash) = hash {
            file = file.hash(hash);
        }
        let id = file.insert().await;
        files.push((id, name, dir, time, hash, rating, kind));
    }
    for iteration in 0..16 {
        let mut source = side(volume, if iteration % 4 == 0 { "" } else { "set0" });
        let mut comparison = side(
            volume,
            if iteration % 3 == 0 {
                ""
            } else if iteration % 3 == 1 {
                "set0"
            } else {
                "set1"
            },
        );
        if iteration % 2 == 0 {
            source.filter.ratings = vec![(next() % 3) as u8];
        }
        if iteration % 5 == 0 {
            comparison.filter.ratings = vec![(next() % 3) as u8];
        }
        type OracleFile = (
            i64,
            String,
            String,
            Option<i64>,
            Option<[u8; 20]>,
            u8,
            FileKind,
        );
        let contains = |side: &SideParams, f: &OracleFile| {
            opm_core::matching::participates(f.6)
                && (side.scope.dir.is_empty() || f.2.starts_with(&format!("{}/", side.scope.dir)))
                && (side.filter.ratings.is_empty() || side.filter.ratings.contains(&f.5))
        };
        for mode in MatchMode::ALL {
            for strict_prefix in [false, true] {
                let mut p = params(mode, source.clone(), comparison.clone());
                p.strict_prefix = strict_prefix;
                let plan = build(&app, &p).await;
                let mut expected = BTreeSet::new();
                for s in files.iter().filter(|f| contains(&source, f)) {
                    for c in files.iter().filter(|f| contains(&comparison, f)) {
                        let sf = MatchFacts {
                            id: s.0,
                            name: &s.1,
                            hash: s.4,
                            capture_ts: s.3,
                            size: 1000,
                        };
                        let cf = MatchFacts {
                            id: c.0,
                            name: &c.1,
                            hash: c.4,
                            capture_ts: c.3,
                            size: 1000,
                        };
                        let pair = Pair::new(s.0, c.0);
                        if is_match(mode, &sf, &cf, MatchOptions { strict_prefix })
                            && is_valid_pair(
                                pair,
                                Overlap {
                                    source_in_comparison: contains(&comparison, s),
                                    comparison_in_source: contains(&source, c),
                                },
                            )
                        {
                            expected.insert(pair);
                        }
                    }
                }
                assert_eq!(
                    pairs(&app, plan.id).await,
                    expected,
                    "{iteration} {mode:?} {strict_prefix}"
                );
            }
        }
    }
    app.state.begin_shutdown();
}

#[tokio::test]
async fn overlays_counts_vanished_ids_and_move_handoff() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    let s = seed
        .file(a, "base/child/nested", "one.jpg")
        .hash(sha1_bytes(b"1"))
        .insert()
        .await;
    let c = seed
        .file(b, "base/child/deeper", "two.jpg")
        .hash(sha1_bytes(b"1"))
        .insert()
        .await;
    seed.dir(a, "base/empty").await;
    seed.dir(b, "base/empty").await;
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(a, "base"), side(b, "base")),
    )
    .await;
    for (side, volume, id) in [(PlanSide::Source, a, s), (PlanSide::Comparison, b, c)] {
        let reference = PlanRef {
            plan_id: plan.id,
            side,
        };
        let flags = app
            .state
            .plans
            .item_flags(reference, &[s, c, 999])
            .await
            .unwrap();
        assert_eq!(flags.len(), 1);
        assert!(flags[&id].matched);
        let view = GhostView {
            volume_id: volume,
            dir: RelPath::new("base").unwrap(),
            filter: Filter::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        let overlay = app
            .state
            .plans
            .folder_overlay(reference, &view, &["child".to_owned(), "empty".to_owned()])
            .await
            .unwrap();
        assert_eq!(overlay.per_child["child"].matches, Some(1));
        assert_eq!(overlay.per_child["empty"].matches, Some(0));
        assert!(overlay.ghost_folders.is_empty());
        let (status, selection) = app
            .call(
                Method::POST,
                &format!("/api/plans/{}/selection", plan.id),
                Some(json!({"side": side, "restrict_ids": null})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(selection["item_count"], 1);
        let selection_id = selection["id"].as_i64().unwrap();
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT file_id FROM selection_items WHERE selection_id = ?")
                .bind(selection_id)
                .fetch_all(app.state.db.read())
                .await
                .unwrap();
        assert_eq!(ids, vec![id]);
    }
    sqlx::query("UPDATE files SET dir_path = 'base/empty' WHERE id = ?")
        .bind(c)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let reference = PlanRef {
        plan_id: plan.id,
        side: PlanSide::Comparison,
    };
    let view = GhostView {
        volume_id: b,
        dir: RelPath::new("base").unwrap(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    };
    let children = ["child".to_owned(), "empty".to_owned()];
    let relocated = app
        .state
        .plans
        .folder_overlay(reference, &view, &children)
        .await
        .unwrap();
    assert_eq!(relocated.per_child["child"].matches, Some(0));
    assert_eq!(relocated.per_child["empty"].matches, Some(1));
    sqlx::query("INSERT INTO duplicate_matches (plan_id, side, file_id, volume_id, dir_path) VALUES (?, 'comparison', 999999, ?, 'base/child')")
        .bind(plan.id).bind(b).execute(app.state.db.write()).await.unwrap();
    let nonexistent: i64 =
        sqlx::query_scalar("SELECT live FROM duplicate_matches WHERE file_id = 999999")
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
    assert_eq!(
        nonexistent, 0,
        "insert synchronization drops ids that vanished while a build read its snapshot"
    );
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(c)
        .execute(app.state.db.write())
        .await
        .unwrap();
    assert!(
        app.state
            .plans
            .item_flags(
                PlanRef {
                    plan_id: plan.id,
                    side: PlanSide::Comparison
                },
                &[c]
            )
            .await
            .unwrap()
            .is_empty()
    );
    let vanished = app
        .state
        .plans
        .folder_overlay(reference, &view, &children)
        .await
        .unwrap();
    assert_eq!(vanished.per_child["empty"].matches, Some(0));
    let (status, value) = app
        .call(
            Method::POST,
            &format!("/api/plans/{}/execute", plan.id),
            Some(json!({"options":{"action":"move"}, "only":null})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/selection")
    );
    app.state.begin_shutdown();
}

async fn disk_pair(app: &TestApp, mode: MatchMode) -> (i64, i64, i64, common::Tree, PlanDto) {
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("a/one.jpg", b"same");
    tree.write("b/one.jpg", b"same");
    let seed = Seed::new(&app.state.db);
    let s = seed
        .file(volume, "a", "one.jpg")
        .size(4)
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let c = seed
        .file(volume, "b", "one.jpg")
        .size(4)
        .capture(1)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let plan = build(app, &params(mode, side(volume, "a"), side(volume, "b"))).await;
    (volume, s, c, tree, plan)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_opposite_deletions_keep_a_survivor_and_waiters_can_cancel() {
    use opm_server::fsops::{FailAction, Failpoints, Step};
    use std::sync::Mutex;

    let mut app = TestApp::new().await;
    let (entered_tx, mut entered) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let fsops = app
        .state
        .fsops
        .clone()
        .with_failpoints(Failpoints::new(move |step| {
            if step == Step::AfterJournal {
                let entered = entered_tx.lock().unwrap().take();
                if let Some(entered) = entered {
                    entered.send(()).unwrap();
                    tokio::task::block_in_place(|| {
                        released
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(10))
                            .unwrap();
                    });
                }
            }
            FailAction::Continue
        }));
    app.state.plans = PlanService::new(deps(&app, fsops)).await.unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (volume, _, _, tree, first) = disk_pair(&app, MatchMode::Hash).await;
    let parameters = params(MatchMode::Hash, side(volume, "a"), side(volume, "b"));
    let second = build(&app, &parameters).await;
    let cancelled = build(&app, &parameters).await;
    let mut tasks = Vec::new();
    for (plan, action) in [
        (first.id, "delete_comparison"),
        (second.id, "delete_source"),
        (cancelled.id, "delete_comparison"),
    ] {
        let (status, body) = app
            .call(
                Method::POST,
                &format!("/api/plans/{plan}/execute"),
                Some(json!({"options": {"action": action}, "only": null})),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        tasks.push(body["id"].as_i64().unwrap());
        if tasks.len() == 1 {
            tokio::time::timeout(Duration::from_secs(5), &mut entered)
                .await
                .unwrap()
                .unwrap();
        }
    }
    assert_eq!(
        app.state.tasks.get(tasks[1]).await.unwrap().state,
        TaskState::Running
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            app.state.tasks.wait_finished(tasks[1])
        )
        .await
        .is_err(),
        "the second deletion must wait for the first survivor check and mutation"
    );
    app.state.tasks.cancel(tasks[2]).await.unwrap();
    let cancelled = tokio::time::timeout(
        Duration::from_secs(1),
        app.state.tasks.wait_finished(tasks[2]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    release.send(()).unwrap();
    for id in &tasks[..2] {
        let done = app.state.tasks.wait_finished(*id).await.unwrap();
        assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    }
    assert!(tree.path("a/one.jpg").exists());
    assert!(!tree.path("b/one.jpg").exists());
    assert_eq!(
        app.state
            .plans
            .get(first.id)
            .await
            .unwrap()
            .result
            .unwrap()
            .done,
        1
    );
    let result = app
        .state
        .plans
        .get(second.id)
        .await
        .unwrap()
        .result
        .unwrap();
    assert_eq!((result.done, result.skipped, result.failed), (0, 1, 0));
    app.state.begin_shutdown();
}

#[tokio::test]
async fn both_delete_actions_use_one_trash_batch_and_restore() {
    for action in [
        DuplicateAction::DeleteSource,
        DuplicateAction::DeleteComparison,
    ] {
        let app = TestApp::new().await;
        let (volume, s, c, tree, plan) = disk_pair(&app, MatchMode::Hash).await;
        let (result, task) = run(&app, plan.id, action, None).await;
        assert_eq!(task.state, TaskState::Done);
        assert_eq!((result.done, result.failed, result.bytes), (1, 0, 4));
        assert_eq!((task.items_done, task.bytes_done), (1, 4));
        let deleted = if action == DuplicateAction::DeleteSource {
            s
        } else {
            c
        };
        let (orig, trash_id): (String, i64) =
            sqlx::query_as("SELECT orig_path, id FROM trash_items WHERE volume_id = ?")
                .bind(volume)
                .fetch_one(app.state.db.read())
                .await
                .unwrap();
        assert!(!tree.path(&orig).exists());
        assert!(
            !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM files WHERE id = ?)")
                .bind(deleted)
                .fetch_one(app.state.db.read())
                .await
                .unwrap()
        );
        app.state
            .fsops
            .restore(
                trash_id,
                ConflictPolicy::Smart,
                &opm_server::fsops::OpContext::detached(),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(tree.path(&orig)).unwrap(), b"same");
        app.state.begin_shutdown();
    }
}

#[tokio::test]
async fn both_actions_on_overlapping_scopes_keep_a_survivor() {
    for action in [
        DuplicateAction::DeleteSource,
        DuplicateAction::DeleteComparison,
    ] {
        let app = TestApp::new().await;
        let (volume, tree) = app.add_volume("overlap").await;
        let seed = Seed::new(&app.state.db);
        for index in 0..3 {
            let dir = format!("p{index}");
            tree.write(&format!("{dir}/one.jpg"), b"same");
            seed.file(volume, &dir, "one.jpg")
                .size(4)
                .hash(sha1_bytes(b"same"))
                .insert()
                .await;
        }
        let plan = build(
            &app,
            &params(MatchMode::Hash, side(volume, ""), side(volume, "")),
        )
        .await;
        assert_eq!(plan.stats["pairs"], 3);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.code == "overlapping_scopes" && w.count == 3)
        );
        let (result, _) = run(&app, plan.id, action, None).await;
        assert_eq!((result.done, result.failed), (2, 0));
        let batches: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT batch) FROM trash_items")
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
        assert_eq!(batches, 1);
        let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
        assert_eq!(files, 1);
        app.state.begin_shutdown();
    }
}

#[tokio::test]
async fn nontransitive_matches_cannot_delete_both_copies() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("nontransitive").await;
    let seed = Seed::new(&app.state.db);
    for (dir, time) in [("a", 1), ("b", 2), ("c", 1)] {
        tree.write(&format!("{dir}/one.jpg"), b"same");
        seed.file(volume, dir, "one.jpg")
            .size(4)
            .capture(time)
            .insert()
            .await;
    }
    let plan = build(
        &app,
        &params(
            MatchMode::NameDifferentTime,
            side(volume, ""),
            side(volume, ""),
        ),
    )
    .await;
    let (result, task) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(result.done, 1);
    assert_eq!(result.failures[0].code, "no_survivor");
    assert!(tree.path("a/one.jpg").exists());
    assert!(tree.path("c/one.jpg").exists());
    app.state.begin_shutdown();
}

#[tokio::test]
async fn live_index_and_disk_changes_refuse_unsafe_deletion() {
    for change in [
        "doomed_size",
        "doomed_mtime",
        "doomed_gone",
        "doomed_disk_gone",
        "survivor_gone",
        "survivor_size",
        "live_hash",
        "live_kind",
        "live_time",
    ] {
        let app = TestApp::new().await;
        let mode = if change == "live_time" {
            MatchMode::NameAndTime
        } else {
            MatchMode::Hash
        };
        let (_, s, c, tree, plan) = disk_pair(&app, mode).await;
        match change {
            "doomed_size" => tree.write("b/one.jpg", b"changed"),
            "doomed_mtime" => tree.set_mtime("b/one.jpg", common::BASE_MTIME + 5),
            "doomed_gone" => {
                sqlx::query("DELETE FROM files WHERE id = ?")
                    .bind(c)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "doomed_disk_gone" => {
                std::fs::remove_file(tree.path("b/one.jpg")).unwrap();
            }
            "survivor_gone" => {
                std::fs::remove_file(tree.path("a/one.jpg")).unwrap();
            }
            "survivor_size" => tree.write("a/one.jpg", b"changed"),
            "live_hash" => {
                sqlx::query("UPDATE files SET hash = ? WHERE id = ?")
                    .bind(sha1_bytes(b"other").as_slice())
                    .bind(s)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "live_kind" => {
                sqlx::query("UPDATE files SET kind = 'other' WHERE id = ?")
                    .bind(s)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            "live_time" => {
                sqlx::query("UPDATE files SET capture_ts = 5 WHERE id = ?")
                    .bind(s)
                    .execute(app.state.db.write())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (result, task) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
        assert_eq!(task.state, TaskState::Done);
        assert_eq!(
            (result.done, result.skipped, result.failed),
            (0, 1, 0),
            "{change}"
        );
        assert_eq!(
            tree.path("b/one.jpg").exists(),
            change != "doomed_disk_gone"
        );
        let expected = match change {
            "doomed_size" | "doomed_mtime" => "stale_index",
            "doomed_gone" | "doomed_disk_gone" => "file_gone",
            _ => "stale_match",
        };
        assert_eq!(result.failures[0].code, expected, "{change}");
        app.state.begin_shutdown();
    }
}

#[tokio::test]
async fn name_mode_tolerates_survivor_size_changes_and_only_restricts_deletion() {
    let app = TestApp::new().await;
    let (volume, _, c, tree, _) = disk_pair(&app, MatchMode::NameAndTime).await;
    tree.write("a/one.jpg", b"changed");
    tree.write("b/extra/one.jpg", b"same");
    let extra = Seed::new(&app.state.db)
        .file(volume, "b/extra", "one.jpg")
        .size(4)
        .capture(1)
        .insert()
        .await;
    let plan = build(
        &app,
        &params(MatchMode::NameAndTime, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let only = Seed::new(&app.state.db).selection(&[c]).await;
    let (result, _) = run(&app, plan.id, DuplicateAction::DeleteComparison, Some(only)).await;
    assert_eq!(result.done, 1);
    assert!(!tree.path("b/one.jpg").exists());
    assert!(tree.path("b/extra/one.jpg").exists());
    assert!(tree.path("a/one.jpg").exists());
    assert_ne!(extra, c);
    app.state.begin_shutdown();
}

#[tokio::test]
async fn shared_sidecars_follow_only_successfully_trashed_last_owner() {
    let app = TestApp::new().await;
    let (volume, s, c, tree, _) = disk_pair(&app, MatchMode::Hash).await;
    tree.write("b/one.CR3", b"same");
    tree.write("b/one.xmp", b"meta");
    let seed = Seed::new(&app.state.db);
    let raw = seed
        .file(volume, "b", "one.CR3")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let xmp = seed.file(volume, "b", "one.xmp").size(4).insert().await;
    seed.sidecar(c, xmp, "stem").await;
    seed.sidecar(raw, xmp, "stem").await;
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let only = seed.selection(&[c]).await;
    let (result, _) = run(&app, plan.id, DuplicateAction::DeleteComparison, Some(only)).await;
    assert_eq!(result.done, 1);
    assert!(
        tree.path("b/one.xmp").exists(),
        "remaining RAW owns the sidecar"
    );
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let (result, _) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
    assert_eq!(result.done, 1);
    assert!(!tree.path("b/one.xmp").exists());
    assert!(tree.path("a/one.jpg").exists());
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM files WHERE id = ?)")
            .bind(s)
            .fetch_one(app.state.db.read())
            .await
            .unwrap()
    );
    app.state.begin_shutdown();
}

#[tokio::test]
async fn invalid_scope_and_filter_name_the_offending_side() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("v").await;
    for (label, scope) in [("source.scope", true), ("comparison.filter", false)] {
        let mut p = params(MatchMode::Hash, side(volume, ""), side(volume, ""));
        if scope {
            p.source.scope.dir = "../escape".to_owned();
        } else {
            p.comparison.filter.ratings = vec![6];
        }
        let (status, value) = app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({"kind":"duplicates", "params":p, "replaces":null})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(value["error"]["message"].as_str().unwrap().contains(label));
    }
    app.state.begin_shutdown();
}
fn deps(app: &TestApp, fsops: FsOps) -> PlanDeps {
    PlanDeps {
        db: app.state.db.clone(),
        fsops,
        settings: app.state.settings.clone(),
        shutdown: app.state.shutdown.clone(),
        tasks: app.state.tasks.clone(),
        volumes: app.state.volumes.clone(),
    }
}

struct DenyFirst(AtomicBool);
impl Mover for DenyFirst {
    fn rename_no_replace(
        &self,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> std::io::Result<()> {
        if !self.0.swap(true, Ordering::SeqCst) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        SystemMover.rename_no_replace(from, to)
    }
}

#[tokio::test]
async fn permission_failure_continues_and_sidecars_of_failed_owner_stay() {
    let mut app = TestApp::new().await;
    let fsops = app
        .state
        .fsops
        .clone()
        .with_mover(Arc::new(DenyFirst(AtomicBool::new(false))));
    app.state.plans = PlanService::new(deps(&app, fsops)).await.unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (volume, _, c, tree, _) = disk_pair(&app, MatchMode::Hash).await;
    tree.write("b/sub/one.jpg", b"same");
    tree.write("b/one.xmp", b"meta");
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "b/sub", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let xmp = seed.file(volume, "b", "one.xmp").size(4).insert().await;
    seed.sidecar(c, xmp, "exact").await;
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let (result, task) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!((result.done, result.failed), (1, 1));
    assert_eq!(result.failures[0].code, "io_error");
    assert!(tree.path("b/one.jpg").exists());
    assert!(tree.path("b/one.xmp").exists());
    assert!(!tree.path("b/sub/one.jpg").exists());
    app.state.begin_shutdown();
}

#[tokio::test]
async fn restart_finishes_duplicate_deletion_and_the_first_owners_sidecar() {
    let mut app = TestApp::new().await;
    struct StopOnApply(opm_server::tasks::TaskEngine);
    impl OpHook for StopOnApply {
        fn on_applied<'a>(
            &'a self,
            _tx: &'a mut sqlx::SqliteConnection,
            _op: &'a AppliedOp,
        ) -> futures_util::future::BoxFuture<'a, Result<(), sqlx::Error>> {
            Box::pin(async move {
                self.0.shutdown_token().cancel();
                Ok(())
            })
        }
    }
    let fsops = app
        .state
        .fsops
        .clone()
        .with_hook(Arc::new(StopOnApply(app.state.tasks.clone())));
    app.state.plans = PlanService::new(deps(&app, fsops)).await.unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (volume, _, owner, tree, _) = disk_pair(&app, MatchMode::Hash).await;
    tree.write("b/sub/one.jpg", b"same");
    tree.write("b/one.xmp", b"meta");
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "b/sub", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let xmp = seed.file(volume, "b", "one.xmp").size(4).insert().await;
    seed.sidecar(owner, xmp, "exact").await;
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let (status, body) = app
        .call(
            Method::POST,
            &format!("/api/plans/{}/execute", plan.id),
            Some(json!({"options": {"action": "delete_comparison"}, "only": null})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["id"].as_i64().unwrap();
    assert_eq!(
        app.state.tasks.wait_finished(id).await.unwrap().state,
        TaskState::Interrupted
    );
    app.state.begin_shutdown();
    app.state.shutdown().await;
    let recovered = opm_server::state::AppState::new((*app.state.config).clone())
        .await
        .unwrap();
    let task = recovered.tasks.wait_finished(id).await.unwrap();
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    let result = recovered.plans.get(plan.id).await.unwrap().result.unwrap();
    assert_eq!((result.done, result.failed), (2, 0), "{result:?}");
    assert_eq!(
        (task.items_done, task.items_total, task.bytes_total),
        (2, Some(2), Some(8))
    );
    assert!(!tree.path("b/one.jpg").exists());
    assert!(!tree.path("b/one.xmp").exists());
    assert!(!tree.path("b/sub/one.jpg").exists());
    assert!(tree.path("a/one.jpg").exists());
    let batches: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT batch) FROM trash_items")
        .fetch_one(recovered.db.read())
        .await
        .unwrap();
    assert_eq!(batches, 1);
    recovered.begin_shutdown();
    recovered.shutdown().await;
}

#[tokio::test]
async fn cancellation_after_one_trash_preserves_partial_result() {
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
    app.state.plans = PlanService::new(deps(&app, fsops)).await.unwrap();
    app.router = opm_server::app::router(app.state.clone());
    let (volume, _, _, tree, _) = disk_pair(&app, MatchMode::Hash).await;
    tree.write("b/sub/one.jpg", b"same");
    Seed::new(&app.state.db)
        .file(volume, "b/sub", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"same"))
        .insert()
        .await;
    let plan = build(
        &app,
        &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
    )
    .await;
    let (result, task) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
    assert!(result.cancelled);
    assert_eq!((result.done, result.failed, task.items_done), (1, 0, 1));
    assert!(tree.path("b/sub/one.jpg").exists());
    assert!(!tree.path("b/one.jpg").exists());
}

#[tokio::test]
async fn overflow_and_abandoned_builds_remove_all_child_rows() {
    let mut app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    for volume in [a, b] {
        for i in 0..300 {
            seed.file(volume, &format!("d{i}"), "same.jpg")
                .hash(sha1_bytes(b"same"))
                .insert()
                .await;
        }
    }
    let p = params(MatchMode::Hash, side(a, ""), side(b, ""));
    let d = deps(&app, app.state.fsops.clone());
    let planner = Arc::new(opm_server::plans::duplicates::DuplicatesPlanner::new(&d));
    app.state.plans = PlanService::with_options(
        d,
        vec![planner],
        PlanOptions {
            max_items: 10,
            ..PlanOptions::default()
        },
    )
    .await
    .unwrap();
    let err = app
        .state
        .plans
        .create(CreatePlanRequest {
            kind: PlanKind::Duplicates,
            params: json!(p),
            replaces: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "plan_too_large");
    app.state.plans.settle().await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplicate_pairs")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.state.plans = PlanService::new(deps(&app, app.state.fsops.clone()))
        .await
        .unwrap();
    let service = app.state.plans.clone();
    let request = tokio::spawn(async move {
        service
            .create(CreatePlanRequest {
                kind: PlanKind::Duplicates,
                params: json!(p),
                replaces: None,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplicate_pairs")
                .fetch_one(app.state.db.read())
                .await
                .unwrap();
            if count >= 2000 {
                break;
            }
            assert!(
                !request.is_finished(),
                "build finished before cancellation test could intercept"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    app.state.plans.settle().await;
    for table in ["duplicate_pairs", "duplicate_matches"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
                .fetch_one(app.state.db.read())
                .await
                .unwrap(),
            0
        );
    }
    app.state.begin_shutdown();
}

#[tokio::test]
async fn reused_ids_never_resurrect_targets_or_survivors_in_old_plans() {
    for replace_source in [false, true] {
        let app = TestApp::new().await;
        let (volume, s, c, tree, plan) = disk_pair(&app, MatchMode::Hash).await;
        let id = if replace_source { s } else { c };
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(id)
            .execute(app.state.db.write())
            .await
            .unwrap();
        // Even a new row with identical facts and path is not the snapshot's original identity.
        sqlx::query("INSERT INTO files (id, volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime, hash, capture_ts) VALUES (?, ?, ?, 'one.jpg', 'one.jpg', 'one', 'image', 4, ?, ?, 1)")
            .bind(id).bind(volume).bind(if replace_source { "a" } else { "b" }).bind(common::BASE_MTIME).bind(sha1_bytes(b"same").as_slice())
            .execute(app.state.db.write()).await.unwrap();
        let tombstone: i64 = sqlx::query_scalar(
            "SELECT live FROM duplicate_matches WHERE plan_id = ? AND file_id = ?",
        )
        .bind(plan.id)
        .bind(id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
        assert_eq!(tombstone, 0);
        let flags = app
            .state
            .plans
            .item_flags(
                PlanRef {
                    plan_id: plan.id,
                    side: if replace_source {
                        PlanSide::Source
                    } else {
                        PlanSide::Comparison
                    },
                },
                &[id],
            )
            .await
            .unwrap();
        assert!(flags.is_empty());
        let (result, _) = run(&app, plan.id, DuplicateAction::DeleteComparison, None).await;
        assert_eq!((result.done, result.skipped, result.failed), (0, 1, 0));
        assert_eq!(
            result.failures[0].code,
            if replace_source {
                "stale_match"
            } else {
                "file_gone"
            }
        );
        assert!(tree.path("a/one.jpg").exists());
        assert!(tree.path("b/one.jpg").exists());
        let fresh = build(
            &app,
            &params(MatchMode::Hash, side(volume, "a"), side(volume, "b")),
        )
        .await;
        assert_eq!(
            fresh.stats["pairs"], 1,
            "new rows can participate only in a fresh plan"
        );
        app.state.begin_shutdown();
    }
}
