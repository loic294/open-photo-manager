//! Stopping the engine: what happens to queued and running tasks, to those that need time to wind
//! down and to those that never stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use apm_core::dto::TaskState;
use axum::http::StatusCode;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::support::{Gate, Harness, eventually, fast, fs, immich, states, task_events, within};
use crate::db::Db;
use crate::tasks::{TaskEngine, TaskError, TaskOptions};

const RESTART_MESSAGE: &str = "interrupted by restart";

/// Sets a flag when the future that owns it is dropped.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_running_task_is_interrupted_when_the_engine_stops() {
    let h = Harness::new().await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;

    h.engine.shutdown().await;

    let task = h.engine.get(id).await.unwrap();
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.error.as_deref(), Some(RESTART_MESSAGE));
    assert!(task.started_at.is_some() && task.finished_at.is_some());
    let row = h.row(id).await;
    assert_eq!(row.state, "interrupted");
    assert_eq!(row.error.as_deref(), Some(RESTART_MESSAGE));
    assert!(row.finished_at.is_some());
    assert_eq!(
        states(&task_events(&events.drain(), id)),
        [
            TaskState::Queued,
            TaskState::Running,
            TaskState::Interrupted
        ]
    );
}

#[tokio::test]
async fn a_waiting_task_is_interrupted_and_never_runs() {
    let h = Harness::new().await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let running = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    h.engine.shutdown().await;

    let task = h.engine.get(waiting).await.unwrap();
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.error.as_deref(), Some(RESTART_MESSAGE));
    assert_eq!(task.started_at, None);
    assert!(task.finished_at.is_some());
    assert_eq!(h.row(waiting).await.state, "interrupted");
    gate.assert_nothing_starts().await;

    let seen = events.drain();
    assert_eq!(
        states(&task_events(&seen, waiting)),
        [TaskState::Queued, TaskState::Interrupted]
    );
    assert_eq!(
        states(&task_events(&seen, running)),
        [
            TaskState::Queued,
            TaskState::Running,
            TaskState::Interrupted
        ]
    );
    assert_eq!(h.active_rows().await, 0);
}

#[tokio::test]
async fn a_user_cancellation_stays_a_cancellation_through_a_shutdown() {
    let h = Harness::new().await;
    let (entered_tx, entered) = oneshot::channel();
    let (finish_cleanup, cleaning_up) = oneshot::channel::<()>();
    let id = h
        .engine
        .submit(fs(), move |ctx| async move {
            entered_tx.send(()).ok();
            ctx.cancel.cancelled().await;
            cleaning_up.await.ok();
            Err(TaskError::Cancelled)
        })
        .await
        .unwrap();
    within(entered).await.unwrap();

    h.engine.cancel(id).await.unwrap();
    let stopping = tokio::spawn({
        let engine = h.engine.clone();
        async move { engine.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    finish_cleanup.send(()).ok();
    within(stopping).await.unwrap();

    let task = h.engine.get(id).await.unwrap();
    assert_eq!(task.state, TaskState::Cancelled);
    assert_eq!(task.error, None);
    assert_eq!(h.row(id).await.state, "cancelled");
}

#[tokio::test]
async fn nothing_can_be_submitted_once_the_engine_is_stopped() {
    let h = Harness::new().await;
    h.engine.shutdown().await;

    let err = h
        .engine
        .submit(fs(), |_ctx| async { Ok(json!({})) })
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code(), "shutting_down");
    assert!(h.stored_ids().await.is_empty(), "no row is left behind");
}

#[tokio::test]
async fn stopping_twice_is_fine() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;

    // Two callers at once, then a third one afterwards.
    tokio::join!(h.engine.shutdown(), h.engine.shutdown());
    let before = h.row(id).await;
    h.engine.shutdown().await;
    let after = h.row(id).await;

    assert_eq!(after.state, "interrupted");
    assert_eq!(after.finished_at, before.finished_at);
    assert_eq!(after.error, before.error);
}

#[tokio::test]
async fn an_interrupted_task_cannot_be_cancelled() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    h.engine.shutdown().await;

    let err = h.engine.cancel(id).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
    assert_eq!(err.code(), "conflict");
    assert_eq!(h.row(id).await.state, "interrupted");
}

#[tokio::test]
async fn a_runner_that_will_not_stop_is_aborted_after_the_grace_period() {
    let grace = Duration::from_millis(100);
    let h = Harness::with(TaskOptions {
        shutdown_grace: grace,
        ..fast()
    })
    .await;
    let dropped = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = oneshot::channel();
    let id = h
        .engine
        .submit(fs(), {
            let flag = DropFlag(Arc::clone(&dropped));
            move |_ctx| async move {
                let _flag = flag;
                entered_tx.send(()).ok();
                std::future::pending::<()>().await;
                Ok(json!({}))
            }
        })
        .await
        .unwrap();
    within(entered).await.unwrap();

    let begun = Instant::now();
    h.engine.shutdown().await;
    assert!(
        begun.elapsed() >= grace,
        "the runner is given its grace period"
    );

    let task = h.engine.get(id).await.unwrap();
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.error.as_deref(), Some(RESTART_MESSAGE));
    assert_eq!(h.row(id).await.state, "interrupted");
    let dropped = &dropped;
    eventually("the aborted runner to be dropped", || async move {
        dropped.load(Ordering::SeqCst).then_some(())
    })
    .await;
}

#[tokio::test]
async fn a_runner_that_needs_time_to_clean_up_is_waited_for() {
    let h = Harness::new().await;
    let cleaned = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = oneshot::channel();
    let id = h
        .engine
        .submit(fs(), {
            let cleaned = Arc::clone(&cleaned);
            move |ctx| async move {
                entered_tx.send(()).ok();
                ctx.cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                cleaned.store(true, Ordering::SeqCst);
                Err(TaskError::Cancelled)
            }
        })
        .await
        .unwrap();
    within(entered).await.unwrap();

    h.engine.shutdown().await;
    assert!(
        cleaned.load(Ordering::SeqCst),
        "the shutdown returned before the runner was done"
    );
    assert_eq!(h.row(id).await.state, "interrupted");
}

#[tokio::test]
async fn cancelling_the_app_wide_token_stops_the_engine() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let running = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    h.shutdown.cancel();
    assert_eq!(h.finished(running).await.state, TaskState::Interrupted);

    // The lane is free again but an engine that is stopping starts nothing more.
    gate.assert_nothing_starts().await;
    assert_eq!(
        h.engine.get(waiting).await.unwrap().state,
        TaskState::Queued
    );
    let err = h.engine.submit(fs(), gate.runner()).await.unwrap_err();
    assert_eq!(err.code(), "shutting_down");

    h.engine.shutdown().await;
    let task = h.finished(waiting).await;
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.started_at, None);
    assert_eq!(h.active_rows().await, 0);
}

#[tokio::test]
async fn the_shutdown_token_fires_with_the_app_token_and_with_the_engine() {
    let h = Harness::new().await;
    let token = h.engine.shutdown_token();
    assert!(!token.is_cancelled());
    h.shutdown.cancel();
    assert!(token.is_cancelled());

    let other = Harness::new().await;
    let token = other.engine.shutdown_token();
    other.engine.shutdown().await;
    assert!(token.is_cancelled());
    assert!(
        !other.shutdown.is_cancelled(),
        "the app-wide token is not the engine's to cancel"
    );
}

#[tokio::test]
async fn dropping_the_last_handle_cancels_the_shutdown_token() {
    let db = Db::open_temp().await.unwrap();
    let engine = TaskEngine::with_options(db.handle(), CancellationToken::new(), fast())
        .await
        .unwrap();
    let token = engine.shutdown_token();
    let clone = engine.clone();

    drop(engine);
    assert!(!token.is_cancelled(), "a clone keeps the engine alive");
    drop(clone);
    assert!(token.is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submissions_racing_a_shutdown_leave_nothing_unfinished() {
    let h = Harness::new().await;
    let submitters: Vec<_> = (0..8)
        .map(|n| {
            let engine = h.engine.clone();
            tokio::spawn(async move {
                let mut accepted = Vec::new();
                for _ in 0..40 {
                    let spec = if n % 2 == 0 { fs() } else { immich() };
                    let submitted = engine
                        .submit(spec, |ctx| async move {
                            tokio::select! {
                                () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                                () = tokio::time::sleep(Duration::from_millis(1)) => Ok(json!({})),
                            }
                        })
                        .await;
                    match submitted {
                        Ok(id) => accepted.push(id),
                        Err(err) => {
                            assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
                            assert_eq!(err.code(), "shutting_down");
                            break;
                        }
                    }
                }
                accepted
            })
        })
        .collect();

    tokio::time::sleep(Duration::from_millis(30)).await;
    h.engine.shutdown().await;

    // A submission that lost the race closes its own row, so wait for all of them.
    let mut accepted = Vec::new();
    for submitter in submitters {
        accepted.extend(within(submitter).await.unwrap());
    }
    assert!(!accepted.is_empty());
    assert_eq!(
        h.active_rows().await,
        0,
        "no task is left queued or running"
    );
    for id in accepted {
        let task = h.engine.get(id).await.unwrap();
        assert!(
            matches!(task.state, TaskState::Done | TaskState::Interrupted),
            "task {id} ended {:?}",
            task.state
        );
    }
}
