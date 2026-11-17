//! Volumes bound to an ISM root: bindings apply at the next start, file changes on them go to a
//! (mock) sync manager as operations, its streamed answers update the index, and OPM never moves
//! or trashes those files itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use common::seed::Seed;
use opm_core::dto::*;
use opm_server::fsops::{Loc, TrashBatch, TrashRequest};
use opm_server::state::AppState;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const INSTANCE: &str = "instance-1";
const PHOTO: &[u8] = b"photo";

fn info(app: &TestApp, direct_writes: &str) -> Value {
    info_with(app, json!(direct_writes), &["read", "manage", "trash"], &[])
}

fn info_with(
    app: &TestApp,
    direct_writes: Value,
    actions: &[&str],
    capabilities: &[&str],
) -> Value {
    json!({
        "instance_id": INSTANCE,
        "instance_name": "Example sync",
        "version": "0.1.0",
        "api_version": 1,
        "token_kind": "opm",
        "actions": actions,
        "capabilities": capabilities,
        "roots": [{
            "root_id": "photos", "name": "Photos",
            "immich_import_path": "/external/photos",
            "opm_prefix": app.state.config.volumes_dir.to_str().unwrap(),
            "available": true,
            "direct_writes": direct_writes
        }]
    })
}

fn location(path: &str) -> Value {
    json!({"root_id": "photos", "path": path})
}

fn event(name: &str, id: u32, data: &Value) -> String {
    format!("id: {id}\nevent: {name}\ndata: {data}\n\n")
}

fn stream(events: &[String]) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(events.concat())
}

fn moved(index: usize, from: &str, to: &str) -> Value {
    json!({"index": index, "kind": "move", "status": "done",
           "result": {"files": [{"from": location(from), "to": location(to), "role": "original"}]}})
}

/// A connected app whose volume `photos` is bound to the ISM root `photos` (prefix `photos`).
struct Bound {
    app: TestApp,
    ism: MockServer,
    volume: i64,
    tree: common::Tree,
}

async fn bound(direct_writes: &str) -> Bound {
    bound_with(|app| info(app, direct_writes)).await
}

async fn bound_with(info: impl FnOnce(&TestApp) -> Value) -> Bound {
    let app = TestApp::new().await;
    let ism = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info(&app)))
        .mount(&ism)
        .await;
    let (status, body) = app
        .call(
            Method::PUT,
            "/api/ism/connection",
            Some(json!({"base_url": ism.uri(), "token": "fixture-token"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (volume, tree) = app.add_volume("photos").await;
    let (status, body) = app
        .call(
            Method::PUT,
            "/api/ism/bindings",
            Some(json!({"volume_id": volume, "root_id": "photos"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["staged"][0]["root_id"], "photos");
    assert_eq!(body["bindings"], json!([]));
    assert!(!app.state.fsops.is_managed(volume));
    let app = restart(app).await;
    let (_, body) = app.call(Method::GET, "/api/ism/status", None).await;
    assert_eq!(body["bindings"][0]["volume_id"], volume);
    assert_eq!(body["bindings"][0]["root_prefix"], "photos");
    assert_eq!(body["staged"], json!([]));
    assert!(app.state.fsops.is_managed(volume));
    Bound {
        app,
        ism,
        volume,
        tree,
    }
}

async fn restart(app: TestApp) -> TestApp {
    app.state.begin_shutdown();
    app.state.shutdown().await;
    let state = AppState::new((*app.state.config).clone()).await.unwrap();
    TestApp {
        router: opm_server::app::router(state.clone()),
        state,
        dir: app.dir,
    }
}

impl Bound {
    /// Writes `src/<name>` on disk and in the index, ready to plan.
    async fn photo(&self, name: &str) -> i64 {
        self.tree.write(&format!("src/{name}"), PHOTO);
        let id = Seed::new(&self.app.state.db)
            .file(self.volume, "src", name)
            .size(5)
            .insert()
            .await;
        sqlx::query("UPDATE files SET meta_state = 1")
            .execute(self.app.state.db.write())
            .await
            .unwrap();
        id
    }

    async fn plan(&self, policy: Option<ConflictPolicy>) -> i64 {
        let params = MoveParams {
            move_type: MoveType::Flat,
            source: SideParams {
                scope: Scope {
                    volume_id: Some(self.volume),
                    dir: "src".into(),
                    recursive: true,
                },
                filter: Filter::default(),
            },
            destination: FolderRef {
                volume_id: self.volume,
                dir: "out".into(),
            },
            pattern: None,
            conflict_policy: policy,
            match_mode: None,
            strict_prefix: false,
        };
        let (status, body) = self
            .app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({"kind": "move", "params": params, "replaces": null})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_i64().unwrap()
    }

    async fn start(&self, plan: i64) -> i64 {
        let (status, body) = self
            .app
            .call(
                Method::POST,
                &format!("/api/plans/{plan}/execute"),
                Some(json!({"options": {}, "only": null})),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        body["id"].as_i64().unwrap()
    }

    async fn finish(&self, plan: i64, task: i64) -> (PlanResult, TaskState) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let task = self.app.state.tasks.get(task).await.unwrap();
                if matches!(
                    task.state,
                    TaskState::Done | TaskState::Failed | TaskState::Cancelled
                ) {
                    let plan = self.app.state.plans.get(plan).await.unwrap();
                    return (plan.result.unwrap(), task.state);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn run(&self, policy: Option<ConflictPolicy>) -> PlanResult {
        let plan = self.plan(policy).await;
        let task = self.start(plan).await;
        self.finish(plan, task).await.0
    }

    /// Mounts an event stream that ends `op-1` with `items`.
    async fn finishes(&self, items: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/operations/op-1/events"))
            .respond_with(stream(&[event(
                "finished",
                1,
                &json!({"operation_id": "op-1", "status": "completed", "items": items}),
            )]))
            .mount(&self.ism)
            .await;
    }

    async fn submitted(&self) -> Vec<Value> {
        self.requests("POST", &format!("/api/v1/instances/{INSTANCE}/operations"))
            .await
    }

    async fn task(&self, id: i64) -> TaskDto {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let task = self.app.state.tasks.get(id).await.unwrap();
                if matches!(
                    task.state,
                    TaskState::Done | TaskState::Failed | TaskState::Cancelled
                ) {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn path_of(&self, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT dir_path || '/' || name FROM files WHERE id = ?")
            .bind(id)
            .fetch_optional(self.app.state.db.read())
            .await
            .unwrap()
    }

    async fn requests(&self, verb: &str, url: &str) -> Vec<Value> {
        self.ism
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.method.as_str() == verb && request.url.path() == url)
            .map(|request| serde_json::from_slice(&request.body).unwrap_or(Value::Null))
            .collect()
    }

    async fn preview_ok(&self) {
        Mock::given(method("POST"))
            .and(path("/api/v1/operations/preview"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": true, "items": [], "blockers": []})),
            )
            .mount(&self.ism)
            .await;
    }

    async fn accept(&self) {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/instances/{INSTANCE}/operations")))
            .respond_with(
                ResponseTemplate::new(202)
                    .set_body_json(json!({"operation_id": "op-1", "status": "queued"})),
            )
            .mount(&self.ism)
            .await;
    }
}

#[tokio::test]
async fn a_move_plan_on_a_bound_volume_is_one_ism_operation_and_never_touches_the_files() {
    let fx = bound("enabled").await;
    let a = fx.photo("a.jpg").await;
    let b = fx.photo("b.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    let finished = json!({"operation_id": "op-1", "status": "completed",
        "items": [moved(0, "photos/src/a.jpg", "photos/out/a.jpg"),
                  moved(1, "photos/src/b.jpg", "photos/out/b.jpg")]});
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1/events"))
        .respond_with(stream(&[
            event("item", 1, &moved(0, "photos/src/a.jpg", "photos/out/a.jpg")),
            event("finished", 2, &finished),
        ]))
        .mount(&fx.ism)
        .await;

    let result = fx.run(None).await;
    assert_eq!(
        (result.done, result.failed),
        (2, 0),
        "{:?}",
        result.failures
    );
    assert_eq!(fx.path_of(a).await.unwrap(), "out/a.jpg");
    assert_eq!(fx.path_of(b).await.unwrap(), "out/b.jpg");
    // ISM moves the files; this mock does not, and neither does OPM.
    assert_eq!(std::fs::read(fx.tree.path("src/a.jpg")).unwrap(), PHOTO);
    assert!(!fx.tree.path("out").exists());

    let submitted = fx
        .requests("POST", &format!("/api/v1/instances/{INSTANCE}/operations"))
        .await;
    assert_eq!(submitted.len(), 1);
    let request = &submitted[0];
    assert!(
        request["idempotency_key"]
            .as_str()
            .unwrap()
            .starts_with("opm:plan:")
    );
    assert_eq!(request["actions"][0]["kind"], "move");
    assert_eq!(
        request["actions"][0]["source"],
        location("photos/src/a.jpg")
    );
    assert_eq!(
        request["actions"][0]["target"],
        location("photos/out/a.jpg")
    );
    assert_eq!(
        request["actions"][0]["expected_revision"],
        "5-1700000000000000000"
    );
    let previews = fx.requests("POST", "/api/v1/operations/preview").await;
    assert_eq!(previews.len(), 1);
    assert_ne!(previews[0]["idempotency_key"], request["idempotency_key"]);
    assert!(request.get("allow_partial").is_none());
    let recorded: (String, String, String) =
        sqlx::query_as("SELECT idempotency_key, operation_id, status FROM ism_operations")
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
    assert_eq!(recorded.0, request["idempotency_key"]);
    assert_eq!(
        (recorded.1.as_str(), recorded.2.as_str()),
        ("op-1", "finished")
    );
}

#[tokio::test]
async fn ism_refusals_are_per_item_failures_and_nothing_is_done_locally() {
    let fx = bound("enabled").await;
    let a = fx.photo("a.jpg").await;
    let b = fx.photo("b.jpg").await;
    let c = fx.photo("c.jpg").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/operations/preview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": false, "blockers": [],
            "items": [{"index": 1, "blockers": [{"code": "revision_changed", "message": "The file changed"}]}]})))
        .mount(&fx.ism)
        .await;
    fx.accept().await;
    // The operation has a and c (b was blocked by the preview); ISM fails c.
    let failed = json!({"index": 1, "kind": "move", "status": "failed",
        "error": {"code": "maintenance_blocked", "message": "Immich is in maintenance"}});
    let finished = json!({"operation_id": "op-1", "status": "partial",
        "items": [moved(0, "photos/src/a.jpg", "photos/out/a.jpg"), failed]});
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1/events"))
        .respond_with(stream(&[event("finished", 1, &finished)]))
        .mount(&fx.ism)
        .await;

    let result = fx.run(None).await;
    assert_eq!(
        (result.done, result.failed),
        (1, 2),
        "{:?}",
        result.failures
    );
    let mut codes: Vec<&str> = result.failures.iter().map(|f| f.code.as_str()).collect();
    codes.sort_unstable();
    assert_eq!(codes, ["maintenance_blocked", "revision_changed"]);
    assert_eq!(fx.path_of(a).await.unwrap(), "out/a.jpg");
    assert_eq!(fx.path_of(b).await.unwrap(), "src/b.jpg");
    assert_eq!(fx.path_of(c).await.unwrap(), "src/c.jpg");
    let submitted = fx
        .requests("POST", &format!("/api/v1/instances/{INSTANCE}/operations"))
        .await;
    assert_eq!(submitted[0]["actions"].as_array().unwrap().len(), 2);
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        assert_eq!(
            std::fs::read(fx.tree.path(&format!("src/{name}"))).unwrap(),
            PHOTO
        );
    }
}

#[tokio::test]
async fn blocked_direct_writes_refuse_before_anything_is_sent() {
    let fx = bound("blocked").await;
    let a = fx.photo("a.jpg").await;
    let result = fx.run(None).await;
    assert_eq!((result.done, result.failed), (0, 1));
    assert_eq!(result.failures[0].code, "direct_writes_unavailable");
    assert_eq!(fx.path_of(a).await.unwrap(), "src/a.jpg");
    assert!(
        fx.requests("POST", "/api/v1/operations/preview")
            .await
            .is_empty()
    );
    assert_eq!(std::fs::read(fx.tree.path("src/a.jpg")).unwrap(), PHOTO);

    // Trash goes the same way: refused, the file stays, the index keeps it.
    let err = fx
        .app
        .state
        .fsops
        .trash_file(
            TrashRequest {
                file_id: Some(a),
                loc: Loc::new(fx.volume, "src/a.jpg".parse().unwrap()),
                expect: None,
                batch: TrashBatch::adhoc(),
                reason: "user".to_owned(),
            },
            &opm_server::fsops::OpContext::detached(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "direct_writes_unavailable");
    assert!(fx.tree.path("src/a.jpg").exists());
    assert!(fx.path_of(a).await.is_some());
}

#[tokio::test]
async fn replace_is_refused_and_an_identical_destination_is_skipped() {
    let fx = bound("enabled").await;
    fx.photo("a.jpg").await;
    fx.tree.write("out/a.jpg", b"other");
    let result = fx.run(Some(ConflictPolicy::Replace)).await;
    assert_eq!(result.failures[0].code, "ism_replace_unsupported");
    fx.tree.write("out/a.jpg", PHOTO);
    let result = fx.run(Some(ConflictPolicy::Smart)).await;
    assert_eq!((result.done, result.skipped, result.failed), (0, 1, 0));
    assert!(
        fx.requests("POST", "/api/v1/operations/preview")
            .await
            .is_empty()
    );
}

/// Answers `running` until the operation was cancelled, then `cancelled`.
struct Running(Arc<AtomicBool>, bool);
impl Respond for Running {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let item = if self.0.load(Ordering::SeqCst) {
            json!({"operation_id": "op-1", "status": "cancelled",
                   "items": [{"index": 0, "status": "skipped"}]})
        } else {
            json!({"operation_id": "op-1", "status": "running",
                   "items": [{"index": 0, "status": "pending"}]})
        };
        if self.1 {
            stream(&[event("progress", 1, &item)])
        } else {
            ResponseTemplate::new(200).set_body_json(item)
        }
    }
}

struct Cancel(Arc<AtomicBool>);
impl Respond for Cancel {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0.store(true, Ordering::SeqCst);
        ResponseTemplate::new(200)
            .set_body_json(json!({"operation_id": "op-1", "status": "cancelling"}))
    }
}

#[tokio::test]
async fn cancelling_the_plan_cancels_the_ism_operation() {
    let fx = bound("enabled").await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    let cancelled = Arc::new(AtomicBool::new(false));
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1/events"))
        .respond_with(Running(cancelled.clone(), true))
        .mount(&fx.ism)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1"))
        .respond_with(Running(cancelled.clone(), false))
        .mount(&fx.ism)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/operations/op-1/cancel"))
        .respond_with(Cancel(cancelled.clone()))
        .mount(&fx.ism)
        .await;

    let plan = fx.plan(None).await;
    let task = fx.start(plan).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while fx
            .requests("GET", "/api/v1/operations/op-1")
            .await
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, _) = fx
        .app
        .call(Method::POST, &format!("/api/tasks/{task}/cancel"), None)
        .await;
    assert!(status.is_success(), "{status}");
    let (result, state) = fx.finish(plan, task).await;
    assert_eq!(state, TaskState::Cancelled);
    assert_eq!(result.done, 0);
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(
        fx.requests("POST", "/api/v1/operations/op-1/cancel")
            .await
            .len(),
        1
    );
    assert_eq!(fx.path_of(a).await.unwrap(), "src/a.jpg");
}

#[tokio::test]
async fn trashing_on_a_bound_volume_goes_to_the_ism_trash() {
    let fx = bound("enabled").await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    let trashed = json!({"index": 0, "kind": "trash", "status": "done",
        "result": {"files": [{"from": location("photos/src/a.jpg"),
                              "to": location(".ism-trash/t1/photos/src/a.jpg"), "role": "original"}]}});
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1/events"))
        .respond_with(stream(&[event(
            "finished",
            1,
            &json!({"operation_id": "op-1", "status": "completed", "items": [trashed]}),
        )]))
        .mount(&fx.ism)
        .await;
    let outcome = fx
        .app
        .state
        .fsops
        .trash_file(
            TrashRequest {
                file_id: Some(a),
                loc: Loc::new(fx.volume, "src/a.jpg".parse().unwrap()),
                expect: None,
                batch: TrashBatch::adhoc(),
                reason: "user".to_owned(),
            },
            &opm_server::fsops::OpContext::detached(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.bytes, 5);
    assert!(fx.path_of(a).await.is_none());
    assert!(fx.tree.path("src/a.jpg").exists());
    let submitted = fx
        .requests("POST", &format!("/api/v1/instances/{INSTANCE}/operations"))
        .await;
    let action = &submitted[0]["actions"][0];
    assert_eq!(action["kind"], "trash");
    assert_eq!(action["source"], location("photos/src/a.jpg"));
    assert!(
        action["expected_revision"]
            .as_str()
            .unwrap()
            .starts_with("5-")
    );
    let trash: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trash_items")
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(trash, 0);
}

#[tokio::test]
async fn unbinding_applies_at_restart_and_the_volume_is_local_again() {
    let fx = bound("enabled").await;
    let (status, body) = fx
        .app
        .call(
            Method::PUT,
            "/api/ism/bindings",
            Some(json!({"volume_id": fx.volume, "root_id": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["staged"][0]["root_id"].is_null());
    let Bound {
        app,
        ism: _ism,
        volume,
        tree,
    } = fx;
    assert!(app.state.fsops.is_managed(volume));
    let app = restart(app).await;
    assert!(!app.state.fsops.is_managed(volume));
    let fx = Bound {
        app,
        ism: _ism,
        volume,
        tree,
    };
    fx.photo("a.jpg").await;
    let result = fx.run(None).await;
    assert_eq!(result.done, 1, "{:?}", result.failures);
    assert_eq!(std::fs::read(fx.tree.path("out/a.jpg")).unwrap(), PHOTO);
    assert!(
        fx.requests("POST", "/api/v1/operations/preview")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn not_linked_roots_take_changes_and_blocked_ones_say_why() {
    let fx = bound("not_linked").await;
    fx.photo("a.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    fx.finishes(json!([moved(0, "photos/src/a.jpg", "photos/out/a.jpg")]))
        .await;
    let result = fx.run(None).await;
    assert_eq!(result.done, 1, "{:?}", result.failures);

    let fx = bound_with(|app| {
        info_with(
            app,
            json!({"state": "blocked", "reason": "Immich upgrade pending"}),
            &["read", "manage", "trash"],
            &[],
        )
    })
    .await;
    fx.photo("a.jpg").await;
    let result = fx.run(None).await;
    assert_eq!(result.failures[0].code, "direct_writes_unavailable");
    assert!(
        result.failures[0]
            .message
            .contains("Immich upgrade pending"),
        "{:?}",
        result.failures
    );
    assert!(fx.submitted().await.is_empty());
}

#[tokio::test]
async fn servers_with_partial_operations_get_no_preview() {
    let fx = bound_with(|app| {
        info_with(
            app,
            json!("enabled"),
            &["read", "manage", "trash"],
            &["partial_submit"],
        )
    })
    .await;
    let a = fx.photo("a.jpg").await;
    fx.photo("b.jpg").await;
    fx.accept().await;
    let failed = json!({"index": 1, "kind": "move", "status": "failed",
        "error": {"code": "revision_changed", "message": "The file changed"}});
    fx.finishes(json!([
        moved(0, "photos/src/a.jpg", "photos/out/a.jpg"),
        failed
    ]))
    .await;
    let result = fx.run(None).await;
    assert_eq!(
        (result.done, result.failed),
        (1, 1),
        "{:?}",
        result.failures
    );
    assert_eq!(result.failures[0].code, "revision_changed");
    assert_eq!(fx.path_of(a).await.unwrap(), "out/a.jpg");
    assert!(
        fx.requests("POST", "/api/v1/operations/preview")
            .await
            .is_empty()
    );
    let submitted = fx.submitted().await;
    assert_eq!(submitted[0]["allow_partial"], true);
    assert_eq!(submitted[0]["actions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_unknown_submit_outcome_is_looked_up_by_key() {
    let fx = bound_with(|app| {
        info_with(
            app,
            json!("enabled"),
            &["read", "manage", "trash"],
            &["find_operation_by_key", "cancel_operation_by_key"],
        )
    })
    .await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/instances/{INSTANCE}/operations")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fx.ism)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/instances/{INSTANCE}/operations")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"operation_id": "op-1", "status": "running", "items": []}])),
        )
        .mount(&fx.ism)
        .await;
    fx.finishes(json!([moved(0, "photos/src/a.jpg", "photos/out/a.jpg")]))
        .await;
    let result = fx.run(None).await;
    assert_eq!(result.done, 1, "{:?}", result.failures);
    assert_eq!(fx.path_of(a).await.unwrap(), "out/a.jpg");
    let key = fx.submitted().await[0]["idempotency_key"].clone();
    let lookups: Vec<String> = fx
        .ism
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "GET"
                && request.url.path() == format!("/api/v1/instances/{INSTANCE}/operations")
        })
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find(|(name, _)| name == "idempotency_key")
                .map(|(_, value)| value.into_owned())
        })
        .collect();
    assert_eq!(lookups, [key.as_str().unwrap()]);
    assert!(
        fx.requests(
            "POST",
            &format!("/api/v1/instances/{INSTANCE}/operations/cancel-by-key")
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn unknown_submit_does_not_use_lookup_or_cancel_when_capabilities_are_absent() {
    let fx = bound("enabled").await;
    fx.photo("a.jpg").await;
    fx.preview_ok().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/instances/{INSTANCE}/operations")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fx.ism)
        .await;

    let result = fx.run(None).await;
    assert_eq!(result.failures[0].code, "ism_unreachable");
    assert!(
        fx.requests("GET", &format!("/api/v1/instances/{INSTANCE}/operations"))
            .await
            .is_empty()
    );
    assert!(
        fx.requests(
            "POST",
            &format!("/api/v1/instances/{INSTANCE}/operations/cancel-by-key")
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn instance_scoped_status_is_used_when_advertised() {
    let fx = bound_with(|app| {
        info_with(
            app,
            json!("enabled"),
            &["read", "manage", "trash"],
            &["instance_scoped_operations"],
        )
    })
    .await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/instances/{INSTANCE}/operations/op-1"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"operation_id": "op-1", "status": "completed",
                   "items": [moved(0, "photos/src/a.jpg", "photos/out/a.jpg")]})),
        )
        .mount(&fx.ism)
        .await;

    let result = fx.run(None).await;
    assert_eq!(result.done, 1, "{:?}", result.failures);
    assert_eq!(fx.path_of(a).await.unwrap(), "out/a.jpg");
    assert_eq!(
        fx.requests(
            "GET",
            &format!("/api/v1/instances/{INSTANCE}/operations/op-1")
        )
        .await
        .len(),
        1
    );
    assert!(
        fx.requests("GET", "/api/v1/operations/op-1")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn without_lookup_an_unknown_submit_is_cancelled_by_key() {
    let fx = bound_with(|app| {
        info_with(
            app,
            json!("enabled"),
            &["read", "manage", "trash"],
            &["find_operation_by_key", "cancel_operation_by_key"],
        )
    })
    .await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/instances/{INSTANCE}/operations")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fx.ism)
        .await;
    // The lookup route is unknown (404): an older server.
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/instances/{INSTANCE}/operations/cancel-by-key"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "cancelled_before_submission"})),
        )
        .mount(&fx.ism)
        .await;
    let result = fx.run(None).await;
    assert_eq!(result.failures[0].code, "ism_unreachable");
    assert_eq!(fx.path_of(a).await.unwrap(), "src/a.jpg");
    assert_eq!(
        fx.requests("GET", &format!("/api/v1/instances/{INSTANCE}/operations"))
            .await
            .len(),
        1
    );
    let probes = fx
        .requests(
            "POST",
            &format!("/api/v1/instances/{INSTANCE}/operations/cancel-by-key"),
        )
        .await;
    assert_eq!(probes.len(), 1);
    assert_eq!(
        probes[0]["idempotency_key"],
        fx.submitted().await[0]["idempotency_key"]
    );
}

#[tokio::test]
async fn folder_cleanup_sends_the_junk_it_checked() {
    let fx = bound("enabled").await;
    fx.tree.write("empty/.DS_Store", b"x");
    fx.preview_ok().await;
    fx.accept().await;
    fx.finishes(
        json!([{"index": 0, "kind": "trash_folder", "status": "done", "result": {"files": []}}]),
    )
    .await;
    fx.app
        .state
        .fsops
        .remove_dir_if_removable(
            Loc::new(fx.volume, "empty".parse().unwrap()),
            &TrashBatch::adhoc(),
            &opm_server::fsops::OpContext::detached(),
        )
        .await
        .unwrap();
    let action = &fx.submitted().await[0]["actions"][0];
    assert_eq!(action["kind"], "trash_folder");
    assert_eq!(action["source"], location("photos/empty"));
    let entries = action["expected_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], ".DS_Store");
    assert!(entries[0]["revision"].as_str().unwrap().starts_with("1-"));
    assert!(fx.tree.path("empty/.DS_Store").exists());
}

/// Moves the folder on disk the way ISM would, then ends `op-1`.
struct MovesFolder {
    from: std::path::PathBuf,
    to: std::path::PathBuf,
}
impl Respond for MovesFolder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.from.exists() {
            std::fs::create_dir_all(self.to.parent().unwrap()).unwrap();
            std::fs::rename(&self.from, &self.to).unwrap();
        }
        let item =
            json!({"index": 0, "kind": "move_folder", "status": "done", "result": {"files": []}});
        stream(&[event(
            "finished",
            1,
            &json!({"operation_id": "op-1", "status": "completed", "items": [item]}),
        )])
    }
}

#[tokio::test]
async fn folders_are_moved_and_created_by_ism() {
    let fx = bound("enabled").await;
    let a = fx.photo("a.jpg").await;
    fx.preview_ok().await;
    fx.accept().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/operations/op-1/events"))
        .respond_with(MovesFolder {
            from: fx.tree.path("src"),
            to: fx.tree.path("albums/trip"),
        })
        .mount(&fx.ism)
        .await;
    let cx = opm_server::fsops::OpContext::detached();
    fx.app
        .state
        .fsops
        .move_folder(
            Loc::new(fx.volume, "src".parse().unwrap()),
            Loc::new(fx.volume, "albums/trip".parse().unwrap()),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(fx.path_of(a).await.unwrap(), "albums/trip/a.jpg");
    let dirs: Vec<String> =
        sqlx::query_scalar("SELECT path FROM dirs WHERE volume_id = ? ORDER BY path")
            .bind(fx.volume)
            .fetch_all(fx.app.state.db.read())
            .await
            .unwrap();
    assert!(dirs.contains(&"albums/trip".to_owned()), "{dirs:?}");
    assert!(!dirs.contains(&"src".to_owned()), "{dirs:?}");
    assert_eq!(
        fx.submitted().await[0]["actions"][0],
        json!({"kind": "move_folder", "source": location("photos/src"),
               "target": location("photos/albums/trip")})
    );

    fx.ism.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/client/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info(&fx.app, "enabled")))
        .mount(&fx.ism)
        .await;
    fx.preview_ok().await;
    fx.accept().await;
    fx.finishes(
        json!([{"index": 0, "kind": "create_folder", "status": "done", "result": {"files": []}}]),
    )
    .await;
    let created = fx
        .app
        .state
        .fsops
        .mkdir_all(Loc::new(fx.volume, "new/sub".parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(created.len(), 2);
    assert_eq!(
        fx.submitted().await[0]["actions"][0],
        json!({"kind": "create_folder", "target": location("photos/new/sub")})
    );
    let made: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM dirs WHERE volume_id = ? AND path IN ('new', 'new/sub')",
    )
    .bind(fx.volume)
    .fetch_one(fx.app.state.db.read())
    .await
    .unwrap();
    assert_eq!(made, 2);
    // A folder move on a local volume stays refused.
    let (other, _) = fx.app.add_volume("local").await;
    let err = fx
        .app
        .state
        .fsops
        .move_folder(
            Loc::new(other, "a".parse().unwrap()),
            Loc::new(other, "b".parse().unwrap()),
            &cx,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unsupported");
}

fn trash_listing() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!([
        {"trash_id": "t1", "location": location("photos/src/a.jpg"), "is_folder": false,
         "size": 5, "trashed_at": "2024-05-01T10:00:00Z", "operation_id": "op-0",
         "asset_ids": ["asset-1"]},
        {"trash_id": "t2", "location": location("elsewhere/b.jpg"), "is_folder": false,
         "size": 7, "trashed_at": "2024-05-01T09:00:00Z", "operation_id": "op-0",
         "asset_ids": []}
    ]))
}

#[tokio::test]
async fn the_ism_trash_of_a_bound_volume_is_listed_and_restored_through_ism() {
    let fx = bound("enabled").await;
    Mock::given(method("GET"))
        .and(path("/api/v1/trash"))
        .respond_with(trash_listing())
        .mount(&fx.ism)
        .await;
    let (status, body) = fx
        .app
        .call(
            Method::GET,
            &format!("/api/trash/ism?volume_id={}", fx.volume),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["root_name"], "Photos");
    assert_eq!(body["can_purge"], false);
    assert_eq!(
        body["items"],
        json!([{"trash_id": "t1", "path": "src/a.jpg", "is_folder": false, "size": 5,
                "trashed_at": "2024-05-01T10:00:00Z", "asset_count": 1}])
    );
    let trash_queries: Vec<String> = fx
        .ism
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/api/v1/trash")
        .filter_map(|request| request.url.query().map(str::to_owned))
        .collect();
    assert_eq!(trash_queries[0], "root_id=photos");

    // Purge needs the grant and a confirmation; neither reaches ISM.
    let (status, body) = fx
        .app
        .call(
            Method::POST,
            "/api/trash/ism/purge",
            Some(json!({"volume_id": fx.volume, "trash_ids": ["t1"], "confirm": true})),
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (status, body) = fx
        .app
        .call(
            Method::POST,
            "/api/trash/ism/purge",
            Some(json!({"volume_id": fx.volume, "trash_ids": ["t1"]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Restore: ISM brings the file back (written here), OPM rescans where it went.
    fx.tree.write("src/a.jpg", PHOTO);
    fx.preview_ok().await;
    fx.accept().await;
    fx.finishes(
        json!([{"index": 0, "kind": "restore", "status": "done", "result": {"files": [
        {"from": location(".ism-trash/t1/photos/src/a.jpg"), "to": location("photos/src/a.jpg"),
         "role": "original"}]}}]),
    )
    .await;
    let (status, body) = fx
        .app
        .call(
            Method::POST,
            "/api/trash/ism/restore",
            Some(json!({"volume_id": fx.volume, "trash_ids": ["t1"]})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = fx.task(body["id"].as_i64().unwrap()).await;
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(
        fx.submitted().await[0]["actions"],
        json!([{"kind": "restore", "trash_id": "t1"}])
    );
    let indexed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE dir_path = 'src' AND name = 'a.jpg'")
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
    assert_eq!(indexed, 1);

    // An unbound volume has no ISM trash.
    let (other, _) = fx.app.add_volume("local").await;
    let (status, body) = fx
        .app
        .call(
            Method::GET,
            &format!("/api/trash/ism?volume_id={other}"),
            None,
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("ism_not_bound"))
    );
}

#[tokio::test]
async fn a_confirmed_purge_with_the_grant_goes_to_ism() {
    let fx = bound_with(|app| {
        info_with(
            app,
            json!("enabled"),
            &["read", "manage", "trash", "purge"],
            &[],
        )
    })
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/trash"))
        .respond_with(trash_listing())
        .mount(&fx.ism)
        .await;
    fx.preview_ok().await;
    fx.accept().await;
    fx.finishes(json!([{"index": 0, "kind": "purge", "status": "done", "result": {"files": []}}]))
        .await;
    let (status, body) = fx
        .app
        .call(
            Method::POST,
            "/api/trash/ism/purge",
            Some(json!({"volume_id": fx.volume, "trash_ids": ["t1"], "confirm": true})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = fx.task(body["id"].as_i64().unwrap()).await;
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(task.kind, TaskKind::TrashEmpty);
    assert_eq!(
        fx.submitted().await[0]["actions"],
        json!([{"kind": "purge", "trash_id": "t1"}])
    );
}
