//! The index controls over HTTP: `POST /api/index/rescan` and `GET /api/index/status`, the task a
//! rescan becomes (kind, title, parameters, result, events), how requests share a task that is
//! still queued, the guard against a vanished mount, the recovery of file operations when a volume
//! comes back, and the scheduler that queues the startup and the periodic scans.
//!
//! The metadata extractor is a fake, so the tests do not need exiftool. The crate's Tokio has no
//! paused clock: the scheduler runs with a tiny "minute" and every wait polls against a deadline.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apm_core::dto::common::ConflictPolicy;
use apm_core::dto::{HeldRemovalsDto, IndexStatusDto, ServerEvent, TaskDto, TaskKind, TaskState};
use apm_core::paths::{TaskId, VolumeId};
use apm_server::fsops::{FsOpError, OpContext, Step};
use apm_server::indexer::{
    Embedded, ExtractOutcome, ExtractRequest, FakeExtractor, IndexJobs, IndexOrigin,
    MetadataExtractor, ScanRequest, SchedulerOptions,
};
use apm_server::tasks::{TaskError, TaskSpec};
use axum::body::Body;
use axum::http::request::Builder;
use axum::http::{Method, Request, StatusCode, header};
use common::browse::{error_code, folders, items, names};
use common::fsops::{Trap, crash_fx, loc, request as move_request};
use common::{TestApp, Tree};
use futures_util::future::BoxFuture;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore, oneshot};
use tower::ServiceExt;

/// How long a test waits for something that has to happen.
const PATIENCE: Duration = Duration::from_secs(30);

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

/// Polls `probe` until it returns a value, and panics after [`PATIENCE`].
async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = probe().await {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// --- the application under test ---------------------------------------------------------------

/// An extractor that answers every file with nothing embedded.
fn photos() -> Arc<dyn MetadataExtractor> {
    Arc::new(FakeExtractor::new(|_| Ok(Embedded::default())))
}

/// Gives the application index jobs that read metadata with `extractor` and run the scheduler on
/// the clock of `options`. The services are the application's own; only the jobs are replaced.
fn rewire(app: &mut TestApp, extractor: Arc<dyn MetadataExtractor>, options: SchedulerOptions) {
    let state = &app.state;
    let jobs = IndexJobs::with_options(
        state.indexer.with_extractor(extractor),
        state.tasks.clone(),
        state.fsops.clone(),
        state.shutdown.clone(),
        options,
    );
    app.state.index_jobs = jobs;
    app.router = apm_server::app::router(app.state.clone());
}

/// The application with one volume, `photos`.
struct Lab {
    app: TestApp,
    volume: VolumeId,
    tree: Tree,
}

impl Lab {
    async fn new() -> Self {
        Self::with(photos(), SchedulerOptions::default()).await
    }

    async fn with(extractor: Arc<dyn MetadataExtractor>, options: SchedulerOptions) -> Self {
        let mut app = TestApp::new().await;
        rewire(&mut app, extractor, options);
        let (volume, tree) = app.add_volume("photos").await;
        Self { app, volume, tree }
    }
}

/// A scope of a rescan request.
fn scope(volume: VolumeId, dir: &str, recursive: bool) -> Value {
    json!({"volume_id": volume, "dir": dir, "recursive": recursive})
}

/// Unmounts a share: its files go away and an empty mount point stays behind.
fn unmount(app: &TestApp, tree: &Tree) -> PathBuf {
    let away = app.dir.path().join("unmounted");
    fs::rename(tree.root(), &away).unwrap();
    fs::create_dir(tree.root()).unwrap();
    away
}

fn remount(tree: &Tree, away: PathBuf) {
    fs::remove_dir(tree.root()).unwrap();
    fs::rename(away, tree.root()).unwrap();
}

/// Takes a volume offline the way the registry notices: its directory is gone.
async fn take_offline(app: &TestApp, tree: &Tree, name: &str) -> PathBuf {
    let away = app.dir.path().join(format!("{name}.away"));
    fs::rename(tree.root(), &away).unwrap();
    app.state.volumes.refresh().await.unwrap();
    away
}

async fn set_rescan_interval(app: &TestApp, minutes: u32) {
    let mut settings = (*app.state.settings.get()).clone();
    settings.index.rescan_interval_minutes = minutes;
    app.state.settings.update(settings).await.unwrap();
}

// --- requests and what they leave behind -------------------------------------------------------

async fn try_rescan(app: &TestApp, body: Value) -> (StatusCode, Value) {
    app.call(Method::POST, "/api/index/rescan", Some(body))
        .await
}

/// A rescan that must be accepted: the task it answers with.
async fn rescan(app: &TestApp, body: Value) -> TaskDto {
    let (status, reply) = try_rescan(app, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
    serde_json::from_value(reply).unwrap()
}

/// The task as it ended.
async fn finish(app: &TestApp, id: TaskId) -> TaskDto {
    within(app.state.tasks.wait_finished(id)).await.unwrap()
}

/// A rescan that must end well: what it found.
async fn rescan_result(app: &TestApp, body: Value) -> Value {
    let task = rescan(app, body).await;
    let done = finish(app, task.id).await;
    assert_eq!(done.state, TaskState::Done, "{done:?}");
    result_of(app, task.id).await
}

async fn status(app: &TestApp) -> IndexStatusDto {
    let (code, body) = app.call(Method::GET, "/api/index/status", None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

/// Waits until no index task is queued or running.
async fn idle(app: &TestApp) {
    eventually("the index to be idle", || async move {
        status(app).await.active_task_id.is_none().then_some(())
    })
    .await;
}

/// The names of the files the index lists in `dir`; nothing for a folder that is not indexed.
async fn listing(app: &TestApp, volume: VolumeId, dir: &str) -> Vec<String> {
    let (code, body) = items(app, json!({"volume_id": volume, "dir": dir})).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    names(&body, "items")
}

/// Is `dir` a folder of the index? Browsing one that is not answers 404 `folder_not_found`.
async fn indexed_dir(app: &TestApp, volume: VolumeId, dir: &str) -> bool {
    let (code, body) = folders(app, json!({"volume_id": volume, "dir": dir})).await;
    match code {
        StatusCode::OK => true,
        StatusCode::NOT_FOUND => {
            assert_eq!(error_code(&body), "folder_not_found", "{body}");
            false
        }
        other => panic!("{other}: {body}"),
    }
}

fn parse_stored(stored: Option<String>, what: &str, id: TaskId) -> Value {
    serde_json::from_str(&stored.unwrap_or_else(|| panic!("task {id} has no {what}"))).unwrap()
}

/// The `IndexSummary` a finished task stored.
async fn result_of(app: &TestApp, id: TaskId) -> Value {
    let stored: Option<String> = sqlx::query_scalar("SELECT result_json FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    parse_stored(stored, "result", id)
}

/// The request a task was queued with.
async fn params_of(app: &TestApp, id: TaskId) -> Value {
    let stored: Option<String> = sqlx::query_scalar("SELECT params_json FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    parse_stored(stored, "parameters", id)
}

/// A counter of a stored result.
fn count(result: &Value, key: &str) -> u64 {
    result[key]
        .as_u64()
        .unwrap_or_else(|| panic!("no counter {key} in {result}"))
}

/// What asked for each index task so far, oldest first.
async fn origins(app: &TestApp) -> Vec<String> {
    let stored: Vec<Option<String>> =
        sqlx::query_scalar("SELECT params_json FROM tasks WHERE kind = 'index' ORDER BY id")
            .fetch_all(app.state.db.read())
            .await
            .unwrap();
    stored
        .into_iter()
        .map(|json| {
            let params: Value = serde_json::from_str(&json.expect("parameters")).unwrap();
            params["origin"].as_str().expect("an origin").to_owned()
        })
        .collect()
}

fn post(path: &str) -> Builder {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, "localhost")
}

/// Sends a request that is not built by `TestApp::call`: the status and the JSON body (`Null` when
/// it is not JSON).
async fn send_raw(app: &TestApp, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

// --- keeping the index lane busy ---------------------------------------------------------------

/// A task that is not one of the jobs and holds the index lane until it is released.
struct Blocker {
    id: TaskId,
    release: oneshot::Sender<()>,
}

impl Blocker {
    fn release(self) {
        self.release.send(()).ok();
    }
}

/// Occupies the index lane, so what is queued meanwhile stays queued.
async fn block_lane(app: &TestApp) -> Blocker {
    let (release, released) = oneshot::channel::<()>();
    let id = app
        .state
        .tasks
        .submit(
            TaskSpec::new(TaskKind::Index, "A task in the way"),
            move |ctx| async move {
                tokio::select! {
                    _ = released => Ok(json!({})),
                    () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                }
            },
        )
        .await
        .unwrap();
    eventually("the blocking task to run", || async move {
        let task = app.state.tasks.get(id).await.unwrap();
        (task.state == TaskState::Running).then_some(())
    })
    .await;
    Blocker { id, release }
}

/// An extractor that waits at its first file until the test opens the gate, for good.
struct Gate {
    reached: Notify,
    open: Semaphore,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reached: Notify::new(),
            open: Semaphore::new(0),
        })
    }

    fn open(&self) {
        self.open.add_permits(1);
    }
}

impl MetadataExtractor for Gate {
    fn probe(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn extract(&self, requests: Vec<ExtractRequest>) -> BoxFuture<'_, Vec<ExtractOutcome>> {
        Box::pin(async move {
            self.reached.notify_one();
            drop(self.open.acquire().await);
            requests.iter().map(|_| Ok(Embedded::default())).collect()
        })
    }
}

// --- the event stream --------------------------------------------------------------------------

/// `GET /api/events`, read one event at a time.
struct Events {
    body: Body,
    buffer: String,
}

impl Events {
    async fn open(app: &TestApp) -> Self {
        let response = app.send(Method::GET, "/api/events", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        Self {
            body: response.into_body(),
            buffer: String::new(),
        }
    }

    /// The next event; the greeting and the keep-alive comments are skipped.
    async fn next(&mut self) -> ServerEvent {
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let message: String = self.buffer.drain(..end + 2).collect();
                if let Some(data) = message.trim_end().strip_prefix("data:") {
                    return serde_json::from_str(data.trim_start())
                        .unwrap_or_else(|err| panic!("{err}: {data}"));
                }
            }
            let frame = tokio::time::timeout(PATIENCE, self.body.frame())
                .await
                .expect("the event stream stalled")
                .expect("the event stream ended")
                .expect("the body is readable");
            if let Ok(data) = frame.into_data() {
                self.buffer
                    .push_str(std::str::from_utf8(&data).expect("the stream is UTF-8"));
            }
        }
    }
}

// --- the endpoints -----------------------------------------------------------------------------

#[tokio::test]
async fn an_idle_index_has_nothing_to_report() {
    let app = TestApp::new().await;

    let (code, body) = app.call(Method::GET, "/api/index/status", None).await;

    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        body,
        json!({"held_removals": [], "active_task_id": null, "warnings": [], "watcher": app.state.watcher.status()})
    );
    let (_, tasks) = app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(tasks, json!([]));
}

#[tokio::test]
async fn a_rescan_is_an_index_task_that_indexes_every_volume() {
    let lab = Lab::new().await;
    for name in ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"] {
        lab.tree.write(name, b"x");
    }

    let queued = rescan(&lab.app, json!({})).await;

    assert_eq!(queued.kind, TaskKind::Index);
    assert_eq!(queued.title, "Indexing");
    assert!(
        matches!(queued.state, TaskState::Queued | TaskState::Running),
        "{queued:?}"
    );
    let done = finish(&lab.app, queued.id).await;
    assert_eq!(done.state, TaskState::Done, "{done:?}");
    assert_eq!(done.title, "Indexing");
    assert_eq!(done.error, None);
    let result = result_of(&lab.app, queued.id).await;
    assert_eq!(
        [
            count(&result, "added"),
            count(&result, "extracted"),
            count(&result, "removed"),
            count(&result, "held")
        ],
        [3, 3, 0, 0],
        "{result}"
    );
    assert_eq!(
        params_of(&lab.app, queued.id).await,
        json!({"origin": "user", "targets": [], "force": false, "confirm_removals": false})
    );
    assert_eq!(
        listing(&lab.app, lab.volume, "").await,
        ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"]
    );
    let (_, tasks) = lab.app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(tasks[0]["id"], queued.id, "{tasks}");
    idle(&lab.app).await;
}

#[tokio::test]
async fn a_rescan_of_folders_indexes_only_those_folders() {
    let lab = Lab::new().await;
    lab.tree.write("a/IMG_1.jpg", b"x");
    lab.tree.write("a/sub/IMG_2.jpg", b"x");
    lab.tree.write("b/IMG_3.jpg", b"x");

    let first = rescan(
        &lab.app,
        json!({"targets": [scope(lab.volume, "a", false)]}),
    )
    .await;
    assert_eq!(first.title, "Indexing 1 folder");
    assert_eq!(finish(&lab.app, first.id).await.state, TaskState::Done);

    assert_eq!(listing(&lab.app, lab.volume, "a").await, ["IMG_1.jpg"]);
    assert!(
        !indexed_dir(&lab.app, lab.volume, "b").await,
        "a folder nobody asked for is not indexed"
    );
    assert!(
        listing(&lab.app, lab.volume, "a/sub").await.is_empty(),
        "a folder scan does not go below the folder"
    );
    assert_eq!(
        params_of(&lab.app, first.id).await["targets"],
        json!([{"volume_id": lab.volume, "dir": "a", "recursive": false}])
    );

    let below = rescan_result(&lab.app, json!({"targets": [scope(lab.volume, "a", true)]})).await;
    assert_eq!((count(&below, "added"), count(&below, "removed")), (1, 0));
    assert_eq!(listing(&lab.app, lab.volume, "a/sub").await, ["IMG_2.jpg"]);
    assert!(!indexed_dir(&lab.app, lab.volume, "b").await);
}

#[tokio::test]
async fn a_forced_rescan_reprocesses_files_that_did_not_change() {
    let lab = Lab::new().await;
    lab.tree.populate(1, 4);
    let first = rescan_result(&lab.app, json!({})).await;
    assert_eq!(count(&first, "added"), 4);
    let plain = rescan_result(&lab.app, json!({})).await;
    assert_eq!(
        [
            count(&plain, "added"),
            count(&plain, "changed"),
            count(&plain, "extracted")
        ],
        [0, 0, 0],
        "an untouched library needs no work: {plain}"
    );

    let forced = rescan(&lab.app, json!({"force": true})).await;

    assert_eq!(forced.title, "Re-indexing all assets");
    assert_eq!(finish(&lab.app, forced.id).await.state, TaskState::Done);
    let result = result_of(&lab.app, forced.id).await;
    assert_eq!(
        [
            count(&result, "added"),
            count(&result, "changed"),
            count(&result, "extracted")
        ],
        [0, 4, 4],
        "{result}"
    );
    assert_eq!(params_of(&lab.app, forced.id).await["force"], json!(true));
}

#[tokio::test]
async fn a_bad_rescan_request_is_refused_and_queues_nothing() {
    let lab = Lab::new().await;
    let (offline, offline_tree) = lab.app.add_volume("offline").await;
    take_offline(&lab.app, &offline_tree, "offline").await;
    let refused = [
        (
            "an unknown volume",
            json!({"targets": [scope(4242, "", true)]}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "an offline volume",
            json!({"targets": [scope(offline, "", true)]}),
            StatusCode::CONFLICT,
            "volume_offline",
        ),
        (
            "a path that climbs out of the volume",
            json!({"targets": [scope(lab.volume, "../x", true)]}),
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
        (
            "an absolute path",
            json!({"targets": [scope(lab.volume, "/etc", true)]}),
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
        (
            "a folder without a volume",
            json!({"targets": [{"volume_id": null, "dir": "2024", "recursive": true}]}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "targets that are not a list",
            json!({"targets": "all"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_json",
        ),
        (
            "a flag that is not a boolean",
            json!({"force": "yes"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_json",
        ),
    ];

    for (what, body, status, code) in refused {
        let (got, reply) = try_rescan(&lab.app, body).await;
        assert_eq!((got, error_code(&reply)), (status, code), "{what}: {reply}");
    }

    let empty = post("/api/index/rescan")
        .header("x-apm-request", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();
    let (got, reply) = send_raw(&lab.app, empty).await;
    assert_eq!(got, StatusCode::BAD_REQUEST, "{reply}");
    assert_eq!(error_code(&reply), "invalid_json");

    let unmarked = post("/api/index/rescan")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (got, reply) = send_raw(&lab.app, unmarked).await;
    assert_eq!(got, StatusCode::FORBIDDEN, "{reply}");
    assert_eq!(error_code(&reply), "csrf_rejected");

    let (got, reply) = lab.app.call(Method::GET, "/api/index/rescan", None).await;
    assert_eq!(got, StatusCode::METHOD_NOT_ALLOWED, "{reply}");
    assert_eq!(error_code(&reply), "method_not_allowed");
    let (got, reply) = lab.app.call(Method::POST, "/api/index/status", None).await;
    assert_eq!(got, StatusCode::METHOD_NOT_ALLOWED, "{reply}");

    let (_, tasks) = lab.app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(
        tasks,
        json!([]),
        "a refused request must not queue anything"
    );
}

#[tokio::test]
async fn a_scope_without_a_volume_stands_for_the_online_volumes() {
    let lab = Lab::new().await;
    let (second, second_tree) = lab.app.add_volume("second").await;
    let everywhere = json!({"targets": [{"volume_id": null, "dir": "", "recursive": true}]});

    let both = rescan(&lab.app, everywhere.clone()).await;
    assert_eq!(finish(&lab.app, both.id).await.state, TaskState::Done);
    assert_eq!(
        params_of(&lab.app, both.id).await["targets"],
        json!([
            {"volume_id": lab.volume, "dir": "", "recursive": true},
            {"volume_id": second, "dir": "", "recursive": true},
        ])
    );

    let away = take_offline(&lab.app, &second_tree, "second").await;
    let one = rescan(&lab.app, everywhere.clone()).await;
    assert_eq!(finish(&lab.app, one.id).await.state, TaskState::Done);
    assert_eq!(
        params_of(&lab.app, one.id).await["targets"],
        json!([{"volume_id": lab.volume, "dir": "", "recursive": true}]),
        "an offline volume is skipped"
    );
    assert!(away.is_dir());

    take_offline(&lab.app, &lab.tree, "photos").await;
    let (code, reply) = try_rescan(&lab.app, everywhere).await;
    assert_eq!(code, StatusCode::CONFLICT, "{reply}");
    assert_eq!(error_code(&reply), "volume_offline");
}

// --- sharing a task ----------------------------------------------------------------------------

#[tokio::test]
async fn requests_for_folders_share_the_task_that_is_still_queued() {
    let lab = Lab::new().await;
    lab.tree.write("a/IMG_1.jpg", b"x");
    lab.tree.write("b/IMG_2.jpg", b"x");
    lab.tree.write("c/IMG_3.jpg", b"x");
    let blocker = block_lane(&lab.app).await;

    let a = rescan(
        &lab.app,
        json!({"targets": [scope(lab.volume, "a", false)]}),
    )
    .await;
    let b = rescan(
        &lab.app,
        json!({"targets": [scope(lab.volume, "b", false)]}),
    )
    .await;
    let again = rescan(
        &lab.app,
        json!({"targets": [scope(lab.volume, "a", false)]}),
    )
    .await;

    assert_eq!(a.state, TaskState::Queued, "{a:?}");
    assert_eq!(
        (b.id, again.id),
        (a.id, a.id),
        "one task covers the three requests"
    );
    assert_eq!(status(&lab.app).await.active_task_id, Some(blocker.id));

    blocker.release();
    let done = finish(&lab.app, a.id).await;
    assert_eq!(done.state, TaskState::Done, "{done:?}");
    assert_eq!(done.title, "Indexing 2 folders");
    assert_eq!(listing(&lab.app, lab.volume, "a").await, ["IMG_1.jpg"]);
    assert_eq!(listing(&lab.app, lab.volume, "b").await, ["IMG_2.jpg"]);
    assert!(
        !indexed_dir(&lab.app, lab.volume, "c").await,
        "what nobody asked for stays unindexed"
    );
    idle(&lab.app).await;
}

#[tokio::test]
async fn a_forced_rescan_never_joins_a_plain_one() {
    let lab = Lab::new().await;
    lab.tree.write("a/IMG_1.jpg", b"x");
    let blocker = block_lane(&lab.app).await;

    let plain = rescan(&lab.app, json!({})).await;
    let forced = rescan(&lab.app, json!({"force": true})).await;
    let forced_again = rescan(&lab.app, json!({"force": true})).await;
    let folder = rescan(&lab.app, json!({"targets": [scope(lab.volume, "a", true)]})).await;

    assert_ne!(forced.id, plain.id);
    assert_eq!(forced_again.id, forced.id, "a double click starts one job");
    assert_eq!(
        folder.id, plain.id,
        "a plain request skips the forced task and joins the plain one"
    );
    blocker.release();
    let plain_done = finish(&lab.app, plain.id).await;
    let forced_done = finish(&lab.app, forced.id).await;
    assert_eq!(
        (plain_done.state, plain_done.title.as_str()),
        (TaskState::Done, "Indexing")
    );
    assert_eq!(
        (forced_done.state, forced_done.title.as_str()),
        (TaskState::Done, "Re-indexing all assets")
    );
}

// --- events ------------------------------------------------------------------------------------

#[tokio::test]
async fn a_rescan_reports_its_progress_and_the_folders_it_changed_as_events() {
    let lab = Lab::new().await;
    lab.tree.write("a/IMG_1.jpg", b"x");
    lab.tree.write("b/IMG_2.jpg", b"x");
    let mut events = Events::open(&lab.app).await;

    let task = rescan(&lab.app, json!({})).await;

    let mut states: Vec<TaskState> = Vec::new();
    let mut running_titles = Vec::new();
    let mut ended_as = None;
    let mut dirs = BTreeSet::new();
    // The folders are reported a moment after the task ends: they are coalesced for a while.
    while ended_as.is_none() || !(dirs.contains("a") && dirs.contains("b")) {
        match events.next().await {
            ServerEvent::Task { task: seen } if seen.id == task.id => {
                if states.last() != Some(&seen.state) {
                    states.push(seen.state);
                }
                if seen.state == TaskState::Running {
                    running_titles.push(seen.title.clone());
                }
                if !matches!(seen.state, TaskState::Queued | TaskState::Running) {
                    ended_as = Some(seen);
                }
            }
            ServerEvent::FsChange {
                volume_id,
                dirs: changed,
            } if volume_id == lab.volume => dirs.extend(changed),
            _ => {}
        }
    }

    assert_eq!(
        states,
        [TaskState::Queued, TaskState::Running, TaskState::Done]
    );
    assert!(
        running_titles
            .iter()
            .all(|title| title.starts_with("Indexing")),
        "{running_titles:?}"
    );
    let ended = ended_as.unwrap();
    assert_eq!(
        (ended.kind, ended.title.as_str()),
        (TaskKind::Index, "Indexing")
    );
}

// --- the guard against a vanished mount --------------------------------------------------------

#[tokio::test]
async fn a_vanished_mount_is_held_until_the_user_confirms_the_removals() {
    let lab = Lab::new().await;
    lab.tree.populate(2, 30);
    let (healthy, healthy_tree) = lab.app.add_volume("archive").await;
    healthy_tree.write("keep/IMG_1.jpg", b"x");
    let first = rescan_result(&lab.app, json!({})).await;
    assert_eq!(count(&first, "added"), 61);
    assert_eq!(
        status(&lab.app).await,
        IndexStatusDto {
            watcher: Some(lab.app.state.watcher.status()),
            ..IndexStatusDto::default()
        }
    );
    let away = unmount(&lab.app, &lab.tree);

    // 60 files gone at once look like a share that is not mounted: the entries are kept.
    let held = rescan_result(&lab.app, json!({})).await;
    assert_eq!((count(&held, "removed"), count(&held, "held")), (0, 60));
    assert_eq!(
        status(&lab.app).await.held_removals,
        [HeldRemovalsDto {
            volume_id: lab.volume,
            volume_name: "photos".to_owned(),
            files: 60
        }],
        "only the volume that lost its files is listed"
    );
    assert_eq!(
        listing(&lab.app, lab.volume, "d000").await.len(),
        30,
        "the entries are still browsable"
    );
    assert_eq!(listing(&lab.app, healthy, "keep").await, ["IMG_1.jpg"]);

    // Neither forcing a re-index nor a request that did not come from a person confirms it.
    let forced = rescan_result(&lab.app, json!({"force": true})).await;
    assert_eq!((count(&forced, "removed"), count(&forced, "held")), (0, 60));
    let watcher = lab
        .app
        .state
        .index_jobs
        .submit(
            ScanRequest {
                confirm_removals: true,
                ..ScanRequest::default()
            },
            IndexOrigin::Watcher,
        )
        .await
        .unwrap();
    assert_eq!(
        finish(&lab.app, watcher.task_id).await.state,
        TaskState::Done
    );
    let result = result_of(&lab.app, watcher.task_id).await;
    assert_eq!((count(&result, "removed"), count(&result, "held")), (0, 60));
    assert_eq!(status(&lab.app).await.held_removals.len(), 1);

    // The user confirms: the entries go, no file is touched.
    let confirmed = rescan_result(&lab.app, json!({"confirm_removals": true})).await;
    assert_eq!(
        (count(&confirmed, "removed"), count(&confirmed, "held")),
        (60, 0)
    );
    assert_eq!(
        status(&lab.app).await,
        IndexStatusDto {
            watcher: Some(lab.app.state.watcher.status()),
            ..IndexStatusDto::default()
        }
    );
    assert!(listing(&lab.app, lab.volume, "d000").await.is_empty());
    assert_eq!(listing(&lab.app, healthy, "keep").await, ["IMG_1.jpg"]);
    assert!(away.join("d000").is_dir());
}

#[tokio::test]
async fn a_returning_mount_voids_the_held_removals() {
    let lab = Lab::new().await;
    lab.tree.populate(2, 30);
    rescan_result(&lab.app, json!({})).await;
    let away = unmount(&lab.app, &lab.tree);
    let held = rescan_result(&lab.app, json!({})).await;
    assert_eq!(count(&held, "held"), 60);
    assert_eq!(status(&lab.app).await.held_removals.len(), 1);

    remount(&lab.tree, away);
    let back = rescan_result(&lab.app, json!({"targets": [scope(lab.volume, "", true)]})).await;

    assert_eq!(
        [
            count(&back, "held"),
            count(&back, "removed"),
            count(&back, "added")
        ],
        [0, 0, 0],
        "{back}"
    );
    assert_eq!(
        status(&lab.app).await,
        IndexStatusDto {
            watcher: Some(lab.app.state.watcher.status()),
            ..IndexStatusDto::default()
        }
    );
    assert_eq!(listing(&lab.app, lab.volume, "d001").await.len(), 30);
}

// --- cancelling and recovering -----------------------------------------------------------------

#[tokio::test]
async fn a_running_rescan_can_be_cancelled_and_the_next_one_finishes_the_work() {
    let gate = Gate::new();
    let lab = Lab::with(gate.clone(), SchedulerOptions::default()).await;
    lab.tree.populate(1, 3);

    let task = rescan(&lab.app, json!({})).await;
    within(gate.reached.notified()).await;
    assert_eq!(status(&lab.app).await.active_task_id, Some(task.id));

    let (code, _) = lab
        .app
        .call(
            Method::POST,
            &format!("/api/tasks/{}/cancel", task.id),
            None,
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED);
    gate.open();
    let ended = finish(&lab.app, task.id).await;
    assert_eq!(ended.state, TaskState::Cancelled, "{ended:?}");
    idle(&lab.app).await;

    let again = rescan_result(&lab.app, json!({})).await;
    assert_eq!(
        (count(&again, "added"), count(&again, "removed")),
        (0, 0),
        "the files the cancelled run found are in the index"
    );
    assert_eq!(
        listing(&lab.app, lab.volume, "d000").await,
        ["IMG_0000.jpg", "IMG_0001.jpg", "IMG_0002.jpg"]
    );
}

#[tokio::test]
async fn a_volume_that_comes_back_gets_its_file_operations_recovered_before_the_scan() {
    let content = b"the bytes of one photograph";
    let trap = Trap::default();
    let mut fx = crash_fx(&trap, false).await;
    rewire(&mut fx.app, photos(), SchedulerOptions::default());
    fx.ta.write("s/IMG_1.jpg", content);
    fx.scan().await;
    let row = fx.row_id(fx.a, "s/IMG_1.jpg").await;

    // The process dies after the file moved and before the index heard of it.
    trap.arm(Step::AfterFinalRename, 1);
    let moved = fx
        .ops
        .move_file(
            move_request(
                loc(fx.a, "s/IMG_1.jpg"),
                loc(fx.a, "d/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &OpContext::detached(),
        )
        .await;
    assert_eq!(moved, Err(FsOpError::Aborted));
    assert_eq!(fx.pending_rows().await, 1);

    // The share goes away and comes back; nothing re-reads the volumes before the rescan.
    let away = take_offline(&fx.app, &fx.ta, "a").await;
    fs::rename(&away, fx.ta.root()).unwrap();

    let result = rescan_result(&fx.app, json!({})).await;

    assert_eq!(
        (count(&result, "added"), count(&result, "removed")),
        (0, 0),
        "the move was completed, not seen as a deletion and a new file: {result}"
    );
    assert_eq!(fx.pending_rows().await, 0);
    let moved_row = fx
        .present(fx.a, "d/IMG_1.jpg", content, "after the rescan")
        .await;
    assert_eq!(moved_row, row, "the file kept its index row");
    fx.gone(fx.a, "s/IMG_1.jpg", "after the rescan").await;
    fx.assert_in_sync().await;
}

// --- the scheduler -----------------------------------------------------------------------------

#[tokio::test]
async fn the_scheduler_starts_once_and_stops_with_the_server() {
    let app = TestApp::new().await;
    let jobs = app.state.index_jobs.clone();

    let handle = jobs.start_background();
    let second = jobs.start_background();

    assert!(second.is_none(), "a second start does nothing");
    let handle = handle.expect("the first start runs the scheduler");
    app.state.begin_shutdown();
    within(handle).await.unwrap();
}

#[tokio::test]
async fn the_scheduler_scans_at_startup_then_periodically() {
    let options = SchedulerOptions {
        startup_delay: Duration::from_millis(50),
        minute: Duration::from_millis(100),
    };
    let lab = Lab::with(photos(), options).await;
    set_rescan_interval(&lab.app, 0).await;
    lab.tree.write("one/IMG_1.jpg", b"x");
    let jobs = lab.app.state.index_jobs.clone();

    let handle = jobs.start_background().expect("the scheduler starts");

    eventually("the startup scan to index the library", || async {
        (listing(&lab.app, lab.volume, "one").await.len() == 1).then_some(())
    })
    .await;
    idle(&lab.app).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        origins(&lab.app).await,
        ["startup"],
        "an interval of 0 means never"
    );

    lab.tree.write("two/IMG_2.jpg", b"x");
    set_rescan_interval(&lab.app, 5).await;
    eventually("a periodic scan to index the new file", || async {
        (listing(&lab.app, lab.volume, "two").await.len() == 1).then_some(())
    })
    .await;

    let origins = origins(&lab.app).await;
    assert_eq!(origins[0], "startup");
    assert!(origins.len() >= 2, "{origins:?}");
    assert!(
        origins[1..].iter().all(|origin| origin == "periodic"),
        "{origins:?}"
    );
    lab.app.state.begin_shutdown();
    within(handle).await.unwrap();
}
