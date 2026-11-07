//! Helpers shared by the integration tests: a router built from the real
//! `AppState` over a scratch directory, and a request helper.
//!
//! Scratch directories live under `CARGO_TARGET_TMPDIR` (inside `target/`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use apm_server::app;
use apm_server::config::Config;
use apm_server::state::AppState;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
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
