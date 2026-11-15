//! Cancelling tasks: queued ones, running ones, twice, and after the fact.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use opm_core::dto::TaskState;
use serde_json::json;
use tokio::sync::oneshot;

use super::support::{Gate, Harness, fs, states, submit_instant, task_events, within};
use crate::tasks::TaskError;

#[tokio::test]
async fn a_queued_task_is_cancelled_and_its_runner_never_starts() {
    let h = Harness::serial_fs().await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, first);

    let started = Arc::new(AtomicBool::new(false));
    let second = h
        .engine
        .submit(fs(), {
            let started = Arc::clone(&started);
            move |_ctx| async move {
                started.store(true, Ordering::SeqCst);
                Ok(json!({}))
            }
        })
        .await
        .unwrap();

    let cancelled = h.engine.cancel(second).await.unwrap();
    assert_eq!(cancelled.id, second);
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.started_at, None);
    assert!(cancelled.finished_at.is_some());
    assert_eq!(cancelled.error, None);
    let row = h.row(second).await;
    assert_eq!(row.state, "cancelled");
    assert_eq!(row.started_at, None);
    assert_eq!(row.finished_at, cancelled.finished_at);
    assert_eq!(h.engine.get(second).await.unwrap(), cancelled);

    // The lane carries on with whatever comes next.
    let third = submit_instant(&h.engine, fs()).await;
    gate.release(1);
    assert_eq!(h.finished(first).await.state, TaskState::Done);
    assert_eq!(h.finished(third).await.state, TaskState::Done);
    assert!(
        !started.load(Ordering::SeqCst),
        "a cancelled task never runs"
    );
    assert_eq!(h.finished(second).await, cancelled);
    assert_eq!(
        states(&task_events(&events.drain(), second)),
        [TaskState::Queued, TaskState::Cancelled]
    );
}

#[tokio::test]
async fn cancelling_a_queued_task_lets_the_next_one_move_up() {
    let h = Harness::serial_fs().await;
    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, first);
    let doomed = h.engine.submit(fs(), gate.runner()).await.unwrap();
    let next = h.engine.submit(fs(), gate.runner()).await.unwrap();

    h.engine.cancel(doomed).await.unwrap();
    gate.release(1);
    assert_eq!(gate.started().await, next);
    gate.release(1);
    assert_eq!(h.finished(next).await.state, TaskState::Done);
}

#[tokio::test]
async fn a_running_task_is_asked_to_stop_and_ends_cancelled() {
    let h = Harness::new().await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, id);

    let answer = h.engine.cancel(id).await.unwrap();
    assert_eq!(
        answer.state,
        TaskState::Running,
        "it ends when its runner returns"
    );

    let done = h.finished(id).await;
    assert_eq!(done.state, TaskState::Cancelled);
    assert_eq!(done.error, None);
    assert!(done.started_at.is_some() && done.finished_at.is_some());
    let row = h.row(id).await;
    assert_eq!(row.state, "cancelled");
    assert_eq!(row.error, None);
    assert_eq!(row.result_json, None);
    let seen = task_events(&events.drain(), id);
    assert_eq!(
        states(&seen),
        [TaskState::Queued, TaskState::Running, TaskState::Cancelled]
    );
    assert_eq!(seen[2], done);
}

#[tokio::test]
async fn cancelling_twice_is_fine() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;

    h.engine.cancel(id).await.unwrap();
    h.engine.cancel(id).await.unwrap();
    h.finished(id).await;
    let again = h.engine.cancel(id).await.unwrap();
    assert_eq!(again.state, TaskState::Cancelled);
}

#[tokio::test]
async fn cancelling_a_queued_task_twice_at_once_gives_both_callers_the_same_answer() {
    let h = Harness::serial_fs().await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, first);
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    let (one, other) = tokio::join!(h.engine.cancel(waiting), h.engine.cancel(waiting));

    let (one, other) = (one.unwrap(), other.unwrap());
    assert_eq!(one.state, TaskState::Cancelled);
    assert_eq!(other, one);
    assert_eq!(h.row(waiting).await.state, "cancelled");
    assert_eq!(
        states(&task_events(&events.drain(), waiting)),
        [TaskState::Queued, TaskState::Cancelled],
        "one end however many callers"
    );
    gate.release(1);
    h.finished(first).await;
}

#[tokio::test]
async fn a_task_that_a_shutdown_is_closing_cannot_be_cancelled() {
    let h = Harness::serial_fs().await;
    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, first);
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    // The shutdown takes the waiting task out of its lane before the cancel gets to it.
    let (_, cancelled) = tokio::join!(h.engine.shutdown(), h.engine.cancel(waiting));

    let err = cancelled.unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
    assert_eq!(err.code(), "conflict");
    assert_eq!(h.row(waiting).await.state, "interrupted");
}

#[tokio::test]
async fn a_task_that_ended_another_way_cannot_be_cancelled() {
    let h = Harness::new().await;
    let done = submit_instant(&h.engine, fs()).await;
    let failed = h
        .engine
        .submit(fs(), |_ctx| async { Err(TaskError::failed("no")) })
        .await
        .unwrap();
    h.finished(done).await;
    h.finished(failed).await;

    for id in [done, failed] {
        let err = h.engine.cancel(id).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT, "task {id}");
        assert_eq!(err.code(), "conflict");
        assert_eq!(err.message(), format!("task {id} is not running"));
    }
    assert_eq!(h.row(done).await.state, "succeeded");
    assert_eq!(h.row(failed).await.state, "failed");
}

#[tokio::test]
async fn a_runner_that_ignores_the_token_keeps_its_lane_until_it_returns() {
    let h = Harness::serial_fs().await;
    let mut gate = Gate::new();
    let (finish, hold) = oneshot::channel::<()>();
    let (entered, running) = oneshot::channel::<()>();
    let stubborn = h
        .engine
        .submit(fs(), |_ctx| async move {
            entered.send(()).ok();
            hold.await.ok();
            Ok(json!({ "late": true }))
        })
        .await
        .unwrap();
    within(running).await.unwrap();
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    let answer = h.engine.cancel(stubborn).await.unwrap();
    assert_eq!(answer.state, TaskState::Running);
    gate.assert_nothing_starts().await;
    assert_eq!(
        h.engine.get(stubborn).await.unwrap().state,
        TaskState::Running
    );

    finish.send(()).unwrap();
    let done = h.finished(stubborn).await;
    assert_eq!(
        done.state,
        TaskState::Cancelled,
        "it returned after being cancelled"
    );
    assert_eq!(
        h.row(stubborn).await.result_json.as_deref(),
        Some(r#"{"late":true}"#),
        "what it returned is kept"
    );
    assert_eq!(gate.started().await, waiting);
    gate.release(1);
    h.finished(waiting).await;
}

#[tokio::test]
async fn returning_cancelled_without_a_request_is_a_cancellation_too() {
    let h = Harness::new().await;
    let id = h
        .engine
        .submit(fs(), |_ctx| async { Err(TaskError::Cancelled) })
        .await
        .unwrap();
    let done = h.finished(id).await;
    assert_eq!(done.state, TaskState::Cancelled);
    assert_eq!(done.error, None);
}

#[tokio::test]
async fn a_task_that_was_told_to_stop_says_so_until_it_ends() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, id);
    assert!(!h.engine.is_stopping(id));

    h.engine.cancel(id).await.unwrap();
    assert!(h.engine.is_stopping(id));
    assert_eq!(
        h.engine.get(id).await.unwrap().state,
        TaskState::Running,
        "the state only changes when its runner returns"
    );

    h.finished(id).await;
    assert!(!h.engine.is_stopping(id), "an ended task is not stopping");
    assert!(!h.engine.is_stopping(id + 1000), "nor is an unknown one");
}

#[tokio::test]
async fn a_task_cancelled_before_its_runner_looked_is_stopping_and_never_runs() {
    let h = Harness::new().await;
    let started = Arc::new(AtomicBool::new(false));
    let id = h
        .engine
        .submit(fs(), {
            let started = Arc::clone(&started);
            move |_ctx| async move {
                started.store(true, Ordering::SeqCst);
                Ok(json!({}))
            }
        })
        .await
        .unwrap();

    // The engine started the task (it reads `running`), but its driver has not run yet.
    h.engine.cancel(id).await.unwrap();
    assert!(h.engine.is_stopping(id));

    assert_eq!(h.finished(id).await.state, TaskState::Cancelled);
    assert!(!started.load(Ordering::SeqCst));
}
