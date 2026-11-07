//! `GET /api/events`: the wire format of the stream, what it carries and when it ends.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::time::Duration;

use apm_core::dto::{ServerEvent, TaskDto, TaskKind, TaskState};
use apm_core::paths::TaskId;
use apm_server::api::events;
use apm_server::security::{self, SecurityOptions};
use apm_server::state::AppState;
use apm_server::tasks::{TaskError, TaskSpec};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use common::TestApp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// How long a test waits for something that has to happen.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long a test watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(300);

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

/// One message of the stream as a browser's `EventSource` reads it: `:` lines are comments, the
/// others are `name:value` fields with one optional space after the colon.
#[derive(Debug, Default, PartialEq)]
struct Message {
    comments: Vec<String>,
    fields: Vec<(String, String)>,
}

impl Message {
    fn parse(raw: &str) -> Self {
        let mut message = Self::default();
        for line in raw.split('\n') {
            if let Some(comment) = line.strip_prefix(':') {
                message
                    .comments
                    .push(comment.strip_prefix(' ').unwrap_or(comment).to_owned());
            } else {
                let (name, value) = line.split_once(':').unwrap_or((line, ""));
                message.fields.push((
                    name.to_owned(),
                    value.strip_prefix(' ').unwrap_or(value).to_owned(),
                ));
            }
        }
        message
    }

    fn field(name: &str, value: &str) -> Self {
        Self {
            comments: Vec::new(),
            fields: vec![(name.to_owned(), value.to_owned())],
        }
    }

    fn comment(text: &str) -> Self {
        Self {
            comments: vec![text.to_owned()],
            fields: Vec::new(),
        }
    }

    fn is_comment(&self) -> bool {
        !self.comments.is_empty() && self.fields.is_empty()
    }

    /// The JSON of a message that is exactly one `data` line: no `event`, no `id`, no second line.
    fn json(&self) -> Value {
        assert!(self.comments.is_empty(), "{self:?}");
        let [(name, data)] = self.fields.as_slice() else {
            panic!("not exactly one field: {self:?}");
        };
        assert_eq!(name, "data", "{self:?}");
        serde_json::from_str(data).unwrap_or_else(|err| panic!("{err}: {data}"))
    }

    fn event(&self) -> ServerEvent {
        serde_json::from_value(self.json()).unwrap_or_else(|err| panic!("{err}: {self:?}"))
    }
}

/// An open event stream, read one message at a time.
struct Stream {
    body: Body,
    buffer: String,
}

impl Stream {
    fn new(response: Response<Body>) -> Self {
        assert_eq!(response.status(), StatusCode::OK);
        Self {
            body: response.into_body(),
            buffer: String::new(),
        }
    }

    /// Opens the stream on the router of the app, without reading anything.
    async fn open(app: &TestApp) -> Self {
        Self::new(app.send(Method::GET, "/api/events", None).await)
    }

    /// Opens the stream and reads the greeting that starts it.
    async fn connect(app: &TestApp) -> Self {
        let mut stream = Self::open(app).await;
        stream.expect_greeting().await;
        stream
    }

    async fn expect_greeting(&mut self) {
        let greeting = self.next_message().await;
        assert_eq!(greeting, Some(Message::field("retry", "3000")));
    }

    /// The next message, `None` once the stream is over.
    async fn next_message(&mut self) -> Option<Message> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let raw: String = self.buffer.drain(..end + 2).collect();
                return Some(Message::parse(&raw[..end]));
            }
            let frame = tokio::time::timeout(PATIENCE, self.body.frame())
                .await
                .unwrap_or_else(|_| panic!("the stream stalled after {:?}", self.buffer));
            let Some(frame) = frame else {
                assert!(
                    self.buffer.is_empty(),
                    "the stream ended inside a message: {:?}",
                    self.buffer
                );
                return None;
            };
            if let Ok(data) = frame.expect("the body is readable").into_data() {
                self.buffer
                    .push_str(std::str::from_utf8(&data).expect("the stream is UTF-8"));
            }
        }
    }

    /// The first `task` event about `id` that `accept` likes; everything else is skipped.
    async fn wait_for_task(&mut self, id: TaskId, accept: impl Fn(&TaskDto) -> bool) -> TaskDto {
        loop {
            let message = self.next_message().await.expect("the stream stays open");
            if message.is_comment() {
                continue;
            }
            if let ServerEvent::Task { task } = message.event()
                && task.id == id
                && accept(&task)
            {
                return task;
            }
        }
    }

    /// Reads until the stream is over and returns what was left in it.
    async fn read_to_the_end(&mut self) -> Vec<Message> {
        let mut rest = Vec::new();
        while let Some(message) = self.next_message().await {
            rest.push(message);
            assert!(rest.len() < 100, "the stream never ends: {rest:?}");
        }
        rest
    }

    /// Fails if a message arrives, or the stream ends, within [`QUIET`].
    async fn assert_quiet(&mut self) {
        if let Ok(message) = tokio::time::timeout(QUIET, self.next_message()).await {
            panic!("expected silence, got {message:?}");
        }
    }
}

/// Submits a task that returns at once.
async fn submit_instant(app: &TestApp) -> TaskId {
    app.state
        .tasks
        .submit(
            TaskSpec::new(TaskKind::FsMove, "Move 2 files"),
            |_ctx| async { Ok(json!({})) },
        )
        .await
        .unwrap()
}

async fn list_tasks(app: &TestApp) -> Vec<TaskDto> {
    let (status, body) = app.call(Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_value(body).unwrap()
}

/// The events route alone, without the rest of the application.
fn events_router(app: &TestApp, routes: Router<AppState>) -> Router {
    Router::new()
        .nest("/api", routes)
        .with_state(app.state.clone())
}

fn get_events() -> Request<Body> {
    Request::builder()
        .uri("/api/events")
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn the_stream_is_an_uncached_event_stream_that_proxies_do_not_buffer() {
    let app = TestApp::new().await;
    let response = app.send(Method::GET, "/api/events", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    assert_eq!(headers["x-accel-buffering"], "no");
}

#[tokio::test]
async fn the_stream_starts_with_the_delay_before_a_browser_reconnects() {
    let app = TestApp::new().await;
    let mut stream = Stream::open(&app).await;
    let greeting = stream.next_message().await.unwrap();
    assert_eq!(greeting, Message::field("retry", "3000"));
}

#[tokio::test]
async fn a_task_is_followed_from_queued_to_done_as_single_data_lines() {
    let app = TestApp::new().await;
    let mut stream = Stream::connect(&app).await;
    let id = submit_instant(&app).await;

    let first = stream.next_message().await.unwrap();
    let json = first.json();
    assert_eq!(json["type"], "task");
    assert_eq!(json["task"]["id"], id);
    assert_eq!(json["task"]["kind"], "fs_move");
    assert_eq!(json["task"]["title"], "Move 2 files");
    assert_eq!(json["task"]["state"], "queued");

    let mut states = vec![TaskState::Queued];
    while states.last() != Some(&TaskState::Done) {
        let message = stream.next_message().await.expect("the stream stays open");
        let ServerEvent::Task { task } = message.event() else {
            panic!("expected a task event: {message:?}");
        };
        assert_eq!(task.id, id);
        states.push(task.state);
    }
    states.dedup();
    assert_eq!(
        states,
        [TaskState::Queued, TaskState::Running, TaskState::Done]
    );
}

#[tokio::test]
async fn a_failed_and_a_cancelled_task_end_with_their_own_event() {
    let app = TestApp::new().await;
    let mut stream = Stream::connect(&app).await;
    let tasks = &app.state.tasks;

    let failing = tasks
        .submit(
            TaskSpec::new(TaskKind::ImmichSync, "Sync Immich"),
            |_ctx| async { Err(TaskError::failed("the server said no")) },
        )
        .await
        .unwrap();
    let failed = stream
        .wait_for_task(failing, |task| task.state == TaskState::Failed)
        .await;
    assert_eq!(failed.error.as_deref(), Some("the server said no"));
    assert!(failed.finished_at.is_some());

    let (started, wait_started) = oneshot::channel::<()>();
    let running = tasks
        .submit(
            TaskSpec::new(TaskKind::Index, "Index photos"),
            |ctx| async move {
                started.send(()).ok();
                ctx.cancel.cancelled().await;
                Err(TaskError::Cancelled)
            },
        )
        .await
        .unwrap();
    within(wait_started).await.unwrap();
    let (status, _) = app
        .call(Method::POST, &format!("/api/tasks/{running}/cancel"), None)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let cancelled = stream
        .wait_for_task(running, |task| task.state == TaskState::Cancelled)
        .await;
    assert!(cancelled.finished_at.is_some());
}

#[tokio::test]
async fn progress_on_the_stream_agrees_with_the_task_list() {
    let app = TestApp::new().await;
    let mut stream = Stream::connect(&app).await;
    let (counted, wait_counted) = oneshot::channel::<()>();
    let (finish, wait_finish) = oneshot::channel::<()>();
    let id = app
        .state
        .tasks
        .submit(
            TaskSpec::new(TaskKind::FsMove, "Move 10 files"),
            |ctx| async move {
                ctx.progress.set_totals(Some(10), Some(1_000));
                ctx.progress.add(4, 400);
                counted.send(()).ok();
                wait_finish.await.ok();
                ctx.progress.add(6, 600);
                Ok(json!({}))
            },
        )
        .await
        .unwrap();
    within(wait_counted).await.unwrap();

    let running = stream.wait_for_task(id, |task| task.items_done == 4).await;
    assert_eq!(running.state, TaskState::Running);
    assert_eq!(running.items_total, Some(10));
    assert_eq!(running.bytes_done, 400);
    assert_eq!(running.bytes_total, Some(1_000));
    let listed = list_tasks(&app).await;
    let listed = listed.iter().find(|task| task.id == id).unwrap();
    assert_eq!(listed.state, TaskState::Running);
    assert_eq!(listed.items_done, running.items_done);
    assert_eq!(listed.bytes_done, running.bytes_done);

    finish.send(()).unwrap();
    let done = stream
        .wait_for_task(id, |task| task.state == TaskState::Done)
        .await;
    assert_eq!(done.items_done, 10);
    assert_eq!(done.items_total, Some(10));
    assert_eq!(done.bytes_done, 1_000);
    assert_eq!(done.bytes_total, Some(1_000));
    let listed = list_tasks(&app).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].state, TaskState::Done);
    assert_eq!(listed[0].items_done, 10);
    assert_eq!(listed[0].bytes_done, 1_000);
}

#[tokio::test]
async fn directory_changes_arrive_as_one_fs_change_event_per_volume() {
    let app = TestApp::new().await;
    let mut stream = Stream::connect(&app).await;
    let tasks = &app.state.tasks;
    tasks.publish_fs_change(7, ["b".to_owned(), "a".to_owned()]);
    tasks.publish_fs_change(7, ["a".to_owned(), "c/d".to_owned()]);
    tasks.publish_fs_change(9, ["x".to_owned()]);

    let first = stream.next_message().await.unwrap();
    assert_eq!(
        first.json(),
        json!({ "type": "fs_change", "volume_id": 7, "dirs": ["a", "b", "c/d"] })
    );
    let second = stream.next_message().await.unwrap();
    assert_eq!(
        second.json(),
        json!({ "type": "fs_change", "volume_id": 9, "dirs": ["x"] })
    );
    stream.assert_quiet().await;
}

#[tokio::test]
async fn every_client_gets_every_event() {
    let app = TestApp::new().await;
    let mut streams = vec![
        Stream::connect(&app).await,
        Stream::connect(&app).await,
        Stream::connect(&app).await,
    ];
    let id = submit_instant(&app).await;
    for stream in &mut streams {
        let done = stream
            .wait_for_task(id, |task| task.state == TaskState::Done)
            .await;
        assert_eq!(done.title, "Move 2 files");
    }
}

#[tokio::test]
async fn a_client_that_connects_late_gets_no_history() {
    let app = TestApp::new().await;
    let tasks = &app.state.tasks;
    let old = submit_instant(&app).await;
    within(tasks.wait_finished(old)).await.unwrap();
    tasks.publish(ServerEvent::FsChange {
        volume_id: 1,
        dirs: vec!["old".to_owned()],
    });

    let mut stream = Stream::connect(&app).await;
    stream.assert_quiet().await;

    // It is not left out of what happens from now on.
    let id = submit_instant(&app).await;
    let first = stream.wait_for_task(id, |_| true).await;
    assert_eq!(first.state, TaskState::Queued);
}

#[tokio::test]
async fn a_quiet_stream_carries_keep_alive_comments_between_the_events() {
    let app = TestApp::new().await;
    let router = events_router(&app, events::routes_with(Duration::from_millis(50)));
    let mut stream = Stream::new(router.oneshot(get_events()).await.unwrap());
    stream.expect_greeting().await;

    for _ in 0..3 {
        let message = stream.next_message().await.unwrap();
        assert_eq!(message, Message::comment("keep-alive"));
    }

    let id = submit_instant(&app).await;
    let done = stream
        .wait_for_task(id, |task| task.state == TaskState::Done)
        .await;
    assert_eq!(done.id, id);
    let message = stream.next_message().await.unwrap();
    assert!(message.is_comment(), "{message:?}");
}

#[tokio::test]
async fn a_client_that_falls_too_far_behind_is_cut_off_so_that_it_reconnects() {
    let app = TestApp::new().await;
    let mut slow = Stream::open(&app).await;
    for n in 0..1_100 {
        app.state.tasks.publish(ServerEvent::FsChange {
            volume_id: 1,
            dirs: vec![format!("dir-{n}")],
        });
    }

    slow.expect_greeting().await;
    assert_eq!(slow.next_message().await, None, "the stream ends");

    // Reconnecting works, and the client reads `GET /api/tasks` again to catch up.
    let mut again = Stream::connect(&app).await;
    let id = submit_instant(&app).await;
    let queued = again.wait_for_task(id, |_| true).await;
    assert_eq!(queued.state, TaskState::Queued);
}

#[tokio::test]
async fn beginning_the_shutdown_ends_every_open_stream() {
    let app = TestApp::new().await;
    let mut first = Stream::connect(&app).await;
    let mut second = Stream::connect(&app).await;

    app.state.begin_shutdown();

    assert_eq!(first.next_message().await, None);
    assert_eq!(second.next_message().await, None);
}

#[tokio::test]
async fn a_stream_opened_while_the_server_stops_ends_at_once() {
    let app = TestApp::new().await;
    app.state.begin_shutdown();

    let mut stream = Stream::open(&app).await;
    let rest = stream.read_to_the_end().await;
    assert!(rest.len() <= 1, "{rest:?}");
}

#[tokio::test]
async fn an_open_stream_does_not_hold_up_a_graceful_shutdown() {
    let app = TestApp::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let server = tokio::spawn({
        let (router, stop) = (app.router.clone(), stop.clone());
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
        }
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut received = Vec::new();
    within(async {
        let mut chunk = [0_u8; 1024];
        while !String::from_utf8_lossy(&received).contains("retry:") {
            let read = client.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the server closed the stream early");
            received.extend_from_slice(&chunk[..read]);
        }
    })
    .await;
    assert!(
        String::from_utf8_lossy(&received).starts_with("HTTP/1.1 200 OK"),
        "{}",
        String::from_utf8_lossy(&received)
    );

    // What `main` does when it receives a stop signal.
    app.state.begin_shutdown();
    stop.cancel();

    within(server).await.unwrap().unwrap();
    within(client.read_to_end(&mut received)).await.unwrap();
    let text = String::from_utf8_lossy(&received);
    assert!(
        text.ends_with("0\r\n\r\n"),
        "the response ended cleanly: {text:?}"
    );
}

#[tokio::test]
async fn the_stream_outlives_the_request_timeout() {
    let app = TestApp::new().await;
    let options = SecurityOptions {
        request_timeout: Duration::from_millis(100),
        ..SecurityOptions::default()
    };
    let router = Router::new()
        .nest("/api", events::routes())
        .layer(security::layers(options))
        .with_state(app.state.clone());
    let mut stream = Stream::new(router.oneshot(get_events()).await.unwrap());
    stream.expect_greeting().await;

    tokio::time::sleep(Duration::from_millis(400)).await;

    let id = submit_instant(&app).await;
    let first = stream.wait_for_task(id, |_| true).await;
    assert_eq!(first.state, TaskState::Queued);
}
