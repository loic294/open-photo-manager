//! `POST /api/browse/items`, `/api/browse/ids` and `/api/facets` inside a volume over the real
//! router: paging, every sort key, filters, limits, ids in the order of the items, facet scopes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::TestApp;
use common::browse;
use common::seed::Seed;
use opm_core::dto::FileKind;
use opm_core::paths::{FileId, VolumeId};
use serde_json::{Value, json};

/// 2024-01-01 00:00:00 as a naive capture time.
const DAY_ONE: i64 = 1_704_067_200;
const DAY: i64 = 86_400;

struct Library {
    lib: VolumeId,
    other: VolumeId,
}

/// Volume `lib`, directory `inbox` (the names sort case-insensitively, digits first):
///
/// | name   | size | capture | rating | camera | tags          | mtime |
/// |--------|------|---------|--------|--------|---------------|-------|
/// | 10.jpg | 400  | day 4   | 5      | Canon  |               | 7000  |
/// | a.jpg  | 100  | day 1   | 3      | Canon  | family        | 3000  |
/// | B.jpg  | 300  | day 3   | 5      | Canon  | family, trip  | 1000  |
/// | c.jpg  | 200  | none    | 3      | Sony   |               | 2000  |
/// | d.jpg  | 200  | day 2   | 0      |        |               | 4000  |
/// | E.mp4  | 500  | day 5   | 0      |        |               | 5000  |
/// | f.jpg  | 100  | none    | 1      | Sony   |               | 6000  |
///
/// plus the sidecar `a.xmp`, `inbox/sub/g.jpg` (Sony, rating 2, tag trip) and `root.jpg`, and one
/// file of another volume that must never show up.
async fn library(app: &TestApp) -> Library {
    let seed = Seed::new(&app.state.db);
    let lib = seed.volume("lib").await;
    let at = |day: i64| DAY_ONE + (day - 1) * DAY;

    seed.file(lib, "inbox", "10.jpg")
        .size(400)
        .capture(at(4))
        .rating(5)
        .camera("Canon")
        .mtime(7000)
        .insert()
        .await;
    seed.file(lib, "inbox", "a.jpg")
        .size(100)
        .capture(at(1))
        .rating(3)
        .camera("Canon")
        .tags(["family"])
        .mtime(3000)
        .insert()
        .await;
    seed.file(lib, "inbox", "B.jpg")
        .size(300)
        .capture(at(3))
        .rating(5)
        .camera("Canon")
        .tags(["family", "trip"])
        .mtime(1000)
        .insert()
        .await;
    seed.file(lib, "inbox", "c.jpg")
        .size(200)
        .rating(3)
        .camera("Sony")
        .mtime(2000)
        .insert()
        .await;
    seed.file(lib, "inbox", "d.jpg")
        .size(200)
        .capture(at(2))
        .mtime(4000)
        .insert()
        .await;
    seed.file(lib, "inbox", "E.mp4")
        .kind(FileKind::Video)
        .size(500)
        .capture(at(5))
        .duration_ms(9_000)
        .mtime(5000)
        .insert()
        .await;
    seed.file(lib, "inbox", "f.jpg")
        .size(100)
        .rating(1)
        .camera("Sony")
        .mtime(6000)
        .insert()
        .await;
    seed.file(lib, "inbox", "a.xmp").insert().await;
    seed.file(lib, "inbox/sub", "g.jpg")
        .rating(2)
        .camera("Sony")
        .tags(["trip"])
        .insert()
        .await;
    seed.file(lib, "", "root.jpg").insert().await;

    let other = seed.volume("other").await;
    seed.file(other, "inbox", "intruder.jpg")
        .rating(5)
        .insert()
        .await;
    Library { lib, other }
}

fn items_request(volume: VolumeId, dir: &str, extra: Value) -> Value {
    let mut body = json!({"volume_id": volume, "dir": dir});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

const BY_NAME_ASC: [&str; 7] = [
    "10.jpg", "a.jpg", "B.jpg", "c.jpg", "d.jpg", "E.mp4", "f.jpg",
];

fn reversed(names: &[&str]) -> Vec<String> {
    names.iter().rev().map(|name| (*name).to_owned()).collect()
}

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[tokio::test]
async fn items_are_the_files_directly_in_the_directory() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (status, response) = browse::items(&app, items_request(lib.lib, "inbox", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(browse::names(&response, "items"), owned(&BY_NAME_ASC));
    assert_eq!(
        response["total"],
        json!(7),
        "the sidecar and the subfolder are not items"
    );
    assert_eq!(response["offset"], json!(0));

    let (_, response) = browse::items(&app, items_request(lib.lib, "", json!({}))).await;
    assert_eq!(browse::names(&response, "items"), ["root.jpg"]);
    let (_, response) = browse::items(&app, items_request(lib.lib, "inbox/sub", json!({}))).await;
    assert_eq!(browse::names(&response, "items"), ["g.jpg"]);
    let (_, response) = browse::items(&app, items_request(lib.other, "inbox", json!({}))).await;
    assert_eq!(browse::names(&response, "items"), ["intruder.jpg"]);
}

#[tokio::test]
async fn an_item_carries_everything_a_pane_shows() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (_, response) = browse::items(&app, items_request(lib.lib, "inbox", json!({}))).await;
    let ten = &response["items"][0];
    assert_eq!(ten["volume_id"], json!(lib.lib));
    assert_eq!(ten["dir"], json!("inbox"));
    assert_eq!(ten["name"], json!("10.jpg"));
    assert_eq!(ten["kind"], json!("image"));
    assert_eq!(ten["size"], json!(400));
    assert_eq!(ten["capture_ts"], json!(DAY_ONE + 3 * DAY));
    assert_eq!(ten["capture_src"], json!("exif"));
    assert_eq!(ten["camera"], json!("Canon"));
    assert_eq!(ten["rating"], json!(5));
    assert_eq!(ten["flag"], json!(0));
    assert_eq!(ten["in_immich"], json!(false));
    assert_eq!(
        ten["overlay"],
        json!({"matched": false, "planned": false, "will_delete": false})
    );
    let video = &response["items"][5];
    assert_eq!(video["name"], json!("E.mp4"));
    assert_eq!(video["kind"], json!("video"));
    assert_eq!(video["duration_ms"], json!(9_000));

    let mtimes: Vec<i64> = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["mtime"].as_i64().expect("every item has an mtime"))
        .collect();
    assert_eq!(mtimes, [7000, 3000, 1000, 2000, 4000, 5000, 6000]);
}

#[tokio::test]
async fn every_sort_key_and_order() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let cases: [(&str, &str, Vec<String>); 6] = [
        ("name", "asc", owned(&BY_NAME_ASC)),
        ("name", "desc", reversed(&BY_NAME_ASC)),
        (
            "date",
            "asc",
            owned(&[
                "a.jpg", "d.jpg", "B.jpg", "10.jpg", "E.mp4", "c.jpg", "f.jpg",
            ]),
        ),
        (
            "date",
            "desc",
            owned(&[
                "E.mp4", "10.jpg", "B.jpg", "d.jpg", "a.jpg", "c.jpg", "f.jpg",
            ]),
        ),
        (
            "size",
            "asc",
            owned(&[
                "a.jpg", "f.jpg", "c.jpg", "d.jpg", "B.jpg", "10.jpg", "E.mp4",
            ]),
        ),
        (
            "size",
            "desc",
            owned(&[
                "E.mp4", "10.jpg", "B.jpg", "c.jpg", "d.jpg", "a.jpg", "f.jpg",
            ]),
        ),
    ];
    for (sort, order, expected) in cases {
        let body = items_request(lib.lib, "inbox", json!({"sort": sort, "order": order}));
        let (status, items) = browse::items(&app, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{sort} {order}: {items}");
        assert_eq!(browse::names(&items, "items"), expected, "{sort} {order}");

        let (status, ids) = browse::ids(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{sort} {order}: {ids}");
        assert_eq!(
            browse::id_list(&ids),
            browse::item_ids(&items),
            "{sort} {order}: ids are in the order of the items"
        );
    }
}

#[tokio::test]
async fn the_default_order_is_name_ascending() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (_, response) = browse::items(&app, items_request(lib.lib, "inbox", json!({}))).await;
    assert_eq!(browse::names(&response, "items"), owned(&BY_NAME_ASC));
}

#[tokio::test]
async fn filters_restrict_items_and_ids_alike() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let cases = [
        (json!({"ratings": [5]}), vec!["10.jpg", "B.jpg"]),
        (json!({"ratings": [0, 1]}), vec!["d.jpg", "E.mp4", "f.jpg"]),
        (
            json!({"cameras": ["canon"]}),
            vec!["10.jpg", "a.jpg", "B.jpg"],
        ),
        (
            json!({"cameras": ["Sony", "CANON"]}),
            BY_NAME_ASC
                .to_vec()
                .into_iter()
                .filter(|n| !matches!(*n, "d.jpg" | "E.mp4"))
                .collect(),
        ),
        (json!({"tags": ["trip"]}), vec!["B.jpg"]),
        (json!({"tags": ["family", "trip"]}), vec!["a.jpg", "B.jpg"]),
        (
            json!({"ratings": [5], "cameras": ["canon"], "tags": ["trip"]}),
            vec!["B.jpg"],
        ),
        (json!({"ratings": [4]}), vec![]),
        (json!({"cameras": ["Nikon"]}), vec![]),
    ];
    for (filter, expected) in cases {
        let body = items_request(lib.lib, "inbox", json!({"filter": filter}));
        let (status, items) = browse::items(&app, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {items}");
        assert_eq!(browse::names(&items, "items"), owned(&expected), "{filter}");
        assert_eq!(items["total"], json!(expected.len()), "{filter}");

        let (_, ids) = browse::ids(&app, body).await;
        assert_eq!(browse::id_list(&ids), browse::item_ids(&items), "{filter}");
    }
}

#[tokio::test]
async fn an_invalid_filter_is_a_bad_request() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    for filter in [json!({"ratings": [6]}), json!({"ratings": [255]})] {
        let body = items_request(lib.lib, "inbox", json!({"filter": filter}));
        for (status, response) in [
            browse::items(&app, body.clone()).await,
            browse::ids(&app, body.clone()).await,
            browse::facets(&app, body.clone()).await,
            browse::folders(&app, body.clone()).await,
        ] {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{filter}: {response}");
            assert_eq!(browse::error_code(&response), "bad_request");
        }
    }
}

#[tokio::test]
async fn malformed_bodies_are_json_errors() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    for body in [
        json!({"sort": "bogus"}),
        json!({"order": "sideways"}),
        json!({"offset": -1}),
        json!({"limit": "ten"}),
        json!({"volume_id": "one"}),
        json!({"dir": 5}),
        json!({"plan": {"side": "source"}}),
    ] {
        let body = items_request(lib.lib, "inbox", body);
        for (status, response) in [
            browse::items(&app, body.clone()).await,
            browse::ids(&app, body.clone()).await,
        ] {
            assert!(status.is_client_error(), "{body}: {status}");
            assert_eq!(browse::error_code(&response), "invalid_json", "{body}");
        }
    }
}

#[tokio::test]
async fn paging_walks_the_whole_list_without_gaps_or_overlaps() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    for (sort, order) in [("name", "asc"), ("date", "desc"), ("size", "asc")] {
        let (_, whole) = browse::items(
            &app,
            items_request(lib.lib, "inbox", json!({"sort": sort, "order": order})),
        )
        .await;
        let expected = browse::item_ids(&whole);

        let mut walked = Vec::new();
        for offset in [0, 3, 6] {
            let body = items_request(
                lib.lib,
                "inbox",
                json!({"sort": sort, "order": order, "offset": offset, "limit": 3}),
            );
            let (status, page) = browse::items(&app, body).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["total"], json!(7), "total ignores paging");
            assert_eq!(page["offset"], json!(offset));
            walked.extend(browse::item_ids(&page));
        }
        assert_eq!(walked, expected, "{sort} {order}");
    }
}

#[tokio::test]
async fn an_offset_past_the_end_is_an_empty_page_with_the_right_total() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    for offset in [7, 8, 1000, 4_000_000_000_u64] {
        let (status, page) = browse::items(
            &app,
            items_request(lib.lib, "inbox", json!({"offset": offset})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{offset}: {page}");
        assert_eq!(page["items"], json!([]), "{offset}");
        assert_eq!(page["total"], json!(7), "{offset}");
        assert_eq!(page["offset"], json!(offset), "{offset}");
    }
}

#[tokio::test]
async fn ids_ignore_offset_and_limit() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (_, whole) = browse::ids(&app, items_request(lib.lib, "inbox", json!({}))).await;
    let (_, paged) = browse::ids(
        &app,
        items_request(lib.lib, "inbox", json!({"offset": 3, "limit": 2})),
    )
    .await;
    assert_eq!(browse::id_list(&whole).len(), 7);
    assert_eq!(whole, paged);
}

#[tokio::test]
async fn limits_default_clamp_and_shrink() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("bulk").await;
    for number in 0..520 {
        seed.file(volume, "all", &format!("img_{number:04}.jpg"))
            .insert()
            .await;
    }

    let page = |extra: Value| {
        let body = items_request(volume, "all", extra);
        let app = &app;
        async move {
            let (status, response) = browse::items(app, body).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            assert_eq!(response["total"], json!(520));
            response
        }
    };
    assert_eq!(
        page(json!({})).await["items"].as_array().unwrap().len(),
        200
    );
    assert_eq!(
        page(json!({"limit": 0})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        200
    );
    assert_eq!(
        page(json!({"limit": 1})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        page(json!({"limit": 500})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        500
    );
    assert_eq!(
        page(json!({"limit": 501})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        500
    );
    assert_eq!(
        page(json!({"limit": 4_000_000_000_u64})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        500
    );
    let tail = page(json!({"offset": 500, "limit": 500})).await;
    assert_eq!(tail["items"].as_array().unwrap().len(), 20);
    assert_eq!(tail["offset"], json!(500));

    let (_, ids) = browse::ids(&app, items_request(volume, "all", json!({"limit": 1}))).await;
    assert_eq!(browse::id_list(&ids).len(), 520);
}

#[tokio::test]
async fn ids_of_a_large_directory_are_in_the_order_of_its_pages() {
    let app = TestApp::new().await;
    let seed = Seed::new(&app.state.db);
    let volume = seed.volume("bulk").await;
    for number in 0..520 {
        seed.file(volume, "all", &format!("img_{number:04}.jpg"))
            .size(i64::from(number % 7))
            .insert()
            .await;
    }

    for (sort, order) in [("name", "desc"), ("size", "asc"), ("date", "asc")] {
        let mut walked: Vec<FileId> = Vec::new();
        for offset in [0, 500] {
            let body = items_request(
                volume,
                "all",
                json!({"sort": sort, "order": order, "offset": offset, "limit": 500}),
            );
            let (_, page) = browse::items(&app, body).await;
            walked.extend(browse::item_ids(&page));
        }
        let (_, ids) = browse::ids(
            &app,
            items_request(volume, "all", json!({"sort": sort, "order": order})),
        )
        .await;
        assert_eq!(browse::id_list(&ids), walked, "{sort} {order}");
    }
}

#[tokio::test]
async fn facets_cover_the_subtree_of_the_directory() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let (status, response) = browse::facets(&app, items_request(lib.lib, "inbox", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Canon".to_owned(), 3), ("Sony".to_owned(), 3)],
        "inbox/sub/g.jpg is part of the subtree"
    );
    assert_eq!(
        browse::facet(&response, "tags"),
        [("family".to_owned(), 2), ("trip".to_owned(), 2)]
    );
    assert_eq!(
        browse::facet(&response, "ratings"),
        [
            ("0".to_owned(), 2),
            ("3".to_owned(), 2),
            ("5".to_owned(), 2),
            ("1".to_owned(), 1),
            ("2".to_owned(), 1),
        ]
    );

    let (_, response) = browse::facets(&app, items_request(lib.lib, "inbox/sub", json!({}))).await;
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Sony".to_owned(), 1)]
    );
    assert_eq!(browse::facet(&response, "tags"), [("trip".to_owned(), 1)]);
    assert_eq!(browse::facet(&response, "ratings"), [("2".to_owned(), 1)]);

    // The root of the volume is the whole volume, and no other volume leaks in.
    let (_, response) = browse::facets(&app, items_request(lib.lib, "", json!({}))).await;
    assert_eq!(
        browse::facet(&response, "ratings"),
        [
            ("0".to_owned(), 3),
            ("3".to_owned(), 2),
            ("5".to_owned(), 2),
            ("1".to_owned(), 1),
            ("2".to_owned(), 1),
        ],
        "root.jpg joins the 0-star files; the 5-star file of `other` is not counted"
    );
    let (_, response) = browse::facets(&app, items_request(lib.other, "", json!({}))).await;
    assert_eq!(browse::facet(&response, "ratings"), [("5".to_owned(), 1)]);
}

#[tokio::test]
async fn facets_count_each_dimension_under_the_other_filters_only() {
    let app = TestApp::new().await;
    let lib = library(&app).await;

    let body = items_request(lib.lib, "inbox", json!({"filter": {"ratings": [5]}}));
    let (status, response) = browse::facets(&app, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        browse::facet(&response, "cameras"),
        [("Canon".to_owned(), 2)],
        "cameras respect the rating filter"
    );
    assert_eq!(
        browse::facet(&response, "tags"),
        [("family".to_owned(), 1), ("trip".to_owned(), 1)]
    );
    assert_eq!(
        browse::facet(&response, "ratings"),
        [
            ("0".to_owned(), 2),
            ("3".to_owned(), 2),
            ("5".to_owned(), 2),
            ("1".to_owned(), 1),
            ("2".to_owned(), 1),
        ],
        "the ratings are counted ignoring the rating filter"
    );
}
