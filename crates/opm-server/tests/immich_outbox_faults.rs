//! The outbox of file operations (`immich/outbox/`) when things go wrong: Immich is down, answers
//! errors or refuses, the key is gone, the person cancels or disconnects, the account cannot ask
//! for a scan. The rules, each with its test:
//!
//! - a pass that fails as a whole leaves its rows as they were, says why and backs off; a person
//!   pressing Retry (or the next pass, once Immich is back) does the work,
//! - nothing is ever deleted in Immich on a doubt: a failing carry-over or delete keeps the old
//!   asset, and a permanent refusal asks a person,
//! - a call that failed for a passing reason is tried again later; Immich is not hammered.
//!
//! The faults are injected into the fake Immich of `immich_support` by method and path (see
//! `FakeImmich::fail`). Every test checks that the fake recorded no forced delete of an asset that
//! was still online.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use axum::http::StatusCode;
use immich_support::{
    Fixture, PassState, delete_of, first_pass, many_photos, moved_photo, next_pass, one_photo,
    one_photo_limited,
};
use opm_core::dto::{TaskKind, TaskState};
use opm_server::immich::assets;
use serde_json::Value;

const SEARCH: &str = "/api/search/metadata";

/// What the outbox shows a person in `last_error` after they cancelled a pass.
const CANCELLED: &str = "The last pass was cancelled.";

// ----- a pass that fails as a whole ------------------------------------------------------------

#[tokio::test]
async fn an_immich_that_is_down_fails_the_pass_and_leaves_the_rows_as_they_were() {
    let (mut fx, _photos, _old) = moved_photo().await;
    let before = fx.only_row().await;
    fx.fake.stop().await;

    let task = fx.pass().await;

    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    let message = task.error.clone().expect("a failed pass says why");
    assert!(!message.is_empty());
    assert_eq!(fx.only_row().await, before);
    let state = fx.pass_state().await;
    assert_eq!(state.consecutive_failures, 1);
    assert_eq!(state.last_error.as_deref(), Some(message.as_str()));
    let status = fx.outbox_status().await;
    assert_eq!(status["pending"], 1, "{status}");
    assert_eq!(status["attention"], 0, "{status}");
    assert_eq!(status["last_error"], message.as_str(), "{status}");
    assert!(fx.deletes().is_empty());
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_failing_read_of_immich_fails_the_pass_and_the_next_one_does_the_work() {
    let (fx, _photos, old) = moved_photo().await;
    let before = fx.only_row().await;
    fx.fake.fail("POST", SEARCH, 500);

    let task = fx.pass().await;

    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(fx.only_row().await, before, "no row was touched");
    assert!(fx.scans_asked().is_empty(), "no scan on a failed read");
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);

    fx.fake.heal();
    first_pass(&fx).await;
    let state = fx.pass_state().await;
    assert_eq!(
        state.consecutive_failures, 0,
        "a pass that ends well ends the back-off"
    );
    assert_eq!(state.last_error, None);
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_failed_pass_backs_the_loop_off_and_retry_ends_the_back_off() {
    let (fx, _photos, _old) = moved_photo().await;
    fx.fake.fail("POST", SEARCH, 500);
    assert_eq!(fx.pass().await.state, TaskState::Failed);

    // The rule of the loop: nothing starts inside the back-off, whatever is due.
    let status = fx.outbox_status().await;
    assert_eq!(status["pending"], 1, "{status}");
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);

    fx.fake.heal();
    fx.retry_all().await;

    assert_eq!(fx.pass_state().await.consecutive_failures, 0);
    first_pass(&fx).await;
    assert_eq!(fx.scans_asked().len(), 1);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_revoked_key_fails_the_pass_and_connecting_again_lets_the_queue_flow() {
    let (fx, _photos, old) = moved_photo().await;
    let before = fx.only_row().await;
    fx.fake.revoke_all_keys();

    let task = fx.pass().await;

    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert_eq!(fx.only_row().await, before);
    assert_eq!(fx.status(false).await["needs_reconnect"], true);
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);
    assert!(fx.deletes().is_empty());

    // The queue was made for this server and account: it survives the new key, and the back-off
    // the rejected key caused is over.
    fx.connect_ok().await;
    assert_eq!(fx.pass_state().await.consecutive_failures, 0);
    assert_eq!(fx.outbox_status().await["pending"], 1);
    first_pass(&fx).await;
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_key_that_cannot_be_read_fails_the_pass_without_asking_immich() {
    let (fx, _photos, old) = moved_photo().await;
    let before = fx.only_row().await;
    fx.lose_the_key().await;
    fx.fake.clear_calls();

    let task = fx.pass().await;

    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert!(task.error.is_some());
    assert_eq!(fx.only_row().await, before);
    assert!(
        fx.fake.calls().is_empty(),
        "nothing is asked with a key that cannot be read: {:?}",
        fx.fake.sequence()
    );
    assert_eq!(fx.status(false).await["needs_reconnect"], true);
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);

    fx.connect_ok().await;
    assert_eq!(fx.pass_state().await.consecutive_failures, 0);
    first_pass(&fx).await;
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

// ----- a person cancels or disconnects ----------------------------------------------------------

#[tokio::test]
async fn cancelling_a_pass_leaves_the_rows_and_a_retry_does_the_work() {
    let (fx, _photos, old) = moved_photo().await;
    let before = fx.only_row().await;
    let task = fx.held_pass().await;

    let done = fx.cancel(task).await;

    assert_eq!(done.state, TaskState::Cancelled, "{done:?}");
    assert_eq!(fx.only_row().await, before);
    let state = fx.pass_state().await;
    assert_eq!(state.last_error.as_deref(), Some(CANCELLED));
    assert_eq!(state.consecutive_failures, 1);
    assert!(fx.scans_asked().is_empty());

    fx.fake.heal();
    fx.retry_all().await;
    first_pass(&fx).await;
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert_eq!(fx.pass_state().await.last_error, None);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn only_one_pass_runs_at_a_time() {
    let (fx, _photos, _old) = moved_photo().await;
    let task = fx.held_pass().await;

    assert_eq!(
        fx.app.state.immich_outbox.submit_now().await.unwrap(),
        None,
        "a pass is running"
    );
    assert_eq!(fx.fake.count("POST", SEARCH), 1);

    fx.cancel(task).await;
    fx.fake.heal();
    assert!(
        fx.app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .is_some(),
        "the next pass may start once the first one ended"
    );
    fx.quiesce().await;
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn disconnecting_during_a_pass_forgets_the_queue_and_the_pass_writes_nothing_back() {
    let (fx, _photos, _old) = moved_photo().await;
    let task = fx.held_pass().await;

    let (status, body) = fx.delete("/api/immich/connection").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done = fx.cancel(task).await;

    assert_eq!(done.state, TaskState::Cancelled, "{done:?}");
    assert!(fx.rows().await.is_empty());
    assert_eq!(
        fx.pass_state().await,
        PassState {
            last_run_at: None,
            last_error: None,
            consecutive_failures: 0
        },
        "the state of a connection that is gone is not written again"
    );
    assert_eq!(fx.status(false).await["outbox"], Value::Null);
    assert!(fx.deletes().is_empty());
    fx.fake.assert_no_violations();
}

// ----- the carry-over and the delete fail -------------------------------------------------------

/// One photo with an album and a favorite on its asset, moved and scanned: the next pass carries.
async fn ready_to_carry() -> (Fixture, immich_support::Site, String) {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    let family = fx.fake.add_tag("family");
    fx.fake.tag_asset(&family, &old);
    fx.fake.update_asset(&old, |asset| asset.is_favorite = true);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    (fx, photos, old)
}

#[tokio::test]
async fn a_carry_over_that_immich_refuses_asks_a_person_and_deletes_nothing() {
    let (fx, photos, old) = ready_to_carry().await;
    fx.fake.fail("PUT", "/api/assets", 400);

    let report = next_pass(&fx).await;

    assert_eq!(report["attention"], 1, "{report}");
    assert_eq!(report["carried"], 0, "{report}");
    assert_eq!(report["deleted"], 0, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention(), "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("carry_failed"));
    assert!(!row.carried);
    assert!(fx.deletes().is_empty());
    assert!(fx.fake.has_asset(&old));
    assert_eq!(
        fx.outbox_status().await["attention"],
        1,
        "the status counts it for the badge"
    );

    // A person fixed it (or the refusal was a mistake) and presses Retry.
    fx.fake.heal();
    assert_eq!(fx.retry_all().await, 1);
    let report = first_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert!(new.is_favorite);
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.fake.tags_of(&new.id), ["family"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_album_that_refuses_the_asset_asks_a_person_and_deletes_nothing() {
    let (fx, _photos, old) = ready_to_carry().await;
    fx.fake.fail("PUT", "/api/albums/*", 400);

    let report = next_pass(&fx).await;

    assert_eq!(report["attention"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention(), "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("carry_failed"));
    assert!(fx.deletes().is_empty());
    assert!(fx.fake.has_asset(&old));
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_carry_over_that_fails_for_a_passing_reason_is_tried_again_later() {
    let (fx, photos, old) = ready_to_carry().await;
    fx.fake.fail("PUT", "/api/assets", 500);

    let report = next_pass(&fx).await;

    assert_eq!(report["retried"], 1, "{report}");
    assert_eq!(report["deleted"], 0, "{report}");
    let row = fx.only_row().await;
    assert!(row.is_pending(), "{row:?}");
    assert_eq!(row.attempts, 1);
    assert_eq!(row.last_error.as_deref(), Some("immich_unavailable"));
    assert!(row.next_attempt_at > row.created_at, "it backs off");
    assert!(fx.deletes().is_empty());
    assert!(fx.fake.has_asset(&old));

    fx.fake.heal();
    let report = next_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert!(new.is_favorite);
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_half_done_carry_over_is_finished_by_the_next_try_without_doubling_anything() {
    let (fx, photos, old) = ready_to_carry().await;
    // The fields and the album go through, the tags do not.
    fx.fake.fail("PUT", "/api/tags/assets", 500);

    let report = next_pass(&fx).await;

    assert_eq!(report["retried"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.is_pending());
    assert!(
        !row.carried,
        "a carry-over that did not finish is not recorded as done"
    );
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert!(new.is_favorite, "the fields went over");
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"], "and the album");
    assert!(fx.fake.tags_of(&new.id).is_empty());
    assert!(fx.deletes().is_empty());

    fx.fake.heal();
    let report = next_pass(&fx).await;

    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"], "once");
    assert_eq!(fx.fake.tags_of(&new.id), ["family"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_delete_that_fails_for_a_passing_reason_keeps_the_carry_over_and_tries_again() {
    let (fx, photos, old) = ready_to_carry().await;
    fx.fake.fail("DELETE", "/api/assets", 500);

    let report = next_pass(&fx).await;

    assert_eq!(report["deleted"], 0, "{report}");
    assert_eq!(report["retried"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.is_pending(), "{row:?}");
    assert!(row.carried, "what was carried is remembered");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_eq!(row.new_asset_id.as_deref(), Some(new.id.as_str()));
    assert!(fx.fake.has_asset(&old));
    assert_eq!(
        fx.carries(),
        3,
        "the fields, the album and the tag went over"
    );

    fx.fake.heal();
    let report = next_pass(&fx).await;

    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(
        report["carried"], 1,
        "a deleted row was carried first: {report}"
    );
    assert_eq!(fx.carries(), 3, "it is not carried twice");
    assert!(!fx.fake.has_asset(&old));
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_delete_that_immich_refuses_asks_a_person() {
    let (fx, _photos, old) = ready_to_carry().await;
    fx.fake.fail("DELETE", "/api/assets", 403);

    let report = next_pass(&fx).await;

    assert_eq!(report["attention"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention(), "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("immich_error"));
    assert!(fx.fake.has_asset(&old));

    fx.fake.heal();
    assert_eq!(fx.retry_all().await, 1);
    let report = first_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    assert!(!fx.fake.has_asset(&old));
    fx.fake.assert_no_violations();
}

// ----- the first look at the old asset and the scan fail ----------------------------------------

#[tokio::test]
async fn a_snapshot_that_cannot_be_taken_keeps_the_row_from_the_scan() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    fx.fake.fail("GET", "/api/albums", 500);

    let report = first_pass(&fx).await;

    // The scan would turn the old asset offline, and its albums can only be read while it is live.
    assert_eq!(report["retried"], 1, "{report}");
    assert_eq!(report["scans"], 0, "{report}");
    assert!(fx.scans_asked().is_empty());
    assert!(!fx.fake.asset(&old).is_offline);
    let row = fx.only_row().await;
    assert_eq!(row.stage, 0);
    assert!(row.is_pending());
    assert_eq!(row.last_error.as_deref(), Some("immich_unavailable"));

    fx.fake.heal();
    fx.make_due().await;
    first_pass(&fx).await;
    assert_eq!(fx.scans_asked().len(), 1);
    next_pass(&fx).await;
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_scan_that_immich_refuses_waits_for_its_own_schedule() {
    let (fx, photos, old) = moved_photo().await;
    fx.fake.fail("POST", "/api/libraries/*", 403);

    let report = first_pass(&fx).await;

    assert_eq!(report["scans"], 0, "{report}");
    assert!(fx.scans_asked().is_empty());
    let row = fx.only_row().await;
    assert_eq!(row.stage, 1, "it waits for Immich to notice: {row:?}");
    assert_eq!(row.scan_requested_at, None, "no scan was asked for");
    assert!(row.is_pending());

    // Immich's own scan (its schedule, or a person pressing the button there) runs later.
    fx.fake.heal();
    fx.fake.scan_library(&photos.library_id);
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_scan_that_fails_for_a_passing_reason_is_asked_again_later() {
    let (fx, _photos, old) = moved_photo().await;
    fx.fake.fail("POST", "/api/libraries/*", 500);

    let report = first_pass(&fx).await;

    assert_eq!(report["scans"], 0, "{report}");
    assert_eq!(report["retried"], 1, "{report}");
    let row = fx.only_row().await;
    assert_eq!(
        row.stage, 0,
        "it is not waiting for a scan that was never asked: {row:?}"
    );
    assert_eq!(row.last_error.as_deref(), Some("immich_unavailable"));
    assert_eq!(row.attempts, 1);

    fx.fake.heal();
    fx.make_due().await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

// ----- a long queue and a failing Immich --------------------------------------------------------

#[tokio::test]
async fn a_failing_immich_is_not_asked_about_every_row_of_a_long_queue() {
    let (fx, photos, olds) = many_photos(30).await;
    for n in 0..30 {
        fx.move_file(
            (&photos, &format!("2020/p{n:03}.jpg")),
            (&photos, &format!("2021/p{n:03}.jpg")),
        )
        .await;
    }
    fx.fake.clear_calls();
    fx.fake.fail("GET", "/api/assets/*", 500);

    let task = fx.pass().await;

    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    assert!(task.error.is_some());
    let asked = fx
        .fake
        .calls()
        .iter()
        .filter(|call| call.method == "GET" && call.path.starts_with("/api/assets/"))
        .count();
    assert!(
        (10..=14).contains(&asked),
        "the circuit breaker opens after 10 rows in a row, not after all 30: {asked} requests"
    );
    let rows = fx.rows().await;
    let tried = rows.iter().filter(|row| row.attempts == 1).count();
    assert!((10..=14).contains(&tried), "{tried} rows were tried");
    assert!(rows.iter().all(|row| row.attempts <= 1));
    assert_eq!(
        rows.iter().filter(|row| row.attempts == 0).count(),
        30 - tried
    );
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);
    assert!(fx.scans_asked().is_empty());

    // Immich is back and a person pressed Retry: every row goes through.
    fx.fake.heal();
    assert_eq!(fx.retry_all().await, i64::try_from(tried).unwrap());
    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 30, "{report}");
    let mut deleted: Vec<String> = fx
        .deletes()
        .iter()
        .map(|body| body["ids"][0].as_str().expect("one id").to_owned())
        .collect();
    deleted.sort();
    let mut expected = olds;
    expected.sort();
    assert_eq!(deleted, expected);
    fx.fake.assert_no_violations();
}

// ----- an account that cannot ask for a scan ----------------------------------------------------

#[tokio::test]
async fn an_account_that_cannot_scan_waits_for_immich_and_a_person_decides_after_three_days() {
    let (fx, photos, old) = one_photo_limited().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;

    let report = first_pass(&fx).await;

    assert_eq!(report["scans"], 0, "{report}");
    assert!(fx.scans_asked().is_empty());
    let row = fx.only_row().await;
    assert_eq!(row.stage, 1, "{row:?}");
    assert_eq!(row.scan_requested_at, None);
    assert_eq!(
        fx.outbox_status().await["scans_on_demand"],
        false,
        "the settings can tell the person why"
    );

    // Immich has not scanned: the old asset is still online, which is no reason to delete it.
    for _ in 0..2 {
        let report = next_pass(&fx).await;
        assert_eq!(report["waiting"], 1, "{report}");
        assert!(fx.fake.has_asset(&old));
    }
    assert_eq!(fx.only_row().await.attempts, 2);
    // Without a scan of the application the wait is long: Immich's own schedule is slow.
    fx.age_rows(260_000).await;
    let report = next_pass(&fx).await;
    assert_eq!(report["attention"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention(), "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("not_offline"));
    assert!(fx.deletes().is_empty());

    // The scan of Immich finally ran: Retry finishes the work.
    fx.fake.scan_library(&photos.library_id);
    assert_eq!(fx.retry_all().await, 1);
    first_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_account_that_cannot_scan_still_mirrors_a_move_when_immich_scans_by_itself() {
    let (fx, photos, old) = one_photo_limited().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;

    fx.fake.scan_library(&photos.library_id);
    let report = next_pass(&fx).await;

    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert!(
        fx.fake
            .calls()
            .iter()
            .all(|call| !call.path.ends_with("/scan"))
    );
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_copy_for_an_account_that_cannot_scan_has_nothing_to_wait_for() {
    let (fx, photos, _old) = one_photo_limited().await;
    fx.copy_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;

    let report = first_pass(&fx).await;

    assert_eq!(report["dropped"], 1, "{report}");
    assert_eq!(fx.row_count().await, 0);
    assert!(fx.deletes().is_empty());
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_queue_that_drains_after_a_change_makes_the_application_read_immich_once() {
    let (fx, photos, old) = moved_photo().await;
    let db = &fx.app.state.db;
    let known = || async {
        assets::find_by_location(db, photos.volume_id, "2021", "a.jpg")
            .await
            .unwrap()
            .len()
    };

    first_pass(&fx).await;
    fx.quiesce().await;
    assert_eq!(
        fx.tasks_of(TaskKind::ImmichSync).await,
        1,
        "the queue still holds the row: only the sync of the setup"
    );
    assert_eq!(known().await, 0, "the new asset is not known yet");

    next_pass(&fx).await;
    fx.quiesce().await;

    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert_eq!(
        fx.tasks_of(TaskKind::ImmichSync).await,
        2,
        "one sync for the queue that drained"
    );
    assert_eq!(known().await, 1, "the application knows the new asset");
    fx.fake.assert_no_violations();
}
