//! `GET /api/tasks` and `POST /api/tasks/{id}/cancel` over the real router, with tasks submitted
//! straight to the engine of the `AppState`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use opm_core::dto::{TaskDto, TaskKind, TaskState};
use opm_core::paths::TaskId;
use opm_server::config::Config;
use opm_server::state::AppState;
use opm_server::tasks::{TaskCtx, TaskError, TaskSpec};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};

/// How long a test waits for something that has to happen.
const PATIENCE: Duration = Duration::from_secs(10);

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

fn spec(kind: TaskKind, title: &str) -> TaskSpec {
    TaskSpec::new(kind, title)
}

/// Submits a task that returns at once and waits until it is over.
async fn run_to_the_end(app: &TestApp, kind: TaskKind, title: &str) -> TaskId {
    let tasks = &app.state.tasks;
    let id = tasks
        .submit(spec(kind, title), |_ctx| async { Ok(json!({})) })
        .await
        .unwrap();
    within(tasks.wait_finished(id)).await.unwrap();
    id
}

/// Runners that hold their lane until the test releases them.
struct Gate {
    permits: Arc<Semaphore>,
    announce: mpsc::UnboundedSender<TaskId>,
    started: mpsc::UnboundedReceiver<TaskId>,
}

impl Gate {
    fn new() -> Self {
        let (announce, started) = mpsc::unbounded_channel();
        Self {
            permits: Arc::new(Semaphore::new(0)),
            announce,
            started,
        }
    }

    /// A runner that announces its start, then ends `Ok` once [`Gate::release`] gives it a permit
    /// and `Cancelled` when its token fires.
    fn runner(
        &self,
    ) -> impl FnOnce(TaskCtx) -> BoxFuture<'static, Result<Value, TaskError>> + Send + 'static {
        let (permits, announce) = (Arc::clone(&self.permits), self.announce.clone());
        move |ctx| {
            async move {
                announce.send(ctx.id).ok();
                tokio::select! {
                    () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                    permit = permits.acquire() => {
                        permit.expect("the gate is never closed").forget();
                        Ok(json!({}))
                    }
                }
            }
            .boxed()
        }
    }

    fn release(&self, count: usize) {
        self.permits.add_permits(count);
    }

    /// The id of the next runner that started.
    async fn started(&mut self) -> TaskId {
        within(self.started.recv())
            .await
            .expect("the gate keeps its sender")
    }

    async fn fill_slots(&mut self, app: &TestApp, kind: TaskKind) -> Vec<TaskId> {
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = app
                .state
                .tasks
                .submit(spec(kind, "Running"), self.runner())
                .await
                .unwrap();
            assert_eq!(self.started().await, id);
            ids.push(id);
        }
        ids
    }

    /// Fails if a runner starts within a short while.
    async fn assert_nothing_starts(&mut self) {
        if let Ok(started) =
            tokio::time::timeout(Duration::from_millis(150), self.started.recv()).await
        {
            panic!("task {started:?} started although it was cancelled");
        }
    }
}

async fn list(app: &TestApp) -> Vec<Value> {
    let (status, body) = app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    body.as_array().expect("the list is a JSON array").clone()
}

fn cancel_path(id: impl std::fmt::Display) -> String {
    format!("/api/tasks/{id}/cancel")
}

fn ids(tasks: &[Value]) -> Vec<i64> {
    tasks
        .iter()
        .map(|task| task["id"].as_i64().expect("every task has an id"))
        .collect()
}

fn states(tasks: &[Value]) -> Vec<&str> {
    tasks
        .iter()
        .map(|task| task["state"].as_str().expect("every task has a state"))
        .collect()
}

fn assert_error(body: &Value, code: &str) {
    assert_eq!(body["error"]["code"], code, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{body}"
    );
}

#[tokio::test]
async fn a_server_without_tasks_lists_none() {
    let app = TestApp::new().await;
    let (status, body) = app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn the_list_has_the_running_task_then_the_queued_ones_then_the_finished_newest_first() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let first = run_to_the_end(&app, TaskKind::FsMove, "First").await;
    let second = run_to_the_end(&app, TaskKind::FsTrash, "Second").await;

    let mut gate = Gate::new();
    let running = gate.fill_slots(&app, TaskKind::FsMove).await;
    let queued_a = tasks
        .submit(spec(TaskKind::FsMove, "Queued A"), gate.runner())
        .await
        .unwrap();
    let queued_b = tasks
        .submit(spec(TaskKind::FsRmdir, "Queued B"), gate.runner())
        .await
        .unwrap();

    let listed = list(&app).await;
    assert_eq!(
        ids(&listed),
        [
            running[0], running[1], running[2], queued_a, queued_b, second, first
        ],
        "{listed:?}"
    );
    assert_eq!(
        states(&listed),
        [
            "running", "running", "running", "queued", "queued", "done", "done"
        ]
    );

    for id in running.iter().copied().chain([queued_a, queued_b]) {
        gate.release(1);
        within(tasks.wait_finished(id)).await.unwrap();
    }
    let listed = list(&app).await;
    assert_eq!(
        ids(&listed),
        [
            queued_b, queued_a, running[2], running[1], running[0], second, first
        ],
        "newest first once everything is over: {listed:?}"
    );
    assert_eq!(states(&listed), ["done"; 7]);
}

#[tokio::test]
async fn a_task_is_listed_with_the_fields_of_the_dto() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let finished = run_to_the_end(&app, TaskKind::FsTrash, "Trash 3 files").await;
    // Fill the three shared slots so the second move stays queued.
    let mut gate = Gate::new();
    let indexing = tasks
        .submit(spec(TaskKind::Index, "Index photos"), gate.runner())
        .await
        .unwrap();
    assert_eq!(gate.started().await, indexing);
    let moving = tasks
        .submit(spec(TaskKind::FsMove, "Move 2 files"), gate.runner())
        .await
        .unwrap();
    assert_eq!(gate.started().await, moving);
    let syncing = tasks
        .submit(spec(TaskKind::ImmichSync, "Sync photos"), gate.runner())
        .await
        .unwrap();
    assert_eq!(gate.started().await, syncing);
    let waiting = tasks
        .submit(spec(TaskKind::FsMove, "Move 4 files"), gate.runner())
        .await
        .unwrap();

    let listed = list(&app).await;
    let by_id = |id: TaskId| {
        listed
            .iter()
            .find(|task| task["id"] == id)
            .unwrap_or_else(|| panic!("task {id} is not listed: {listed:?}"))
    };

    let indexing_task = by_id(indexing);
    assert_eq!(indexing_task["kind"], "index");
    assert_eq!(indexing_task["title"], "Index photos");
    assert_eq!(indexing_task["state"], "running");
    assert_eq!(indexing_task["items_done"], 0);
    assert_eq!(indexing_task["items_total"], Value::Null);
    assert_eq!(indexing_task["bytes_done"], 0);
    assert_eq!(indexing_task["bytes_total"], Value::Null);
    assert_eq!(indexing_task["error"], Value::Null);
    assert!(indexing_task["started_at"].is_i64(), "{indexing_task}");
    assert_eq!(indexing_task["finished_at"], Value::Null);

    let moving_task = by_id(moving);
    assert_eq!(moving_task["kind"], "fs_move");
    assert_eq!(moving_task["state"], "running");
    let waiting_task = by_id(waiting);
    assert_eq!(waiting_task["state"], "queued");
    assert_eq!(waiting_task["started_at"], Value::Null);
    assert_eq!(waiting_task["finished_at"], Value::Null);
    assert_eq!(waiting_task["speed"], Value::Null);
    assert_eq!(waiting_task["eta_secs"], Value::Null);

    let finished_task = by_id(finished);
    assert_eq!(finished_task["kind"], "fs_trash");
    assert_eq!(finished_task["title"], "Trash 3 files");
    assert_eq!(finished_task["state"], "done");
    assert_eq!(finished_task["error"], Value::Null);
    assert!(finished_task["started_at"].is_i64(), "{finished_task}");
    assert!(finished_task["finished_at"].is_i64(), "{finished_task}");

    // Every entry reads back as the DTO the frontend is generated from.
    let parsed: Vec<TaskDto> = serde_json::from_value(Value::Array(listed.clone())).unwrap();
    assert_eq!(parsed.len(), 5);

    app.state.begin_shutdown();
    app.state.shutdown().await;
}

#[tokio::test]
async fn a_failed_task_is_listed_with_its_error() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let id = tasks
        .submit(spec(TaskKind::ImmichSync, "Sync Immich"), |_ctx| async {
            Err(TaskError::failed("the server said no"))
        })
        .await
        .unwrap();
    within(tasks.wait_finished(id)).await.unwrap();

    let listed = list(&app).await;
    assert_eq!(ids(&listed), [id]);
    assert_eq!(listed[0]["kind"], "immich_sync");
    assert_eq!(listed[0]["state"], "failed");
    assert_eq!(listed[0]["error"], "the server said no");
    assert!(listed[0]["finished_at"].is_i64(), "{}", listed[0]);
}

#[tokio::test]
async fn an_error_message_never_leaks_a_url_with_credentials() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let id = tasks
        .submit(spec(TaskKind::ImmichSync, "Sync Immich"), |_ctx| async {
            Err(TaskError::failed(
                "cannot reach https://robot:hunter2@photos.example.com/api?key=abc123",
            ))
        })
        .await
        .unwrap();
    within(tasks.wait_finished(id)).await.unwrap();

    let listed = list(&app).await;
    assert_eq!(
        listed[0]["error"],
        "cannot reach https://photos.example.com/api?key=[redacted]"
    );
}

#[tokio::test]
async fn cancelling_a_queued_task_answers_202_with_the_cancelled_task_and_it_never_runs() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let mut gate = Gate::new();
    let running = gate.fill_slots(&app, TaskKind::FsMove).await;
    let queued = tasks
        .submit(spec(TaskKind::FsMove, "Waiting"), gate.runner())
        .await
        .unwrap();

    let (status, body) = app.call(Method::POST, &cancel_path(queued), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["id"], queued);
    assert_eq!(body["state"], "cancelled");
    assert_eq!(body["started_at"], Value::Null);
    assert!(body["finished_at"].is_i64(), "{body}");

    // A second request changes nothing and is not an error either.
    let (status, again) = app.call(Method::POST, &cancel_path(queued), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(again["state"], "cancelled");

    let listed = list(&app).await;
    assert_eq!(ids(&listed), [running[0], running[1], running[2], queued]);
    assert_eq!(
        states(&listed),
        ["running", "running", "running", "cancelled"]
    );

    // The lane frees up when the running task ends, but nothing is left to run in it.
    gate.release(3);
    for id in running {
        within(tasks.wait_finished(id)).await.unwrap();
    }
    gate.assert_nothing_starts().await;
}

#[tokio::test]
async fn cancelling_a_running_task_answers_202_and_the_task_ends_cancelled() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let mut gate = Gate::new();
    let id = tasks
        .submit(spec(TaskKind::Index, "Index photos"), gate.runner())
        .await
        .unwrap();
    assert_eq!(gate.started().await, id);

    let (status, body) = app.call(Method::POST, &cancel_path(id), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["id"], id);
    assert_eq!(
        body["state"], "running",
        "it ends when its runner returns: {body}"
    );

    let ended = within(tasks.wait_finished(id)).await.unwrap();
    assert_eq!(ended.state, TaskState::Cancelled);
    let listed = list(&app).await;
    assert_eq!(states(&listed), ["cancelled"]);
    assert!(listed[0]["finished_at"].is_i64(), "{}", listed[0]);

    let (status, again) = app.call(Method::POST, &cancel_path(id), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(again["state"], "cancelled");
}

#[tokio::test]
async fn cancelling_an_unknown_task_is_a_404() {
    let app = TestApp::new().await;
    for id in [1, 4242, i64::MAX] {
        let (status, body) = app.call(Method::POST, &cancel_path(id), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_error(&body, "not_found");
    }
}

#[tokio::test]
async fn cancelling_a_task_that_is_over_is_a_409() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let done = run_to_the_end(&app, TaskKind::FsMove, "Done").await;
    let failed = tasks
        .submit(spec(TaskKind::FsMove, "Failed"), |_ctx| async {
            Err(TaskError::failed("disk full"))
        })
        .await
        .unwrap();
    within(tasks.wait_finished(failed)).await.unwrap();

    for id in [done, failed] {
        let (status, body) = app.call(Method::POST, &cancel_path(id), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{id}");
        assert_error(&body, "conflict");
    }

    // The refusal changed nothing.
    let listed = list(&app).await;
    assert_eq!(states(&listed), ["failed", "done"]);
}

#[tokio::test]
async fn a_task_id_that_is_not_a_number_is_a_400() {
    let app = TestApp::new().await;
    for id in ["abc", "1.5", "-", "1%20"] {
        let (status, body) = app.call(Method::POST, &cancel_path(id), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{id}");
        assert_error(&body, "invalid_path");
    }
}

#[tokio::test]
async fn a_stopped_server_leaves_its_tasks_interrupted_for_the_next_start() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let mut gate = Gate::new();
    let running = gate.fill_slots(&app, TaskKind::Index).await;
    let queued = tasks
        .submit(spec(TaskKind::Index, "Index again"), gate.runner())
        .await
        .unwrap();

    // What `main` does when it receives a stop signal.
    app.state.begin_shutdown();
    app.state.shutdown().await;

    // The next start reads the same data directory.
    let next = AppState::new(Config::for_tests(app.dir.path()))
        .await
        .unwrap();
    let listed = next.tasks.list(100).await.unwrap();
    let mut seen: Vec<_> = listed
        .iter()
        .map(|task| (task.id, task.state, task.error.as_deref()))
        .collect();
    seen.sort_by_key(|(id, ..)| *id);
    let restart = Some("interrupted by restart");
    assert_eq!(
        seen,
        [
            (running[0], TaskState::Interrupted, restart),
            (running[1], TaskState::Interrupted, restart),
            (running[2], TaskState::Interrupted, restart),
            (queued, TaskState::Interrupted, restart)
        ]
    );
    assert!(
        listed
            .iter()
            .all(|task| task.finished_at.is_some() && task.eta_secs.is_none())
    );
    let never_ran = listed.iter().find(|task| task.id == queued).unwrap();
    assert_eq!(never_ran.started_at, None);

    next.begin_shutdown();
    next.shutdown().await;
}
