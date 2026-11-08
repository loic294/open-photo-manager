//! The scheduler of the outbox of file operations (`immich/outbox/service.rs`) in real time. The
//! other tests of the outbox drive passes themselves (`submit_now`); here the loop of the
//! application (`start_background_every`) decides, so these tests wait on the clock and are few:
//!
//! - a pass never starts while files are still being moved, and a burst of moves is worked off by
//!   ONE task per step (the scan, then the check),
//! - a pass that fails as a whole costs one task, not one per tick, until Retry ends the back-off,
//! - looks that overlap start one pass,
//! - the loop ends with its shutdown token (the one of the application, in production).
//!
//! The 15 seconds Immich is given to scan are not waited for: a test makes the rows due itself,
//! like the other tests of the outbox. The quiet period is a whole number of seconds counted from
//! the whole second of the last change, so it can end up to a second early: tests that need it
//! to hold use a longer one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::time::{Duration, Instant};

use apm_core::dto::{TaskDto, TaskKind, TaskState};
use apm_server::db::now_secs;
use immich_support::{Fixture, eventually, moved_photo, photos_in};
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};

const SEARCH: &str = "/api/search/metadata";

/// How long the loop sleeps at most, so a test does not wait for the real tick of 30 seconds.
const TICK: Duration = Duration::from_millis(100);

/// The quiet period of the test that moves files for a while.
const QUIET_SECS: u64 = 3;

/// Starts the loop as the application does, with a short tick. It ends when the guard is dropped.
fn start_loop(fx: &Fixture) -> DropGuard {
    let stop = CancellationToken::new();
    fx.app
        .state
        .immich_outbox
        .start_background_every(stop.clone(), TICK);
    stop.drop_guard()
}

fn is_over(task: &TaskDto) -> bool {
    !matches!(task.state, TaskState::Queued | TaskState::Running)
}

/// Every pass the application runs or ran.
async fn passes(fx: &Fixture) -> Vec<TaskDto> {
    fx.app
        .state
        .tasks
        .list(500)
        .await
        .unwrap()
        .into_iter()
        .filter(|task| task.kind == TaskKind::ImmichCleanup)
        .collect()
}

/// Waits until at least `count` passes exist and all of them ended; the passes.
async fn passes_after(fx: &Fixture, count: usize) -> Vec<TaskDto> {
    eventually("the passes to end", || async {
        let passes = passes(fx).await;
        passes.len() >= count && passes.iter().all(is_over)
    })
    .await;
    passes(fx).await
}

#[tokio::test]
async fn the_loop_waits_for_the_moving_to_stop_then_works_a_burst_off_with_two_passes() {
    let fx = Fixture::connected_with(|config| config.immich_quiet_secs = QUIET_SECS).await;
    let (photos, olds) = photos_in(&fx, 12).await;
    let syncs = fx.tasks_of(TaskKind::ImmichSync).await;
    let _loop = start_loop(&fx);

    for n in 0..olds.len() {
        let from = format!("2020/p{n:03}.jpg");
        let to = format!("2021/p{n:03}.jpg");
        fx.move_file((&photos, &from), (&photos, &to)).await;
        if n + 1 < olds.len() {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    let last_move = now_secs();
    assert_eq!(fx.row_count().await, 12);
    assert!(
        passes(&fx).await.is_empty(),
        "a pass started while files were still being moved"
    );

    let ended = passes_after(&fx, 1).await;
    assert_eq!(ended.len(), 1, "{ended:?}");
    let first = &ended[0];
    assert_eq!(first.state, TaskState::Done, "{first:?}");
    assert_eq!(first.title, "Updating Immich (12 changes)");
    let started = first.started_at.expect("a pass that ran has a start");
    assert!(
        started >= last_move + 2,
        "the pass started at {started}, the last move was at {last_move}: the quiet period is {QUIET_SECS} s"
    );
    assert!(
        fx.rows().await.iter().all(|row| row.stage == 1),
        "the first pass asked for the scan of every change"
    );
    assert_eq!(fx.scans_asked().len(), 1, "one scan for the whole burst");

    // Changes that wait for the scan do not start a pass by themselves.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(passes(&fx).await.len(), 1);

    fx.make_due().await;
    eventually("the changes to be worked off", || async {
        fx.row_count().await == 0
    })
    .await;
    fx.quiesce().await;

    let ended = passes(&fx).await;
    assert_eq!(ended.len(), 2, "a burst is two passes: {ended:?}");
    assert!(
        ended.iter().all(|task| task.state == TaskState::Done),
        "{ended:?}"
    );
    let mut deleted: Vec<String> = fx
        .deletes()
        .iter()
        .map(|body| body["ids"][0].as_str().expect("one id").to_owned())
        .collect();
    deleted.sort();
    let mut expected = olds.clone();
    expected.sort();
    assert_eq!(deleted, expected, "every old asset was removed, once");
    assert_eq!(
        fx.tasks_of(TaskKind::ImmichSync).await,
        syncs + 1,
        "the drained queue is followed by one sync"
    );
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_failing_immich_costs_one_failed_pass_not_one_per_tick_and_retry_ends_the_back_off() {
    let (fx, _photos, _old) = moved_photo().await;
    fx.fake.fail("POST", SEARCH, 500);
    let _loop = start_loop(&fx);

    let ended = passes_after(&fx, 1).await;
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].state, TaskState::Failed, "{:?}", ended[0]);
    assert!(ended[0].error.is_some(), "a failed pass says why");
    assert_eq!(fx.pass_state().await.consecutive_failures, 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let searches = fx.fake.count("POST", SEARCH);
    assert!(searches > 0);

    // The row is due and the loop looks every tick, yet the back-off of a minute holds.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(passes(&fx).await.len(), 1, "no second pass");
    assert_eq!(
        fx.fake.count("POST", SEARCH),
        searches,
        "Immich was not asked again"
    );
    assert_eq!(fx.only_row().await.stage, 0, "the row was not touched");

    // Retry ends the back-off and wakes the loop.
    fx.fake.heal();
    let retried = fx.app.state.immich_outbox.retry(None).await.unwrap();
    assert_eq!(retried.retried, 0, "the row was due already");
    let ended = passes_after(&fx, 2).await;
    assert_eq!(ended.len(), 2, "{ended:?}");
    assert_eq!(
        ended
            .iter()
            .filter(|task| task.state == TaskState::Done)
            .count(),
        1,
        "{ended:?}"
    );
    let state = fx.pass_state().await;
    assert_eq!(state.consecutive_failures, 0);
    assert_eq!(state.last_error, None);
    assert_eq!(fx.only_row().await.stage, 1, "the pass asked for the scan");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(passes(&fx).await.len(), 2, "nothing is due until the scan");
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn looks_that_overlap_start_one_pass() {
    let (fx, _photos, _old) = moved_photo().await;

    let mut looks = JoinSet::new();
    for _ in 0..8 {
        let outbox = fx.app.state.immich_outbox.clone();
        looks.spawn(async move { outbox.submit_now().await.unwrap() });
    }
    let mut started = Vec::new();
    while let Some(look) = looks.join_next().await {
        started.extend(look.unwrap());
    }

    assert_eq!(started.len(), 1, "{started:?}");
    let task = fx.finish(started[0]).await;
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(passes(&fx).await.len(), 1);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn the_loop_ends_with_its_shutdown_token() {
    let fx = Fixture::connected().await;
    fx.quiesce().await;
    let alive = || {
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
    };
    let before = alive();

    let stop = CancellationToken::new();
    fx.app
        .state
        .immich_outbox
        .start_background_every(stop.clone(), TICK);
    assert_eq!(alive(), before + 1, "the loop is one task");

    let started = Instant::now();
    stop.cancel();
    eventually("the loop to end", || async { alive() <= before }).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the loop took {:?} to end",
        started.elapsed()
    );
}
