//! The volumes root of the browse API (`volume_id = null`, `dir = ""`), `GET /api/volumes` and
//! `GET /api/files/{id}`, over the real router.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use common::browse;
use common::seed::Seed;
use opm_core::dto::FileKind;
use opm_core::paths::{FileId, VolumeId};
use serde_json::{Value, json};

/// `alpha`, `empty` and `photos` are online, `Zeta` is offline (and sorts last only because names
/// compare ignoring case).
struct Library {
    photos: VolumeId,
    zeta: VolumeId,
    alpha: VolumeId,
    empty: VolumeId,
}

/// Assets per volume (the sidecar of `photos` does not count): `alpha` 1, `empty` 0, `photos` 4,
/// `Zeta` 2. Ratings: `photos` 5, 3, 5, 1; `Zeta` 5, 0; `alpha` 2.
async fn library(app: &TestApp) -> Library {
    let seed = Seed::new(&app.state.db);
    let photos = seed.volume("photos").await;
    let zeta = seed.volume("Zeta").await;
    let alpha = seed.volume("alpha").await;
    let empty = seed.volume("empty").await;

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
    seed.file(photos, "2024", "c.jpg")
        .rating(5)
        .camera("Sony A7")
        .insert()
        .await;
    seed.file(photos, "", "d.jpg").rating(1).insert().await;
    seed.file(photos, "", "d.xmp").insert().await;
    seed.file(zeta, "old", "x.jpg").rating(5).insert().await;
    seed.file(zeta, "", "y.mp4")
        .kind(FileKind::Video)
        .insert()
        .await;
    seed.file(alpha, "", "z.jpg").rating(2).insert().await;

    sqlx::query("UPDATE volumes SET online = 0 WHERE id = ?")
        .bind(zeta)
        .execute(app.state.db.write())
        .await
        .unwrap();
    Library {
        photos,
        zeta,
        alpha,
        empty,
    }
}

#[tokio::test]
async fn the_volumes_root_lists_every_volume_as_a_folder() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    for body in [
        json!({}),
        json!({"volume_id": null, "dir": ""}),
        json!({"volume_id": null, "dir": "", "filter": {}}),
    ] {
        let (status, response) = browse::folders(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            browse::names(&response, "folders"),
            ["alpha", "empty", "photos", "Zeta"]
        );
        assert_eq!(
            browse::counts(&response),
            [(1, 1), (0, 0), (4, 4), (2, 2)],
            "assets only: the sidecar of photos is not counted"
        );
        assert_eq!(
            response["counts"],
            json!({"in_folder": 0, "in_subfolders": 7})
        );

        let ids = [lib.alpha, lib.empty, lib.photos, lib.zeta];
        for (folder, id) in response["folders"].as_array().unwrap().iter().zip(ids) {
            assert_eq!(folder["volume_id"], json!(id));
            assert_eq!(folder["path"], json!(""));
            assert_eq!(folder["matches"], Value::Null);
            assert_eq!(
                folder["overlay"],
                json!({"matched": false, "planned": false, "will_delete": false})
            );
        }
    }
}

#[tokio::test]
async fn the_volumes_root_counts_honour_the_filter_and_keep_the_totals() {
    let app = TestApp::new().await;
    library(&app).await;

    let cases = [
        (
            json!({"ratings": [5]}),
            vec![(0, 1), (0, 0), (2, 4), (1, 2)],
            3,
        ),
        (
            json!({"cameras": ["canon eos r5"]}),
            vec![(0, 1), (0, 0), (1, 4), (0, 2)],
            1,
        ),
        (
            json!({"tags": ["FAMILY"]}),
            vec![(0, 1), (0, 0), (1, 4), (0, 2)],
            1,
        ),
        (
            json!({"ratings": [4]}),
            vec![(0, 1), (0, 0), (0, 4), (0, 2)],
            0,
        ),
        (
            json!({"ratings": [5], "cameras": ["Sony A7"]}),
            vec![(0, 1), (0, 0), (1, 4), (0, 2)],
            1,
        ),
    ];
    for (filter, expected, in_subfolders) in cases {
        let (status, response) = browse::folders(&app, json!({"filter": filter})).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {response}");
        assert_eq!(
            browse::names(&response, "folders"),
            ["alpha", "empty", "photos", "Zeta"],
            "a volume with no match is still listed: {filter}"
        );
        assert_eq!(browse::counts(&response), expected, "{filter}");
        assert_eq!(
            response["counts"],
            json!({"in_folder": 0, "in_subfolders": in_subfolders}),
            "{filter}"
        );
    }
}

#[tokio::test]
async fn the_volumes_root_filters_by_selection() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let a = seed.volume("a").await;
    let b = seed.volume("b").await;
    let in_a = seed.file(a, "", "1.jpg").insert().await;
    seed.file(a, "", "2.jpg").insert().await;
    seed.file(b, "", "3.jpg").insert().await;
    let selection = seed.selection(&[in_a]).await;

    let (status, response) =
        browse::folders(&app, json!({"filter": {"selection_id": selection}})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::counts(&response), [(1, 2), (0, 1)]);
    assert_eq!(response["counts"]["in_subfolders"], json!(1));
}

#[tokio::test]
async fn an_invalid_filter_at_the_volumes_root_is_a_bad_request() {
    let app = TestApp::new().await;
    library(&app).await;
    let body = json!({"filter": {"ratings": [6]}});

    for (status, response) in [
        browse::folders(&app, body.clone()).await,
        browse::items(&app, body.clone()).await,
        browse::ids(&app, body.clone()).await,
        browse::facets(&app, body.clone()).await,
    ] {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(browse::error_code(&response), "bad_request");
    }
}

#[tokio::test]
async fn the_volumes_root_ignores_the_plan() {
    let app = TestApp::new().await;
    library(&app).await;

    // The default provider answers 404 for every plan, so a 200 means it was never called.
    let (status, response) = browse::folders(&app, json!({"plan": {"plan_id": 99}})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::counts(&response), [(1, 1), (0, 0), (4, 4), (2, 2)]);

    let (status, response) = browse::items(&app, json!({"plan": {"plan_id": 99}})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["total"], json!(0));
}

#[tokio::test]
async fn the_volumes_root_has_neither_items_nor_ids() {
    let app = TestApp::new().await;
    library(&app).await;

    for body in [
        json!({}),
        json!({"offset": 40, "limit": 10, "sort": "date", "order": "desc"}),
        json!({"filter": {"ratings": [5]}}),
    ] {
        let (status, response) = browse::items(&app, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["items"], json!([]));
        assert_eq!(response["total"], json!(0));
        assert_eq!(
            response["offset"],
            body.get("offset").cloned().unwrap_or(json!(0))
        );

        let (status, response) = browse::ids(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response, json!({"ids": []}));
    }
}

#[tokio::test]
async fn an_empty_library_has_an_empty_volumes_root() {
    let app = TestApp::new().await;

    let (status, response) = browse::folders(&app, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        response,
        json!({"folders": [], "counts": {"in_folder": 0, "in_subfolders": 0}})
    );
    let (status, response) = browse::items(&app, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({"items": [], "total": 0, "offset": 0}));
    let (status, response) = browse::facets(&app, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!({"cameras": [], "tags": [], "ratings": []}));
    let (status, response) = browse::get(&app, "/api/volumes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response, json!([]));
}

#[tokio::test]
async fn a_volume_without_files_is_listed_with_zero_counts() {
    let app = TestApp::new().await;
    Seed::new(&app.state.db).volume("blank").await;

    let (_, response) = browse::folders(&app, json!({"filter": {"ratings": [5]}})).await;
    assert_eq!(browse::names(&response, "folders"), ["blank"]);
    assert_eq!(browse::counts(&response), [(0, 0)]);
}

#[tokio::test]
async fn the_volumes_root_facets_cover_every_volume() {
    let app = TestApp::new().await;
    library(&app).await;

    let (status, response) = browse::facets(&app, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Canon EOS R5".to_owned(), 1), ("Sony A7".to_owned(), 1)]
    );
    assert_eq!(browse::facet(&response, "tags"), [("family".to_owned(), 1)]);
    assert_eq!(
        browse::facet(&response, "ratings"),
        [
            ("5".to_owned(), 3),
            ("0".to_owned(), 1),
            ("1".to_owned(), 1),
            ("2".to_owned(), 1),
            ("3".to_owned(), 1),
        ]
    );

    // Every dimension is counted under the other filters, not under its own.
    let (_, response) = browse::facets(&app, json!({"filter": {"cameras": ["Sony A7"]}})).await;
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Canon EOS R5".to_owned(), 1), ("Sony A7".to_owned(), 1)]
    );
    assert_eq!(browse::facet(&response, "ratings"), [("5".to_owned(), 1)]);
    assert_eq!(browse::facet(&response, "tags"), []);
}

#[tokio::test]
async fn the_volume_list_is_sorted_and_includes_offline_volumes() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (status, response) = browse::get(&app, "/api/volumes").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        response,
        json!([
            {"id": lib.alpha, "name": "alpha", "writable": true, "online": true, "fs_type": null, "case_insensitive": null, "watch_mode": "watching", "watch_reason": null},
            {"id": lib.empty, "name": "empty", "writable": true, "online": true, "fs_type": null, "case_insensitive": null, "watch_mode": "watching", "watch_reason": null},
            {"id": lib.photos, "name": "photos", "writable": true, "online": true, "fs_type": null, "case_insensitive": null, "watch_mode": "watching", "watch_reason": null},
            {"id": lib.zeta, "name": "Zeta", "writable": true, "online": false, "fs_type": null, "case_insensitive": null, "watch_mode": "watching", "watch_reason": null},
        ])
    );
}

#[tokio::test]
async fn volumes_that_differ_only_by_case_sort_by_name_then_id() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let lower = seed.volume("photos").await;
    let upper = seed.volume("Photos").await;
    let before = seed.volume("Album").await;

    let (_, response) = browse::get(&app, "/api/volumes").await;
    let order: Vec<i64> = response
        .as_array()
        .unwrap()
        .iter()
        .map(|volume| volume["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, [before, upper, lower]);

    let (_, response) = browse::folders(&app, json!({})).await;
    assert_eq!(
        browse::names(&response, "folders"),
        ["Album", "Photos", "photos"],
        "the volumes root sorts the same way"
    );
}

#[tokio::test]
async fn listing_the_volumes_never_writes_or_refreshes_the_registry() {
    let app = TestApp::new().await;
    Seed::new(&app.state.db).volume("known").await;
    std::fs::create_dir_all(app.state.config.volumes_dir.join("unregistered")).unwrap();

    for _ in 0..2 {
        let (status, response) = browse::get(&app, "/api/volumes").await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = response
            .as_array()
            .unwrap()
            .iter()
            .map(|volume| volume["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["known"], "a directory on disk is not a volume yet");
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volumes")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn a_scanned_volume_is_listed_like_a_seeded_one() {
    let app = TestApp::new().await;
    let (id, _tree) = app.add_volume("scanned").await;

    let (status, response) = browse::get(&app, "/api/volumes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response[0]["id"], json!(id));
    assert_eq!(response[0]["name"], json!("scanned"));
    assert_eq!(response[0]["online"], json!(true));
}

async fn file_detail_fixture(app: &TestApp) -> (VolumeId, FileId) {
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("photos").await;
    let id = seed
        .file(volume, "2023/Trip", "a.jpg")
        .rating(4)
        .size(12_345)
        .mtime(1_650_000_000)
        .capture(1_700_000_000)
        .camera("Canon EOS R5")
        .dimensions(6000, 4000)
        .mime("image/jpeg")
        .tags(["family", "trip"])
        .insert()
        .await;
    (volume, id)
}

#[tokio::test]
async fn a_file_has_a_detail() {
    let app = TestApp::new().await;
    let (volume, id) = file_detail_fixture(&app).await;

    let (status, response) = browse::get(&app, &format!("/api/files/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let item = &response["item"];
    assert_eq!(item["id"], json!(id));
    assert_eq!(item["volume_id"], json!(volume));
    assert_eq!(item["dir"], json!("2023/Trip"));
    assert_eq!(item["name"], json!("a.jpg"));
    assert_eq!(item["kind"], json!("image"));
    assert_eq!(item["size"], json!(12_345));
    assert_eq!(item["mtime"], json!(1_650_000_000));
    assert_eq!(item["capture_ts"], json!(1_700_000_000));
    assert_eq!(item["camera"], json!("Canon EOS R5"));
    assert_eq!(item["rating"], json!(4));
    assert_eq!(item["width"], json!(6000));
    assert_eq!(item["height"], json!(4000));
    assert_eq!(response["mime"], json!("image/jpeg"));
    let mut tags: Vec<&str> = response["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag.as_str().unwrap())
        .collect();
    tags.sort_unstable();
    assert_eq!(tags, ["family", "trip"]);
}

#[tokio::test]
async fn an_unknown_file_is_not_found_and_a_malformed_id_is_a_bad_path() {
    let app = TestApp::new().await;
    let (_, id) = file_detail_fixture(&app).await;

    for unknown in [id + 1000, 0, -5] {
        let (status, response) = browse::get(&app, &format!("/api/files/{unknown}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}: {response}");
        assert_eq!(browse::error_code(&response), "not_found");
    }
    for malformed in ["not-a-number", "1.5", "99999999999999999999"] {
        let (status, response) = browse::get(&app, &format!("/api/files/{malformed}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{malformed}: {response}");
        assert_eq!(browse::error_code(&response), "invalid_path");
    }
}
