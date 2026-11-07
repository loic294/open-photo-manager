//! Helpers shared by the integration tests: a router built from the real
//! `AppState` over a scratch directory, and a request helper.
//!
//! Scratch directories live under `CARGO_TARGET_TMPDIR` (inside `target/`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod seed;

use apm_server::app;
use apm_server::config::Config;
use apm_server::state::AppState;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;
use tower::ServiceExt;

/// A fully wired application over a throw-away directory.
pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    /// Root of the scratch directory (`volumes/`, `data/` and `web/` live
    /// below it); deleted on drop.
    pub dir: TempDir,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_config(|_| {}).await
    }

    /// Like [`TestApp::new`], letting `tweak` adjust the test `Config` first.
    pub async fn with_config(tweak: impl FnOnce(&mut Config)) -> Self {
        let dir = scratch_dir();
        let mut config = Config::for_tests(dir.path());
        std::fs::create_dir_all(&config.volumes_dir).unwrap();
        std::fs::create_dir_all(&config.data_dir).unwrap();
        tweak(&mut config);
        let state = AppState::new(config).await.unwrap();
        let router = app::router(state.clone());
        Self { router, state, dir }
    }

    /// Sends one request through the router. Every request carries
    /// `Host: localhost`; non-GET requests also carry `x-apm-request: 1`, and a
    /// JSON `body` gets a JSON content type.
    pub async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Response<Body> {
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::HOST, "localhost");
        if method != Method::GET && method != Method::HEAD {
            builder = builder.header("x-apm-request", "1");
        }
        let request = match body {
            Some(json) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// Sends a request and returns the status plus the parsed JSON body
    /// (`Value::Null` when the response is not JSON). Event streams are never
    /// drained.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self.send(method, path, body.as_ref()).await;
        let status = response.status();
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        if !is_json {
            return (status, Value::Null);
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// `GET path` returning the status and the body as text.
    pub async fn get_text(&self, path: &str) -> (StatusCode, String) {
        let response = self.send(Method::GET, path, None).await;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// A scratch directory below `target/tmp`, removed when dropped.
pub fn scratch_dir() -> TempDir {
    TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// Modification time (epoch seconds) of the files [`Tree::write`] creates, so that two scans of an
/// untouched tree compare equal and a test can change a file by changing only its size or only its
/// time.
pub const BASE_MTIME: i64 = 1_700_000_000;

impl TestApp {
    /// Creates the empty volume directory `name` below the volumes directory and registers it with
    /// the volume registry. Returns the volume id and a handle on its tree.
    pub async fn add_volume(&self, name: &str) -> (i64, Tree) {
        let root = self.state.config.volumes_dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let volumes = self.state.volumes.refresh().await.unwrap();
        let id = volumes
            .iter()
            .find(|volume| volume.name == name)
            .map(|volume| volume.id)
            .unwrap();
        (id, Tree { root })
    }
}

/// The directory tree of one volume: builds it and edits it between scans. Paths are relative to
/// the volume root and use `/`.
pub struct Tree {
    root: PathBuf,
}

impl Tree {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn mkdir(&self, rel: &str) {
        std::fs::create_dir_all(self.path(rel)).unwrap();
    }

    /// Writes a file (and the directories above it) with the modification time [`BASE_MTIME`].
    pub fn write(&self, rel: &str, bytes: &[u8]) {
        self.write_at(rel, bytes, BASE_MTIME);
    }

    pub fn write_at(&self, rel: &str, bytes: &[u8], mtime: i64) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        self.set_mtime(rel, mtime);
    }

    pub fn set_mtime(&self, rel: &str, mtime: i64) {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(mtime).unwrap());
        std::fs::File::options()
            .write(true)
            .open(self.path(rel))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    pub fn set_dir_mtime(&self, rel: &str, mtime: i64) {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(mtime).unwrap());
        std::fs::File::open(self.path(rel))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.path(rel)).unwrap();
    }

    pub fn remove_dir(&self, rel: &str) {
        std::fs::remove_dir_all(self.path(rel)).unwrap();
    }

    pub fn rename(&self, from: &str, to: &str) {
        std::fs::rename(self.path(from), self.path(to)).unwrap();
    }

    /// `dirs` directories (`d000`, `d001`, ...) of `files` small photos each.
    pub fn populate(&self, dirs: usize, files: usize) {
        for dir in 0..dirs {
            for file in 0..files {
                self.write(&format!("d{dir:03}/IMG_{file:04}.jpg"), b"x");
            }
        }
    }
}
