//! The libraries of Immich, the volume-to-library mappings, the exclusion of the trash folder and
//! the detection of the path prefix, through `/api/immich/*` against the stateful fake Immich of
//! `immich_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use axum::http::StatusCode;
use immich_support::{Fixture, entry, problem_fields};
use opm_core::paths::RelPath;
use serde_json::{Value, json};

const TRASH_PATTERN: &str = "**/.opm-trash/**";

/// The files `index_media` writes into the index of a volume, relative to its root.
const MEDIA: [&str; 4] = [
    "2019/IMG_1.jpg",
    "2019/IMG_2.jpg",
    "2020/summer/IMG_3.jpg",
    "root.jpg",
];

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

/// The `id` of every item of `list`, sorted.
fn ids(list: &Value) -> Vec<String> {
    let mut ids: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

fn sorted(ids: &[&str]) -> Vec<String> {
    let mut sorted: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    sorted.sort();
    sorted
}

/// Puts the pictures of `MEDIA` into the index of `volume`, and a text file that is no media.
async fn index_media(fx: &Fixture, volume: i64) {
    for rel in MEDIA {
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        fx.index_file(volume, dir, name).await;
    }
    fx.index_file(volume, "", "notes.txt").await;
}

/// Gives the library an asset for every file of `MEDIA`, as Immich sees it under `prefix`.
fn add_media_assets(fx: &Fixture, library: &str, prefix: &str) {
    for rel in MEDIA {
        fx.fake.add_asset(library, &format!("{prefix}/{rel}"));
    }
}

async fn detect(fx: &Fixture, volume: i64, library: Option<&str>) -> (StatusCode, Value) {
    fx.post(
        "/api/immich/detect-prefix",
        json!({ "volume_id": volume, "library_id": library }),
    )
    .await
}

// ----------------------------------------------------------------------------
// Libraries and mappings
// ----------------------------------------------------------------------------

#[tokio::test]
async fn every_route_needs_a_connection() {
    let fx = Fixture::new().await;
    let volume = fx.volume("photos").await;
    let library = "00000000-0000-4000-8000-0000000000aa";

    let answers = [
        fx.get("/api/immich/libraries").await,
        fx.put_mappings(vec![entry(volume, Some(library), "/mnt/photo")])
            .await,
        fx.post_empty("/api/immich/exclude-trash").await,
        detect(&fx, volume, None).await,
    ];

    for (status, body) in answers {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(error_code(&body), "immich_not_connected");
    }
    assert!(fx.fake.calls().is_empty());
}

#[tokio::test]
async fn a_valid_set_is_saved_reported_and_used_by_the_path_map() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    fx.fake.clear_calls();
    let set = || vec![entry(volume, Some(&library), "/mnt/photo")];

    let (status, saved) = fx.put_mappings(set()).await;

    assert_eq!(status, StatusCode::OK, "{saved}");
    let expected = json!([{
        "volume_id": volume,
        "volume_name": "photos",
        "library_id": library,
        "library_name": "Photos",
        "path_prefix": "/mnt/photo",
    }]);
    assert_eq!(saved["mappings"], expected);
    assert_eq!(fx.status(false).await["mappings"], expected);
    // The library was looked up in Immich, and nothing else was asked.
    assert_eq!(fx.fake.sequence(), ["GET /api/libraries"]);

    // Saving the same set again changes nothing.
    let (status, again) = fx.put_mappings(set()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, saved);

    let map = fx.app.state.immich.path_map().await.unwrap();
    assert_eq!(map.len(), 1);
    let rel = RelPath::new("2019/IMG_1.jpg").unwrap();
    assert_eq!(
        map.to_immich(volume, &rel).as_deref(),
        Some("/mnt/photo/2019/IMG_1.jpg")
    );
    let (found_volume, found_rel) = map.from_immich("/mnt/photo/2019/IMG_1.jpg").unwrap();
    assert_eq!(
        (found_volume, found_rel.as_str()),
        (volume, "2019/IMG_1.jpg")
    );
    assert!(map.from_immich("/mnt/other/2019/IMG_1.jpg").is_none());
    assert!(
        map.from_immich("/mnt/photographs/IMG_1.jpg").is_none(),
        "a prefix ends at a path segment"
    );
}

#[tokio::test]
async fn saving_replaces_the_whole_set_and_an_empty_set_clears_it() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt"]);
    let first = fx.volume("first").await;
    let second = fx.volume("second").await;
    let rel = RelPath::new("a.jpg").unwrap();

    let (status, body) = fx
        .put_mappings(vec![entry(first, Some(&library), "/mnt/first")])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = fx
        .put_mappings(vec![entry(second, Some(&library), "/mnt/second")])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mappings = fx.status(false).await["mappings"].clone();
    assert_eq!(mappings.as_array().unwrap().len(), 1);
    assert_eq!(mappings[0]["volume_id"], second);
    let map = fx.app.state.immich.path_map().await.unwrap();
    assert!(map.to_immich(first, &rel).is_none());
    assert!(map.to_immich(second, &rel).is_some());

    fx.fake.clear_calls();
    let (status, body) = fx.put_mappings(Vec::new()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "mappings": [] }));
    assert_eq!(fx.status(false).await["mappings"], json!([]));
    assert!(fx.app.state.immich.path_map().await.unwrap().is_empty());
    assert!(
        fx.fake.calls().is_empty(),
        "an empty set has no library to look up"
    );
}

#[tokio::test]
async fn a_set_that_breaks_a_rule_is_refused_as_a_whole_without_asking_immich() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt"]);
    let library = library.as_str();
    let a = fx.volume("a").await;
    let b = fx.volume("b").await;
    let (status, kept) = fx
        .put_mappings(vec![entry(a, Some(library), "/mnt/a")])
        .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    fx.fake.clear_calls();

    // Every case lists the fields that must be named, in order.
    let cases: Vec<(&str, Vec<Value>, Vec<&str>)> = vec![
        (
            "the same prefix twice",
            vec![
                entry(a, Some(library), "/mnt/x"),
                entry(b, Some(library), "/mnt/x"),
            ],
            vec!["mappings[1].path_prefix"],
        ),
        (
            "a prefix inside an earlier one",
            vec![
                entry(a, Some(library), "/mnt/x"),
                entry(b, Some(library), "/mnt/x/y"),
            ],
            vec!["mappings[1].path_prefix"],
        ),
        (
            "a prefix that contains a later one",
            vec![
                entry(a, Some(library), "/mnt/x/y"),
                entry(b, Some(library), "/mnt/x"),
            ],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "a relative prefix",
            vec![entry(a, Some(library), "mnt/x")],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "a prefix with ..",
            vec![entry(a, Some(library), "/mnt/../x")],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "a prefix with a trailing slash",
            vec![entry(a, Some(library), "/mnt/x/")],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "an empty prefix",
            vec![entry(a, Some(library), "")],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "a prefix with an empty segment",
            vec![entry(a, Some(library), "/mnt//x")],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "the root next to another mapping",
            vec![
                entry(a, Some(library), "/"),
                entry(b, Some(library), "/mnt/x"),
            ],
            vec!["mappings[0].path_prefix"],
        ),
        (
            "the same volume twice",
            vec![
                entry(a, Some(library), "/mnt/x"),
                entry(a, Some(library), "/mnt/y"),
            ],
            vec!["mappings[1].volume_id"],
        ),
        (
            "a volume that does not exist",
            vec![entry(9999, Some(library), "/mnt/x")],
            vec!["mappings[0].volume_id"],
        ),
        (
            "no library",
            vec![entry(a, None, "/mnt/x")],
            vec!["mappings[0].library_id"],
        ),
        (
            "a library id that is no id",
            vec![entry(a, Some("../x"), "/mnt/x")],
            vec!["mappings[0].library_id"],
        ),
        (
            "several problems at once",
            vec![entry(a, None, "rel"), entry(a, Some("../x"), "/mnt/y")],
            vec![
                "mappings[0].path_prefix",
                "mappings[0].library_id",
                "mappings[1].library_id",
                "mappings[1].volume_id",
            ],
        ),
    ];
    for (what, entries, fields) in cases {
        let (status, body) = fx.put_mappings(entries).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {body}");
        assert_eq!(error_code(&body), "validation_failed", "{what}");
        assert_eq!(problem_fields(&body), fields, "{what}");
    }
    assert!(
        fx.fake.calls().is_empty(),
        "a set that is wrong in itself is refused before Immich is asked: {:?}",
        fx.fake.sequence()
    );
    assert_eq!(fx.status(false).await["mappings"], kept["mappings"]);
}

#[tokio::test]
async fn a_library_that_immich_does_not_have_is_refused_and_nothing_changes() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt"]);
    let a = fx.volume("a").await;
    let b = fx.volume("b").await;
    let (_, kept) = fx
        .put_mappings(vec![entry(a, Some(&library), "/mnt/a")])
        .await;
    fx.fake.clear_calls();

    let (status, body) = fx
        .put_mappings(vec![
            entry(a, Some(&library), "/mnt/a"),
            entry(b, Some("00000000-0000-4000-8000-0000000000ff"), "/mnt/b"),
        ])
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(problem_fields(&body), ["mappings[1].library_id"]);
    assert_eq!(fx.fake.sequence(), ["GET /api/libraries"]);
    assert_eq!(fx.status(false).await["mappings"], kept["mappings"]);
}

#[tokio::test]
async fn without_administrator_rights_a_mapping_is_only_a_path_prefix() {
    let fx = Fixture::connected_limited().await;
    let volume = fx.volume("photos").await;
    fx.fake.clear_calls();

    let (status, libraries) = fx.get("/api/immich/libraries").await;
    assert_eq!(status, StatusCode::OK, "{libraries}");
    assert_eq!(libraries, json!({ "libraries": [], "limited": true }));

    let (status, saved) = fx
        .put_mappings(vec![entry(volume, None, "/mnt/photo")])
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["mappings"],
        json!([{
            "volume_id": volume,
            "volume_name": "photos",
            "library_id": null,
            "library_name": "",
            "path_prefix": "/mnt/photo",
        }])
    );

    // The prefix is checked all the same.
    let (status, body) = fx.put_mappings(vec![entry(volume, None, "photo")]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(problem_fields(&body), ["mappings[0].path_prefix"]);

    let (status, body) = fx.post_empty("/api/immich/exclude-trash").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "immich_admin_required");

    assert!(
        fx.fake.calls().is_empty(),
        "an account that is no administrator cannot use the libraries, so none is asked: {:?}",
        fx.fake.sequence()
    );
    // The prefix is all the sync and the outbox need.
    let map = fx.app.state.immich.path_map().await.unwrap();
    assert_eq!(
        map.to_immich(volume, &RelPath::new("a.jpg").unwrap())
            .as_deref(),
        Some("/mnt/photo/a.jpg")
    );
}

#[tokio::test]
async fn the_libraries_are_listed_live_and_the_names_of_mappings_follow() {
    let fx = Fixture::connected().await;
    let photos = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let videos = fx.fake.add_library("Videos", &["/mnt/video", "/mnt/film"]);
    fx.fake.add_asset(&photos, "/mnt/photo/a.jpg");
    fx.fake.add_asset(&photos, "/mnt/photo/b.jpg");
    let volume = fx.volume("photos").await;
    let (status, body) = fx
        .put_mappings(vec![entry(volume, Some(&photos), "/mnt/photo")])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, listed) = fx.get("/api/immich/libraries").await;

    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        listed,
        json!({
            "libraries": [
                {
                    "id": photos,
                    "name": "Photos",
                    "import_paths": ["/mnt/photo"],
                    "exclusion_patterns": [],
                    "asset_count": 2,
                },
                {
                    "id": videos,
                    "name": "Videos",
                    "import_paths": ["/mnt/video", "/mnt/film"],
                    "exclusion_patterns": [],
                    "asset_count": 0,
                },
            ],
            "limited": false,
        })
    );

    // A library that is renamed in Immich shows its new name once the libraries were listed.
    fx.fake.state().libraries[0].name = "Family photos".to_owned();
    assert_eq!(
        fx.status(false).await["mappings"][0]["library_name"],
        "Photos"
    );
    fx.get("/api/immich/libraries").await;
    assert_eq!(
        fx.status(false).await["mappings"][0]["library_name"],
        "Family photos"
    );
}

#[tokio::test]
async fn the_mappings_stay_when_the_connection_ends() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    let (_, saved) = fx
        .put_mappings(vec![entry(volume, Some(&library), "/mnt/photo")])
        .await;

    fx.delete("/api/immich/connection").await;

    let status = fx.status(false).await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["mappings"], saved["mappings"]);
    assert_eq!(fx.app.state.immich.path_map().await.unwrap().len(), 1);
    let (code, body) = fx.put_mappings(Vec::new()).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "immich_not_connected");

    fx.connect_ok().await;
    assert_eq!(fx.status(false).await["mappings"], saved["mappings"]);
}

// ----------------------------------------------------------------------------
// The trash folder
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_trash_folder_is_excluded_once_in_every_library_that_is_used() {
    let fx = Fixture::connected().await;
    let photos = fx.fake.add_library("Photos", &["/mnt"]);
    let videos = fx.fake.add_library("Videos", &["/mnt/video"]);
    let unused = fx.fake.add_library("Unused", &["/mnt/unused"]);
    fx.fake.state().libraries[0].exclusion_patterns = vec!["**/@eaDir/**".to_owned()];
    let a = fx.volume("a").await;
    let b = fx.volume("b").await;
    let c = fx.volume("c").await;
    let (status, body) = fx
        .put_mappings(vec![
            entry(a, Some(&photos), "/mnt/a"),
            entry(b, Some(&videos), "/mnt/video"),
            entry(c, Some(&photos), "/mnt/c"),
        ])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fx.fake.clear_calls();

    let (status, result) = fx.post_empty("/api/immich/exclude-trash").await;

    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["pattern"], TRASH_PATTERN);
    assert_eq!(ids(&result["updated"]), sorted(&[&photos, &videos]));
    assert_eq!(result["already_excluded"], json!([]));
    assert_eq!(result["failed"], json!([]));
    // What the administrator configured stays; the pattern comes after it.
    assert_eq!(
        fx.fake.library(&photos).exclusion_patterns,
        ["**/@eaDir/**", TRASH_PATTERN]
    );
    assert_eq!(fx.fake.library(&videos).exclusion_patterns, [TRASH_PATTERN]);
    assert!(fx.fake.library(&unused).exclusion_patterns.is_empty());
    // Photos holds two volumes and is still updated once, with the complete list.
    let photos_path = format!("/api/libraries/{photos}");
    let updates = fx.fake.calls_to("PUT", &photos_path);
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0].body,
        Some(json!({ "exclusionPatterns": ["**/@eaDir/**", TRASH_PATTERN] }))
    );
    assert_eq!(fx.fake.count("PUT", &format!("/api/libraries/{videos}")), 1);

    // A second time there is nothing to do, so nothing is changed.
    fx.fake.clear_calls();
    let (status, again) = fx.post_empty("/api/immich/exclude-trash").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["updated"], json!([]));
    assert_eq!(ids(&again["already_excluded"]), sorted(&[&photos, &videos]));
    assert!(fx.fake.calls().iter().all(|call| call.method != "PUT"));
}

#[tokio::test]
async fn a_library_that_cannot_be_updated_is_reported_and_the_others_are_still_done() {
    let fx = Fixture::connected().await;
    let photos = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let videos = fx.fake.add_library("Videos", &["/mnt/video"]);
    let a = fx.volume("a").await;
    let b = fx.volume("b").await;
    let (status, body) = fx
        .put_mappings(vec![
            entry(a, Some(&photos), "/mnt/photo"),
            entry(b, Some(&videos), "/mnt/video"),
        ])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fx.fake
        .fail("PUT", &format!("/api/libraries/{photos}"), 500);

    let (status, result) = fx.post_empty("/api/immich/exclude-trash").await;

    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(ids(&result["updated"]), sorted(&[&videos]));
    let failed = result["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{result}");
    assert_eq!(failed[0]["id"], photos);
    assert_eq!(failed[0]["name"], "Photos");
    assert!(failed[0]["message"].as_str().unwrap().contains("500"));
    assert!(fx.fake.library(&photos).exclusion_patterns.is_empty());

    // Once Immich works again, running it again finishes the job.
    fx.fake.heal();
    let (status, healed) = fx.post_empty("/api/immich/exclude-trash").await;
    assert_eq!(status, StatusCode::OK, "{healed}");
    assert_eq!(ids(&healed["updated"]), sorted(&[&photos]));
    assert_eq!(ids(&healed["already_excluded"]), sorted(&[&videos]));
    assert_eq!(healed["failed"], json!([]));
    assert_eq!(fx.fake.library(&photos).exclusion_patterns, [TRASH_PATTERN]);
}

#[tokio::test]
async fn when_immich_cannot_be_reached_the_exclusion_fails_as_a_whole() {
    let mut fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    let (status, body) = fx
        .put_mappings(vec![entry(volume, Some(&library), "/mnt/photo")])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fx.fake.stop().await;

    let (status, body) = fx.post_empty("/api/immich/exclude-trash").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_unreachable");
}

#[tokio::test]
async fn with_no_mapping_there_is_nothing_to_exclude() {
    let fx = Fixture::connected().await;
    fx.fake.add_library("Photos", &["/mnt/photo"]);
    fx.fake.clear_calls();

    let (status, result) = fx.post_empty("/api/immich/exclude-trash").await;

    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result,
        json!({
            "pattern": TRASH_PATTERN,
            "updated": [],
            "already_excluded": [],
            "failed": [],
        })
    );
    assert!(fx.fake.calls().is_empty());
}

// ----------------------------------------------------------------------------
// Detecting the prefix
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_prefix_of_a_volume_is_found_from_the_files_immich_knows() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    add_media_assets(&fx, &library, "/mnt/photo");
    fx.fake.clear_calls();

    let (status, body) = detect(&fx, volume, None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "suggestions": [{ "prefix": "/mnt/photo", "matched": 4, "sampled": 4 }] })
    );
    // One look-up per media file, by its name; the text file is not one, and nothing was changed.
    assert_eq!(fx.fake.count("POST", "/api/search/metadata"), 4);
    assert_eq!(fx.fake.calls().len(), 4);
    assert!(!fx.fake.calls().iter().any(|call| {
        call.body
            .as_ref()
            .unwrap()
            .to_string()
            .contains("notes.txt")
    }));
}

#[tokio::test]
async fn candidates_are_ranked_by_votes_and_look_alikes_get_none() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    add_media_assets(&fx, &library, "/mnt/photo");
    // A second place that holds one of the files under the same relative path.
    fx.fake.add_asset(&library, "/data/2019/IMG_1.jpg");
    // The same name in another folder, and a folder whose name only ends like a real one.
    fx.fake.add_asset(&library, "/mnt/other/IMG_2.jpg");
    fx.fake
        .add_asset(&library, "/mnt/photo/x2020/summer/IMG_3.jpg");

    let (status, body) = detect(&fx, volume, None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "suggestions": [
            { "prefix": "/mnt/photo", "matched": 4, "sampled": 4 },
            { "prefix": "/data", "matched": 1, "sampled": 4 },
        ] })
    );
}

#[tokio::test]
async fn a_volume_whose_files_are_unknown_to_immich_gets_no_suggestion() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    let empty = json!({ "suggestions": [] });

    let (status, body) = detect(&fx, volume, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, empty);

    fx.fake.add_asset(&library, "/mnt/photo/unrelated.jpg");
    let (status, body) = detect(&fx, volume, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, empty);

    let nothing = fx.volume("nothing").await;
    let (status, body) = detect(&fx, nothing, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an empty volume has no file to look up"
    );
    assert_eq!(body, empty);
}

#[tokio::test]
async fn a_library_can_restrict_the_look_up() {
    let fx = Fixture::connected().await;
    let photos = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let backup = fx.fake.add_library("Backup", &["/mnt/backup"]);
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    add_media_assets(&fx, &photos, "/mnt/photo");
    add_media_assets(&fx, &backup, "/mnt/backup");

    let (status, everywhere) = detect(&fx, volume, None).await;
    assert_eq!(status, StatusCode::OK, "{everywhere}");
    assert_eq!(
        everywhere,
        json!({ "suggestions": [
            { "prefix": "/mnt/backup", "matched": 4, "sampled": 4 },
            { "prefix": "/mnt/photo", "matched": 4, "sampled": 4 },
        ] })
    );

    let (status, restricted) = detect(&fx, volume, Some(&photos)).await;
    assert_eq!(status, StatusCode::OK, "{restricted}");
    assert_eq!(
        restricted,
        json!({ "suggestions": [{ "prefix": "/mnt/photo", "matched": 4, "sampled": 4 }] })
    );
}

#[tokio::test]
async fn a_large_volume_is_sampled_not_read_through() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    for number in 0..100 {
        let name = format!("IMG_{number:03}.jpg");
        fx.index_file(volume, "2021", &name).await;
        fx.fake
            .add_asset(&library, &format!("/mnt/photo/2021/{name}"));
    }
    fx.fake.clear_calls();

    let (status, body) = detect(&fx, volume, None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let searches = fx.fake.count("POST", "/api/search/metadata");
    assert!((1..=30).contains(&searches), "{searches} look-ups");
    let suggestion = &body["suggestions"][0];
    assert_eq!(suggestion["prefix"], "/mnt/photo");
    assert_eq!(suggestion["matched"], suggestion["sampled"]);
    assert_eq!(suggestion["sampled"], searches);
}

#[tokio::test]
async fn a_prefix_can_be_found_without_administrator_rights() {
    let fx = Fixture::connected_limited().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photo"]);
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    add_media_assets(&fx, &library, "/mnt/photo");

    let (status, body) = detect(&fx, volume, None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "suggestions": [{ "prefix": "/mnt/photo", "matched": 4, "sampled": 4 }] })
    );
}

#[tokio::test]
async fn a_bad_detect_request_is_refused_before_asking_immich() {
    let fx = Fixture::connected().await;
    let volume = fx.volume("photos").await;
    fx.fake.clear_calls();

    let (status, body) = detect(&fx, 9999, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(problem_fields(&body), ["volume_id"]);

    let (status, body) = detect(&fx, volume, Some("../x")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(problem_fields(&body), ["library_id"]);

    let (status, body) = detect(&fx, 9999, Some("../x")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(problem_fields(&body), ["library_id", "volume_id"]);

    assert!(fx.fake.calls().is_empty());
}

#[tokio::test]
async fn when_immich_cannot_be_reached_the_detection_says_so() {
    let mut fx = Fixture::connected().await;
    let volume = fx.volume("photos").await;
    index_media(&fx, volume).await;
    fx.fake.stop().await;

    let (status, body) = detect(&fx, volume, None).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_unreachable");
}
