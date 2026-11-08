//! Helpers of the trash tests: the real application over two scratch volumes, items put in the
//! trash through its own `FsOps` (so the rows, the files and the journal are the real ones), rows
//! inserted straight into `trash_items` for the tests that only read the listing, and the trash
//! API with the tasks it queues.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use apm_core::dto::{SettingsDto, TaskDto, TaskKind};
use apm_core::paths::{TaskId, TrashItemId, VolumeId};
use apm_server::fsops::{
    AppliedOp, FsOps, OpContext, OpHook, TrashBatch, TrashOutcome, TrashRequest,
};
use apm_server::indexer::{RunContext, ScanRequest};
use apm_server::tasks::TaskSpec;
use apm_server::trash::TrashService;
use axum::http::{Method, StatusCode};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use tokio::sync::{Notify, Semaphore};

use super::fsops::{disk_files, loc};
use super::{TestApp, Tree};

/// How long a test waits for something that has to happen.
pub const PATIENCE: Duration = Duration::from_secs(15);

const SYNC: &str = "/api/trash/sync";

pub async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

/// Two volumes, `a` and `b`, served by the application.
pub struct TrashFx {
    pub app: TestApp,
    pub a: VolumeId,
    pub b: VolumeId,
    pub ta: Tree,
    pub tb: Tree,
}

impl TrashFx {
    pub async fn new() -> Self {
        let app = TestApp::new().await;
        let (a, ta) = app.add_volume("a").await;
        let (b, tb) = app.add_volume("b").await;
        Self { app, a, b, ta, tb }
    }

    pub fn tree(&self, volume: VolumeId) -> &Tree {
        if volume == self.a { &self.ta } else { &self.tb }
    }

    /// Writes a file and trashes it as the user would, into a batch of its own.
    pub async fn trash(&self, volume: VolumeId, path: &str, bytes: &[u8]) -> TrashOutcome {
        self.trash_into(volume, path, bytes, &TrashBatch::adhoc())
            .await
    }

    /// Writes a file and trashes it into `batch`, which several files can share.
    pub async fn trash_into(
        &self,
        volume: VolumeId,
        path: &str,
        bytes: &[u8],
        batch: &TrashBatch,
    ) -> TrashOutcome {
        self.tree(volume).write(path, bytes);
        self.trash_existing(volume, path, batch, "user").await
    }

    /// Trashes what is on disk already, a file or a folder.
    pub async fn trash_existing(
        &self,
        volume: VolumeId,
        path: &str,
        batch: &TrashBatch,
        reason: &str,
    ) -> TrashOutcome {
        self.trash_with(&OpContext::detached(), volume, path, batch, reason)
            .await
    }

    /// [`TrashFx::trash_existing`] as a task would do it: the row records `task_id`.
    pub async fn trash_as_task(
        &self,
        volume: VolumeId,
        path: &str,
        batch: &TrashBatch,
        reason: &str,
        task_id: TaskId,
    ) -> TrashOutcome {
        let mut cx = OpContext::detached();
        cx.task_id = Some(task_id);
        self.trash_with(&cx, volume, path, batch, reason).await
    }

    async fn trash_with(
        &self,
        cx: &OpContext,
        volume: VolumeId,
        path: &str,
        batch: &TrashBatch,
        reason: &str,
    ) -> TrashOutcome {
        let request = TrashRequest {
            file_id: None,
            loc: loc(volume, path),
            expect: None,
            batch: batch.clone(),
            reason: reason.to_owned(),
        };
        self.app.state.fsops.trash_file(request, cx).await.unwrap()
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.app.call(Method::GET, path, None).await
    }

    /// Indexes both volumes.
    pub async fn scan(&self) {
        self.app
            .state
            .indexer
            .scan(&ScanRequest::default(), &RunContext::noop())
            .await
            .unwrap();
    }

    /// The files of a volume that are not in the trash: path -> content.
    pub fn files(&self, volume: VolumeId) -> BTreeMap<String, Vec<u8>> {
        let mut files = disk_files(self.tree(volume).root());
        files.retain(|path, _| !path.starts_with(".apm-trash/"));
        files
    }

    /// The files in the trash folder of a volume: path below `.apm-trash/` -> content.
    pub fn trashed_files(&self, volume: VolumeId) -> BTreeMap<String, Vec<u8>> {
        disk_files(self.tree(volume).root())
            .into_iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix(".apm-trash/")
                    .map(|rest| (rest.to_owned(), bytes))
            })
            .collect()
    }

    /// The paths of the files the index knows on a volume.
    pub async fn indexed(&self, volume: VolumeId) -> BTreeSet<String> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT CASE WHEN dir_path = '' THEN name ELSE dir_path || '/' || name END \
             FROM files WHERE volume_id = ?",
        )
        .bind(volume)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap();
        rows.into_iter().collect()
    }

    /// Changes the settings the way the settings page does.
    pub async fn configure(&self, change: impl FnOnce(&mut SettingsDto)) {
        let mut settings = (*self.app.state.settings.get()).clone();
        change(&mut settings);
        self.app.state.settings.update(settings).await.unwrap();
    }

    /// A trash service over the application's database whose `FsOps` also runs `hook`, so a test
    /// can stop an operation at a known point.
    pub fn service_with_hook(&self, hook: Arc<dyn OpHook>) -> TrashService {
        let state = &self.app.state;
        let fsops = FsOps::new(
            state.db.clone(),
            state.volumes.clone(),
            state.settings.clone(),
            state.indexer.clone(),
        )
        .with_hook(hook);
        TrashService::new(
            state.db.clone(),
            state.settings.clone(),
            state.tasks.clone(),
            state.volumes.clone(),
            fsops,
        )
    }

    /// A trash service of the application that reads the time from `clock` (epoch seconds), so a
    /// test can age the trash without waiting.
    pub fn service_with_clock(
        &self,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> TrashService {
        let state = &self.app.state;
        TrashService::new(
            state.db.clone(),
            state.settings.clone(),
            state.tasks.clone(),
            state.volumes.clone(),
            state.fsops.clone(),
        )
        .with_clock(clock)
    }

    /// Makes an item look as if it had been trashed at `trashed_at` (epoch seconds).
    pub async fn age(&self, id: TrashItemId, trashed_at: i64) {
        sqlx::query("UPDATE trash_items SET trashed_at = ? WHERE id = ?")
            .bind(trashed_at)
            .bind(id)
            .execute(self.app.state.db.write())
            .await
            .unwrap();
    }

    /// Starts a task that keeps the `fs` lane busy until the gate is opened, so the tasks queued
    /// after it stay queued.
    pub async fn hold_the_queue(&self) -> Gate {
        let notify = Arc::new(Notify::new());
        let waiting = Arc::clone(&notify);
        let spec = TaskSpec::new(TaskKind::FsMove, "Holding the queue");
        let task = self
            .app
            .state
            .tasks
            .submit(spec, move |_| async move {
                waiting.notified().await;
                Ok(json!({}))
            })
            .await
            .unwrap();
        Gate { task, notify }
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.app.call(Method::POST, path, Some(body)).await
    }

    /// A POST that lacks the `x-apm-request` header every state-changing request must carry.
    pub async fn post_unmarked(&self, path: &str, body: &Value) -> StatusCode {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;

        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.app
            .router
            .clone()
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    /// `GET /api/trash` with a query string, expecting 200.
    pub async fn list(&self, query: &str) -> Value {
        let (status, body) = self.get(&format!("/api/trash?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// `GET /api/trash/batches` with a query string, expecting 200.
    pub async fn batches(&self, query: &str) -> Value {
        let (status, body) = self.get(&format!("/api/trash/batches?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// Posts a request that queues a task, expects 202 and returns the task's id.
    pub async fn queue(&self, path: &str, body: Value) -> TaskId {
        let (status, task) = self.post(path, body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        task["id"].as_i64().unwrap()
    }

    /// `POST /api/trash/sync`, which takes no body, expecting 202: the task's id.
    pub async fn queue_sync(&self) -> TaskId {
        let (status, task) = self.app.call(Method::POST, SYNC, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        task["id"].as_i64().unwrap()
    }

    /// [`TrashFx::queue`], then waits until the task is over.
    pub async fn run(&self, path: &str, body: Value) -> TaskDto {
        let id = self.queue(path, body).await;
        self.finished(id).await
    }

    pub async fn finished(&self, id: TaskId) -> TaskDto {
        within(self.app.state.tasks.wait_finished(id))
            .await
            .unwrap()
    }

    /// What a task returned (`tasks.result_json`); the API does not show it.
    pub async fn result_of(&self, id: TaskId) -> Option<Value> {
        let text: Option<String> = sqlx::query_scalar("SELECT result_json FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap();
        text.map(|text| serde_json::from_str(&text).unwrap())
    }

    /// Makes a volume unreachable, the way an unmounted share is: its folder moves out of the
    /// volumes folder.
    pub async fn take_offline(&self, volume: VolumeId) -> Outage {
        let root = self.tree(volume).root().to_path_buf();
        let parking = self.app.dir.path().join("parking");
        fs::create_dir_all(&parking).unwrap();
        let away = parking.join(root.file_name().unwrap());
        fs::rename(&root, &away).unwrap();
        self.app.state.volumes.refresh().await.unwrap();
        Outage::Offline { root, away }
    }

    /// Makes a volume read-only. `None` where the platform does not enforce permissions (the
    /// tests run as an administrator there).
    pub async fn make_read_only(&self, volume: VolumeId) -> Option<Outage> {
        let root = self.tree(volume).root().to_path_buf();
        if !super::fsops::permissions_are_enforced(&root) {
            return None;
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
        self.app.state.volumes.refresh().await.unwrap();
        Some(Outage::ReadOnly { root })
    }

    /// Ends an [`Outage`].
    pub async fn recover(&self, outage: Outage) {
        match outage {
            Outage::Offline { root, away } => fs::rename(away, root).unwrap(),
            Outage::ReadOnly { root } => {
                fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        self.app.state.volumes.refresh().await.unwrap();
    }

    /// The ids of the rows of `trash_items`, ascending.
    pub async fn ids(&self) -> Vec<TrashItemId> {
        sqlx::query_scalar("SELECT id FROM trash_items ORDER BY id")
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// `(volume_id, trash_path)` of every row, ascending by id.
    pub async fn rows(&self) -> Vec<(VolumeId, String)> {
        sqlx::query_as("SELECT volume_id, trash_path FROM trash_items ORDER BY id")
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// Inserts a row without a file behind it: enough for what only reads the listing.
    pub async fn insert(&self, row: Row) -> TrashItemId {
        sqlx::query_scalar(
            "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, \
             reason, task_id, meta_json, trashed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(row.volume_id)
        .bind(&row.batch)
        .bind(&row.orig_path)
        .bind(format!(".apm-trash/{}/{}", row.batch, row.orig_path))
        .bind(row.is_dir)
        .bind(row.size)
        .bind(&row.reason)
        .bind(row.task_id)
        .bind(row.snapshot.then_some("{}"))
        .bind(row.trashed_at)
        .fetch_one(self.app.state.db.write())
        .await
        .unwrap()
    }
}

/// A volume that cannot be used until [`TrashFx::recover`] is called.
#[derive(Debug)]
pub enum Outage {
    Offline { root: PathBuf, away: PathBuf },
    ReadOnly { root: PathBuf },
}

/// The task [`TrashFx::hold_the_queue`] started.
pub struct Gate {
    pub task: TaskId,
    notify: Arc<Notify>,
}

impl Gate {
    /// Lets the holding task finish, and the queue go on.
    pub fn open(&self) {
        self.notify.notify_one();
    }
}

/// A hook that stops the first restore or purge it sees inside its transaction until the test lets
/// it go, so a test can act while an operation is half way.
pub struct Parking {
    parked: AtomicBool,
    arrived: Notify,
    go: Semaphore,
}

impl Parking {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            parked: AtomicBool::new(false),
            arrived: Notify::new(),
            go: Semaphore::new(0),
        })
    }

    /// Resolves once an operation is parked.
    pub async fn arrived(&self) {
        within(self.arrived.notified()).await;
    }

    /// Lets the parked operation, and every later one, go on.
    pub fn release(&self) {
        self.go.add_permits(1_000_000);
    }
}

impl OpHook for Parking {
    fn on_applied<'a>(
        &'a self,
        _tx: &'a mut SqliteConnection,
        op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
        Box::pin(async move {
            let ours = matches!(op, AppliedOp::Restored { .. } | AppliedOp::Purged { .. });
            if ours && !self.parked.swap(true, Ordering::SeqCst) {
                self.arrived.notify_one();
                if let Ok(permit) = self.go.acquire().await {
                    permit.forget();
                }
            }
            Ok(())
        })
    }
}

/// A `trash_items` row to insert with [`TrashFx::insert`].
#[derive(Debug, Clone)]
pub struct Row {
    pub volume_id: VolumeId,
    pub batch: String,
    pub orig_path: String,
    pub size: i64,
    pub reason: Option<String>,
    pub task_id: Option<TaskId>,
    pub trashed_at: i64,
    pub is_dir: bool,
    pub snapshot: bool,
}

impl Row {
    pub fn new(volume_id: VolumeId, batch: &str, orig_path: &str) -> Self {
        Self {
            volume_id,
            batch: batch.to_owned(),
            orig_path: orig_path.to_owned(),
            size: 0,
            reason: None,
            task_id: None,
            trashed_at: 1_700_000_000,
            is_dir: false,
            snapshot: false,
        }
    }

    pub fn size(mut self, size: i64) -> Self {
        self.size = size;
        self
    }

    pub fn reason(mut self, reason: &str) -> Self {
        self.reason = Some(reason.to_owned());
        self
    }

    pub fn task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn at(mut self, trashed_at: i64) -> Self {
        self.trashed_at = trashed_at;
        self
    }

    pub fn dir(mut self) -> Self {
        self.is_dir = true;
        self
    }

    pub fn snapshot(mut self) -> Self {
        self.snapshot = true;
        self
    }
}

/// The ids of the items of a listing response, in order.
pub fn ids_of(listing: &Value) -> Vec<i64> {
    listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect()
}

/// The names of the items of a listing response, in order.
pub fn names_of(listing: &Value) -> Vec<String> {
    listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_owned())
        .collect()
}
