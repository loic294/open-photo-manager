//! Folder listing inside a volume over the real router: counts, footer counts, filters, nested and
//! empty folders, request validation, `folder_not_found`, index-only browsing, inert names and the
//! default plan provider.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use common::browse;
use common::seed::Seed;
use opm_core::paths::VolumeId;
use serde_json::{Value, json};

/// Volume `photos`:
///
/// ```text
/// top.jpg (1) top.xmp (sidecar)
/// 2023/Trip/   a.jpg (5, Canon EOS R5, family)  b.jpg (3)
/// 2023/Empty/  (nothing)
/// 2024/        c.jpg (5, Sony A7)
/// 2024-05/     e.jpg (2)
/// Zebra/       f.jpg (0)
/// apple/       g.jpg (4)
/// ```
///
/// Volume `other` has `2023/Trip/z.jpg`, which must never show up in `photos`.
async fn library(app: &TestApp) -> VolumeId {
    let seed = Seed::new(&app.state.db);
    let photos = seed.volume("photos").await;
    seed.file(photos, "", "top.jpg").rating(1).insert().await;
    seed.file(photos, "", "top.xmp").insert().await;
    seed.file(photos, "2023/Trip", "a.jpg")
        .rating(5)
        .camera("Canon EOS R5")
        .tags(["family"])
        .insert()
        .await;
    seed.file(photos, "2023/Trip", "b.jpg")
        .rating(3)
        .insert()
        .await;
    seed.dir(photos, "2023/Empty").await;
    seed.file(photos, "2024", "c.jpg")
        .rating(5)
        .camera("Sony A7")
        .insert()
        .await;
    seed.file(photos, "2024-05", "e.jpg")
        .rating(2)
        .insert()
        .await;
    seed.file(photos, "Zebra", "f.jpg").rating(0).insert().await;
    seed.file(photos, "apple", "g.jpg").rating(4).insert().await;

    let other = seed.volume("other").await;
    seed.file(other, "2023/Trip", "z.jpg")
        .rating(5)
        .insert()
        .await;
    photos
}

fn request(volume: VolumeId, dir: &str) -> Value {
    json!({"volume_id": volume, "dir": dir})
}

#[tokio::test]
async fn folder_and_volume_previews_use_filtered_images_from_their_own_subtrees() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("photos").await;
    let other = seed.volume("other").await;
    seed.file(other, "trip", "foreign.jpg")
        .rating(5)
        .insert()
        .await;
    seed.file(volume, "trip", "unmatched.jpg")
        .rating(1)
        .insert()
        .await;
    seed.file(volume, "trip", "video.mp4")
        .rating(5)
        .insert()
        .await;
    let image = seed
        .file(volume, "trip/nested", "photo.jpg")
        .rating(5)
        .mtime(321)
        .insert()
        .await;
    seed.file(volume, "trip.old", "sibling.jpg")
        .rating(5)
        .insert()
        .await;
    seed.dir(volume, "empty").await;
    let mut body = request(volume, "");
    body["filter"] = json!({"ratings": [5]});
    let (status, response) = browse::folders(&app, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let folders = response["folders"].as_array().unwrap();
    let trip = folders.iter().find(|f| f["name"] == "trip").unwrap();
    assert_eq!(trip["preview"], json!({"id": image, "mtime": 321}));
    assert_eq!(folders[0]["name"], "empty");
    assert_eq!(folders[0]["preview"], Value::Null);

    let (_, root) = browse::folders(&app, json!({"filter": {"ratings": [5]}})).await;
    let photos = root["folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["volume_id"] == volume)
        .unwrap();
    assert_eq!(photos["preview"], trip["preview"]);

    let (_, empty) = browse::folders(
        &app,
        json!({"volume_id": volume, "filter": {"ratings": [4]}}),
    )
    .await;
    assert!(
        empty["folders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["preview"].is_null())
    );
}

#[tokio::test]
async fn folders_list_the_subfolders_with_their_counts() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    let (status, response) = browse::folders(&app, request(photos, "")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::names(&response, "folders"),
        ["2023", "2024", "2024-05", "apple", "Zebra"],
        "sorted by normalised name, so `apple` comes before `Zebra`"
    );
    assert_eq!(
        browse::counts(&response),
        [(2, 2), (1, 1), (1, 1), (1, 1), (1, 1)],
        "2024 does not count the files of its sibling 2024-05"
    );
    assert_eq!(
        response["counts"],
        json!({"in_folder": 1, "in_subfolders": 6}),
        "top.xmp is a sidecar and is not counted"
    );

    let folder = &response["folders"][0];
    assert_eq!(folder["name"], json!("2023"));
    assert_eq!(folder["path"], json!("2023"));
    assert_eq!(folder["volume_id"], json!(photos));
    assert_eq!(folder["matches"], Value::Null);
    assert_eq!(
        folder["overlay"],
        json!({"matched": false, "planned": false, "will_delete": false})
    );
}

#[tokio::test]
async fn filtered_out_folders_stay_listed_with_their_totals() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    let mut body = request(photos, "");
    body["filter"] = json!({"ratings": [5]});
    let (status, response) = browse::folders(&app, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::names(&response, "folders"),
        ["2023", "2024", "2024-05", "apple", "Zebra"]
    );
    assert_eq!(
        browse::counts(&response),
        [(1, 2), (1, 1), (0, 1), (0, 1), (0, 1)]
    );
    assert_eq!(
        response["counts"],
        json!({"in_folder": 0, "in_subfolders": 2})
    );
}

#[tokio::test]
async fn nested_and_empty_folders() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    let (status, response) = browse::folders(&app, request(photos, "2023")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::names(&response, "folders"), ["Empty", "Trip"]);
    assert_eq!(browse::counts(&response), [(0, 0), (2, 2)]);
    assert_eq!(response["folders"][0]["path"], json!("2023/Empty"));
    assert_eq!(response["folders"][1]["path"], json!("2023/Trip"));
    assert_eq!(
        response["counts"],
        json!({"in_folder": 0, "in_subfolders": 2})
    );

    let (_, response) = browse::folders(&app, request(photos, "2023/Trip")).await;
    assert_eq!(response["folders"], json!([]));
    assert_eq!(
        response["counts"],
        json!({"in_folder": 2, "in_subfolders": 0})
    );

    let (status, response) = browse::folders(&app, request(photos, "2023/Empty")).await;
    assert_eq!(status, StatusCode::OK, "an empty folder exists: {response}");
    assert_eq!(response["folders"], json!([]));
    assert_eq!(
        response["counts"],
        json!({"in_folder": 0, "in_subfolders": 0})
    );
}

#[tokio::test]
async fn the_footer_counts_follow_the_filter() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    let mut body = request(photos, "2023");
    body["filter"] = json!({"cameras": ["canon eos r5"]});
    let (_, response) = browse::folders(&app, body).await;
    assert_eq!(browse::counts(&response), [(0, 0), (1, 2)]);
    assert_eq!(
        response["counts"],
        json!({"in_folder": 0, "in_subfolders": 1})
    );

    let mut body = request(photos, "2023/Trip");
    body["filter"] = json!({"ratings": [3]});
    let (_, response) = browse::folders(&app, body).await;
    assert_eq!(
        response["counts"],
        json!({"in_folder": 1, "in_subfolders": 0})
    );
}

#[tokio::test]
async fn a_known_volume_always_has_a_root() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("bare").await;
    sqlx::query("DELETE FROM dirs WHERE volume_id = ?")
        .bind(volume)
        .execute(app.state.db.write())
        .await
        .unwrap();

    let (status, response) = browse::folders(&app, request(volume, "")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        response,
        json!({"folders": [], "counts": {"in_folder": 0, "in_subfolders": 0}})
    );
}

#[tokio::test]
async fn an_unknown_volume_is_not_found_on_every_endpoint() {
    let app = TestApp::new().await;
    let photos = library(&app).await;
    let unknown = photos + 1000;

    for (name, (status, response)) in [
        ("folders", browse::folders(&app, request(unknown, "")).await),
        ("items", browse::items(&app, request(unknown, "")).await),
        ("ids", browse::ids(&app, request(unknown, "")).await),
        ("facets", browse::facets(&app, request(unknown, "")).await),
        (
            "folders below the root",
            browse::folders(&app, request(unknown, "2023")).await,
        ),
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}: {response}");
        assert_eq!(browse::error_code(&response), "not_found", "{name}");
    }
}

const BAD_DIRS: [&str; 14] = [
    "..",
    "../x",
    "2023/..",
    "2023/../2024",
    "/",
    "/2023",
    "2023/",
    "2023//Trip",
    "./2023",
    "2023/./Trip",
    "2023\\Trip",
    "\\",
    "2023/Tr\u{0}ip",
    "C:\\Users",
];

#[tokio::test]
async fn an_invalid_dir_is_a_bad_request_on_every_endpoint() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    for dir in BAD_DIRS {
        let body = request(photos, dir);
        for (name, (status, response)) in [
            ("folders", browse::folders(&app, body.clone()).await),
            ("items", browse::items(&app, body.clone()).await),
            ("ids", browse::ids(&app, body.clone()).await),
            ("facets", browse::facets(&app, body.clone()).await),
        ] {
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{name} {dir:?}: {response}"
            );
            assert_eq!(
                browse::error_code(&response),
                "bad_request",
                "{name} {dir:?}"
            );
            let message = response["error"]["message"].as_str().unwrap();
            assert!(message.contains("dir"), "{name} {dir:?}: {message}");
        }
    }
}

#[tokio::test]
async fn an_invalid_dir_is_rejected_before_the_volume_is_looked_up() {
    let app = TestApp::new().await;

    let (status, response) = browse::folders(&app, request(9999, "../x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
}

#[tokio::test]
async fn no_volume_with_a_dir_is_a_bad_request() {
    let app = TestApp::new().await;
    library(&app).await;

    for dir in ["2023", "a/b", "photos"] {
        let body = json!({"volume_id": null, "dir": dir});
        for (status, response) in [
            browse::folders(&app, body.clone()).await,
            browse::items(&app, body.clone()).await,
            browse::ids(&app, body.clone()).await,
            browse::facets(&app, body.clone()).await,
        ] {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{dir}: {response}");
            assert_eq!(browse::error_code(&response), "bad_request");
        }
    }
}

#[tokio::test]
async fn a_missing_folder_is_folder_not_found_for_the_folder_list_only() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    for dir in ["Gone", "2023/Gone", "2023/Trip/deeper", "zebra"] {
        let (status, response) = browse::folders(&app, request(photos, dir)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{dir}: {response}");
        assert_eq!(browse::error_code(&response), "folder_not_found", "{dir}");
        assert_eq!(
            response["error"]["message"],
            json!(format!("folder `{dir}` not found in volume {photos}"))
        );

        let (status, response) = browse::items(&app, request(photos, dir)).await;
        assert_eq!(status, StatusCode::OK, "{dir}: {response}");
        assert_eq!(response, json!({"items": [], "total": 0, "offset": 0}));
        let (status, response) = browse::ids(&app, request(photos, dir)).await;
        assert_eq!(status, StatusCode::OK, "{dir}: {response}");
        assert_eq!(response, json!({"ids": []}));
        let (status, response) = browse::facets(&app, request(photos, dir)).await;
        assert_eq!(status, StatusCode::OK, "{dir}: {response}");
        assert_eq!(response, json!({"cameras": [], "tags": [], "ratings": []}));
    }
}

#[tokio::test]
async fn a_folder_of_another_volume_is_not_found_in_this_one() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    seed.file(a, "only/here", "x.jpg").insert().await;

    let (status, _) = browse::folders(&app, request(a, "only/here")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, response) = browse::folders(&app, request(b, "only/here")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert_eq!(browse::error_code(&response), "folder_not_found");
}

/// One answer of every browse endpoint for a volume that holds `2023/Trip`, plus the volumes root.
async fn every_endpoint(app: &TestApp, volume: VolumeId) -> Vec<(StatusCode, Value)> {
    vec![
        browse::folders(app, request(volume, "2023")).await,
        browse::items(app, request(volume, "2023/Trip")).await,
        browse::ids(app, request(volume, "2023/Trip")).await,
        browse::facets(app, request(volume, "")).await,
        browse::folders(app, json!({})).await,
    ]
}

#[tokio::test]
async fn browsing_reads_the_index_and_never_the_file_system() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("real").await;
    let seed = Seed::new(&app.state.db);
    seed.file(volume, "2023/Trip", "a.jpg").insert().await;
    seed.file(volume, "2023/Trip", "b.jpg").insert().await;
    seed.file(volume, "", "top.jpg").insert().await;
    assert!(
        !tree.path("2023").exists(),
        "the files only exist in the index"
    );

    let before = every_endpoint(&app, volume).await;
    assert_eq!(before[1].1["total"], json!(2));

    std::fs::remove_dir_all(tree.root()).unwrap();
    assert!(!tree.root().exists());
    let after = every_endpoint(&app, volume).await;
    assert_eq!(
        after, before,
        "deleting the volume directory changes nothing"
    );

    let (_, volumes) = browse::get(&app, "/api/volumes").await;
    assert_eq!(
        volumes[0]["online"],
        json!(true),
        "nothing refreshed the registry"
    );
}

#[tokio::test]
async fn an_offline_volume_is_browsed_like_an_online_one() {
    let app = TestApp::new().await;
    let photos = library(&app).await;
    sqlx::query("UPDATE volumes SET online = 0 WHERE id = ?")
        .bind(photos)
        .execute(app.state.db.write())
        .await
        .unwrap();

    let (status, response) = browse::folders(&app, request(photos, "2023")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::names(&response, "folders"), ["Empty", "Trip"]);
    let (status, response) = browse::items(&app, request(photos, "2023/Trip")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["total"], json!(2));
}

#[tokio::test]
async fn injection_looking_names_are_inert() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("photos").await;
    let hostile_dir = "a'; DROP TABLE files; --/\"quoted\"";
    let hostile_camera = "x' OR '1'='1";
    let hostile_tag = "t'); DELETE FROM tags; --";
    seed.file(volume, hostile_dir, "it's.jpg")
        .camera(hostile_camera)
        .tags([hostile_tag])
        .insert()
        .await;
    seed.file(volume, "plain", "p.jpg").insert().await;

    // The hostile directory is found by its exact name, and only by it.
    let (status, response) = browse::items(&app, request(volume, hostile_dir)).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::names(&response, "items"), ["it's.jpg"]);
    assert_eq!(response["items"][0]["dir"], json!(hostile_dir));
    let (_, response) = browse::folders(&app, request(volume, "")).await;
    assert_eq!(
        browse::names(&response, "folders"),
        ["a'; DROP TABLE files; --", "plain"]
    );
    let (status, response) = browse::folders(&app, request(volume, "a' OR '1'='1")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert_eq!(browse::error_code(&response), "folder_not_found");

    // Filters bind their values: the hostile camera and tag match themselves and nothing else.
    for (filter, expected) in [
        (json!({"cameras": [hostile_camera]}), 1),
        (json!({"tags": [hostile_tag]}), 1),
        (json!({"cameras": ["' OR 1=1 --"]}), 0),
        (json!({"tags": ["%"]}), 0),
        (json!({"cameras": ["_"]}), 0),
        (json!({"cameras": ["x%"]}), 0),
    ] {
        let mut body = request(volume, hostile_dir);
        body["filter"] = filter.clone();
        let (status, response) = browse::items(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {response}");
        assert_eq!(response["total"], json!(expected), "{filter}");
    }

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('files', 'tags', 'cameras')",
    )
    .fetch_one(app.state.db.read())
    .await
    .unwrap();
    assert_eq!(tables, 3);
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(files, 2);
}

#[tokio::test]
async fn like_wildcards_in_a_dir_are_literal() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("photos").await;
    seed.file(volume, "100%_done", "own.jpg").insert().await;
    seed.file(volume, "100%_done/sub", "deep.jpg")
        .insert()
        .await;
    seed.file(volume, "1000_done", "other.jpg").insert().await;
    seed.file(volume, "100xxdone", "another.jpg").insert().await;

    let (_, response) = browse::folders(&app, request(volume, "")).await;
    assert_eq!(
        browse::names(&response, "folders"),
        ["100%_done", "1000_done", "100xxdone"]
    );
    assert_eq!(browse::counts(&response), [(2, 2), (1, 1), (1, 1)]);

    let (_, response) = browse::items(&app, request(volume, "100%_done")).await;
    assert_eq!(browse::names(&response, "items"), ["own.jpg"]);
    let (_, response) = browse::facets(&app, request(volume, "100%_done")).await;
    assert_eq!(browse::facet(&response, "ratings"), [("0".to_owned(), 2)]);
}

#[tokio::test]
async fn the_default_provider_rejects_a_plan_it_has_never_seen() {
    let app = TestApp::new().await;
    let photos = library(&app).await;
    let plan = json!({"plan_id": 7, "side": "comparison"});

    for dir in ["", "2023", "Gone"] {
        let mut body = request(photos, dir);
        body["plan"] = plan.clone();
        for (name, (status, response)) in [
            ("folders", browse::folders(&app, body.clone()).await),
            ("items", browse::items(&app, body.clone()).await),
        ] {
            assert_eq!(status, StatusCode::NOT_FOUND, "{name} {dir:?}: {response}");
            assert_eq!(
                browse::error_code(&response),
                "plan_not_found",
                "{name} {dir:?}: the plan error wins over folder_not_found"
            );
            assert_eq!(response["error"]["message"], json!("plan 7 not found"));
        }
    }
}

#[tokio::test]
async fn ids_and_facets_ignore_the_plan() {
    let app = TestApp::new().await;
    let photos = library(&app).await;
    let mut body = request(photos, "2023/Trip");
    body["plan"] = json!({"plan_id": 7});

    let (status, response) = browse::ids(&app, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::id_list(&response).len(), 2);
    let (status, response) = browse::facets(&app, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Canon EOS R5".to_owned(), 1)]
    );
}

#[tokio::test]
async fn without_a_plan_the_overlay_is_empty_everywhere() {
    let app = TestApp::new().await;
    let photos = library(&app).await;

    let (_, response) = browse::items(&app, request(photos, "2023/Trip")).await;
    for item in response["items"].as_array().unwrap() {
        assert_eq!(
            item["overlay"],
            json!({"matched": false, "planned": false, "will_delete": false})
        );
    }
}
