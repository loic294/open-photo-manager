//! The routes of the outbox of file operations (`GET /api/immich/outbox`, `POST
//! /api/immich/outbox/retry` and `POST /api/immich/outbox/dismiss`) and the `outbox` of the Immich
//! status: what a person sees of the queue and what they may do with it. The rules, each with its
//! test:
//!
//! - without a connection nothing is queued, the list is empty and changing the queue answers 409,
//! - the list names every change by its kind and by volume, folder and file, oldest first, in
//!   pages of at most 200, and can be limited to one state,
//! - reading never changes anything and never asks Immich,
//! - retry and dismiss are POSTs that need the marker of the application and name at most 10,000
//!   ids; dismiss never asks Immich,
//! - disconnecting, or connecting as another account, drops the queue; a new key for the same
//!   account keeps it,
//! - the status counts what waits, what waits for Immich and what needs attention,
//! - a burst of changes is worked off by one task.
//!
//! Every test checks that the fake recorded no forced delete of an asset that was still online.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use apm_core::dto::TaskKind;
use apm_server::db::now_secs;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use immich_support::{
    Fixture, PassState, Site, first_pass, many_photos, moved_photo, new_id, one_photo_limited,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const OUTBOX: &str = "/api/immich/outbox";
const RETRY: &str = "/api/immich/outbox/retry";
const DISMISS: &str = "/api/immich/outbox/dismiss";

/// The most ids a request may name.
const MAX_IDS: i64 = 10_000;

/// The code of an error body.
fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

/// `POST path` with `body` exactly as given. The marker header of the application is sent only
/// when `marked`.
async fn post_raw(fx: &Fixture, path: &str, body: &str, marked: bool) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json");
    if marked {
        request = request.header("x-apm-request", "1");
    }
    let request = request.body(Body::from(body.to_owned())).unwrap();
    let response = fx.app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The ids `from..from + count` as a JSON body of a request.
fn ids_body(from: i64, count: i64) -> String {
    let ids: Vec<i64> = (from..from + count).collect();
    json!({ "ids": ids }).to_string()
}

/// Moves the photos `2020/p000.jpg` and on of [`many_photos`] to `2021/`: one change each.
async fn move_all(fx: &Fixture, photos: &Site, count: usize) {
    for n in 0..count {
        fx.move_file(
            (photos, &format!("2020/p{n:03}.jpg")),
            (photos, &format!("2021/p{n:03}.jpg")),
        )
        .await;
    }
}

/// Makes the change `id` one that needs attention, as a pass does when it gave up on it.
async fn give_up(fx: &Fixture, id: i64, why: &str) {
    sqlx::query(
        "UPDATE immich_outbox SET state = 'attention', attempts = 3, last_error = ? WHERE id = ?",
    )
    .bind(why)
    .bind(id)
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
}

/// Makes the change `id` wait `secs` seconds for its next try, as a pass does after Immich failed.
async fn wait_for(fx: &Fixture, id: i64, secs: i64) {
    sqlx::query(
        "UPDATE immich_outbox SET attempts = 2, last_error = 'immich_unavailable', \
         next_attempt_at = ? WHERE id = ?",
    )
    .bind(now_secs() + secs)
    .bind(id)
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
}

/// What the worker says about its runs: it failed `failures` times in a row.
async fn fail_runs(fx: &Fixture, failures: i64) {
    sqlx::query(
        "UPDATE immich_outbox_state SET last_run_at = ?, last_error = 'Immich did not answer', \
         consecutive_failures = ? WHERE id = 1",
    )
    .bind(now_secs())
    .bind(failures)
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
}

/// The ids of the changes of a page, in the order of the page.
fn ids_of(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .expect("a page has items")
        .iter()
        .map(|item| item["id"].as_i64().expect("an item has an id"))
        .collect()
}

// ----- without a connection ------------------------------------------------------------------

#[tokio::test]
async fn without_a_connection_nothing_is_queued_and_the_routes_say_so() {
    let fx = Fixture::new().await;
    let photos = fx.site("photos").await;
    fx.seed_one(&photos, "2020/a.jpg").await;

    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    fx.trash_file(&photos, "2021/a.jpg").await;
    assert_eq!(fx.row_count().await, 0, "no connection, nothing to mirror");

    let (status, page) = fx.get(OUTBOX).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page, json!({ "items": [], "total": 0 }));
    assert_eq!(fx.status(false).await["outbox"], Value::Null);

    for (path, body) in [
        (RETRY, json!({})),
        (RETRY, json!({ "ids": [1] })),
        (DISMISS, json!({ "ids": [1] })),
    ] {
        let (status, reply) = fx.post(path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {reply}");
        assert_eq!(code(&reply), "immich_not_connected", "{path}: {reply}");
    }
    let (status, reply) = fx.post_empty(RETRY).await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    assert_eq!(code(&reply), "immich_not_connected", "{reply}");

    let outbox = &fx.app.state.immich_outbox;
    assert!(
        outbox.submit_now().await.unwrap().is_none(),
        "no pass without a connection"
    );
    assert_eq!(fx.tasks_of(TaskKind::ImmichCleanup).await, 0);
    fx.fake.assert_no_violations();
}

// ----- the list ------------------------------------------------------------------------------

#[tokio::test]
async fn the_list_names_each_change_by_kind_and_by_volume_folder_and_file_oldest_first() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    let archive = fx.site("archive").await;
    fx.map_sites(&[&photos, &archive]).await;
    fx.seed(
        &photos,
        &["2020/a.jpg", "2020/b.jpg", "2020/c.jpg", "root.jpg"],
    )
    .await;

    fx.move_file((&photos, "2020/a.jpg"), (&archive, "2021/a.jpg"))
        .await;
    fx.trash_file(&photos, "2020/b.jpg").await;
    fx.copy_file((&photos, "2020/c.jpg"), (&photos, "2020/c copy.jpg"))
        .await;
    fx.move_file((&photos, "root.jpg"), (&photos, "2021/root.jpg"))
        .await;

    let (status, page) = fx.get(OUTBOX).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 4, "{page}");
    let items = page["items"].as_array().unwrap();
    let shown: Vec<(&str, Option<&str>, Option<&str>)> = items
        .iter()
        .map(|item| {
            (
                item["kind"].as_str().unwrap(),
                item["from"].as_str(),
                item["to"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "moved",
                Some("photos/2020/a.jpg"),
                Some("archive/2021/a.jpg")
            ),
            ("removed", Some("photos/2020/b.jpg"), None),
            ("added", None, Some("photos/2020/c copy.jpg")),
            (
                "moved",
                Some("photos/root.jpg"),
                Some("photos/2021/root.jpg")
            ),
        ],
        "{page}"
    );

    let rows = fx.rows().await;
    assert_eq!(ids_of(&page), rows.iter().map(|r| r.id).collect::<Vec<_>>());
    for (item, row) in items.iter().zip(&rows) {
        let mut keys: Vec<&str> = item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "attempts",
                "created_at",
                "from",
                "id",
                "kind",
                "last_error",
                "next_attempt_at",
                "state",
                "to"
            ]
        );
        assert_eq!(item["state"], "pending", "{item}");
        assert_eq!(item["attempts"], 0, "{item}");
        assert_eq!(item["last_error"], Value::Null, "{item}");
        assert_eq!(item["created_at"], row.created_at, "{item}");
        assert_eq!(item["next_attempt_at"], row.next_attempt_at, "{item}");
        assert!(row.created_at > 0);
    }
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn the_list_is_paged_and_can_be_limited_to_one_state() {
    let (fx, photos, _ids) = many_photos(5).await;
    move_all(&fx, &photos, 5).await;
    let rows = fx.rows().await;
    assert_eq!(rows.len(), 5);
    let id = |n: usize| rows[n].id;
    give_up(&fx, id(1), "scan_timeout").await;
    give_up(&fx, id(3), "carry_failed").await;

    let fx = &fx;
    let page = |query: &str| {
        let path = format!("{OUTBOX}?{query}");
        async move {
            let (status, page) = fx.get(&path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {page}");
            page
        }
    };

    let first = page("limit=2").await;
    assert_eq!(ids_of(&first), [id(0), id(1)]);
    assert_eq!(first["total"], 5, "the total is not the page");
    assert_eq!(ids_of(&page("limit=2&offset=2").await), [id(2), id(3)]);
    assert_eq!(ids_of(&page("limit=2&offset=4").await), [id(4)]);
    let past_the_end = page("offset=5").await;
    assert!(ids_of(&past_the_end).is_empty());
    assert_eq!(past_the_end["total"], 5);

    let attention = page("state=attention").await;
    assert_eq!(ids_of(&attention), [id(1), id(3)]);
    assert_eq!(attention["total"], 2);
    assert_eq!(attention["items"][0]["state"], "attention");
    assert_eq!(attention["items"][0]["last_error"], "scan_timeout");
    assert_eq!(attention["items"][0]["attempts"], 3);
    let pending = page("state=pending&limit=2&offset=1").await;
    assert_eq!(ids_of(&pending), [id(2), id(4)]);
    assert_eq!(
        pending["total"], 3,
        "the total counts the state, not the page"
    );

    // 60 more changes, to see the default page and the largest one
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 60) \
         INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, new_volume_id, \
         new_dir, new_name, next_attempt_at, created_at) \
         SELECT 1000 + i, ?1, '2020', 'x' || i, ?1, '2021', 'x' || i, 0, 0 FROM n",
    )
    .bind(photos.volume_id)
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
    let default = page("").await;
    assert_eq!(
        default["items"].as_array().unwrap().len(),
        50,
        "the default page"
    );
    assert_eq!(default["total"], 65);
    let largest = page("limit=200").await;
    assert_eq!(largest["items"].as_array().unwrap().len(), 65);
    assert_eq!(page("limit=1").await["items"].as_array().unwrap().len(), 1);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_page_that_cannot_be_served_is_refused() {
    let (fx, _photos, _old) = moved_photo().await;

    for query in ["limit=0", "limit=201", "limit=-1", "offset=-1"] {
        let (status, reply) = fx.get(&format!("{OUTBOX}?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {reply}");
        assert_eq!(code(&reply), "bad_request", "{query}: {reply}");
    }
    for query in [
        "limit=many",
        "limit=1.5",
        "offset=x",
        "state=nope",
        "state=PENDING",
    ] {
        let (status, reply) = fx.get(&format!("{OUTBOX}?{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {reply}");
        assert_eq!(code(&reply), "invalid_query", "{query}: {reply}");
    }
    let (status, reply) = fx.get(&format!("{OUTBOX}?limit=200&offset=0")).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn reading_the_queue_changes_nothing_and_never_asks_immich() {
    let (fx, _photos, _old) = moved_photo().await;
    let rows = fx.rows().await;
    let state = fx.pass_state().await;
    let passes = fx.tasks_of(TaskKind::ImmichCleanup).await;
    fx.fake.clear_calls();

    for path in [
        OUTBOX,
        "/api/immich/outbox?state=attention",
        "/api/immich/outbox?state=pending&limit=1&offset=0",
        "/api/immich/status",
    ] {
        let (status, reply) = fx.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {reply}");
    }

    assert!(
        fx.fake.calls().is_empty(),
        "reading asked Immich: {:?}",
        fx.fake.sequence()
    );
    assert_eq!(fx.rows().await, rows);
    assert_eq!(fx.pass_state().await, state);
    assert_eq!(fx.tasks_of(TaskKind::ImmichCleanup).await, passes);
    fx.fake.assert_no_violations();
}

// ----- retry ---------------------------------------------------------------------------------

#[tokio::test]
async fn retry_makes_waiting_and_attention_changes_due_and_counts_the_ones_it_changed() {
    let (fx, photos, _ids) = many_photos(3).await;
    move_all(&fx, &photos, 3).await;
    let rows = fx.rows().await;
    let (due, waiting, stuck) = (rows[0].id, rows[1].id, rows[2].id);
    wait_for(&fx, waiting, 3600).await;
    give_up(&fx, stuck, "scan_timeout").await;
    fail_runs(&fx, 3).await;

    let retry = |body: Value| {
        let fx = &fx;
        async move {
            let (status, reply) = fx.post(RETRY, body.clone()).await;
            assert_eq!(status, StatusCode::OK, "{body}: {reply}");
            reply["retried"].as_i64().expect("retried is a number")
        }
    };

    assert_eq!(retry(json!({ "ids": [due] })).await, 0, "it is due already");
    assert_eq!(retry(json!({ "ids": [999_999] })).await, 0, "an unknown id");
    assert_eq!(fx.pass_state().await.consecutive_failures, 0);

    assert_eq!(retry(json!({ "ids": [stuck] })).await, 1);
    let row = fx.rows().await.into_iter().find(|r| r.id == stuck).unwrap();
    assert!(row.is_pending(), "{row:?}");
    assert_eq!(row.attempts, 0);
    assert_eq!(row.last_error, None);
    assert!(row.next_attempt_at <= now_secs(), "{row:?}");
    let row = fx
        .rows()
        .await
        .into_iter()
        .find(|r| r.id == waiting)
        .unwrap();
    assert!(row.next_attempt_at > now_secs(), "still waiting: {row:?}");

    // no body: every change that waits or needs attention, here the one that waits
    let (status, reply) = fx.post_empty(RETRY).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "retried": 1 }));
    let row = fx
        .rows()
        .await
        .into_iter()
        .find(|r| r.id == waiting)
        .unwrap();
    assert_eq!((row.attempts, row.last_error.clone()), (0, None), "{row:?}");
    assert!(row.next_attempt_at <= now_secs(), "{row:?}");

    // nothing is left to change, however the request says "all of them"
    assert_eq!(retry(json!({})).await, 0);
    assert_eq!(retry(json!({ "ids": null })).await, 0);
    let (status, reply) = post_raw(&fx, RETRY, "  \n", true).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "retried": 0 }));
    assert!(fx.rows().await.iter().all(|r| r.is_pending()));

    // the retried changes are worked off by the next pass
    fx.drain(4).await;
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn retry_ends_the_back_off_of_failed_passes_even_when_no_change_needed_it() {
    let (fx, _photos, _old) = moved_photo().await;
    fail_runs(&fx, 4).await;
    assert_eq!(fx.pass_state().await.consecutive_failures, 4);

    let (status, reply) = fx.post_empty(RETRY).await;

    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "retried": 0 }), "the one change was due");
    assert_eq!(fx.pass_state().await.consecutive_failures, 0);
    let first = first_pass(&fx).await;
    assert_eq!(first["rows"], 1, "{first}");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn retry_and_dismiss_refuse_what_is_not_a_request_and_change_nothing() {
    let (fx, _photos, _old) = moved_photo().await;
    let rows = fx.rows().await;
    let state = fx.pass_state().await;

    for (path, body, status, error) in [
        (RETRY, "{not json".to_owned(), 400, "invalid_json"),
        (RETRY, r#"{"ids": "7"}"#.to_owned(), 422, "invalid_json"),
        (RETRY, r#"{"ids": [1.5]}"#.to_owned(), 422, "invalid_json"),
        (RETRY, ids_body(1, MAX_IDS + 1), 400, "bad_request"),
        (DISMISS, "{}".to_owned(), 422, "invalid_json"),
        (DISMISS, r#"{"ids": null}"#.to_owned(), 422, "invalid_json"),
        (DISMISS, "{not json".to_owned(), 400, "invalid_json"),
        (DISMISS, ids_body(1, MAX_IDS + 1), 400, "bad_request"),
    ] {
        let (got, reply) = post_raw(&fx, path, &body, true).await;
        let what = format!("{path} {}", &body[..body.len().min(40)]);
        assert_eq!(got.as_u16(), status, "{what}: {reply}");
        assert_eq!(code(&reply), error, "{what}: {reply}");
    }
    assert_eq!(fx.rows().await, rows, "a refused request changed nothing");
    assert_eq!(fx.pass_state().await, state);

    // the most that is allowed is allowed
    let (status, reply) = post_raw(&fx, RETRY, &ids_body(1, MAX_IDS), true).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = post_raw(&fx, DISMISS, &ids_body(1_000_000, MAX_IDS), true).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "dismissed": 0 }));
    assert_eq!(fx.rows().await, rows);
    fx.fake.assert_no_violations();
}

// ----- dismiss -------------------------------------------------------------------------------

#[tokio::test]
async fn dismiss_forgets_changes_without_asking_immich_and_leaves_its_assets_alone() {
    let (fx, photos, assets) = many_photos(3).await;
    move_all(&fx, &photos, 3).await;
    let rows = fx.rows().await;
    give_up(&fx, rows[0].id, "scan_timeout").await;
    fx.fake.clear_calls();

    let (status, reply) = fx
        .post(DISMISS, json!({ "ids": [rows[0].id, 999_999] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        reply,
        json!({ "dismissed": 1 }),
        "an unknown id is no change"
    );
    let left: Vec<i64> = fx.rows().await.iter().map(|r| r.id).collect();
    assert_eq!(left, [rows[1].id, rows[2].id]);

    let (status, reply) = fx.post(DISMISS, json!({ "ids": [] })).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "dismissed": 0 }));

    let (status, reply) = fx
        .post(DISMISS, json!({ "ids": [rows[1].id, rows[2].id] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "dismissed": 2 }), "a pending change too");
    assert_eq!(fx.row_count().await, 0);
    assert_eq!(fx.outbox_status().await["pending"], 0);
    assert_eq!(fx.outbox_status().await["attention"], 0);

    assert!(
        fx.fake.calls().is_empty(),
        "dismissing asked Immich: {:?}",
        fx.fake.sequence()
    );
    for asset in &assets {
        assert!(fx.fake.has_asset(asset), "{asset} was deleted");
    }
    assert!(
        fx.app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .is_none(),
        "a dismissed change is never worked on"
    );
    fx.fake.assert_no_violations();
}

// ----- the marker of the application ---------------------------------------------------------

#[tokio::test]
async fn changing_the_queue_needs_the_marker_of_the_application() {
    let (fx, photos, _ids) = many_photos(2).await;
    move_all(&fx, &photos, 2).await;
    let rows = fx.rows().await;
    give_up(&fx, rows[0].id, "scan_timeout").await;
    let before = fx.rows().await;
    let dismiss = json!({ "ids": [rows[0].id] }).to_string();

    for (path, body) in [(RETRY, "{}"), (RETRY, ""), (DISMISS, dismiss.as_str())] {
        let (status, reply) = post_raw(&fx, path, body, false).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {reply}");
        assert_eq!(code(&reply), "csrf_rejected", "{path}: {reply}");
    }
    assert_eq!(fx.rows().await, before, "a refused request changed nothing");

    let (status, reply) = post_raw(&fx, DISMISS, &dismiss, true).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({ "dismissed": 1 }));
    let (status, reply) = post_raw(&fx, RETRY, "{}", true).await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    // reading needs no marker: `fx.get` sends none
    let (status, page) = fx.get(OUTBOX).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    fx.fake.assert_no_violations();
}

// ----- disconnecting and connecting again ----------------------------------------------------

#[tokio::test]
async fn disconnecting_drops_the_queue_and_what_the_worker_said_about_it() {
    let (fx, photos, _old) = moved_photo().await;
    fail_runs(&fx, 2).await;
    assert_eq!(fx.outbox_status().await["pending"], 1);

    let (status, reply) = fx.delete("/api/immich/connection").await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    assert_eq!(fx.row_count().await, 0);
    assert_eq!(
        fx.pass_state().await,
        PassState {
            last_run_at: None,
            last_error: None,
            consecutive_failures: 0
        }
    );
    let (status, page) = fx.get(OUTBOX).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page, json!({ "items": [], "total": 0 }));
    assert_eq!(fx.status(false).await["outbox"], Value::Null);
    for (path, body) in [(RETRY, json!({})), (DISMISS, json!({ "ids": [1] }))] {
        let (status, reply) = fx.post(path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {reply}");
        assert_eq!(code(&reply), "immich_not_connected", "{path}: {reply}");
    }

    fx.move_file((&photos, "2021/a.jpg"), (&photos, "2022/a.jpg"))
        .await;
    assert_eq!(fx.row_count().await, 0, "no queue without a connection");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn connecting_again_keeps_the_queue_for_the_same_account_and_drops_it_for_another() {
    let (fx, _photos, _old) = moved_photo().await;
    let before = fx.only_row().await;
    fail_runs(&fx, 2).await;

    fx.connect_ok().await;
    assert_eq!(fx.only_row().await, before, "the same account, a new key");
    assert_eq!(
        fx.pass_state().await.consecutive_failures,
        0,
        "a new key ends the back-off of the passes"
    );
    assert_eq!(fx.outbox_status().await["pending"], 1);

    fx.fake.state().user_id = new_id();
    fx.connect_ok().await;
    assert_eq!(fx.row_count().await, 0, "another account sees other assets");
    assert_eq!(fx.pass_state().await.consecutive_failures, 0);
    let (status, page) = fx.get(OUTBOX).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page, json!({ "items": [], "total": 0 }));
    let summary = fx.outbox_status().await;
    assert_eq!(
        (summary["pending"].as_i64(), summary["attention"].as_i64()),
        (Some(0), Some(0))
    );
    fx.fake.assert_no_violations();
}

// ----- the status ----------------------------------------------------------------------------

#[tokio::test]
async fn the_status_counts_what_waits_what_waits_for_immich_and_what_needs_attention() {
    let (fx, photos, _ids) = many_photos(3).await;
    let idle = fx.outbox_status().await;
    assert_eq!(
        idle,
        json!({
            "pending": 0,
            "waiting_for_immich": 0,
            "attention": 0,
            "oldest_pending_at": null,
            "last_run_at": null,
            "last_error": null,
            "scans_on_demand": true,
        })
    );

    fx.move_file((&photos, "2020/p000.jpg"), (&photos, "2021/p000.jpg"))
        .await;
    fx.move_file((&photos, "2020/p001.jpg"), (&photos, "2021/p001.jpg"))
        .await;
    first_pass(&fx).await;
    fx.move_file((&photos, "2020/p002.jpg"), (&photos, "2021/p002.jpg"))
        .await;
    let rows = fx.rows().await;
    assert_eq!(rows.len(), 3);
    give_up(&fx, rows[1].id, "scan_timeout").await;

    let status = fx.outbox_status().await;
    assert_eq!(status["pending"], 2, "{status}");
    assert_eq!(
        status["waiting_for_immich"], 1,
        "only the first change had its scan asked for: {status}"
    );
    assert_eq!(status["attention"], 1, "{status}");
    let oldest = [&rows[0], &rows[2]].iter().map(|r| r.created_at).min();
    assert_eq!(status["oldest_pending_at"].as_i64(), oldest, "{status}");
    assert_eq!(
        status["last_run_at"].as_i64(),
        fx.pass_state().await.last_run_at,
        "{status}"
    );
    assert!(status["last_run_at"].as_i64().unwrap() > 0, "{status}");
    assert_eq!(status["last_error"], Value::Null, "{status}");
    assert_eq!(status["scans_on_demand"], true, "{status}");

    // the older fields of the status still say the same
    let whole = fx.status(false).await;
    assert_eq!(whole["outbox_pending"], 2, "{whole}");
    assert_eq!(whole["needs_attention"], 1, "{whole}");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn the_status_says_why_the_last_pass_failed() {
    let (fx, _photos, _old) = moved_photo().await;
    fail_runs(&fx, 1).await;

    let status = fx.outbox_status().await;

    assert_eq!(status["last_error"], "Immich did not answer", "{status}");
    assert!(status["last_run_at"].as_i64().unwrap() > 0, "{status}");
    assert_eq!(status["pending"], 1, "{status}");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_account_that_cannot_ask_for_scans_says_so() {
    let (fx, _photos, _old) = one_photo_limited().await;

    let status = fx.outbox_status().await;

    assert_eq!(status["scans_on_demand"], false, "{status}");
    assert_eq!(status["pending"], 0, "{status}");
    fx.fake.assert_no_violations();
}

// ----- a burst -------------------------------------------------------------------------------

#[tokio::test]
async fn a_burst_of_changes_is_worked_off_by_one_task_that_counts_them() {
    let (fx, photos, _ids) = many_photos(30).await;
    move_all(&fx, &photos, 30).await;
    assert_eq!(fx.row_count().await, 30);
    assert_eq!(
        fx.tasks_of(TaskKind::ImmichCleanup).await,
        0,
        "the hook only writes rows"
    );

    let task = fx.pass_ok().await;

    assert_eq!(task.title, "Updating Immich (30 changes)");
    assert_eq!(task.kind, TaskKind::ImmichCleanup);
    assert_eq!(fx.tasks_of(TaskKind::ImmichCleanup).await, 1);
    let report = fx.report(&task).await;
    assert_eq!(report["rows"], 30, "{report}");
    assert_eq!(report["scans"], 1, "one scan for the library: {report}");
    assert!(
        fx.app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .is_none(),
        "the changes wait for Immich now: nothing is due"
    );
    assert_eq!(fx.tasks_of(TaskKind::ImmichCleanup).await, 1);

    fx.drain(4).await;
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}
