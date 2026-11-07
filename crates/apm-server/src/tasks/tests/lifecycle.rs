//! How a task gets from `queued` to its final state, and how it is read back.

use apm_core::dto::{ServerEvent, TaskKind, TaskState};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::support::{
    Gate, Harness, Outcome, fs, immich, index, states, submit_instant, task_events, within,
};
use crate::error::AppError;
use crate::tasks::{Lane, TaskError, TaskSpec};

fn explode() -> Outcome {
    panic!("boom while talking to https://u:hunter2@host/x?token=abc123")
}

#[tokio::test]
async fn a_successful_task_is_announced_and_stored() {
    let h = Harness::new().await;
    let mut events = h.events();
    let spec = TaskSpec::new(TaskKind::FsTrash, "Trash 3 files").with_params(json!({ "files": 3 }));

    let id = h
        .engine
        .submit(spec, |_ctx| async { Ok(json!({ "trashed": 3 })) })
        .await
        .unwrap();
    let done = h.finished(id).await;

    assert_eq!(done.id, id);
    assert_eq!(done.kind, TaskKind::FsTrash);
    assert_eq!(done.title, "Trash 3 files");
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(done.error, None);
    assert!(done.started_at.is_some() && done.finished_at.is_some());
    assert_eq!((done.speed, done.eta_secs), (None, None));

    let row = h.row(id).await;
    assert_eq!(row.state, "succeeded");
    assert_eq!(row.lane, "fs");
    assert_eq!(row.params_json, r#"{"files":3}"#);
    assert_eq!(row.result_json.as_deref(), Some(r#"{"trashed":3}"#));
    assert_eq!(row.error, None);
    assert_eq!(row.started_at, done.started_at);
    assert_eq!(row.finished_at, done.finished_at);

    let seen = task_events(&events.drain(), id);
    assert_eq!(
        states(&seen),
        [TaskState::Queued, TaskState::Running, TaskState::Done]
    );
    assert_eq!(seen[0].started_at, None, "announced before it starts");
    assert!(seen[1].started_at.is_some());
    assert_eq!(seen[2], done, "the last event is the final task");
    assert_eq!(h.engine.get(id).await.unwrap(), done);
}

#[tokio::test]
async fn the_runner_gets_the_id_of_its_task() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, id);
    gate.release(1);
    let done = h.finished(id).await;
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(
        h.row(id).await.result_json.as_deref(),
        Some(format!(r#"{{"task":{id}}}"#).as_str())
    );
}

#[tokio::test]
async fn submit_returns_while_the_task_waits_or_runs() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, first);
    let second = h.engine.submit(fs(), gate.runner()).await.unwrap();

    let running = h.row(first).await;
    assert_eq!(running.state, "running");
    assert!(running.started_at.is_some());
    let waiting = h.row(second).await;
    assert_eq!(waiting.state, "queued");
    assert_eq!(waiting.started_at, None);
    assert_eq!(h.engine.get(first).await.unwrap().state, TaskState::Running);
    assert_eq!(h.engine.get(second).await.unwrap().state, TaskState::Queued);

    gate.release(2);
    h.finished(first).await;
    h.finished(second).await;
}

#[tokio::test]
async fn a_failed_task_keeps_its_message_without_secrets() {
    let h = Harness::new().await;
    let mut events = h.events();
    let id = h
        .engine
        .submit(index(), |_ctx| async {
            Err(TaskError::failed(
                "sync with https://alice:hunter2@immich.example/api?token=s3cr3t&page=2 refused",
            ))
        })
        .await
        .unwrap();
    let failed = h.finished(id).await;

    assert_eq!(failed.state, TaskState::Failed);
    let message = failed.error.clone().unwrap();
    assert!(
        message.starts_with("sync with ") && message.ends_with(" refused"),
        "{message}"
    );
    for secret in ["alice", "hunter2", "s3cr3t"] {
        assert!(!message.contains(secret), "{secret} leaked into {message}");
    }
    let row = h.row(id).await;
    assert_eq!(row.state, "failed");
    assert_eq!(row.error.as_deref(), Some(message.as_str()));
    assert_eq!(row.result_json, None);
    let last = task_events(&events.drain(), id).pop().unwrap();
    assert_eq!(last, failed);
}

#[tokio::test]
async fn an_app_error_becomes_the_failure_message() {
    let h = Harness::new().await;
    let id = h
        .engine
        .submit(fs(), |_ctx| async {
            let refused: Result<(), AppError> =
                Err(AppError::bad_request("bad_plan", "nothing to move"));
            refused?;
            Ok(Value::Null)
        })
        .await
        .unwrap();
    let failed = h.finished(id).await;
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.error.as_deref(), Some("nothing to move"));
}

#[tokio::test]
async fn a_panicking_runner_fails_its_task_and_the_lane_keeps_working() {
    let h = Harness::new().await;
    let id = h
        .engine
        .submit(fs(), |_ctx| async { explode() })
        .await
        .unwrap();
    let failed = h.finished(id).await;

    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.error.as_deref(), Some("task failed unexpectedly"));
    let row = h.row(id).await;
    assert_eq!(row.state, "failed");
    assert_eq!(row.error.as_deref(), Some("task failed unexpectedly"));

    let next = submit_instant(&h.engine, fs()).await;
    assert_eq!(h.finished(next).await.state, TaskState::Done);
}

#[tokio::test]
async fn a_null_result_is_fine() {
    let h = Harness::new().await;
    let id = h
        .engine
        .submit(index(), |_ctx| async { Ok(Value::Null) })
        .await
        .unwrap();
    assert_eq!(h.finished(id).await.state, TaskState::Done);
    assert_eq!(h.row(id).await.result_json, None);
}

#[tokio::test]
async fn an_explicit_lane_is_stored_with_the_task() {
    let h = Harness::new().await;
    let id = submit_instant(&h.engine, fs().with_lane(Lane::Immich)).await;
    h.finished(id).await;
    assert_eq!(h.row(id).await.lane, "immich");
}

#[tokio::test]
async fn unknown_ids_are_not_found() {
    let h = Harness::new().await;
    for result in [
        h.engine.get(4242).await,
        h.engine.wait_finished(4242).await,
        h.engine.cancel(4242).await,
    ] {
        let err = result.unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.code(), "not_found");
    }
}

#[tokio::test]
async fn waiting_for_a_finished_task_answers_at_once() {
    let h = Harness::new().await;
    let id = submit_instant(&h.engine, immich()).await;
    let first = h.finished(id).await;
    let again = within(h.engine.wait_finished(id)).await.unwrap();
    assert_eq!(again, first);
    assert_eq!(again.state, TaskState::Done);
}

#[tokio::test]
async fn several_waiters_all_see_the_final_state() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(index(), gate.runner()).await.unwrap();
    gate.started().await;

    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let engine = h.engine.clone();
            tokio::spawn(async move { engine.wait_finished(id).await })
        })
        .collect();
    gate.release(1);
    for waiter in waiters {
        let dto = within(waiter).await.unwrap().unwrap();
        assert_eq!(dto.state, TaskState::Done);
    }
}

#[tokio::test]
async fn the_list_shows_running_then_queued_then_the_newest_finished() {
    let h = Harness::new().await;
    let older = submit_instant(&h.engine, immich()).await;
    h.finished(older).await;
    let newer = submit_instant(&h.engine, immich()).await;
    h.finished(newer).await;

    let mut gate = Gate::new();
    let first = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    let second = h.engine.submit(fs(), gate.runner()).await.unwrap();
    let third = h.engine.submit(fs(), gate.runner()).await.unwrap();
    let remote = h.engine.submit(immich(), gate.runner()).await.unwrap();
    gate.started().await;

    let listed = h.engine.list(100).await.unwrap();
    let order: Vec<_> = listed.iter().map(|task| (task.id, task.state)).collect();
    assert_eq!(
        order,
        [
            (first, TaskState::Running),
            (remote, TaskState::Running),
            (second, TaskState::Queued),
            (third, TaskState::Queued),
            (newer, TaskState::Done),
            (older, TaskState::Done),
        ]
    );

    let active_only = h.engine.list(0).await.unwrap();
    assert_eq!(active_only.len(), 4);
    assert_eq!(
        h.engine.list(-7).await.unwrap().len(),
        4,
        "negative counts as zero"
    );
    assert_eq!(h.engine.list(1).await.unwrap().len(), 5);
    assert_eq!(h.engine.list(1_000_000).await.unwrap().len(), 6);

    gate.release(4);
    for id in [first, second, third, remote] {
        h.finished(id).await;
    }
    let after = h.engine.list(100).await.unwrap();
    assert_eq!(after.len(), 6);
    assert!(after.iter().all(|task| task.state == TaskState::Done));
}

#[tokio::test]
async fn every_task_event_ends_with_a_terminal_state_and_nothing_follows_it() {
    let h = Harness::new().await;
    let mut events = h.events();
    let mut ids = Vec::new();
    for n in 0..6 {
        let spec = if n % 2 == 0 { fs() } else { immich() };
        ids.push(
            h.engine
                .submit(spec, move |_ctx| async move {
                    if n == 3 {
                        Err(TaskError::failed("no"))
                    } else {
                        Ok(json!(n))
                    }
                })
                .await
                .unwrap(),
        );
    }
    for id in &ids {
        h.finished(*id).await;
    }

    let all = events.drain();
    for (n, id) in ids.iter().enumerate() {
        let seen = states(&task_events(&all, *id));
        let end = if n == 3 {
            TaskState::Failed
        } else {
            TaskState::Done
        };
        assert_eq!(
            seen,
            [TaskState::Queued, TaskState::Running, end],
            "task {n}"
        );
    }
    assert!(
        all.iter()
            .all(|event| matches!(event, ServerEvent::Task { .. })),
        "only task events were published"
    );
}

#[tokio::test]
async fn a_subscriber_sees_no_history() {
    let h = Harness::new().await;
    let id = submit_instant(&h.engine, index()).await;
    h.finished(id).await;

    let mut late = h.events();
    assert!(late.drain().is_empty());
    let next = submit_instant(&h.engine, index()).await;
    h.finished(next).await;
    let seen = late.drain();
    assert!(task_events(&seen, id).is_empty());
    assert_eq!(task_events(&seen, next).len(), 3);
}
