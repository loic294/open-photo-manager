//! Selections: `POST /api/selections` and `GET /api/selections/{id}`, their limits, their body
//! limits and their life span. The hand-off of a plan's files as a selection is in
//! `plans_overlay.rs`. The real application runs around the scripted planner of
//! `common::fake_planner`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::paths::{FileId, SelectionId, VolumeId};
use apm_server::plans::Swept;
use axum::http::{Method, StatusCode};
use common::browse::{self, error_code};
use common::fake_planner::Harness;
use common::seed::Seed;
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60;

/// The volume `lib` and three files in it: a.jpg (100 bytes), b.jpg (200) and c.jpg (400).
async fn files(h: &Harness) -> (VolumeId, [FileId; 3]) {
    let seed = Seed::new(h.db());
    let lib = seed.volume("lib").await;
    let mut ids = [0; 3];
    for (slot, (name, size)) in ids
        .iter_mut()
        .zip([("a.jpg", 100), ("b.jpg", 200), ("c.jpg", 400)])
    {
        *slot = seed.file(lib, "", name).size(size).insert().await;
    }
    (lib, ids)
}

async fn create(h: &Harness, body: Value) -> (StatusCode, Value) {
    h.app
        .call(Method::POST, "/api/selections", Some(body))
        .await
}

async fn read(h: &Harness, id: SelectionId) -> (StatusCode, Value) {
    h.app
        .call(Method::GET, &format!("/api/selections/{id}"), None)
        .await
}

fn a_long_label(chars: usize, ch: char) -> String {
    std::iter::repeat_n(ch, chars).collect()
}

/// More ids than fit in the default body limit of 1 MiB.
fn many_ids() -> Vec<FileId> {
    (1..=200_000).collect()
}

// ---- create and read ----

#[tokio::test]
async fn a_selection_keeps_the_files_that_exist_once() {
    let h = Harness::new().await;
    let (_, [a, b, c]) = files(&h).await;

    let (status, selection) = create(
        &h,
        json!({ "ids": [c, a, a, 999_999, b, c], "label": "  Holiday picks  " }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(
        selection["item_count"], 3,
        "duplicates and missing files are dropped"
    );
    assert_eq!(selection["total_size"], 700);
    assert_eq!(selection["label"], "Holiday picks");
    assert_eq!(selection["created_at"], h.clock.now());
    assert_eq!(selection["expires_at"], h.clock.now() + DAY);
    assert!(selection["id"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn a_selection_can_be_read_back() {
    let h = Harness::new().await;
    let (_, [a, _, c]) = files(&h).await;
    let (status, created) = create(&h, json!({ "ids": [a, c] })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["label"], Value::Null);

    let (status, read_back) = read(&h, created["id"].as_i64().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(read_back, created);
}

#[tokio::test]
async fn every_selection_has_an_id_of_its_own() {
    let h = Harness::new().await;
    let (_, [a, b, _]) = files(&h).await;
    let (_, first) = create(&h, json!({ "ids": [a] })).await;
    let (_, second) = create(&h, json!({ "ids": [b] })).await;
    assert_ne!(first["id"], second["id"]);
    assert_eq!(
        read(&h, first["id"].as_i64().unwrap()).await.1["total_size"],
        100
    );
    assert_eq!(
        read(&h, second["id"].as_i64().unwrap()).await.1["total_size"],
        200
    );
}

#[tokio::test]
async fn an_unknown_selection_is_not_found() {
    let h = Harness::new().await;
    let (status, body) = read(&h, 4242).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "selection_not_found");

    let (status, body) = h.app.call(Method::GET, "/api/selections/abc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_path");
}

#[tokio::test]
async fn a_selection_without_files_is_still_a_selection() {
    let h = Harness::new().await;
    files(&h).await;
    for ids in [json!([]), json!([999_999])] {
        let (status, selection) = create(&h, json!({ "ids": ids })).await;
        assert_eq!(status, StatusCode::CREATED, "{ids}: {selection}");
        assert_eq!(selection["item_count"], 0, "{ids}");
        assert_eq!(selection["total_size"], 0, "{ids}");
    }
}

#[tokio::test]
async fn a_file_that_goes_away_leaves_its_selections() {
    let h = Harness::new().await;
    let (_, [a, b, c]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [a, b, c] })).await;
    let id = selection["id"].as_i64().unwrap();

    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(b)
        .execute(h.db().write())
        .await
        .unwrap();

    let (status, now) = read(&h, id).await;
    assert_eq!(status, StatusCode::OK, "{now}");
    assert_eq!(now["item_count"], 2);
    assert_eq!(now["total_size"], 500);
}

#[tokio::test]
async fn a_selection_restricts_a_listing() {
    let h = Harness::new().await;
    let (lib, [a, _, c]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [c, a] })).await;

    let (status, items) = browse::items(
        &h.app,
        json!({ "volume_id": lib, "dir": "", "filter": { "selection_id": selection["id"] } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(browse::names(&items, "items"), ["a.jpg", "c.jpg"]);
}

/// The `matched` flag of every item of a listing, in order.
fn matched_flags(items: &Value) -> Vec<bool> {
    items["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["overlay"]["matched"].as_bool().unwrap())
        .collect()
}

#[tokio::test]
async fn a_plan_is_built_from_the_files_of_a_selection() {
    let h = Harness::new().await;
    let (lib, [a, _, c]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [c, a] })).await;
    let scope = json!({ "volume_id": lib, "dir": "" });

    let everything = h.new_plan(json!({ "select": { "scope": scope } })).await;
    assert_eq!(h.fake_rows(Some(everything)).await, 3);

    let restricted = h
        .new_plan(json!({
            "select": { "scope": scope, "filter": { "selection_id": selection["id"] } }
        }))
        .await;
    assert_eq!(
        h.fake_rows(Some(restricted)).await,
        2,
        "only the selection is planned"
    );

    let (status, handed) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{restricted}/selection"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{handed}");
    assert_eq!(handed["item_count"], 2);
    assert_eq!(handed["total_size"], 500);
}

#[tokio::test]
async fn a_plan_is_browsed_restricted_to_a_selection() {
    let h = Harness::new().await;
    let (lib, [a, b, c]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [a, b] })).await;
    let plan = h.new_plan(json!({ "matched": [a, c] })).await;
    let pane = |filter: Value| {
        json!({
            "volume_id": lib,
            "dir": "",
            "plan": { "plan_id": plan, "side": "source" },
            "filter": filter,
        })
    };

    let (status, everything) = browse::items(&h.app, pane(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{everything}");
    assert_eq!(
        browse::names(&everything, "items"),
        ["a.jpg", "b.jpg", "c.jpg"]
    );
    assert_eq!(matched_flags(&everything), [true, false, true]);

    let (status, restricted) =
        browse::items(&h.app, pane(json!({ "selection_id": selection["id"] }))).await;
    assert_eq!(status, StatusCode::OK, "{restricted}");
    assert_eq!(browse::names(&restricted, "items"), ["a.jpg", "b.jpg"]);
    assert_eq!(matched_flags(&restricted), [true, false]);
    assert_eq!(restricted["total"], 2);
}

#[tokio::test]
async fn a_selection_outlives_the_plan_it_came_from() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;
    let plan = h.new_plan(json!({ "matched": [a] })).await;
    let (status, selection) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/selection"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");

    let (status, _) = h.delete_plan(plan).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    h.settle().await;
    assert_eq!(h.raw_state(plan).await, None);

    let (status, kept) = read(&h, selection["id"].as_i64().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["item_count"], 1);
}

#[tokio::test]
async fn ids_must_be_a_list_of_integers() {
    let h = Harness::new().await;
    for body in [
        json!({}),
        json!({ "ids": "1,2" }),
        json!({ "ids": [1.5] }),
        json!({ "ids": ["a"] }),
        json!({ "ids": [1], "label": 7 }),
    ] {
        let (status, reply) = create(&h, body.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {reply}");
        assert_eq!(error_code(&reply), "invalid_json", "{body}");
    }
}

// ---- labels ----

#[tokio::test]
async fn a_blank_label_is_no_label() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;
    for label in [json!(null), json!(""), json!("   \t ")] {
        let (status, selection) = create(&h, json!({ "ids": [a], "label": label })).await;
        assert_eq!(status, StatusCode::CREATED, "{label}: {selection}");
        assert_eq!(selection["label"], Value::Null, "{label}");
    }
}

#[tokio::test]
async fn a_label_has_a_limit_of_two_hundred_characters() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;

    for label in [a_long_label(200, 'x'), a_long_label(200, 'é')] {
        let (status, selection) = create(&h, json!({ "ids": [a], "label": label })).await;
        assert_eq!(status, StatusCode::CREATED, "{selection}");
        assert_eq!(selection["label"], label, "characters count, not bytes");
    }

    let (status, body) = create(&h, json!({ "ids": [a], "label": a_long_label(201, 'x') })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_selection");
    let (status, body) = create(
        &h,
        json!({ "ids": [a], "label": format!("  {}  ", a_long_label(201, 'x')) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_selection");

    let (_, kept) = create(
        &h,
        json!({ "ids": [a], "label": format!("  {}  ", a_long_label(200, 'x')) }),
    )
    .await;
    assert_eq!(
        kept["label"],
        a_long_label(200, 'x'),
        "the padding does not count"
    );
}

// ---- limits ----

#[tokio::test]
async fn the_number_of_ids_is_limited() {
    let h = Harness::tuned(|options| options.max_selection_ids = 3).await;
    let (_, [a, b, c]) = files(&h).await;

    let (status, selection) = create(&h, json!({ "ids": [a, b, c] })).await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(selection["item_count"], 3);

    let (status, selection) = create(&h, json!({ "ids": [a, a, a, a, b, c] })).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "each id counts once: {selection}"
    );

    let (status, body) = create(&h, json!({ "ids": [a, b, c, 999_999] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "selection_too_large");

    let (status, body) = create(&h, json!({ "ids": [1, 2, 3, 4, 5, 6] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "selection_too_large");
}

#[tokio::test]
async fn the_number_of_ids_a_plan_hands_over_is_limited_too() {
    let h = Harness::tuned(|options| options.max_selection_ids = 2).await;
    let (_, [a, b, c]) = files(&h).await;
    let plan = h.new_plan(json!({ "matched": [a, b, c] })).await;

    let (status, body) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/selection"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "selection_too_large");

    let (status, selection) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/selection"),
            Some(json!({ "restrict_ids": [a, c] })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(selection["item_count"], 2);
}

#[tokio::test]
async fn only_the_routes_that_carry_id_sets_accept_a_large_body() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;
    let plan = h.new_plan(json!({ "matched": [a] })).await;
    let ids = many_ids();
    assert!(
        json!({ "ids": ids }).to_string().len() > 1024 * 1024,
        "the body is over the default limit"
    );

    let (status, selection) = create(&h, json!({ "ids": ids })).await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(
        selection["item_count"], 3,
        "the files in the list are found"
    );

    let (status, selection) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/selection"),
            Some(json!({ "restrict_ids": ids })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(selection["item_count"], 1);

    let (status, body) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/execute"),
            Some(json!({ "options": { "fail": ids } })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(error_code(&body), "payload_too_large");

    let (status, body) = h
        .app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({ "kind": "cleanup", "params": { "matched": ids } })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(error_code(&body), "payload_too_large");
    assert_eq!(h.planner().builds().len(), 1, "that build never started");
}

// ---- life span ----

#[tokio::test]
async fn an_unused_selection_expires_and_a_sweep_removes_it() {
    let h = Harness::new().await;
    let (_, [a, b, _]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [a, b] })).await;
    let id = selection["id"].as_i64().unwrap();

    h.clock.advance(DAY - 1);
    assert_eq!(
        read(&h, id).await.0,
        StatusCode::OK,
        "still there a second before"
    );

    h.clock.advance(DAY);
    let (status, body) = read(&h, id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "selection_not_found");

    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(
        swept,
        Swept {
            selections: 1,
            ..Swept::default()
        }
    );
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM selection_items")
        .fetch_one(h.db().read())
        .await
        .unwrap();
    assert_eq!(members, 0, "the members go with the selection");
}

#[tokio::test]
async fn a_selection_that_is_read_lives_on() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [a] })).await;
    let id = selection["id"].as_i64().unwrap();
    let created = h.clock.now();

    h.clock.advance(DAY - 3_600);
    assert_eq!(read(&h, id).await.0, StatusCode::OK);
    h.settle().await;
    h.clock.advance(2 * 3_600);

    let (status, body) = read(&h, id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "it was used an hour before its end: {body}"
    );
    assert_eq!(body["expires_at"], created + DAY - 3_600 + DAY);
    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(swept.selections, 0);
}

#[tokio::test]
async fn a_selection_that_restricts_a_run_lives_on() {
    let h = Harness::new().await;
    let (_, [a, ..]) = files(&h).await;
    let (_, selection) = create(&h, json!({ "ids": [a] })).await;
    let id = selection["id"].as_i64().unwrap();
    let created = h.clock.now();

    // A plan lives two hours, so this one is made a few minutes before the selection ends.
    h.clock.advance(DAY - 600);
    let plan = h.new_plan(json!({ "matched": [a] })).await;
    let (status, body) = h.execute_only(plan, json!({}), id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    h.finish(body["id"].as_i64().unwrap()).await;
    h.settle().await;

    h.clock.advance(1_200);
    let (status, kept) = read(&h, id).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["expires_at"], created + DAY - 600 + DAY);
}
