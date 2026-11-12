//! What starts a sync and what stops one: `POST /api/immich/sync`, the Rust entry points the loop
//! and the outbox use (`ImmichSync::submit` and `submit_if_due`), coalescing, cancelling, losing
//! the connection in the middle of a run, and the background loop itself.
//!
//! What a sync reads and stores is in `immich_sync.rs`. The searches that must be "in flight" while
//! a test acts are held back by the fake (`hold_searches`), never by sleeping on the clock of the
//! test alone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::time::Duration;

use axum::http::StatusCode;
use immich_support::{Call, Fixture, START_MS, entry, eventually, new_id, rfc3339, updated_after};
use opm_core::dto::TaskState;
use opm_server::immich::SyncRequest;
use opm_server::immich::crypto::key_path;
use serde_json::{Value, json};

const PREFIX: &str = "/mnt/photos";
const SEARCH: &str = "/api/search/metadata";
const SKEW_MS: i64 = 5 * 60 * 1000;
const HOUR_MS: i64 = 60 * 60 * 1000;
const MINUTE: i64 = 60;
const DAY: i64 = 24 * 60 * 60;

/// Long enough for a test to do several things while a search is in flight.
const HOLD: Duration = Duration::from_secs(1);

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

/// An administrator connection with a library "Photos" mapped to the volume "photos" at
/// [`PREFIX`]; returns the fixture, the id of the library and the id of the volume.
async fn setup() -> (Fixture, String, i64) {
    let fx = Fixture::connected().await;
    let (library, volume) = fx.map_library("Photos", "photos", PREFIX).await;
    (fx, library, volume)
}

/// The sync tasks the engine knows: queued, running or over.
async fn sync_tasks(fx: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE kind = 'immich_sync'")
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap()
}

/// Makes the last attempt and the last completed pass of every scope `secs` older.
async fn age(fx: &Fixture, secs: i64) {
    fx.age_attempts(secs).await;
    fx.age_syncs(secs).await;
}

async fn cancel(fx: &Fixture, task: i64) {
    let (status, body) = fx.post_empty(&format!("/api/tasks/{task}/cancel")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// Loses the key file. The service keeps the client it built until the stored connection changes,
/// so the loss is only felt when a client is built again: here after a new server version, which
/// a status check stores.
async fn lose_the_key(fx: &Fixture) {
    std::fs::remove_file(key_path(&fx.data_dir())).unwrap();
    fx.fake.state().version = "v3.9.9".into();
    fx.status(true).await;
    assert_eq!(fx.status(false).await["needs_reconnect"], true);
}

// ----------------------------------------------------------------------------
// POST /api/immich/sync
// ----------------------------------------------------------------------------

#[tokio::test]
async fn a_sync_is_refused_without_a_connection_a_mapping_or_a_key_that_can_be_read() {
    let fx = Fixture::new().await;
    let (status, body) = fx.start_sync(None).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "immich_not_connected")
    );

    fx.connect_ok().await;
    let (status, body) = fx.start_sync(Some(json!({ "full": true }))).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "immich_not_mapped")
    );

    fx.map_library("Photos", "photos", PREFIX).await;
    lose_the_key(&fx).await;
    let (status, body) = fx.start_sync(None).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "immich_needs_reconnect")
    );

    assert_eq!(fx.fake.count("POST", SEARCH), 0);
    assert_eq!(sync_tasks(&fx).await, 0, "a refused request leaves no task");
}

#[tokio::test]
async fn the_body_may_be_missing_empty_or_say_full_and_only_full_reads_everything() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.sync_ok(false).await;

    let cases = [
        ("no body", None, false),
        ("an empty object", Some("{}"), false),
        ("full: false", Some(r#"{"full":false}"#), false),
        ("only white space", Some("  \n"), false),
        ("a field it does not know", Some(r#"{"later":1}"#), false),
        ("full: true", Some(r#"{"full":true}"#), true),
    ];
    for (label, body, full) in cases {
        fx.fake.clear_calls();
        let (status, task) = match body {
            None => fx.start_sync(None).await,
            Some(text) => fx.start_sync_raw(text).await,
        };
        assert_eq!(status, StatusCode::ACCEPTED, "{label}: {task}");
        assert_eq!(task["kind"], "immich_sync", "{label}");

        let ended = fx.finish(task["id"].as_i64().unwrap()).await;
        assert_eq!(ended.state, TaskState::Done, "{label}: {ended:?}");
        let bodies = fx.fake.search_bodies();
        assert_eq!(bodies.len(), 1, "{label}");
        assert_eq!(
            updated_after(&bodies[0]).is_none(),
            full,
            "{label}: only `full` leaves the lower bound out"
        );
    }
}

#[tokio::test]
async fn a_body_that_is_not_a_sync_request_is_a_client_error_and_starts_nothing() {
    let (fx, _, _) = setup().await;
    for (text, expected) in [
        (r#"{"full":"yes"}"#, StatusCode::UNPROCESSABLE_ENTITY),
        ("null", StatusCode::UNPROCESSABLE_ENTITY),
        (r#"{"full":"#, StatusCode::BAD_REQUEST),
        ("full=true", StatusCode::BAD_REQUEST),
    ] {
        let (status, body) = fx.start_sync_raw(text).await;
        assert_eq!(status, expected, "{text}: {body}");
        assert_eq!(code(&body), "invalid_json", "{text}");
    }
    assert_eq!(sync_tasks(&fx).await, 0);
}

// ----------------------------------------------------------------------------
// Coalescing
// ----------------------------------------------------------------------------

#[tokio::test]
async fn requests_that_arrive_while_a_sync_runs_become_one_follow_up_that_waits_its_turn() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.hold_searches(HOLD);
    let first = fx.submit_sync(false).await;
    fx.fake.wait_for_searches(1).await;
    fx.fake.heal();

    let follow_up = fx.submit_sync(false).await;
    assert_ne!(
        follow_up, first,
        "a sync runs: this request is another task"
    );
    let (status, merged) = fx.start_sync(Some(json!({ "full": true }))).await;
    assert_eq!(
        (status, merged["id"].as_i64()),
        (StatusCode::ACCEPTED, Some(follow_up)),
        "the task that waits takes the request"
    );
    let again = fx
        .app
        .state
        .immich_sync
        .submit(SyncRequest::default())
        .await
        .unwrap();
    assert_eq!((again.task_id, again.coalesced), (follow_up, true));

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fx.fake.count("POST", SEARCH),
        1,
        "one sync at a time: the follow-up waits for the one that runs"
    );
    assert_eq!(fx.task(first).await.state, TaskState::Running);

    assert_eq!(fx.finish(first).await.state, TaskState::Done);
    assert_eq!(fx.finish(follow_up).await.state, TaskState::Done);
    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(updated_after(&bodies[0]), None, "the first pass of a scope");
    assert_eq!(
        updated_after(&bodies[1]),
        None,
        "the follow-up took the `full` of the request merged into it"
    );
    assert_eq!(fx.scope(&library).await.run, 2);
    assert_eq!(sync_tasks(&fx).await, 2, "three requests, two tasks");
}

#[tokio::test]
async fn a_follow_up_that_was_cancelled_does_not_swallow_the_next_request() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.hold_searches(HOLD);
    let first = fx.submit_sync(false).await;
    fx.fake.wait_for_searches(1).await;
    fx.fake.heal();

    let doomed = fx.submit_sync(false).await;
    cancel(&fx, doomed).await;
    let next = fx.submit_sync(true).await;
    assert_ne!(next, doomed, "a cancelled task takes no more requests");

    assert_eq!(fx.finish(doomed).await.state, TaskState::Cancelled);
    assert_eq!(fx.finish(first).await.state, TaskState::Done);
    assert_eq!(fx.finish(next).await.state, TaskState::Done);
    assert_eq!(
        fx.fake.count("POST", SEARCH),
        2,
        "the first sync and the new one; the cancelled one read nothing"
    );
    assert_eq!(fx.scope(&library).await.run, 2);
}

// ----------------------------------------------------------------------------
// Cancelling and losing the connection
// ----------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_running_sync_keeps_the_marks_and_the_rows_and_the_next_sync_catches_up() {
    let (fx, library, _) = setup().await;
    fx.fake.page_cap(Some(1));
    let old: Vec<String> = (0..3)
        .map(|n| {
            fx.fake
                .add_asset_with(&library, &format!("/mnt/photos/{n}.jpg"), |a| {
                    a.updated_at_ms = START_MS - HOUR_MS;
                })
        })
        .collect();
    fx.sync_ok(false).await;
    let before = fx.scope(&library).await;
    assert_eq!(before.assets_total, 3);

    // Immich changes: the first asset is trashed, the second deleted for good, one is new.
    fx.fake.advance(Duration::from_secs(60 * 60));
    fx.fake.update_asset(&old[0], |a| a.is_trashed = true);
    fx.fake.remove_asset(&old[1]);
    let added = fx.fake.add_asset(&library, "/mnt/photos/added.jpg");
    fx.fake.clear_calls();
    fx.fake.hold_searches_after(1, Duration::from_secs(30));

    let task = fx.submit_sync(true).await;
    fx.fake.wait_for_searches(2).await;
    cancel(&fx, task).await;
    let ended = fx.finish(task).await;

    assert_eq!(ended.state, TaskState::Cancelled, "{ended:?}");
    assert_eq!(ended.error, None);
    let after = fx.scope(&library).await;
    assert_eq!(after.high_water_ms, before.high_water_ms);
    assert_eq!(after.last_sync_at, before.last_sync_at);
    assert_eq!(after.last_full_at, before.last_full_at);
    assert_eq!(
        (after.last_error.as_deref(), after.consecutive_failures),
        (None, 0)
    );
    assert_eq!(after.run, before.run + 1, "the pass began");
    assert_eq!(
        (
            after.assets_total,
            after.assets_live,
            after.assets_matched,
            after.assets_unmapped
        ),
        (
            before.assets_total,
            before.assets_live,
            before.assets_matched,
            before.assets_unmapped
        ),
        "the counters are those of the last pass that completed"
    );
    assert!(
        fx.stored(&old[1]).await.is_some(),
        "a pass that does not complete deletes nothing"
    );
    let first_page = fx.row(&old[0]).await;
    assert!(first_page.is_trashed, "the page that was read is stored");
    assert_eq!(first_page.sync_run, after.run);
    assert_eq!(fx.row(&old[2]).await.sync_run, before.run);
    assert_eq!(fx.stored(&added).await, None);

    fx.fake.heal();
    fx.fake.clear_calls();
    fx.sync_ok(false).await;
    let bodies = fx.fake.search_bodies();
    assert_eq!(
        updated_after(&bodies[0]),
        Some(rfc3339(before.high_water_ms - SKEW_MS)),
        "the cancelled pass left the mark where it was"
    );
    assert!(fx.row(&added).await.volume_id.is_some());
    assert!(
        fx.stored(&old[1]).await.is_some(),
        "an incremental pass never deletes"
    );

    fx.sync_ok(true).await;
    assert_eq!(
        fx.stored(&old[1]).await,
        None,
        "the next full pass deletes what Immich lost"
    );
    assert_eq!(fx.asset_count().await, 3);
}

#[tokio::test]
async fn disconnecting_in_the_middle_of_a_sync_leaves_both_tables_empty_and_the_next_sync_reads_everything()
 {
    let (fx, library, _) = setup().await;
    fx.fake.page_cap(Some(1));
    for n in 0..3 {
        fx.fake.add_asset(&library, &format!("/mnt/photos/{n}.jpg"));
    }
    fx.fake.hold_searches_after(1, HOLD);

    let task = fx.submit_sync(false).await;
    fx.fake.wait_for_searches(2).await;
    assert_eq!(fx.asset_count().await, 1, "the first page is stored");
    let (status, body) = fx.delete("/api/immich/connection").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ended = fx.finish(task).await;
    assert_eq!(ended.state, TaskState::Cancelled, "{ended:?}");
    assert_eq!(ended.error, None);
    assert_eq!(fx.asset_count().await, 0, "the sync put nothing back");
    assert!(fx.scopes().await.is_empty());
    assert_eq!(fx.status(false).await["sync"], Value::Null);
    assert_eq!(
        fx.status(false).await["mappings"].as_array().unwrap().len(),
        1,
        "the mappings are the user's, not the connection's"
    );

    fx.fake.heal();
    fx.fake.clear_calls();
    fx.connect_ok().await;
    fx.sync_ok(false).await;
    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 3);
    assert!(
        bodies.iter().all(|body| updated_after(body).is_none()),
        "the first sync after a connect reads everything"
    );
    assert_eq!(fx.asset_count().await, 3);
}

#[tokio::test]
async fn connecting_again_keeps_what_was_synced_unless_the_account_changed() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.sync_ok(false).await;

    fx.connect_ok().await;
    assert_eq!(fx.asset_count().await, 1, "the same account again");
    assert_eq!(fx.scopes().await.len(), 1);
    fx.fake.clear_calls();
    fx.sync_ok(false).await;
    assert!(
        updated_after(&fx.fake.search_bodies()[0]).is_some(),
        "the next sync is incremental"
    );

    fx.fake.state().user_id = new_id();
    fx.connect_ok().await;
    assert_eq!(
        fx.asset_count().await,
        0,
        "another account sees other assets"
    );
    assert!(fx.scopes().await.is_empty());
    assert_eq!(fx.status(false).await["sync"], Value::Null);
    fx.fake.clear_calls();
    fx.sync_ok(false).await;
    assert_eq!(updated_after(&fx.fake.search_bodies()[0]), None);
    assert_eq!(fx.asset_count().await, 1);
}

// ----------------------------------------------------------------------------
// Read-only
// ----------------------------------------------------------------------------

/// What a sync may ask of Immich: search, and read libraries, the account and the server.
fn reads_only(call: &Call) -> bool {
    call.is("POST", SEARCH)
        || (call.method == "GET"
            && (call.path == "/api/libraries"
                || call.path.starts_with("/api/libraries/")
                || call.path == "/api/users/me"
                || call.path.starts_with("/api/server/")))
}

#[tokio::test]
async fn a_sync_only_reads_from_immich_in_both_modes() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.clear_calls();
    fx.sync_ok(false).await;
    fx.sync_ok(true).await;
    fx.sync_ok(false).await;

    let calls = fx.fake.calls();
    assert_eq!(fx.fake.count("POST", SEARCH), 3);
    assert_eq!(
        fx.fake.count("GET", "/api/libraries"),
        3,
        "one look per sync"
    );
    for call in &calls {
        assert!(reads_only(call), "a sync asked Immich for {call:?}");
        assert!(
            call.api_key.is_some(),
            "the sync works with the API key: {call:?}"
        );
        assert!(
            call.authorization.is_none(),
            "and never with a session: {call:?}"
        );
    }

    let limited = Fixture::connected_limited().await;
    let library = limited.fake.add_library("Photos", &[PREFIX]);
    limited.fake.add_asset(&library, "/mnt/photos/a.jpg");
    let volume = limited.volume("photos").await;
    limited.map(vec![entry(volume, None, PREFIX)]).await;
    limited.fake.clear_calls();
    limited.sync_ok(false).await;
    limited.sync_ok(true).await;

    assert_eq!(
        limited.fake.count("GET", "/api/libraries"),
        0,
        "no library to look at"
    );
    let calls = limited.fake.calls();
    assert!(!calls.is_empty());
    for call in &calls {
        assert!(reads_only(call), "a sync asked Immich for {call:?}");
        assert!(
            call.api_key.is_some() && call.authorization.is_none(),
            "{call:?}"
        );
    }
}

// ----------------------------------------------------------------------------
// What the loop decides
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_loop_starts_nothing_without_a_connection_a_mapping_or_a_key_that_can_be_read() {
    let fx = Fixture::new().await;
    let sync = &fx.app.state.immich_sync;
    assert_eq!(sync.submit_if_due().await.unwrap(), None, "not connected");

    fx.connect_ok().await;
    assert_eq!(
        sync.submit_if_due().await.unwrap(),
        None,
        "nothing is mapped"
    );

    let (library, _) = fx.map_library("Photos", "photos", PREFIX).await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    lose_the_key(&fx).await;
    fx.fake.clear_calls();
    assert_eq!(sync.submit_if_due().await.unwrap(), None, "the key is lost");

    assert!(fx.fake.calls().is_empty());
    assert_eq!(sync_tasks(&fx).await, 0);
}

#[tokio::test]
async fn the_loop_starts_a_sync_when_a_scope_is_due_and_not_before() {
    let (fx, library, _) = setup().await;
    let sync = &fx.app.state.immich_sync;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");

    let due = sync
        .submit_if_due()
        .await
        .unwrap()
        .expect("a scope that was never read is due");
    assert!(!due.coalesced);
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Done);
    assert_eq!(sync.submit_if_due().await.unwrap(), None, "just read");

    age(&fx, 14 * MINUTE).await;
    assert_eq!(
        sync.submit_if_due().await.unwrap(),
        None,
        "14 minutes are not 15"
    );

    age(&fx, 2 * MINUTE).await;
    fx.fake.clear_calls();
    let due = sync.submit_if_due().await.unwrap().expect("16 minutes");
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Done);
    assert!(
        updated_after(&fx.fake.search_bodies()[0]).is_some(),
        "the pass of 15 minutes is incremental"
    );
    assert_eq!(sync.submit_if_due().await.unwrap(), None);

    fx.age_full_syncs(DAY + 60).await;
    age(&fx, 2 * MINUTE).await;
    fx.fake.clear_calls();
    let due = sync
        .submit_if_due()
        .await
        .unwrap()
        .expect("a day without a full pass");
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Done);
    assert_eq!(
        updated_after(&fx.fake.search_bodies()[0]),
        None,
        "the daily pass reads everything"
    );
    assert_eq!(sync.submit_if_due().await.unwrap(), None);
    assert_eq!(sync_tasks(&fx).await, 3);
}

#[tokio::test]
async fn a_failing_scope_waits_longer_after_each_failure_and_recovers_when_immich_does() {
    let (fx, library, _) = setup().await;
    let sync = &fx.app.state.immich_sync;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.sync_ok(false).await;
    age(&fx, 16 * MINUTE).await;
    fx.fake.fail("POST", SEARCH, 500);

    // 1 failure: two minutes. 2 failures: two minutes. 3 failures: four minutes.
    for (failures, gap) in [(1, 120), (2, 120), (3, 240)] {
        let due = sync
            .submit_if_due()
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("due before failure {failures}"));
        let ended = fx.finish(due.task_id).await;
        assert_eq!(ended.state, TaskState::Failed, "{ended:?}");
        assert_eq!(fx.scope(&library).await.consecutive_failures, failures);

        assert_eq!(
            sync.submit_if_due().await.unwrap(),
            None,
            "a failed scope waits"
        );
        fx.age_attempts(gap - 1).await;
        assert_eq!(
            sync.submit_if_due().await.unwrap(),
            None,
            "{gap} s have not passed"
        );
        fx.age_attempts(2).await;
    }

    fx.fake.heal();
    let due = sync
        .submit_if_due()
        .await
        .unwrap()
        .expect("the wait is over");
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Done);
    let scope = fx.scope(&library).await;
    assert_eq!((scope.consecutive_failures, scope.last_error), (0, None));
    assert_eq!(
        sync.submit_if_due().await.unwrap(),
        None,
        "read, and not due again"
    );
}

#[tokio::test]
async fn connecting_again_lifts_the_wait_of_a_scope_whose_key_was_rejected() {
    let (fx, library, _) = setup().await;
    let sync = &fx.app.state.immich_sync;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.sync_ok(false).await;
    age(&fx, 16 * MINUTE).await;
    fx.fake.revoke_all_keys();

    let due = sync.submit_if_due().await.unwrap().expect("due");
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Failed);
    assert_eq!(fx.status(false).await["needs_reconnect"], true);
    assert_eq!(
        sync.submit_if_due().await.unwrap(),
        None,
        "a failed scope waits"
    );

    fx.connect_ok().await;

    assert_eq!(fx.status(false).await["needs_reconnect"], false);
    let due = sync
        .submit_if_due()
        .await
        .unwrap()
        .expect("the new key is tried at once");
    assert_eq!(fx.finish(due.task_id).await.state, TaskState::Done);
    let scope = fx.scope(&library).await;
    assert_eq!((scope.consecutive_failures, scope.last_error), (0, None));
    assert_eq!(fx.asset_count().await, 1);
}

#[tokio::test]
async fn the_loop_queues_nothing_behind_a_running_sync() {
    let (fx, library, _) = setup().await;
    let sync = &fx.app.state.immich_sync;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.sync_ok(false).await;
    age(&fx, 16 * MINUTE).await;
    fx.fake.clear_calls();
    fx.fake.hold_searches(HOLD);

    let running = sync.submit_if_due().await.unwrap().expect("due");
    fx.fake.wait_for_searches(1).await;
    // The scope is due again as far as its state says (its attempt began a moment ago).
    age(&fx, 16 * MINUTE).await;
    assert_eq!(
        sync.submit_if_due().await.unwrap(),
        None,
        "a sync runs: the loop does not queue another"
    );

    assert_eq!(fx.finish(running.task_id).await.state, TaskState::Done);
    assert_eq!(sync_tasks(&fx).await, 2);
}

// ----------------------------------------------------------------------------
// The loop itself
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_loop_syncs_by_itself_and_stops_with_the_server() {
    let fx = Fixture::connected().await;
    let fx = &fx;
    let shutdown = fx.app.state.shutdown.child_token();
    fx.app.state.immich_sync.start_background_every(
        shutdown.clone(),
        Duration::from_millis(20),
        Duration::from_millis(50),
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sync_tasks(fx).await, 0, "connected, but nothing is mapped");

    let (library, photos) = fx.map_library("Photos", "photos", PREFIX).await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    eventually(
        "the first sync",
        || async move { fx.asset_count().await == 1 },
    )
    .await;
    eventually("the task to end", || async move {
        let tasks = fx.app.state.tasks.list(10).await.unwrap();
        tasks.len() == 1 && tasks[0].state == TaskState::Done
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sync_tasks(fx).await, 1, "read, so not due again");
    assert_eq!(fx.scope(&library).await.run, 1);

    // The new mappings make the scope due, but not before two minutes passed since the last
    // attempt: the loop does not hurry a scope it has just read.
    let more = fx.volume("more").await;
    fx.map(vec![
        entry(photos, Some(&library), PREFIX),
        entry(more, Some(&library), "/mnt/more"),
    ])
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fx.scope(&library).await.run, 1);
    fx.age_attempts(3 * MINUTE).await;
    let scope_key = &library;
    eventually("the edited mappings to be read", || async move {
        fx.scope(scope_key).await.run == 2
    })
    .await;
    eventually("the second sync to end", || async move {
        let tasks = fx.app.state.tasks.list(10).await.unwrap();
        tasks.len() == 2 && tasks.iter().all(|task| task.state == TaskState::Done)
    })
    .await;
    assert_eq!(fx.fake.search_bodies().len(), 2);
    assert_eq!(
        updated_after(&fx.fake.search_bodies()[1]),
        None,
        "other mappings, so everything is read again"
    );

    shutdown.cancel();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let tasks = sync_tasks(fx).await;
    age(fx, DAY).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sync_tasks(fx).await, tasks, "the loop is over");
}
