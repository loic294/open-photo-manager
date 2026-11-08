//! Running a plan: `POST /api/plans/{id}/execute`, the `plan_execute` task, progress,
//! cancellation, the result a plan keeps, and what becomes of a run when the server stops. The
//! real application runs around the scripted planner of `common::fake_planner`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{TaskDto, TaskKind, TaskState};
use axum::http::{Method, StatusCode};
use common::browse::error_code;
use common::fake_planner::{Call, FILLER_BASE, Harness, SECRET};
use common::seed::Seed;
use serde_json::{Value, json};

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

/// The plan as the API shows it.
async fn plan(h: &Harness, id: i64) -> Value {
    let (status, body) = h.get_plan(id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

// ---- the lifecycle ----

#[tokio::test]
async fn a_plan_runs_as_a_task_and_keeps_its_result() {
    let h = Harness::new().await;
    let id = h
        .new_plan(json!({ "matched": [1, 2, 3], "filler": 2 }))
        .await;
    let options = json!({ "bytes": 10, "fail": [2], "skip": [3] });

    let task = h.run(id, options.clone()).await;
    assert_eq!(task.kind, TaskKind::PlanExecute);
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.items_total, Some(5));
    assert_eq!(task.items_done, 5, "every item counts, failed or not");
    assert_eq!(task.bytes_total, Some(50));
    assert_eq!(
        task.bytes_done, 30,
        "only the items that were done count bytes"
    );
    assert_eq!(task.title, "Handled 3 of 5 items, 1 skipped, 1 failed");
    assert_eq!(task.error, None);

    let plan = plan(&h, id).await;
    assert_eq!(
        plan["state"], "executed",
        "executed by the time the task is over"
    );
    assert_eq!(plan["task_id"], task.id);
    assert_eq!(plan["stats"], json!({}));
    assert_eq!(
        plan["result"],
        json!({
            "done": 3,
            "skipped": 1,
            "failed": 1,
            "bytes": 30,
            "failures": [{
                "file_id": 2,
                "path": "fake/2",
                "code": "source_changed",
                "message": "the file changed",
            }],
            "cancelled": false,
            "note": null,
        })
    );

    assert_eq!(
        h.planner().executions(),
        vec![Call::Execute {
            plan_id: id,
            options: options.clone(),
            only: None,
            only_ids: None,
        }]
    );
    assert!(h.planner().calls().contains(&Call::PrepareExecute {
        plan_id: id,
        options,
    }));
    assert_eq!(
        h.planner().processed(),
        vec![1, 2, 3, FILLER_BASE, FILLER_BASE + 1]
    );
}

#[tokio::test]
async fn executing_answers_with_the_task_before_it_ends() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;

    let (status, body) = h.execute(id, json!({ "hold": true })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["kind"], "plan_execute");
    assert_eq!(body["title"], format!("Run fake plan {id}"));
    assert!(
        matches!(body["state"].as_str(), Some("queued" | "running")),
        "{body}"
    );
    let task_id = body["id"].as_i64().unwrap();

    let running = plan(&h, id).await;
    assert_eq!(running["state"], "executing");
    assert_eq!(running["task_id"], task_id);
    assert_eq!(running["result"], Value::Null);

    let (status, again) = h.execute(id, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(error_code(&again), "plan_not_ready");

    h.planner().open_exec_gate();
    let task = h.finish(task_id).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(plan(&h, id).await["state"], "executed");
}

#[tokio::test]
async fn absent_options_reach_the_planner_as_an_empty_object() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let (status, body) = h
        .app
        .call(
            Method::POST,
            &format!("/api/plans/{id}/execute"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    h.finish(body["id"].as_i64().unwrap()).await;
    assert_eq!(
        h.planner().executions(),
        vec![Call::Execute {
            plan_id: id,
            options: json!({}),
            only: None,
            only_ids: None,
        }]
    );
}

#[tokio::test]
async fn a_plan_runs_once() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    h.run(id, json!({})).await;

    let (status, body) = h.execute(id, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "plan_executed");
    assert_eq!(h.planner().executions().len(), 1);
    assert_eq!(h.planner().processed(), vec![1]);
}

#[tokio::test]
async fn racing_executions_start_exactly_one_task() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2] })).await;
    let options = json!({ "hold": true });

    let (a, b, c) = tokio::join!(
        h.execute(id, options.clone()),
        h.execute(id, options.clone()),
        h.execute(id, options.clone()),
    );
    let replies = [a, b, c];
    let accepted: Vec<&Value> = replies
        .iter()
        .filter(|(status, _)| *status == StatusCode::ACCEPTED)
        .map(|(_, body)| body)
        .collect();
    assert_eq!(accepted.len(), 1, "{replies:?}");
    for (status, body) in replies.iter().filter(|(s, _)| *s != StatusCode::ACCEPTED) {
        assert_eq!(*status, StatusCode::CONFLICT, "{body}");
        assert_eq!(error_code(body), "plan_not_ready");
    }

    h.planner().open_exec_gate();
    h.finish(accepted[0]["id"].as_i64().unwrap()).await;
    assert_eq!(h.planner().executions().len(), 1, "the plan ran once");
    assert_eq!(h.planner().processed(), vec![1, 2]);
}

#[tokio::test]
async fn an_unknown_or_expired_plan_cannot_be_executed() {
    let h = Harness::new().await;
    let (status, body) = h.execute(4242, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");

    let id = h.new_plan(json!({ "matched": [1] })).await;
    h.clock.advance(2 * 60 * 60 + 1);
    let (status, body) = h.execute(id, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "plan_not_found");
    assert!(h.planner().executions().is_empty());
}

#[tokio::test]
async fn a_kind_without_a_planner_cannot_be_executed() {
    let h = Harness::new().await;
    let now = h.clock.now();
    let id = h.insert_raw("duplicates", "ready", now, now + 7200).await;
    let (status, body) = h.execute(id, json!({})).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(error_code(&body), "not_implemented");
    assert!(
        message(&body).contains("implementing-duplicate-matching"),
        "{body}"
    );
    assert_eq!(h.raw_state(id).await.as_deref(), Some("ready"));
}

// ---- options ----

#[tokio::test]
async fn options_the_planner_rejects_start_nothing() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;

    let (status, body) = h.execute(id, json!({ "reject": "mode" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "invalid_plan_options");
    assert!(message(&body).contains("mode"), "names the field: {body}");

    assert!(h.app.state.tasks.list(100).await.unwrap().is_empty());
    assert!(h.planner().executions().is_empty());
    assert_eq!(
        plan(&h, id).await["state"],
        "ready",
        "the plan is still there"
    );

    let task = h.run(id, json!({})).await;
    assert_eq!(task.state, TaskState::Done, "and can still be run");
}

#[tokio::test]
async fn options_that_are_not_an_object_or_not_known_are_refused() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    for options in [json!([1]), json!("fast"), json!(3), json!({ "bogus": 1 })] {
        let (status, body) = h.execute(id, options.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{options}: {body}");
        assert_eq!(error_code(&body), "invalid_plan_options", "{options}");
    }
    assert!(h.planner().executions().is_empty());
    assert_eq!(plan(&h, id).await["state"], "ready");
}

#[tokio::test]
async fn an_execution_can_be_restricted_to_a_selection() {
    let h = Harness::new().await;
    let seed = Seed::new(h.db());
    let lib = seed.volume("lib").await;
    let a = seed.file(lib, "", "a.jpg").insert().await;
    let b = seed.file(lib, "", "b.jpg").insert().await;
    let c = seed.file(lib, "", "c.jpg").insert().await;
    let id = h.new_plan(json!({ "matched": [a, b, c] })).await;
    let only = seed.selection(&[c, a]).await;

    let (status, body) = h.execute_only(id, json!({}), only).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = h.finish(body["id"].as_i64().unwrap()).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.items_total, Some(2));

    let mut involved = vec![a, c];
    involved.sort_unstable();
    assert_eq!(
        h.planner().executions(),
        vec![Call::Execute {
            plan_id: id,
            options: json!({}),
            only: Some(only),
            only_ids: Some(involved),
        }]
    );
    assert_eq!(h.planner().processed(), vec![a, c], "b was not selected");
    assert_eq!(plan(&h, id).await["result"]["done"], 2);
}

#[tokio::test]
async fn a_selection_that_does_not_exist_cannot_restrict_a_run() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let (status, body) = h.execute_only(id, json!({}), 4242).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "selection_not_found");
    assert!(h.planner().executions().is_empty());
    assert_eq!(
        plan(&h, id).await["state"],
        "ready",
        "the plan can still run"
    );
}

// ---- progress and cancellation ----

#[tokio::test]
async fn a_running_plan_reports_progress_and_can_be_cancelled() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2, 3, 4, 5] })).await;
    let task = h.start(id, json!({ "stop_after": 2, "bytes": 7 })).await;
    h.planner()
        .wait_for("two items", |counters| counters.items_processed == 2)
        .await;

    let running = h.task(task.id).await;
    assert_eq!(running.state, TaskState::Running);
    assert_eq!(running.items_done, 2);
    assert_eq!(running.items_total, Some(5));
    assert_eq!(running.bytes_done, 14);
    assert_eq!(running.bytes_total, Some(35));
    assert_eq!(plan(&h, id).await["state"], "executing");

    h.cancel_task(task.id).await;
    let ended = h.finish(task.id).await;
    assert_eq!(ended.state, TaskState::Cancelled);
    assert_eq!(ended.title, "Handled 2 of 5 items (cancelled)");

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed", "a cancelled run ends the plan");
    assert_eq!(plan["result"]["cancelled"], true);
    assert_eq!(plan["result"]["done"], 2);
    assert_eq!(plan["result"]["bytes"], 14);
    assert_eq!(h.planner().processed(), vec![1, 2]);

    let (status, body) = h.execute(id, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "plan_executed");
}

#[tokio::test]
async fn a_failure_of_the_planner_is_recorded_without_its_details() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let task = h.run(id, json!({ "error": "internal" })).await;
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.error.as_deref(), Some("internal server error"));

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["result"]["note"], "internal server error");
    assert_eq!(plan["result"]["cancelled"], false);
    assert_eq!(plan["result"]["done"], 0);
    assert!(!plan.to_string().contains(SECRET), "{plan}");
    let task_json = serde_json::to_string(&task).unwrap();
    assert!(!task_json.contains(SECRET), "{task_json}");
}

#[tokio::test]
async fn an_error_the_planner_explains_is_recorded_as_a_note() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let task = h.run(id, json!({ "error": "invalid" })).await;
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.error.as_deref(), Some("the fake gave up"));

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["result"]["note"], "the fake gave up");
}

#[tokio::test]
async fn a_planner_that_reports_cancellation_ends_the_task_as_cancelled() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let task = h.run(id, json!({ "error": "cancelled" })).await;
    assert_eq!(task.state, TaskState::Cancelled);

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["result"]["cancelled"], true);
}

#[tokio::test]
async fn the_result_lists_only_the_first_failures() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "filler": 250 })).await;
    let task = h.run(id, json!({ "fail_all": true })).await;
    assert_eq!(task.title, "Handled 0 of 250 items, 250 failed");

    let plan = plan(&h, id).await;
    assert_eq!(plan["result"]["failed"], 250, "every failure is counted");
    let failures = plan["result"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 200, "the first ones are described");
    assert_eq!(failures[0]["file_id"], FILLER_BASE);
    assert_eq!(failures[0]["path"], format!("fake/{FILLER_BASE}"));
}

#[tokio::test]
async fn a_plan_with_many_rows_is_written_run_and_removed_in_chunks() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "filler": 4_500 })).await;
    assert_eq!(h.fake_rows(Some(id)).await, 4_500);

    let task = h.run(id, json!({})).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.items_done, 4_500);
    assert_eq!(h.planner().processed().len(), 4_500);

    let (status, _) = h.delete_plan(id).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    h.settle().await;
    assert_eq!(h.fake_rows(None).await, 0);
    assert_eq!(h.plan_rows().await, 0);
}

// ---- the queue ----

#[tokio::test]
async fn a_plan_waits_in_the_queue_for_its_lane() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2] })).await;
    let block = h.block_lane().await;

    let task = h.start(id, json!({})).await;
    assert_eq!(task.state, TaskState::Queued);
    assert_eq!(h.planner().counters().executions_started, 0);
    assert_eq!(plan(&h, id).await["state"], "executing", "claimed at once");

    block.release();
    let task = h.finish(task.id).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(plan(&h, id).await["state"], "executed");
    assert_eq!(h.planner().processed(), vec![1, 2]);
}

#[tokio::test]
async fn cancelling_a_queued_plan_ends_it_without_running_it() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2] })).await;
    let block = h.block_lane().await;

    let task = h.start(id, json!({})).await;
    h.cancel_task(task.id).await;
    assert_eq!(h.finish(task.id).await.state, TaskState::Cancelled);
    block.release();

    let plan = h.wait_state(id, "executed").await;
    assert_eq!(plan["result"]["cancelled"], true);
    assert_eq!(plan["result"]["note"], "cancelled before it ran");
    assert_eq!(plan["result"]["done"], 0);
    assert_eq!(plan["task_id"], task.id);
    assert!(h.planner().executions().is_empty(), "the planner never ran");
}

// ---- a plan that is running ----

#[tokio::test]
async fn a_plan_that_is_running_cannot_be_deleted() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1], "filler": 2 })).await;
    let task = h.start(id, json!({ "hold": true })).await;

    let (status, body) = h.delete_plan(id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "plan_executing");
    assert_eq!(h.raw_state(id).await.as_deref(), Some("executing"));
    assert_eq!(h.fake_rows(Some(id)).await, 3, "the rows are intact");

    h.planner().open_exec_gate();
    h.finish(task.id).await;
    let (status, _) = h.delete_plan(id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "a finished plan can go");
    h.settle().await;
    assert_eq!(h.raw_state(id).await, None);
    assert_eq!(h.fake_rows(None).await, 0);
}

#[tokio::test]
async fn a_plan_that_is_running_is_not_retired_by_a_new_one() {
    let h = Harness::new().await;
    let old = h.new_plan(json!({ "matched": [1], "filler": 2 })).await;
    let task = h.start(old, json!({ "hold": true })).await;

    let (status, new) = h
        .app
        .call(
            Method::POST,
            "/api/plans",
            Some(json!({ "kind": "cleanup", "params": { "matched": [2] }, "replaces": old })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{new}");
    assert_eq!(h.raw_state(old).await.as_deref(), Some("executing"));
    assert_eq!(h.fake_rows(Some(old)).await, 3);

    h.planner().open_exec_gate();
    h.finish(task.id).await;
    h.settle().await;
    assert_eq!(plan(&h, old).await["state"], "executed");
    assert_eq!(
        h.fake_rows(Some(old)).await,
        3,
        "its rows were never removed"
    );
    assert_eq!(
        h.planner().processed(),
        vec![1, FILLER_BASE, FILLER_BASE + 1]
    );
}

#[tokio::test]
async fn a_plan_that_is_running_does_not_expire_and_gets_a_fresh_deadline_when_done() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let task = h.start(id, json!({ "hold": true })).await;

    h.clock.advance(10_000);
    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(swept.plans, 0, "a running plan is never swept");
    assert_eq!(plan(&h, id).await["state"], "executing");
    assert_eq!(h.fake_rows(Some(id)).await, 1);

    h.planner().open_exec_gate();
    h.finish(task.id).await;
    assert_eq!(plan(&h, id).await["state"], "executed");
    assert_eq!(
        h.raw_expires_at(id).await,
        h.clock.now() + 2 * 60 * 60,
        "the result stays readable for a while after the run"
    );
    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(swept.plans, 0);
}

#[tokio::test]
async fn an_executed_plan_expires_like_any_other() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    h.run(id, json!({})).await;
    h.clock.advance(2 * 60 * 60 + 1);

    let (status, body) = h.get_plan(id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let swept = h.app.state.plans.sweep().await.unwrap();
    assert_eq!(swept.plans, 1);
    assert_eq!(h.raw_state(id).await, None);
}

#[tokio::test]
async fn forgetting_the_task_keeps_the_plan_and_its_result() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let task = h.run(id, json!({})).await;

    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(task.id)
        .execute(h.db().write())
        .await
        .unwrap();
    h.clock.advance(10);

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["task_id"], Value::Null);
    assert_eq!(plan["result"]["done"], 1);
}

// ---- the server stops ----

#[tokio::test]
async fn a_restart_ends_the_runs_it_interrupted() {
    let h = Harness::new().await;
    let now = h.clock.now();
    let id = h.insert_raw("cleanup", "executing", now, now + 7200).await;
    let ready = h.new_plan(json!({ "matched": [1] })).await;

    let engine = h.engine(|_| {}).await;
    engine.settle().await;

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["result"]["cancelled"], true);
    assert_eq!(plan["result"]["note"], "interrupted by restart");
    assert_eq!(plan["result"]["done"], 0);
    assert_eq!(h.raw_state(ready).await.as_deref(), Some("ready"));
    engine.shutdown().await;
}

#[tokio::test]
async fn stopping_the_task_engine_ends_a_run_and_records_it() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1, 2, 3] })).await;
    let task = h.start(id, json!({ "stop_after": 1 })).await;
    h.planner()
        .wait_for("one item", |counters| counters.items_processed == 1)
        .await;

    h.app.state.tasks.shutdown().await;

    let plan = plan(&h, id).await;
    assert_eq!(plan["state"], "executed");
    assert_eq!(plan["result"]["cancelled"], true);
    assert_eq!(plan["result"]["done"], 1);
    assert_eq!(plan["task_id"], task.id);
}

#[tokio::test]
async fn a_queued_run_that_never_started_is_recorded_when_the_task_engine_stops() {
    let h = Harness::new().await;
    let id = h.new_plan(json!({ "matched": [1] })).await;
    let _block = h.block_lane().await;
    let task = h.start(id, json!({})).await;
    assert_eq!(task.state, TaskState::Queued);

    h.app.state.tasks.shutdown().await;

    let plan = h.wait_state(id, "executed").await;
    assert_eq!(plan["result"]["cancelled"], true);
    assert_eq!(plan["result"]["note"], "interrupted by restart");
    assert!(h.planner().executions().is_empty());
    let ended: TaskDto = h.task(task.id).await;
    assert_eq!(ended.state, TaskState::Interrupted);
}
