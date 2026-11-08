//! The generic plan API (`POST /api/plans`, `GET` and `DELETE /api/plans/{id}`) and the life of a
//! plan before it runs: building, the size and time limits, cancellation, expiry, startup
//! recovery. The real application runs around the scripted planner of `common::fake_planner`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use apm_server::plans::Swept;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::browse::{self, error_code};
use common::fake_planner::{Harness, SECRET};
use common::seed::Seed;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

async fn add_fake_row(h: &Harness, plan_id: i64) {
    sqlx::query(
        "INSERT INTO fake_plan_items (plan_id, side, file_id, flag) \
         VALUES (?, 'source', 1, 'matched')",
    )
    .bind(plan_id)
    .execute(h.db().write())
    .await
    .unwrap();
}

// ---- building a plan ----

#[tokio::test]
async fn a_built_plan_is_ready_and_described() {
    let h = Harness::new().await;
    let (status, plan) = h
        .post_plan(
            "cleanup",
            json!({
                "matched": [1, 2, 3],
                "will_delete": [4],
                "stats": { "pairs": 3, "bytes": 9 },
                "warnings": [
                    { "code": "not_hashed", "message": "2 files are not hashed", "count": 2 },
                    { "code": "nothing", "message": "never shown", "count": 0 },
                ],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{plan}");
    let id = plan["id"].as_i64().unwrap();
    assert!(id > 0);
    assert_eq!(plan["kind"], "cleanup");
    assert_eq!(plan["state"], "ready");
    assert_eq!(plan["stats"], json!({ "bytes": 9, "pairs": 3 }));
    assert_eq!(
        plan["warnings"],
        json!([{ "code": "not_hashed", "message": "2 files are not hashed", "count": 2 }]),
        "a warning that counts nothing is dropped"
    );
    assert_eq!(plan["result"], Value::Null);
    assert_eq!(plan["task_id"], Value::Null);
    let now = h.clock.now();
    assert_eq!(plan["created_at"], now);
    assert_eq!(plan["expires_at"], now + 2 * 60 * 60);
    assert!(
        plan.get("params").is_none(),
        "the parameters are not echoed: {plan}"
    );

    let (status, again) = h.get_plan(id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, plan, "GET describes the plan the way POST did");
    assert_eq!(h.fake_rows(Some(id)).await, 4);
}

#[tokio::test]
async fn every_plan_gets_its_own_id_and_rows() {
    let h = Harness::new().await;
    let first = h.new_plan(json!({ "matched": [1, 2] })).await;
    let second = h.new_plan(json!({ "matched": [3], "filler": 2 })).await;
    assert_ne!(first, second);
    assert_eq!(h.fake_rows(Some(first)).await, 2);
    assert_eq!(h.fake_rows(Some(second)).await, 3);
    assert_eq!(h.plan_rows().await, 2);
}

#[tokio::test]
async fn absent_params_reach_the_planner_as_an_empty_object() {
    let h = Harness::new().await;
    for body in [
        json!({ "kind": "cleanup" }),
        json!({ "kind": "cleanup", "params": null }),
    ] {
        let (status, plan) = h.app.call(Method::POST, "/api/plans", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{plan}");
    }
    assert_eq!(h.planner().builds(), vec![json!({}), json!({})]);
}

#[tokio::test]
async fn params_that_are_not_an_object_are_refused() {
    let h = Harness::new().await;
    for params in [json!([1, 2]), json!("text"), json!(5), json!(true)] {
        let (status, body) = h.post_plan("cleanup", params.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{params}: {body}");
        assert_eq!(error_code(&body), "invalid_plan_params", "{params}");
    }
    assert!(
        h.planner().builds().is_empty(),
        "the planner is never asked"
    );
    assert_eq!(h.plan_rows().await, 0);
}

#[tokio::test]
async fn kinds_without_a_planner_are_not_implemented_yet() {
    let h = Harness::new().await;
    for (kind, todo) in [
        ("duplicates", "implementing-duplicate-matching"),
        ("move", "implementing-move-planner"),
    ] {
        let (status, body) = h.post_plan(kind, json!({})).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{kind}: {body}");
        assert_eq!(error_code(&body), "not_implemented", "{kind}");
        assert!(message(&body).contains(todo), "{kind}: {body}");
    }
    assert_eq!(h.plan_rows().await, 0, "no row for a kind nobody builds");
}

#[tokio::test]
async fn a_body_of_the_wrong_shape_is_refused_before_the_engine_sees_it() {
    let h = Harness::new().await;
    for body in [
        json!({ "kind": "bogus", "params": {} }),
        json!({ "params": {} }),
        json!({ "kind": "cleanup", "replaces": "x" }),
    ] {
        let (status, reply) = h
            .app
            .call(Method::POST, "/api/plans", Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {reply}");
        assert_eq!(error_code(&reply), "invalid_json", "{body}");
    }
    assert!(h.planner().builds().is_empty());
    assert_eq!(h.plan_rows().await, 0);
}

#[tokio::test]
async fn params_the_planner_rejects_are_a_client_error_and_leave_nothing_behind() {
    let h = Harness::new().await;
    let (status, body) = h
        .post_plan("cleanup", json!({ "matched": [1], "fail": "invalid" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_plan_params");
    assert!(message(&body).contains("fail"), "names the field: {body}");

    let (status, body) = h.post_plan("cleanup", json!({ "unknown": 1 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_plan_params");

    h.settle().await;
    assert_eq!(h.plan_rows().await, 0);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn an_internal_failure_never_leaks_its_details() {
    let h = Harness::new().await;
    let (status, body) = h
        .post_plan("cleanup", json!({ "matched": [1], "fail": "internal" }))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(error_code(&body), "internal");
    assert!(!body.to_string().contains(SECRET), "{body}");
    assert!(!body.to_string().contains("/volumes"), "{body}");

    h.settle().await;
    assert_eq!(h.plan_rows().await, 0);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn a_plan_over_the_item_limit_is_refused_and_removed() {
    let h = Harness::tuned(|options| options.max_items = 10).await;

    let (status, body) = h.post_plan("cleanup", json!({ "filler": 11 })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "plan_too_large");

    // A planner that only learns the size once it is done: the engine checks what it reports.
    let (status, body) = h
        .post_plan("cleanup", json!({ "matched": [1], "claim_items": 11 }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "plan_too_large");

    h.settle().await;
    assert_eq!(h.plan_rows().await, 0);
    assert_eq!(h.fake_rows(None).await, 0, "the rows written were removed");

    let (status, body) = h.post_plan("cleanup", json!({ "filler": 10 })).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "exactly the limit is fine: {body}"
    );
    assert_eq!(h.fake_rows(None).await, 10);
}

#[tokio::test]
async fn a_build_that_takes_too_long_is_cancelled_and_removed() {
    let h = Harness::tuned(|options| options.build_timeout = Duration::from_millis(300)).await;
    let (status, body) = h
        .post_plan("cleanup", json!({ "matched": [1], "hold": true }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(error_code(&body), "plan_build_timeout");
    assert!(
        h.planner().tokens()[0].is_cancelled(),
        "the planner is told to stop"
    );

    h.settle().await;
    assert_eq!(h.plan_rows().await, 0);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn only_a_few_plans_are_built_at_once() {
    let h = Harness::tuned(|options| options.max_concurrent_builds = 2).await;
    let hold = json!({ "matched": [1], "hold": true });
    let requests = async {
        tokio::join!(
            h.post_plan("cleanup", hold.clone()),
            h.post_plan("cleanup", hold.clone()),
            h.post_plan("cleanup", hold.clone()),
            h.post_plan("cleanup", hold.clone()),
        )
    };
    let control = async {
        h.planner()
            .wait_for("two builds that hold", |counters| counters.builds_held == 2)
            .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            h.planner().counters().builds_started,
            2,
            "the other two wait for a slot"
        );
        h.planner().open_build_gate();
    };
    let ((a, b, c, d), ()) = tokio::join!(requests, control);
    for (status, body) in [a, b, c, d] {
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let counters = h.planner().counters();
    assert_eq!(counters.builds_started, 4);
    assert_eq!(counters.builds_max_running, 2);
    assert_eq!(h.plan_rows().await, 4);
}

#[tokio::test]
async fn a_client_that_gives_up_cancels_the_build_and_leaves_nothing() {
    let h = Harness::new().await;
    h.abandon_plan(
        "cleanup",
        json!({ "matched": [1, 2], "hold": true }),
        "a build that holds",
        |counters| counters.builds_held == 1,
    )
    .await;
    assert!(
        h.planner().tokens()[0].is_cancelled(),
        "dropping the request cancels the build"
    );

    h.settle().await;
    assert_eq!(h.plan_rows().await, 0);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn a_plan_that_is_still_building_is_invisible() {
    let h = Harness::new().await;
    let lib = Seed::new(h.db()).volume("lib").await;
    let request = h.post_plan("cleanup", json!({ "matched": [1], "hold": true }));
    let control = async {
        h.planner()
            .wait_for("a build that holds", |counters| counters.builds_held == 1)
            .await;
        let id: i64 = sqlx::query_scalar("SELECT id FROM plans")
            .fetch_one(h.db().read())
            .await
            .unwrap();
        assert_eq!(h.raw_state(id).await.as_deref(), Some("building"));

        let (status, body) = h.get_plan(id).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(error_code(&body), "plan_not_found");
        let (status, body) = h.execute(id, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(error_code(&body), "plan_not_found");
        let (status, body) = browse::items(
            &h.app,
            json!({ "volume_id": lib, "dir": "", "plan": { "plan_id": id } }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(error_code(&body), "plan_not_found");

        let (status, _) = h.delete_plan(id).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            h.raw_state(id).await.as_deref(),
            Some("building"),
            "a delete does not touch a build in flight"
        );
        h.planner().open_build_gate();
        id
    };
    let ((status, plan), id) = tokio::join!(request, control);
    assert_eq!(status, StatusCode::CREATED, "{plan}");
    assert_eq!(plan["id"], id);
    assert_eq!(h.get_plan(id).await.0, StatusCode::OK);
}

#[tokio::test]
async fn no_plan_is_built_while_the_server_shuts_down() {
    let h = Harness::new().await;
    h.app.state.begin_shutdown();
    let (status, body) = h.post_plan("cleanup", json!({ "matched": [1] })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(error_code(&body), "shutting_down");
    assert!(h.planner().builds().is_empty());
    assert_eq!(h.plan_rows().await, 0);
}

#[tokio::test]
async fn deleting_a_plan_row_cascades_to_the_rows_of_the_planner() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2], "filler": 3 })).await;
    let other = h.new_plan(json!({ "matched": [3] })).await;
    assert_eq!(h.fake_rows(Some(id)).await, 5);

    sqlx::query("DELETE FROM plans WHERE id = ?")
        .bind(id)
        .execute(h.db().write())
        .await
        .unwrap();
    assert_eq!(h.fake_rows(Some(id)).await, 0);
    assert_eq!(h.fake_rows(Some(other)).await, 1);
}

// ---- reading, deleting, replacing ----

#[tokio::test]
async fn an_unknown_plan_is_not_found() {
    let h = Harness::new().await;
    let (status, body) = h.get_plan(4242).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");
    assert!(message(&body).contains("4242"), "{body}");
}

#[tokio::test]
async fn a_plan_id_must_be_an_integer() {
    let h = Harness::new().await;
    for path in ["/api/plans/abc", "/api/plans/1.5", "/api/plans/-"] {
        let (status, body) = h.app.call(Method::GET, path, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(error_code(&body), "invalid_path", "{path}");
    }
    let (status, body) = h
        .app
        .call(Method::POST, "/api/plans/abc/execute", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_path");
}

#[tokio::test]
async fn deleting_a_plan_hides_it_at_once_and_removes_its_rows_in_the_background() {
    let h = Harness::new().await;
    let keep = h.new_plan(json!({ "matched": [1] })).await;
    let id = h.new_plan(json!({ "matched": [2], "filler": 3 })).await;

    let (status, body) = h.delete_plan(id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(body, Value::Null);
    let (status, body) = h.get_plan(id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");

    h.settle().await;
    assert_eq!(h.raw_state(id).await, None);
    assert_eq!(h.fake_rows(Some(id)).await, 0);
    assert_eq!(
        h.fake_rows(Some(keep)).await,
        1,
        "other plans are untouched"
    );
    assert_eq!(h.get_plan(keep).await.0, StatusCode::OK);
}

#[tokio::test]
async fn deleting_is_idempotent() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    for _ in 0..2 {
        let (status, _) = h.delete_plan(id).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    h.settle().await;
    let (status, _) = h.delete_plan(id).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "even after the rows are gone"
    );
    let (status, _) = h.delete_plan(4242).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "and for a plan that never existed"
    );
}

#[tokio::test]
async fn a_plan_that_replaces_another_retires_it() {
    let h = Harness::new().await;
    let old = h.new_plan(json!({ "matched": [1], "filler": 2 })).await;
    let (status, new) = h
        .app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({ "kind": "cleanup", "params": { "matched": [2] }, "replaces": old })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{new}");
    let new_id = new["id"].as_i64().unwrap();

    let (status, body) = h.get_plan(old).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");
    h.settle().await;
    assert_eq!(h.raw_state(old).await, None);
    assert_eq!(h.fake_rows(Some(old)).await, 0);
    assert_eq!(h.fake_rows(Some(new_id)).await, 1);
    assert_eq!(h.get_plan(new_id).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_failed_build_keeps_the_plan_it_would_have_replaced() {
    let h = Harness::new().await;
    let old = h.new_plan(json!({ "matched": [1] })).await;
    let (status, body) = h
        .app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({ "kind": "cleanup", "params": { "fail": "invalid" }, "replaces": old })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    h.settle().await;
    assert_eq!(h.get_plan(old).await.0, StatusCode::OK);
    assert_eq!(h.fake_rows(Some(old)).await, 1);
}

#[tokio::test]
async fn replacing_a_plan_that_does_not_exist_is_not_an_error() {
    let h = Harness::new().await;
    let (status, new) = h
        .app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({ "kind": "cleanup", "params": { "matched": [1] }, "replaces": 4242 })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{new}");
}

// ---- the CSRF marker ----

#[tokio::test]
async fn changing_requests_need_the_csrf_header() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let requests = [
        (
            "POST",
            "/api/plans".to_owned(),
            json!({ "kind": "cleanup" }),
        ),
        ("DELETE", format!("/api/plans/{id}"), Value::Null),
        ("POST", format!("/api/plans/{id}/execute"), json!({})),
        ("POST", format!("/api/plans/{id}/selection"), json!({})),
        ("POST", "/api/selections".to_owned(), json!({ "ids": [1] })),
    ];
    for (method, path, body) in requests {
        let request = Request::builder()
            .method(method)
            .uri(&path)
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = h.app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error_code(&reply), "csrf_rejected", "{method} {path}");
    }
    assert_eq!(h.planner().builds().len(), 1, "no plan was built");
    assert!(h.planner().executions().is_empty());
    assert_eq!(h.raw_state(id).await.as_deref(), Some("ready"));
    assert_eq!(
        h.get_plan(id).await.0,
        StatusCode::OK,
        "nothing was deleted"
    );
}

// ---- expiry and garbage collection ----

#[tokio::test]
async fn an_unused_plan_expires_and_a_sweep_removes_it() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1], "filler": 2 })).await;
    let kept = h.new_plan(json!({ "matched": [2] })).await;
    h.clock.advance(2 * 60 * 60 + 1);

    let (status, body) = h.get_plan(id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");
    assert_eq!(
        h.raw_state(id).await.as_deref(),
        Some("ready"),
        "the row waits for the sweep"
    );

    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(
        swept,
        Swept {
            plans: 2,
            building: 0,
            selections: 0
        }
    );
    assert_eq!(h.raw_state(id).await, None);
    assert_eq!(h.raw_state(kept).await, None);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn a_sweep_leaves_the_plans_that_are_alive() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    h.clock.advance(60);
    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(swept, Swept::default());
    assert_eq!(h.get_plan(id).await.0, StatusCode::OK);
    assert_eq!(h.fake_rows(Some(id)).await, 1);
}

#[tokio::test]
async fn reading_a_plan_keeps_it_alive() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let created = h.clock.now();

    h.clock.advance(7000);
    assert_eq!(h.get_plan(id).await.0, StatusCode::OK);
    h.settle().await;
    assert_eq!(h.raw_expires_at(id).await, created + 7000 + 7200);

    h.clock.advance(7000);
    assert_eq!(
        h.get_plan(id).await.0,
        StatusCode::OK,
        "past the original deadline, inside the extended one"
    );
}

#[tokio::test]
async fn the_deadline_moves_only_when_that_gains_a_minute() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let created = h.clock.now();

    for elapsed in [0, 30] {
        h.clock.advance(elapsed);
        assert_eq!(h.get_plan(id).await.0, StatusCode::OK);
        h.settle().await;
        assert_eq!(
            h.raw_expires_at(id).await,
            created + 7200,
            "{elapsed} s after the build: nothing worth writing"
        );
    }
    h.clock.advance(70);
    assert_eq!(h.get_plan(id).await.0, StatusCode::OK);
    h.settle().await;
    assert_eq!(h.raw_expires_at(id).await, created + 100 + 7200);
}

#[tokio::test]
async fn a_sweep_removes_the_leftovers_of_builds_that_never_ended() {
    let h = Harness::new().await;
    let now = h.clock.now();
    let stale = h.insert_raw("cleanup", "building", now - 301, now).await;
    let fresh = h.insert_raw("cleanup", "building", now - 10, now).await;
    add_fake_row(&h, stale).await;
    add_fake_row(&h, fresh).await;

    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(
        swept,
        Swept {
            plans: 0,
            building: 1,
            selections: 0
        }
    );
    assert_eq!(h.raw_state(stale).await, None);
    assert_eq!(h.fake_rows(Some(stale)).await, 0);
    assert_eq!(
        h.raw_state(fresh).await.as_deref(),
        Some("building"),
        "a build that may still run is left alone"
    );
    assert_eq!(h.fake_rows(Some(fresh)).await, 1);
}

// ---- startup ----

#[tokio::test]
async fn a_restart_removes_the_builds_it_interrupted_but_not_the_ones_it_starts() {
    let h = Harness::new().await;
    let now = h.clock.now();
    let leftover = h.insert_raw("cleanup", "building", now, now).await;
    add_fake_row(&h, leftover).await;
    let ready = h.new_plan(json!({ "matched": [1] })).await;

    let engine = h.engine(|_| {}).await;
    // A build that starts after the engine did is not a leftover of the previous run.
    let started_later = h.insert_raw("cleanup", "building", now, now).await;
    add_fake_row(&h, started_later).await;
    engine.settle().await;

    assert_eq!(h.raw_state(leftover).await, None);
    assert_eq!(h.fake_rows(Some(leftover)).await, 0);
    assert_eq!(
        h.raw_state(started_later).await.as_deref(),
        Some("building")
    );
    assert_eq!(h.fake_rows(Some(started_later)).await, 1);
    assert_eq!(h.raw_state(ready).await.as_deref(), Some("ready"));
    engine.shutdown().await;
}

#[tokio::test]
async fn a_restart_sweeps_the_plans_that_expired_meanwhile() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1], "filler": 2 })).await;
    h.clock.advance(3 * 60 * 60);

    let engine = h.engine(|_| {}).await;
    engine.settle().await;
    assert_eq!(h.raw_state(id).await, None);
    assert_eq!(h.fake_rows(None).await, 0);
    engine.shutdown().await;
}
