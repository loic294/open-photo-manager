//! Batch rename plans over indexed disposable files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{
    ConflictPolicy, PlanKind, RenameMatchMode, RenameMatcher, RenameParams, RenameRewrite,
    RenameRules, Scope, SideParams,
};
use apm_core::hash::sha1_bytes;
use apm_core::paths::FileId;
use axum::http::{Method, StatusCode};
use common::TestApp;
use common::browse;
use common::seed::Seed;
use serde_json::{Value, json};

fn side(volume: i64, dir: &str, recursive: bool) -> SideParams {
    SideParams {
        scope: Scope {
            volume_id: Some(volume),
            dir: dir.to_owned(),
            recursive,
        },
        filter: Default::default(),
    }
}

fn rules(pattern: &str) -> RenameRules {
    RenameRules {
        pattern: pattern.to_owned(),
        ..RenameRules::default()
    }
}

fn params(
    source: SideParams,
    rules: RenameRules,
    conflict_policy: Option<ConflictPolicy>,
) -> RenameParams {
    RenameParams {
        source,
        rules,
        conflict_policy,
    }
}

async fn ready(app: &TestApp) {
    sqlx::query("UPDATE files SET meta_state = 1")
        .execute(app.state.db.write())
        .await
        .unwrap();
}

async fn build(app: &TestApp, params: &RenameParams) -> Value {
    let (status, body) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "rename",
                "params": params,
                "replaces": null
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn move_rows(
    app: &TestApp,
    plan_id: i64,
) -> Vec<(FileId, String, String, String, Option<i64>)> {
    sqlx::query_as(
        "SELECT file_id, dst_dir, dst_name, status, seq FROM move_items \
         WHERE plan_id = ? ORDER BY src_dir, src_name",
    )
    .bind(plan_id)
    .fetch_all(app.state.db.read())
    .await
    .unwrap()
}

async fn execute(app: &TestApp, plan_id: i64, only: Option<i64>) -> Value {
    let (status, task) = app
        .call(
            Method::POST,
            &format!("/api/plans/{plan_id}/execute"),
            Some(json!({"options": {}, "only": only})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    app.state
        .tasks
        .wait_finished(task["id"].as_i64().unwrap())
        .await
        .unwrap();
    let (status, result) = app
        .call(Method::GET, &format!("/api/plans/{plan_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["result"].clone()
}

#[tokio::test]
async fn plan_overlay_selection_and_only_execution_keep_rename_in_place() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    for (name, bytes) in [
        ("one.jpg", b"one".as_slice()),
        ("other.jpg", b"other".as_slice()),
        ("z-notes.txt", b"notes".as_slice()),
    ] {
        tree.write(&format!("album/child/{name}"), bytes);
    }
    let seed = Seed::new(&app.state.db);
    let one = seed
        .file(volume, "album/child", "one.jpg")
        .size(3)
        .insert()
        .await;
    let other = seed
        .file(volume, "album/child", "other.jpg")
        .size(5)
        .insert()
        .await;
    seed.file(volume, "album/child", "z-notes.txt")
        .size(5)
        .insert()
        .await;
    ready(&app).await;

    let plan = build(
        &app,
        &params(
            side(volume, "album", true),
            {
                let mut rules = rules("Renamed_{name}");
                rules.matcher = RenameMatcher {
                    mode: RenameMatchMode::Glob,
                    pattern: "*.jpg".to_owned(),
                    ignore_case: true,
                };
                rules
            },
            None,
        ),
    )
    .await;
    let plan_id = plan["id"].as_i64().unwrap();
    assert_eq!(
        serde_json::from_value::<PlanKind>(plan["kind"].clone()).unwrap(),
        PlanKind::Rename
    );
    assert_eq!(plan["stats"]["candidates"], 3);
    assert_eq!(plan["stats"]["matched"], 2);
    assert_eq!(plan["stats"]["to_rename"], 2);
    assert_eq!(plan["stats"]["already_in_place"], 0);
    assert_eq!(plan["warnings"], json!([]));

    let (status, source) = browse::items(
        &app,
        json!({
            "volume_id": volume,
            "dir": "album/child",
            "plan": {"plan_id": plan_id, "side": "source"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source}");
    assert_eq!(source["total"], 3);
    assert_eq!(source["items"][0]["id"], one);
    assert_eq!(source["items"][0]["overlay"]["matched"], true);
    assert_eq!(source["items"][1]["id"], other);
    assert_eq!(source["items"][1]["overlay"]["matched"], true);
    assert_eq!(source["items"][2]["overlay"]["matched"], false);

    let (status, comparison) = browse::items(
        &app,
        json!({
            "volume_id": volume,
            "dir": "album/child",
            "plan": {"plan_id": plan_id, "side": "comparison"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{comparison}");
    assert_eq!(comparison["total"], 5);
    assert_eq!(
        browse::names(&comparison, "items"),
        [
            "one.jpg",
            "other.jpg",
            "z-notes.txt",
            "Renamed_one.jpg",
            "Renamed_other.jpg"
        ]
    );
    assert_eq!(comparison["items"][3]["id"], one);
    assert_eq!(comparison["items"][3]["overlay"]["planned"], true);
    assert_eq!(comparison["items"][4]["id"], other);

    let (status, ids) = browse::ids(
        &app,
        json!({
            "volume_id": volume,
            "dir": "album/child",
            "plan": {"plan_id": plan_id, "side": "comparison"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ids}");
    assert_eq!(ids["ids"].as_array().unwrap().len(), 3);
    assert!(ids["ids"].as_array().unwrap().contains(&json!(one)));
    assert_eq!(ids["ids"].as_array().unwrap()[0], json!(one));

    let (status, folders) = browse::folders(
        &app,
        json!({
            "volume_id": volume,
            "dir": "album",
            "plan": {"plan_id": plan_id, "side": "comparison"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{folders}");
    assert_eq!(folders["folders"][0]["name"], "child");
    assert_eq!(folders["folders"][0]["count"], 3);
    assert_eq!(folders["folders"][0]["total"], 3);
    assert_eq!(folders["folders"][0]["overlay"]["planned"], false);
    assert_eq!(folders["counts"]["in_subfolders"], 3);

    let (status, handoff) = app
        .call(
            Method::POST,
            &format!("/api/plans/{plan_id}/selection"),
            Some(json!({"side": "source"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{handoff}");
    assert_eq!(handoff["item_count"], 2);

    let only = seed.selection(&[one]).await;
    let result = execute(&app, plan_id, Some(only)).await;
    assert_eq!(result["done"], 1);
    assert_eq!(result["skipped"], 0);
    assert_eq!(result["failed"], 0);
    assert!(tree.path("album/child/Renamed_one.jpg").exists());
    assert!(!tree.path("album/child/one.jpg").exists());
    assert!(tree.path("album/child/other.jpg").exists());
    assert!(!tree.path("album/child/Renamed_other.jpg").exists());
}

#[tokio::test]
async fn preview_uses_the_first_matching_folder_and_matches_the_plan_counters() {
    let app = TestApp::new().await;
    let (volume, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "z-folder", "last.jpg").insert().await;
    for index in 1..=100 {
        seed.file(volume, "a-folder", &format!("photo-{index:03}.jpg"))
            .capture(1_700_000_000 + i64::from(index))
            .insert()
            .await;
    }
    ready(&app).await;
    let mut rename_rules = rules("Renamed_{count}");
    rename_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::Glob,
        pattern: "*.jpg".to_owned(),
        ignore_case: true,
    };
    rename_rules.count_digits = 3;
    let request = json!({
        "rules": rename_rules,
        "source": side(volume, "", true),
        "limit": 5
    });
    let (status, preview) = app
        .call(Method::POST, "/api/rename/preview", Some(request.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["dir"], "a-folder");
    assert_eq!(preview["folder_files"], 100);
    assert_eq!(preview["matched"], 100);
    assert_eq!(preview["truncated"], false);
    assert_eq!(
        browse::names(&preview, "samples"),
        [
            "photo-001.jpg",
            "photo-002.jpg",
            "photo-003.jpg",
            "photo-004.jpg",
            "photo-005.jpg"
        ]
    );
    assert_eq!(
        preview["samples"][4]["new_name"], "Renamed_005.jpg",
        "the count is exact even beyond the returned sample limit"
    );

    let plan = build(
        &app,
        &params(side(volume, "", true), rename_rules.clone(), None),
    )
    .await;
    let plan_id = plan["id"].as_i64().unwrap();
    let planned: Vec<(FileId, String)> = sqlx::query_as(
        "SELECT file_id, dst_name FROM move_items WHERE plan_id = ? AND src_dir = 'a-folder' ORDER BY src_name",
    )
    .bind(plan_id)
    .fetch_all(app.state.db.read())
    .await
    .unwrap();
    let samples: Vec<(FileId, String)> = preview["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sample| {
            (
                sample["file_id"].as_i64().unwrap(),
                sample["new_name"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(samples, planned[..5]);
    let plan_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(plan_count, 1, "preview does not create plan rows");

    let invalid = RenameRules {
        matcher: RenameMatcher {
            mode: RenameMatchMode::Regex,
            pattern: "(".to_owned(),
            ignore_case: true,
        },
        rewrite: Some(RenameRewrite {
            find: "[".to_owned(),
            replace: String::new(),
            ignore_case: false,
        }),
        pattern: "{unknown}".to_owned(),
        count_digits: 10,
        ..RenameRules::default()
    };
    let (status, issues) = app
        .call(
            Method::POST,
            "/api/rename/preview",
            Some(json!({
                "rules": invalid,
                "source": side(volume, "", true)
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{issues}");
    assert!(issues["issues"].as_array().unwrap().len() >= 4);
    assert_eq!(issues["samples"], json!([]));

    let (status, clamped) = app
        .call(
            Method::POST,
            "/api/rename/preview",
            Some(json!({
                "rules": rename_rules,
                "source": side(volume, "", true),
                "limit": 0
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{clamped}");
    assert_eq!(clamped["samples"].as_array().unwrap().len(), 1);
    let (status, tokens) = app.call(Method::GET, "/api/rename/tokens", None).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert!(
        tokens["tokens"]
            .as_array()
            .is_some_and(|tokens| !tokens.is_empty())
    );
    assert!(
        tokens["examples"]
            .as_array()
            .is_some_and(|examples| !examples.is_empty())
    );

    let (status, invalid_source) = app
        .call(
            Method::POST,
            "/api/rename/preview",
            Some(json!({
                "rules": rules("{name}"),
                "source": side(0, "", true)
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid_source}");
}

#[tokio::test]
async fn preview_caps_counter_input_and_returns_a_truncation_marker() {
    let app = TestApp::new().await;
    let (volume, _) = app.add_volume("photos").await;
    Seed::new(&app.state.db).dir(volume, "album").await;
    sqlx::query(
        "WITH digits(d) AS (VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)), \
         numbers(n) AS (SELECT a.d + b.d * 10 + c.d * 100 + d.d * 1000 + e.d * 10000 + 1 \
         FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d CROSS JOIN digits e) \
         INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, meta_state) \
         SELECT ?, 'album', printf('f%05d.jpg', n), printf('f%05d.jpg', n), \
         printf('f%05d', n), 'jpg', 'image', 1, 1700000000, 1 FROM numbers WHERE n <= 20001",
    )
    .bind(volume)
    .execute(app.state.db.write())
    .await
    .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    let mut count_rules = rules("Photo_{count}");
    count_rules.count_digits = 3;
    let (status, preview) = app
        .call(
            Method::POST,
            "/api/rename/preview",
            Some(json!({
                "rules": count_rules,
                "source": side(volume, "album", true),
                "limit": 500
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["matched"], 20001);
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["samples"].as_array().unwrap().len(), 200);
    assert_eq!(preview["samples"][0]["new_name"], "Photo_001.jpg");
    assert_eq!(preview["samples"][199]["new_name"], "Photo_200.jpg");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn conflicts_and_execution_revalidation_use_the_shared_move_rules() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("album/a.jpg", b"a");
    tree.write("album/b.jpg", b"b");
    let seed = Seed::new(&app.state.db);
    let a = seed
        .file(volume, "album", "a.jpg")
        .size(1)
        .capture(1_600_000_000)
        .insert()
        .await;
    let b = seed
        .file(volume, "album", "b.jpg")
        .size(1)
        .capture(1_700_000_000)
        .insert()
        .await;
    seed.dir(volume, "album/Same.jpg").await;
    ready(&app).await;

    let mut conflict_rules = rules("Same");
    conflict_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::All,
        pattern: String::new(),
        ignore_case: true,
    };
    let plan = build(
        &app,
        &params(
            side(volume, "album", false),
            conflict_rules,
            Some(ConflictPolicy::Rename),
        ),
    )
    .await;
    let id = plan["id"].as_i64().unwrap();
    let rows = move_rows(&app, id).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.3 == "move"));
    assert_eq!(rows[0].2, "Same_1.jpg");
    assert_eq!(rows[1].2, "Same_2.jpg");
    assert_eq!(plan["stats"]["renamed_by_policy"], 2);

    let (status, replacement) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "rename",
                "params": params(
                    side(volume, "album", false),
                    rules("Same"),
                    Some(ConflictPolicy::Replace)
                ),
                "replaces": null
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{replacement}");
    assert_eq!(replacement["error"]["code"], "invalid_plan_params");

    let mut date_rules = rules("{YYYY}_{name}");
    date_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::All,
        pattern: String::new(),
        ignore_case: true,
    };
    let date_plan = build(
        &app,
        &params(side(volume, "album", false), date_rules, None),
    )
    .await;
    let date_plan_id = date_plan["id"].as_i64().unwrap();
    sqlx::query("UPDATE files SET capture_ts = 1800000000 WHERE id = ?")
        .bind(a)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let date_result = execute(&app, date_plan_id, None).await;
    assert_eq!(date_result["done"], 1);
    assert_eq!(date_result["skipped"], 1);
    assert_eq!(
        date_result["failures"][0]["code"], "stale_plan",
        "a file fact that changes the rendered name cannot be renamed under the old plan"
    );
    assert!(tree.path("album/a.jpg").exists());
    assert!(!tree.path("album/b.jpg").exists());
    assert!(tree.path("album/2023_b.jpg").exists());
    assert!(a != b);
}

#[tokio::test]
async fn already_in_place_case_changes_and_same_name_conflicts_are_distinct() {
    let app = TestApp::new().await;
    let (volume, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "album", "same.jpg").insert().await;
    seed.file(volume, "album", "upper.jpg").insert().await;
    seed.file(volume, "album", "lower.jpg").insert().await;
    seed.file(volume, "collisions", "upper.jpg").insert().await;
    seed.file(volume, "collisions", "lower.jpg").insert().await;
    ready(&app).await;

    let unchanged = build(
        &app,
        &params(side(volume, "album", false), rules("{name}"), None),
    )
    .await;
    assert_eq!(unchanged["stats"]["already_in_place"], 3);
    assert_eq!(unchanged["stats"]["to_rename"], 0);

    let case_only = build(
        &app,
        &params(
            side(volume, "album", false),
            {
                let mut rules = rules("UPPER");
                rules.matcher = RenameMatcher {
                    mode: RenameMatchMode::Regex,
                    pattern: "^upper\\.jpg$".to_owned(),
                    ignore_case: false,
                };
                rules
            },
            None,
        ),
    )
    .await;
    assert_eq!(case_only["stats"]["already_in_place"], 0);
    assert_eq!(case_only["stats"]["to_rename"], 1);
    assert_eq!(
        move_rows(&app, case_only["id"].as_i64().unwrap()).await[0].2,
        "UPPER.jpg"
    );

    for (policy, expected_statuses, expected_names) in [
        (
            ConflictPolicy::Smart,
            ["move", "move"],
            ["Same.jpg", "Same_1.jpg"],
        ),
        (
            ConflictPolicy::Skip,
            ["move", "skipped_by_policy"],
            ["Same.jpg", "Same.jpg"],
        ),
    ] {
        let plan = build(
            &app,
            &params(
                side(volume, "collisions", false),
                {
                    let mut rules = rules("Same");
                    rules.matcher = RenameMatcher {
                        mode: RenameMatchMode::Regex,
                        pattern: "^(upper|lower)\\.jpg$".to_owned(),
                        ignore_case: false,
                    };
                    rules
                },
                Some(policy),
            ),
        )
        .await;
        let rows = move_rows(&app, plan["id"].as_i64().unwrap()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!([rows[0].2.as_str(), rows[1].2.as_str()], expected_names);
        assert_eq!([rows[0].3.as_str(), rows[1].3.as_str()], expected_statuses);
    }
}

#[tokio::test]
async fn default_replace_setting_falls_back_to_smart_for_rename() {
    let app = TestApp::new().await;
    let (volume, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "album", "one.jpg")
        .size(4)
        .hash(sha1_bytes(b"one!"))
        .insert()
        .await;
    let target = seed
        .file(volume, "album", "New_one.jpg")
        .size(4)
        .hash(sha1_bytes(b"else"))
        .insert()
        .await;
    ready(&app).await;
    let mut settings = (*app.state.settings.get()).clone();
    settings.default_conflict_policy = ConflictPolicy::Replace;
    app.state.settings.update(settings).await.unwrap();

    let mut rename_rules = rules("New_{name}");
    rename_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::Regex,
        pattern: "^one\\.jpg$".to_owned(),
        ignore_case: false,
    };
    let plan = build(
        &app,
        &params(side(volume, "album", false), rename_rules, None),
    )
    .await;
    let row = move_rows(&app, plan["id"].as_i64().unwrap()).await[0].clone();
    assert_eq!(row.2, "New_one_1.jpg");
    let replaced: Option<i64> =
        sqlx::query_scalar("SELECT replaced_id FROM move_items WHERE plan_id = ? AND file_id = ?")
            .bind(plan["id"].as_i64().unwrap())
            .bind(row.0)
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
    assert_eq!(replaced, None);
    let target_name: String = sqlx::query_scalar("SELECT name FROM files WHERE id = ?")
        .bind(target)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(target_name, "New_one.jpg");
}

#[tokio::test]
async fn execution_uses_stored_folder_counters_and_moves_shared_sidecars_once() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    for (name, bytes) in [
        ("one.CR3", b"raw".as_slice()),
        ("one.JPG", b"jpeg".as_slice()),
        ("one.xmp", b"metadata".as_slice()),
        ("one.CR3.xmp", b"full-name metadata".as_slice()),
        ("b.jpg", b"b".as_slice()),
        ("a.jpg", b"a".as_slice()),
    ] {
        tree.write(&format!("album/{name}"), bytes);
    }
    let seed = Seed::new(&app.state.db);
    let raw = seed.file(volume, "album", "one.CR3").size(3).insert().await;
    let jpeg = seed.file(volume, "album", "one.JPG").size(4).insert().await;
    let sidecar = seed.file(volume, "album", "one.xmp").size(8).insert().await;
    let exact = seed
        .file(volume, "album", "one.CR3.xmp")
        .size(18)
        .insert()
        .await;
    seed.sidecar(raw, sidecar, "stem").await;
    seed.sidecar(jpeg, sidecar, "stem").await;
    seed.sidecar(raw, exact, "exact").await;
    let a = seed
        .file(volume, "album", "a.jpg")
        .size(1)
        .capture(1_700_000_000)
        .insert()
        .await;
    let b = seed
        .file(volume, "album", "b.jpg")
        .size(1)
        .capture(1_700_000_100)
        .insert()
        .await;
    ready(&app).await;

    let mut count_rules = rules("Photo_{count}");
    count_rules.count_digits = 3;
    count_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::Regex,
        pattern: "^[ab]\\.jpg$".to_owned(),
        ignore_case: false,
    };
    let count_plan = build(
        &app,
        &params(side(volume, "album", false), count_rules, None),
    )
    .await;
    let count_plan_id = count_plan["id"].as_i64().unwrap();
    let stored_params: String = sqlx::query_scalar("SELECT params_json FROM plans WHERE id = ?")
        .bind(count_plan_id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored_params).unwrap()["rules"]["count_digits"],
        3
    );
    let original_counts = move_rows(&app, count_plan_id).await;
    assert_eq!(
        original_counts
            .iter()
            .map(|row| (row.0, row.2.as_str(), row.4))
            .collect::<Vec<_>>(),
        [(a, "Photo_001.jpg", Some(1)), (b, "Photo_002.jpg", Some(2))]
    );

    tree.write("album/0.jpg", b"new");
    let inserted = seed
        .file(volume, "album", "0.jpg")
        .size(3)
        .capture(1_699_999_900)
        .insert()
        .await;
    let mut added_count_rules = rules("New_{name}");
    added_count_rules.matcher = RenameMatcher {
        mode: RenameMatchMode::Regex,
        pattern: "^one\\.(CR3|JPG)$".to_owned(),
        ignore_case: false,
    };
    let sidecar_plan = build(
        &app,
        &params(side(volume, "album", false), added_count_rules, None),
    )
    .await;
    let sidecar_result = execute(&app, sidecar_plan["id"].as_i64().unwrap(), None).await;
    assert_eq!(sidecar_result["done"], 2);
    assert!(!tree.path("album/one.CR3").exists());
    assert!(!tree.path("album/one.JPG").exists());
    assert!(tree.path("album/one.xmp").exists());
    assert!(!tree.path("album/one.CR3.xmp").exists());
    assert!(tree.path("album/New_one.CR3").exists());
    assert!(tree.path("album/New_one.JPG").exists());
    assert!(tree.path("album/New_one.xmp").exists());
    assert!(tree.path("album/New_one.CR3.xmp").exists());
    let copies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fs_journal WHERE op = 'copy'")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(copies, 1);

    let count_result = execute(&app, count_plan_id, None).await;
    assert_eq!(count_result["done"], 2);
    assert!(tree.path("album/Photo_001.jpg").exists());
    assert!(tree.path("album/Photo_002.jpg").exists());
    assert!(tree.path("album/0.jpg").exists());
    assert!(inserted > 0);
}

#[tokio::test]
async fn execution_reports_deleted_stale_and_disk_changed_sources_without_renaming_them() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    for (dir, name, bytes) in [
        ("gone", "gone.jpg", b"gone".as_slice()),
        ("stale", "stale.jpg", b"stale".as_slice()),
        ("changed", "changed.jpg", b"changed".as_slice()),
    ] {
        tree.write(&format!("{dir}/{name}"), bytes);
    }
    let seed = Seed::new(&app.state.db);
    let gone = seed.file(volume, "gone", "gone.jpg").size(4).insert().await;
    let stale = seed
        .file(volume, "stale", "stale.jpg")
        .size(5)
        .insert()
        .await;
    let changed = seed
        .file(volume, "changed", "changed.jpg")
        .size(7)
        .insert()
        .await;
    ready(&app).await;

    let make_plan = |dir: &str| params(side(volume, dir, false), rules("Renamed_{name}"), None);
    let gone_plan = build(&app, &make_plan("gone")).await;
    let stale_plan = build(&app, &make_plan("stale")).await;
    let changed_plan = build(&app, &make_plan("changed")).await;

    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(gone)
        .execute(app.state.db.write())
        .await
        .unwrap();
    sqlx::query("UPDATE files SET name = 'renamed.jpg', name_norm = 'renamed.jpg', stem_norm = 'renamed' WHERE id = ?")
        .bind(stale)
        .execute(app.state.db.write())
        .await
        .unwrap();
    tree.write_at("changed/changed.jpg", b"changed on disk", 1_700_000_100);

    for (plan, expected_code, file_path) in [
        (
            gone_plan["id"].as_i64().unwrap(),
            "file_gone",
            "gone/gone.jpg",
        ),
        (
            stale_plan["id"].as_i64().unwrap(),
            "stale_plan",
            "stale/stale.jpg",
        ),
        (
            changed_plan["id"].as_i64().unwrap(),
            "source_changed",
            "changed/changed.jpg",
        ),
    ] {
        let result = execute(&app, plan, None).await;
        if expected_code == "source_changed" {
            assert_eq!(result["failed"], 1, "{result}");
        } else {
            assert_eq!(result["skipped"], 1, "{result}");
        }
        assert_eq!(result["failures"][0]["code"], expected_code, "{result}");
        assert!(tree.path(file_path).exists(), "{expected_code}");
    }
    assert!(!tree.path("gone/Renamed_gone.jpg").exists());
    assert!(!tree.path("stale/Renamed_stale.jpg").exists());
    assert!(!tree.path("changed/Renamed_changed.jpg").exists());
    assert_ne!(gone, stale);
    assert_ne!(stale, changed);
}

#[tokio::test]
async fn delete_expiry_and_invalid_volumes_clean_or_reject_rename_plans() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "album", "one.jpg").insert().await;
    ready(&app).await;

    let first = build(
        &app,
        &params(side(volume, "album", false), rules("Renamed_{name}"), None),
    )
    .await;
    let first_id = first["id"].as_i64().unwrap();
    let (status, body) = app
        .call(Method::DELETE, &format!("/api/plans/{first_id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    app.state.plans.settle().await;
    for (table, statement) in [
        (
            "move_items",
            "SELECT COUNT(*) FROM move_items WHERE plan_id = ?",
        ),
        (
            "move_plans",
            "SELECT COUNT(*) FROM move_plans WHERE plan_id = ?",
        ),
        (
            "plan_file_tracking",
            "SELECT COUNT(*) FROM plan_file_tracking WHERE plan_id = ?",
        ),
        (
            "plan_file_identities",
            "SELECT COUNT(*) FROM plan_file_identities WHERE plan_id = ?",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(statement)
            .bind(first_id)
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }

    let expired = build(
        &app,
        &params(side(volume, "album", false), rules("Renamed_{name}"), None),
    )
    .await;
    let expired_id = expired["id"].as_i64().unwrap();
    sqlx::query("UPDATE plans SET expires_at = 0 WHERE id = ?")
        .bind(expired_id)
        .execute(app.state.db.write())
        .await
        .unwrap();
    app.state.plans.sweep().await.unwrap();
    app.state.plans.settle().await;
    let expired_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM move_items WHERE plan_id = ?")
        .bind(expired_id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(expired_rows, 0);

    let invalid_rules = RenameRules {
        matcher: RenameMatcher {
            mode: RenameMatchMode::Regex,
            pattern: "(".to_owned(),
            ignore_case: false,
        },
        ..rules("Renamed_{name}")
    };
    let (status, invalid) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "rename",
                "params": params(side(volume, "album", false), invalid_rules, None),
                "replaces": null
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_eq!(invalid["error"]["code"], "invalid_plan_params");

    std::fs::remove_dir(tree.root()).unwrap();
    app.state.volumes.refresh().await.unwrap();
    let (status, offline) = app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({
                "kind": "rename",
                "params": params(side(volume, "album", false), rules("New_{name}"), None),
                "replaces": null
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{offline}");
    assert_eq!(offline["error"]["code"], "invalid_plan_params");
}
