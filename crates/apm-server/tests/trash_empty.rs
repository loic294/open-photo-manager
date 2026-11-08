//! `POST /api/trash/empty` over the real application: the only way to delete for good. What the
//! request covers, what it never touches and how a failing item is reported.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use apm_core::dto::{ServerEvent, TaskKind, TaskState, TrashEmptyRequest, ValidationErrorBody};
use apm_server::fsops::TrashBatch;
use apm_server::tasks::TaskSpec;
use axum::http::StatusCode;
use common::fsops::{disk_dirs, disk_files};
use common::trash::{Parking, Row, TrashFx, within};
use serde_json::{Value, json};
use tokio::sync::Notify;

const PAYLOAD: &[u8] = b"the first payload";
const OTHER: &[u8] = b"another payload, a longer one";
const EMPTY: &str = "/api/trash/empty";

fn request(volume: Option<i64>, up_to_id: i64) -> Value {
    json!({ "volume_id": volume, "up_to_id": up_to_id })
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap()
}

fn field_names(body: &Value) -> Vec<String> {
    let body: ValidationErrorBody = serde_json::from_value(body.clone()).unwrap();
    body.error.fields.into_iter().map(|f| f.field).collect()
}

/// The `max_id` of the summary, what the page sends back when the user confirms.
async fn max_id(fx: &TrashFx) -> i64 {
    fx.list("limit=1").await["summary"]["max_id"]
        .as_i64()
        .unwrap()
}

/// Everything on disk below a volume that is not a user file: the trash content.
fn trash_content(fx: &TrashFx, volume: i64) -> Vec<String> {
    let root = fx.tree(volume).root();
    let mut found: Vec<String> = disk_files(root)
        .into_keys()
        .filter(|path| path.starts_with(".apm-trash/"))
        .collect();
    found.extend(
        disk_dirs(root)
            .into_iter()
            .filter(|dir| dir.starts_with(".apm-trash/")),
    );
    found
}

async fn journal(fx: &TrashFx) -> Vec<(String, String)> {
    sqlx::query_as("SELECT op, state FROM fs_journal ORDER BY id")
        .fetch_all(fx.app.state.db.read())
        .await
        .unwrap()
}

#[tokio::test]
async fn emptying_deletes_what_the_user_saw_for_good() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await;
    fx.trash(fx.a, "y/IMG_2.jpg", OTHER).await;
    fx.trash(fx.b, "z/IMG_3.jpg", b"third").await;
    fx.ta.write("keep/IMG_9.jpg", b"not trashed");
    let seen = max_id(&fx).await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.kind, TaskKind::TrashEmpty);
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 3 items");
    assert_eq!((task.items_done, task.items_total), (3, Some(3)));
    let bytes = (PAYLOAD.len() + OTHER.len() + 5) as i64;
    assert_eq!((task.bytes_done, task.bytes_total), (bytes, Some(bytes)));
    assert_eq!(
        fx.result_of(task.id).await.unwrap(),
        json!({ "deleted": 3, "skipped": 0, "failed": 0, "bytes": bytes, "failures": [] })
    );
    assert!(fx.ids().await.is_empty());
    assert!(
        trash_content(&fx, fx.a).is_empty(),
        "{:?}",
        trash_content(&fx, fx.a)
    );
    assert!(trash_content(&fx, fx.b).is_empty());
    assert_eq!(
        fx.files(fx.a).keys().collect::<Vec<_>>(),
        ["keep/IMG_9.jpg"]
    );
    assert!(fx.files(fx.b).is_empty());

    let summary = &fx.list("").await["summary"];
    assert_eq!(
        (summary["items"].as_i64(), summary["bytes"].as_i64()),
        (Some(0), Some(0))
    );

    let deletes = journal(&fx)
        .await
        .into_iter()
        .filter(|(op, _)| op == "delete")
        .collect::<Vec<_>>();
    assert_eq!(deletes.len(), 3, "{deletes:?}");
    assert!(deletes.iter().all(|(_, state)| state == "done"));
}

#[tokio::test]
async fn what_was_trashed_after_the_user_looked_survives() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/old1.jpg", PAYLOAD).await;
    fx.trash(fx.a, "x/old2.jpg", OTHER).await;
    let seen = max_id(&fx).await;
    let newer = fx.trash(fx.a, "x/new.jpg", b"newer").await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 2 items");
    assert_eq!(fx.ids().await, vec![newer.trash_item_id]);
    let kept = fx.trashed_files(fx.a);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        kept[&newer.trash_path.as_str()[".apm-trash/".len()..]],
        b"newer"
    );
}

#[tokio::test]
async fn an_id_is_never_given_twice_so_a_confirmed_number_never_covers_later_items() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    let seen = max_id(&fx).await;
    fx.run(EMPTY, request(None, seen)).await;
    assert!(fx.ids().await.is_empty());

    let later = fx.trash(fx.a, "x/b.jpg", OTHER).await;
    assert!(later.trash_item_id > seen, "ids are not reused");
    let (status, body) = fx.post(EMPTY, request(None, seen)).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "trash_already_empty");
    assert_eq!(fx.ids().await, vec![later.trash_item_id]);
}

#[tokio::test]
async fn one_volume_can_be_emptied_alone() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    let on_b = fx.trash(fx.b, "x/b.jpg", OTHER).await;
    let seen = max_id(&fx).await;

    let task = fx.run(EMPTY, request(Some(fx.a), seen)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 1 item");
    assert_eq!(fx.ids().await, vec![on_b.trash_item_id]);
    assert!(trash_content(&fx, fx.a).is_empty());
    assert_eq!(fx.trashed_files(fx.b).len(), 1);
}

#[tokio::test]
async fn nothing_to_delete_is_a_conflict_and_queues_no_task() {
    let fx = TrashFx::new().await;

    let (status, body) = fx.post(EMPTY, request(None, 100)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "trash_already_empty");

    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    let seen = max_id(&fx).await;
    for (volume, up_to_id) in [(Some(fx.b), seen), (None, 0)] {
        let (status, body) = fx.post(EMPTY, request(volume, up_to_id)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{volume:?} {up_to_id}: {body}"
        );
        assert_eq!(error_code(&body), "trash_already_empty");
    }
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
    assert_eq!(fx.ids().await.len(), 1);
}

#[tokio::test]
async fn an_unknown_or_offline_volume_is_refused() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    let seen = max_id(&fx).await;

    let (status, body) = fx.post(EMPTY, request(Some(9_999), seen)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let outage = fx.take_offline(fx.a).await;
    let (status, body) = fx.post(EMPTY, request(Some(fx.a), seen)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "volume_offline");
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
    assert_eq!(fx.ids().await.len(), 1);

    fx.recover(outage).await;
    let task = fx.run(EMPTY, request(Some(fx.a), seen)).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
}

#[tokio::test]
async fn invalid_requests_are_refused() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;

    let (status, body) = fx.post(EMPTY, request(None, -1)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "validation_failed");
    assert_eq!(field_names(&body), ["up_to_id"]);

    for body in [
        json!({}),
        json!({ "volume_id": null }),
        json!({ "up_to_id": "all" }),
        json!({ "volume_id": "a", "up_to_id": 1 }),
    ] {
        let (status, answer) = fx.post(EMPTY, body.clone()).await;
        assert!(status.is_client_error(), "{body}: {status} {answer}");
        assert_eq!(error_code(&answer), "invalid_json", "{body}");
    }
    assert_eq!(fx.ids().await.len(), 1);
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_row_that_names_more_than_one_item_is_refused_and_the_next_item_still_goes() {
    let fx = TrashFx::new().await;
    let shared = TrashBatch::adhoc();
    let corrupt = fx
        .trash_into(fx.a, "x/corrupt.jpg", PAYLOAD, &shared)
        .await
        .trash_item_id;
    let fine = fx.trash(fx.a, "x/fine.jpg", OTHER).await.trash_item_id;
    let seen = max_id(&fx).await;
    let bystander = fx
        .trash_into(fx.a, "x/bystander.jpg", b"not selected", &shared)
        .await;
    // The row now names a whole batch folder, which holds the bystander too.
    sqlx::query("UPDATE trash_items SET trash_path = ? WHERE id = ?")
        .bind(format!(".apm-trash/{}", shared.name()))
        .bind(corrupt)
        .execute(fx.app.state.db.write())
        .await
        .unwrap();

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.title, "Deleted 1 of 2 items");
    let error = task.error.unwrap();
    assert!(
        error.starts_with("Deleted 1 of 2 items (1 failed): x/corrupt.jpg (outside_trash:"),
        "{error}"
    );
    assert_eq!(fx.ids().await, vec![corrupt, bystander.trash_item_id]);
    assert!(!fx.ids().await.contains(&fine));
    let bystander_file = fx.ta.path(bystander.trash_path.as_str());
    assert_eq!(
        std::fs::read(bystander_file).unwrap(),
        b"not selected",
        "a row can never make the server delete more than its own item"
    );
}

#[tokio::test]
async fn an_item_whose_content_is_already_gone_is_still_removed() {
    let fx = TrashFx::new().await;
    let lost = fx.trash(fx.a, "x/lost.jpg", PAYLOAD).await;
    std::fs::remove_file(fx.ta.path(lost.trash_path.as_str())).unwrap();
    let seen = max_id(&fx).await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 1 item");
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn a_read_only_volume_fails_its_items_and_the_others_go() {
    let fx = TrashFx::new().await;
    let on_a = fx.trash(fx.a, "x/a.jpg", PAYLOAD).await.trash_item_id;
    fx.trash(fx.b, "x/b.jpg", OTHER).await;
    let seen = max_id(&fx).await;
    let Some(outage) = fx.make_read_only(fx.a).await else {
        return;
    };

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.title, "Deleted 1 of 2 items");
    assert!(task.error.unwrap().contains("volume_read_only"));
    assert_eq!(fx.ids().await, vec![on_a]);
    assert_eq!(fx.trashed_files(fx.a).len(), 1);
    assert!(trash_content(&fx, fx.b).is_empty());

    fx.recover(outage).await;
    let task = fx.run(EMPTY, request(None, seen)).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn an_offline_volume_keeps_its_items_when_every_volume_is_emptied() {
    let fx = TrashFx::new().await;
    let on_a = fx.trash(fx.a, "x/a.jpg", PAYLOAD).await.trash_item_id;
    fx.trash(fx.b, "x/b.jpg", OTHER).await;
    let seen = max_id(&fx).await;
    let outage = fx.take_offline(fx.a).await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Failed);
    assert!(task.error.unwrap().contains("volume_offline"));
    assert_eq!(fx.ids().await, vec![on_a]);
    assert!(trash_content(&fx, fx.b).is_empty());

    fx.recover(outage).await;
    assert_eq!(
        fx.trashed_files(fx.a).len(),
        1,
        "nothing was lost while it was away"
    );
}

#[tokio::test]
async fn a_trashed_folder_is_deleted_with_everything_in_it_and_links_are_not_followed() {
    let fx = TrashFx::new().await;
    fx.tb.write("outside/precious.jpg", b"never to be deleted");
    fx.ta.write("trip/a.jpg", PAYLOAD);
    fx.ta.write("trip/sub/b.jpg", OTHER);
    std::os::unix::fs::symlink(
        fx.tb.path("outside"),
        fx.ta.path("trip/sub/link-to-a-folder"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        fx.tb.path("outside/precious.jpg"),
        fx.ta.path("trip/link-to-a-file.jpg"),
    )
    .unwrap();
    fx.trash_existing(fx.a, "trip", &TrashBatch::adhoc(), "user")
        .await;
    let seen = max_id(&fx).await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 1 item");
    assert!(
        trash_content(&fx, fx.a).is_empty(),
        "{:?}",
        trash_content(&fx, fx.a)
    );
    assert!(fx.files(fx.a).is_empty());
    assert_eq!(
        fx.files(fx.b)["outside/precious.jpg"],
        b"never to be deleted",
        "a link is unlinked, never followed"
    );
}

#[tokio::test]
async fn emptying_can_be_cancelled_between_two_items_and_resumed_by_emptying_again() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    for n in 1..=3 {
        fx.trash_into(fx.a, &format!("x/IMG_{n}.jpg"), PAYLOAD, &batch)
            .await;
    }
    let seen = max_id(&fx).await;
    let parking = Parking::new();
    let service = fx.service_with_hook(parking.clone());

    let task_id = service
        .empty(TrashEmptyRequest {
            volume_id: None,
            up_to_id: seen,
        })
        .await
        .unwrap();
    parking.arrived().await;
    fx.app.state.tasks.cancel(task_id).await.unwrap();
    parking.release();
    let task = fx.finished(task_id).await;

    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    let left = fx.ids().await;
    assert!(
        (2..=3).contains(&left.len()),
        "the item in flight finished, the others were never started: {left:?}"
    );

    let again = fx.run(EMPTY, request(None, seen)).await;
    assert_eq!(again.state, TaskState::Done, "{:?}", again.error);
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn a_second_request_for_the_same_items_queues_behind_and_finds_nothing_left() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    fx.trash(fx.a, "x/b.jpg", OTHER).await;
    let seen = max_id(&fx).await;
    // Both requests wait in the queue of file operations behind this task.
    let gate = Arc::new(Notify::new());
    let blocker = {
        let gate = Arc::clone(&gate);
        let spec = TaskSpec::new(TaskKind::FsMove, "Holding the queue");
        fx.app
            .state
            .tasks
            .submit(spec, move |_| async move {
                gate.notified().await;
                Ok(json!({}))
            })
            .await
            .unwrap()
    };

    let first = fx.queue(EMPTY, request(None, seen)).await;
    let second = fx.queue(EMPTY, request(None, seen)).await;
    assert_eq!(
        fx.app.state.tasks.get(second).await.unwrap().state,
        TaskState::Queued
    );
    gate.notify_one();

    assert_eq!(fx.finished(blocker).await.state, TaskState::Done);
    let first = fx.finished(first).await;
    let second = fx.finished(second).await;
    assert_eq!(first.state, TaskState::Done, "{:?}", first.error);
    assert_eq!(first.title, "Deleted 2 items");
    assert_eq!(second.state, TaskState::Done, "{:?}", second.error);
    assert_eq!(second.title, "Deleted 0 items");
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn rows_without_files_are_deleted_by_the_same_request() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, "20240101-000000-t1", "ghost.jpg").size(10))
        .await;
    let seen = max_id(&fx).await;

    let task = fx.run(EMPTY, request(None, seen)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn progress_and_the_end_of_the_task_are_published() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/a.jpg", PAYLOAD).await;
    let seen = max_id(&fx).await;
    let mut events = fx.app.state.tasks.subscribe();

    let id = fx.queue(EMPTY, request(None, seen)).await;

    let last = within(async {
        loop {
            if let ServerEvent::Task { task } = events.recv().await.unwrap()
                && task.id == id
                && task.state == TaskState::Done
            {
                break task;
            }
        }
    })
    .await;
    assert_eq!(last.kind, TaskKind::TrashEmpty);
    assert_eq!(last.title, "Deleted 1 item");
}
