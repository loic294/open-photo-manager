//! What the tests of the asset sync need on top of the [`Fixture`]: asking for a sync and waiting
//! for it, reading what it stored, and the `in_immich` badge of a directory.
//!
//! The rows are read with plain SQL on purpose: the tests check the tables the way a person
//! inspecting the database would, not through the code under test.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use opm_core::dto::{TaskDto, TaskState};
use opm_core::paths::TaskId;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{Fixture, entry};
use crate::common::browse;

/// How long a test waits for something that must happen.
const PATIENCE: Duration = Duration::from_secs(60);

/// A row of `immich_assets`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct StoredAsset {
    pub asset_id: String,
    pub scope: String,
    pub library_id: Option<String>,
    pub original_path: String,
    pub is_offline: bool,
    pub is_trashed: bool,
    /// Epoch milliseconds.
    pub updated_at: i64,
    pub sync_run: i64,
    pub volume_id: Option<i64>,
    pub dir_path: Option<String>,
    pub name: Option<String>,
}

impl StoredAsset {
    /// `(volume_id, dir_path, name)` when the path of the asset resolved to a volume.
    pub fn location(&self) -> Option<(i64, &str, &str)> {
        Some((
            self.volume_id?,
            self.dir_path.as_deref()?,
            self.name.as_deref()?,
        ))
    }
}

/// A row of `immich_sync_state`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct StoredScope {
    pub scope: String,
    pub fingerprint: String,
    /// Epoch milliseconds.
    pub high_water_ms: i64,
    pub run: i64,
    pub last_attempt_at: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_full_at: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub assets_total: i64,
    pub assets_live: i64,
    pub assets_matched: i64,
    pub assets_unmapped: i64,
}

const ASSETS_SQL: &str = "SELECT asset_id, scope, library_id, original_path, is_offline, \
     is_trashed, updated_at, sync_run, volume_id, dir_path, name FROM immich_assets \
     ORDER BY original_path, asset_id";

const ASSET_SQL: &str = "SELECT asset_id, scope, library_id, original_path, is_offline, \
     is_trashed, updated_at, sync_run, volume_id, dir_path, name FROM immich_assets \
     WHERE asset_id = ?";

const SCOPES_SQL: &str = "SELECT scope, fingerprint, high_water_ms, run, last_attempt_at, \
     last_sync_at, last_full_at, last_error, consecutive_failures, assets_total, assets_live, \
     assets_matched, assets_unmapped FROM immich_sync_state ORDER BY scope";

/// Polls `check` until it holds; panics with `what` after [`PATIENCE`].
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let started = Instant::now();
    while !check().await {
        assert!(started.elapsed() < PATIENCE, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The lower bound on `updatedAt` a search body asks for, in either request generation.
pub fn updated_after(body: &Value) -> Option<String> {
    let bound = match body.get("filter") {
        Some(filter) => &filter["updatedAt"]["gt"],
        None => &body["updatedAfter"],
    };
    bound.as_str().map(str::to_owned)
}

impl Fixture {
    /// Saves mappings (see [`super::entry`]) and checks that they were accepted.
    pub async fn map(&self, entries: Vec<Value>) {
        let (status, body) = self.put_mappings(entries).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// A library of the fake with the import path `prefix`, a volume called `volume`, and the
    /// mapping of the volume to the library under `prefix`; the ids of the library and the volume.
    pub async fn map_library(&self, name: &str, volume: &str, prefix: &str) -> (String, i64) {
        let library = self.fake.add_library(name, &[prefix]);
        let volume = self.volume(volume).await;
        self.map(vec![entry(volume, Some(&library), prefix)]).await;
        (library, volume)
    }

    /// `POST /api/immich/sync`; `None` sends no body at all.
    pub async fn start_sync(&self, body: Option<Value>) -> (StatusCode, Value) {
        match body {
            Some(body) => self.post("/api/immich/sync", body).await,
            None => self.post_empty("/api/immich/sync").await,
        }
    }

    /// `POST /api/immich/sync` with `body` as it is, for what JSON values cannot say.
    pub async fn start_sync_raw(&self, body: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/immich/sync")
            .header(header::HOST, "localhost")
            .header("x-opm-request", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = self.app.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Asks for a sync, which must be accepted, and returns the id of its task.
    pub async fn submit_sync(&self, full: bool) -> TaskId {
        let (status, body) = self.start_sync(Some(json!({ "full": full }))).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        body["id"].as_i64().expect("the task has an id")
    }

    /// Waits for the task to end, whatever way it ends.
    pub async fn finish(&self, task: TaskId) -> TaskDto {
        tokio::time::timeout(PATIENCE, self.app.state.tasks.wait_finished(task))
            .await
            .expect("the task did not end in time")
            .expect("the task exists")
    }

    /// Asks for a sync and waits for its task.
    pub async fn sync(&self, full: bool) -> TaskDto {
        let task = self.submit_sync(full).await;
        self.finish(task).await
    }

    /// A sync that must end well.
    pub async fn sync_ok(&self, full: bool) -> TaskDto {
        let task = self.sync(full).await;
        assert_eq!(task.state, TaskState::Done, "{task:?}");
        assert_eq!(task.error, None);
        task
    }

    /// The task as the engine reports it now.
    pub async fn task(&self, task: TaskId) -> TaskDto {
        self.app
            .state
            .tasks
            .get(task)
            .await
            .expect("the task exists")
    }

    /// What the task returned (`tasks.result_json`).
    pub async fn task_result(&self, task: TaskId) -> Option<Value> {
        let text: Option<String> = sqlx::query_scalar("SELECT result_json FROM tasks WHERE id = ?")
            .bind(task)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap();
        text.map(|text| serde_json::from_str(&text).unwrap())
    }

    /// Every stored asset, by Immich path.
    pub async fn assets(&self) -> Vec<StoredAsset> {
        sqlx::query_as(ASSETS_SQL)
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// The stored row of an asset of the fake.
    pub async fn stored(&self, asset_id: &str) -> Option<StoredAsset> {
        sqlx::query_as(ASSET_SQL)
            .bind(asset_id)
            .fetch_optional(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// The stored row of an asset of the fake, which must exist.
    pub async fn row(&self, asset_id: &str) -> StoredAsset {
        self.stored(asset_id)
            .await
            .unwrap_or_else(|| panic!("asset {asset_id} is not stored"))
    }

    pub async fn asset_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM immich_assets")
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// Makes the last completed pass of every scope `secs` seconds older.
    pub async fn age_syncs(&self, secs: i64) {
        sqlx::query(
            "UPDATE immich_sync_state SET last_sync_at = last_sync_at - ? \
             WHERE last_sync_at IS NOT NULL",
        )
        .bind(secs)
        .execute(self.app.state.db.write())
        .await
        .unwrap();
    }

    /// Makes the last complete full pass of every scope `secs` seconds older.
    pub async fn age_full_syncs(&self, secs: i64) {
        sqlx::query(
            "UPDATE immich_sync_state SET last_full_at = last_full_at - ? \
             WHERE last_full_at IS NOT NULL",
        )
        .bind(secs)
        .execute(self.app.state.db.write())
        .await
        .unwrap();
    }

    /// Makes the last attempt on every scope `secs` seconds older.
    pub async fn age_attempts(&self, secs: i64) {
        sqlx::query(
            "UPDATE immich_sync_state SET last_attempt_at = last_attempt_at - ? \
             WHERE last_attempt_at IS NOT NULL",
        )
        .bind(secs)
        .execute(self.app.state.db.write())
        .await
        .unwrap();
    }

    /// Every stored scope, by key.
    pub async fn scopes(&self) -> Vec<StoredScope> {
        sqlx::query_as(SCOPES_SQL)
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// The stored state of a scope (a library id, or `path:` and the prefix), which must exist.
    pub async fn scope(&self, key: &str) -> StoredScope {
        self.scopes()
            .await
            .into_iter()
            .find(|scope| scope.scope == key)
            .unwrap_or_else(|| panic!("scope {key} has no state"))
    }

    /// The `in_immich` badge of every file directly in `dir`, by name.
    pub async fn badges(&self, volume_id: i64, dir: &str) -> BTreeMap<String, bool> {
        let (status, body) =
            browse::items(&self.app, json!({ "volume_id": volume_id, "dir": dir })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap().to_owned(),
                    item["in_immich"].as_bool().unwrap(),
                )
            })
            .collect()
    }
}
