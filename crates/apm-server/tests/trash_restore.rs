//! `POST /api/trash/restore` over the real application: the task it queues, what comes back, what
//! stays in the trash and how a failing item is reported.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{
    ConflictPolicy, ServerEvent, TaskKind, TaskState, TrashRestoreRequest, TrashTarget,
    ValidationErrorBody,
};
use apm_server::fsops::TrashBatch;
use axum::http::StatusCode;
use common::fsops::disk_dirs;
use common::trash::{Parking, TrashFx, within};
use serde_json::{Value, json};

const PAYLOAD: &[u8] = b"the first payload";
const OTHER: &[u8] = b"another payload, a longer one";
const RESTORE: &str = "/api/trash/restore";

fn ids(ids: &[i64]) -> Value {
    json!({ "target": { "kind": "ids", "ids": ids } })
}

fn ids_with(ids_: &[i64], policy: &str) -> Value {
    let mut body = ids(ids_);
    body["conflict_policy"] = json!(policy);
    body
}

fn deletion(volume: i64, batch: &TrashBatch) -> Value {
    json!({ "target": { "kind": "batch", "volume_id": volume, "batch": batch.name() } })
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap()
}

fn field_names(body: &Value) -> Vec<String> {
    let body: ValidationErrorBody = serde_json::from_value(body.clone()).unwrap();
    body.error.fields.into_iter().map(|f| f.field).collect()
}

/// What is still in the trash, by original path.
async fn left_in_trash(fx: &TrashFx) -> Vec<String> {
    let listing = fx.list("sort=path&order=asc").await;
    listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let dir = item["dir"].as_str().unwrap();
            let name = item["name"].as_str().unwrap();
            if dir.is_empty() {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            }
        })
        .collect()
}

#[tokio::test]
async fn ticked_items_come_back_and_the_rest_stays() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    let one = fx.trash_into(fx.a, "x/IMG_1.jpg", PAYLOAD, &batch).await;
    let two = fx.trash_into(fx.a, "x/IMG_2.jpg", OTHER, &batch).await;
    let kept = fx.trash_into(fx.a, "y/IMG_3.jpg", b"third", &batch).await;
    assert!(fx.files(fx.a).is_empty());

    let task = fx
        .run(RESTORE, ids(&[one.trash_item_id, two.trash_item_id]))
        .await;

    assert_eq!(task.kind, TaskKind::FsMove);
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Restored 2 items");
    assert_eq!((task.items_done, task.items_total), (2, Some(2)));
    let bytes = (PAYLOAD.len() + OTHER.len()) as i64;
    assert_eq!((task.bytes_done, task.bytes_total), (bytes, Some(bytes)));
    assert_eq!(task.error, None);

    let files = fx.files(fx.a);
    assert_eq!(files.len(), 2, "{files:?}");
    assert_eq!(files["x/IMG_1.jpg"], PAYLOAD);
    assert_eq!(files["x/IMG_2.jpg"], OTHER);
    assert_eq!(fx.ids().await, vec![kept.trash_item_id]);
    assert_eq!(left_in_trash(&fx).await, vec!["y/IMG_3.jpg"]);
    let trashed = fx.trashed_files(fx.a);
    assert_eq!(trashed.len(), 1, "{trashed:?}");
    assert_eq!(
        trashed[&kept.trash_path.as_str()[".apm-trash/".len()..]],
        b"third"
    );

    let result = fx.result_of(task.id).await.unwrap();
    assert_eq!(
        result,
        json!({
            "restored": 2, "skipped": 0, "failed": 0, "bytes": bytes, "failures": [],
        })
    );
    let indexed = fx.indexed(fx.a).await;
    assert!(indexed.contains("x/IMG_1.jpg") && indexed.contains("x/IMG_2.jpg"));
    assert!(!indexed.contains("y/IMG_3.jpg"));
}

#[tokio::test]
async fn a_restore_is_in_the_journal() {
    let fx = TrashFx::new().await;
    let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;

    let task = fx.run(RESTORE, ids(&[item])).await;
    assert_eq!(task.state, TaskState::Done);

    let ops: Vec<(String, String)> = sqlx::query_as("SELECT op, state FROM fs_journal ORDER BY id")
        .fetch_all(fx.app.state.db.read())
        .await
        .unwrap();
    let ops: Vec<(&str, &str)> = ops.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(ops, vec![("trash", "done"), ("restore", "done")]);
}

#[tokio::test]
async fn a_deletion_comes_back_as_a_whole_and_only_that_deletion() {
    let fx = TrashFx::new().await;
    let first = TrashBatch::new(1);
    let second = TrashBatch::new(2);
    fx.trash_into(fx.a, "x/a.jpg", PAYLOAD, &first).await;
    fx.trash_into(fx.a, "x/b.jpg", OTHER, &first).await;
    fx.trash_into(fx.a, "x/c.jpg", b"c", &second).await;
    fx.trash_into(fx.b, "x/d.jpg", b"d", &first).await;

    let task = fx.run(RESTORE, deletion(fx.a, &first)).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Restored 2 items");
    let files = fx.files(fx.a);
    assert_eq!(files.keys().collect::<Vec<_>>(), ["x/a.jpg", "x/b.jpg"]);
    assert_eq!(left_in_trash(&fx).await, vec!["x/c.jpg", "x/d.jpg"]);
    assert!(
        fx.files(fx.b).is_empty(),
        "the same batch name on another volume is another deletion"
    );
}

#[tokio::test]
async fn a_deletion_that_is_not_in_the_trash_is_a_404() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::new(1);
    fx.trash_into(fx.a, "x/a.jpg", PAYLOAD, &batch).await;

    let other = TrashBatch::new(9);
    let (status, body) = fx.post(RESTORE, deletion(fx.a, &other)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "batch_not_found");

    let (status, body) = fx.post(RESTORE, deletion(fx.b, &batch)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "batch_not_found");

    let (status, body) = fx.post(RESTORE, deletion(9_999, &batch)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    assert_eq!(left_in_trash(&fx).await, vec!["x/a.jpg"]);
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_taken_name_follows_the_policy_of_the_request() {
    // (policy, content of the file that took the name, restored, where the item ends up)
    let cases: [(&str, &[u8], bool); 8] = [
        ("smart", PAYLOAD, false),
        ("smart", OTHER, true),
        ("rename", PAYLOAD, true),
        ("rename", OTHER, true),
        ("skip", PAYLOAD, false),
        ("skip", OTHER, false),
        ("replace", PAYLOAD, true),
        ("replace", OTHER, true),
    ];
    for (policy, occupant, restored) in cases {
        let label = format!("{policy} over {} bytes", occupant.len());
        let fx = TrashFx::new().await;
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let item = fx
            .trash_existing(fx.a, "x/IMG_1.jpg", &TrashBatch::adhoc(), "user")
            .await
            .trash_item_id;
        fx.ta.write("x/IMG_1.jpg", occupant);
        fx.scan().await;

        let task = fx.run(RESTORE, ids_with(&[item], policy)).await;

        assert_eq!(task.state, TaskState::Done, "{label}: {:?}", task.error);
        let files = fx.files(fx.a);
        match (policy, restored) {
            (_, false) => {
                assert_eq!(
                    task.title, "Restored 0 of 1 item, 1 left in the trash",
                    "{label}"
                );
                assert_eq!(files["x/IMG_1.jpg"], occupant, "{label}");
                assert_eq!(files.len(), 1, "{label}: {files:?}");
                assert_eq!(fx.ids().await, vec![item], "{label}: it stays in the trash");
                assert_eq!(fx.trashed_files(fx.a).len(), 1, "{label}");
            }
            ("replace", true) => {
                assert_eq!(task.title, "Restored 1 item", "{label}");
                assert_eq!(files["x/IMG_1.jpg"], PAYLOAD, "{label}");
                assert_eq!(files.len(), 1, "{label}: {files:?}");
                let listing = fx.list("").await;
                let items = listing["items"].as_array().unwrap();
                assert_eq!(items.len(), 1, "{label}: the replaced file is recoverable");
                assert_eq!(items[0]["reason"], json!("replaced"), "{label}");
                assert_eq!(items[0]["name"], json!("IMG_1.jpg"), "{label}");
                let trashed = fx.trashed_files(fx.a);
                assert_eq!(trashed.values().collect::<Vec<_>>(), [&occupant.to_vec()]);
            }
            (_, true) => {
                assert_eq!(task.title, "Restored 1 item", "{label}");
                assert_eq!(files["x/IMG_1.jpg"], occupant, "{label}");
                assert_eq!(files["x/IMG_1_1.jpg"], PAYLOAD, "{label}");
                assert!(fx.ids().await.is_empty(), "{label}");
            }
        }
    }
}

#[tokio::test]
async fn a_request_without_a_policy_follows_the_setting() {
    for (setting, restored) in [("skip", false), ("rename", true)] {
        let fx = TrashFx::new().await;
        fx.configure(|settings| {
            settings.default_conflict_policy = if setting == "skip" {
                ConflictPolicy::Skip
            } else {
                ConflictPolicy::Rename
            };
        })
        .await;
        let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
        fx.ta.write("x/IMG_1.jpg", OTHER);

        // `null` and an omitted field mean the same.
        let mut body = ids(&[item]);
        if restored {
            body["conflict_policy"] = Value::Null;
        }
        let task = fx.run(RESTORE, body).await;

        assert_eq!(task.state, TaskState::Done, "{setting}: {:?}", task.error);
        let files = fx.files(fx.a);
        assert_eq!(files["x/IMG_1.jpg"], OTHER, "{setting}");
        assert_eq!(files.contains_key("x/IMG_1_1.jpg"), restored, "{setting}");
        assert_eq!(fx.ids().await.is_empty(), restored, "{setting}");
    }
}

#[tokio::test]
async fn a_trashed_folder_comes_back_whole_and_is_indexed() {
    let fx = TrashFx::new().await;
    fx.ta.write("trip/a.jpg", PAYLOAD);
    fx.ta.write("trip/sub/b.jpg", OTHER);
    fx.scan().await;
    let item = fx
        .trash_existing(fx.a, "trip", &TrashBatch::adhoc(), "user")
        .await;
    assert!(item.is_dir);
    assert!(fx.files(fx.a).is_empty());
    assert!(fx.indexed(fx.a).await.is_empty());

    let task = fx.run(RESTORE, ids(&[item.trash_item_id])).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Restored 1 item");
    let files = fx.files(fx.a);
    assert_eq!(files["trip/a.jpg"], PAYLOAD);
    assert_eq!(files["trip/sub/b.jpg"], OTHER);
    let indexed: Vec<String> = fx.indexed(fx.a).await.into_iter().collect();
    assert_eq!(indexed, ["trip/a.jpg", "trip/sub/b.jpg"]);
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn a_folder_whose_name_is_taken_comes_back_under_a_number_and_is_never_merged() {
    let fx = TrashFx::new().await;
    fx.ta.write("trip/a.jpg", PAYLOAD);
    let item = fx
        .trash_existing(fx.a, "trip", &TrashBatch::adhoc(), "user")
        .await
        .trash_item_id;
    fx.ta.write("trip/new.jpg", OTHER);

    let task = fx.run(RESTORE, ids_with(&[item], "rename")).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    let files = fx.files(fx.a);
    assert_eq!(files["trip/new.jpg"], OTHER);
    assert_eq!(files["trip_1/a.jpg"], PAYLOAD);
    assert!(!files.contains_key("trip/a.jpg"));

    // Replace never swallows a folder: the item fails and stays.
    let again = fx
        .trash_existing(fx.a, "trip_1", &TrashBatch::adhoc(), "user")
        .await
        .trash_item_id;
    fx.ta.write("trip_1/occupant.jpg", b"occupant");
    let task = fx.run(RESTORE, ids_with(&[again], "replace")).await;
    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.contains("destination_invalid"), "{error}");
    assert_eq!(fx.ids().await, vec![again]);
    assert_eq!(fx.files(fx.a)["trip_1/occupant.jpg"], b"occupant");
}

#[tokio::test]
async fn removed_parent_folders_are_created_again() {
    let fx = TrashFx::new().await;
    let item = fx
        .trash(fx.a, "deep/er/IMG_1.jpg", PAYLOAD)
        .await
        .trash_item_id;
    fx.ta.remove_dir("deep/er");
    fx.ta.remove_dir("deep");

    let task = fx.run(RESTORE, ids(&[item])).await;

    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(fx.files(fx.a)["deep/er/IMG_1.jpg"], PAYLOAD);
}

#[tokio::test]
async fn the_trash_folders_a_restore_empties_are_removed() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    let a = fx.trash_into(fx.a, "x/y/IMG_1.jpg", PAYLOAD, &batch).await;
    let b = fx.trash_into(fx.a, "x/IMG_2.jpg", OTHER, &batch).await;
    let dirs_below_trash = |fx: &TrashFx| -> Vec<String> {
        disk_dirs(fx.ta.root())
            .into_iter()
            .filter(|dir| dir.starts_with(".apm-trash/"))
            .collect()
    };
    assert!(!dirs_below_trash(&fx).is_empty());

    fx.run(RESTORE, ids(&[a.trash_item_id])).await;
    let left: Vec<String> = fx.trashed_files(fx.a).into_keys().collect();
    assert_eq!(
        left,
        [b.trash_path.as_str()[".apm-trash/".len()..].to_owned()]
    );
    assert!(
        dirs_below_trash(&fx)
            .iter()
            .all(|dir| !dir.ends_with("/x/y")),
        "the emptied folder is pruned: {:?}",
        dirs_below_trash(&fx)
    );

    fx.run(RESTORE, ids(&[b.trash_item_id])).await;
    assert!(
        dirs_below_trash(&fx).is_empty(),
        "nothing is left of the deletion: {:?}",
        dirs_below_trash(&fx)
    );
    assert_eq!(fx.files(fx.a).len(), 2);
}

#[tokio::test]
async fn a_missing_trash_file_fails_only_its_item() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    let lost = fx.trash_into(fx.a, "x/lost.jpg", PAYLOAD, &batch).await;
    let fine = fx.trash_into(fx.a, "x/fine.jpg", OTHER, &batch).await;
    std::fs::remove_file(fx.ta.path(lost.trash_path.as_str())).unwrap();

    let task = fx
        .run(RESTORE, ids(&[lost.trash_item_id, fine.trash_item_id]))
        .await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(
        error.starts_with("Restored 1 of 2 items (1 failed): x/lost.jpg (source_missing:"),
        "{error}"
    );
    assert_eq!(task.title, "Restored 1 of 2 items");
    assert_eq!((task.items_done, task.items_total), (2, Some(2)));
    assert_eq!(fx.files(fx.a).keys().collect::<Vec<_>>(), ["x/fine.jpg"]);
    assert_eq!(fx.ids().await, vec![lost.trash_item_id]);
}

#[tokio::test]
async fn ids_that_are_not_in_the_trash_are_failures_and_repeated_ids_count_once() {
    let fx = TrashFx::new().await;
    let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;

    let task = fx.run(RESTORE, ids(&[item, item, item, 9_999])).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.title, "Restored 1 of 2 items");
    assert_eq!((task.items_done, task.items_total), (2, Some(2)));
    let error = task.error.unwrap();
    assert!(
        error.contains("item 9999 (not_in_trash: the item is not in the trash (any more))"),
        "{error}"
    );
    assert!(
        error.starts_with("Restored 1 of 2 items (1 failed): "),
        "{error}"
    );
    assert_eq!(fx.files(fx.a)["x/IMG_1.jpg"], PAYLOAD);
}

#[tokio::test]
async fn restoring_twice_reports_that_the_item_is_gone() {
    let fx = TrashFx::new().await;
    let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
    let first = fx.run(RESTORE, ids(&[item])).await;
    assert_eq!(first.state, TaskState::Done);

    let second = fx.run(RESTORE, ids(&[item])).await;

    assert_eq!(second.state, TaskState::Failed);
    assert!(second.error.unwrap().contains("not_in_trash"));
    assert_eq!(fx.files(fx.a)["x/IMG_1.jpg"], PAYLOAD);
}

#[tokio::test]
async fn a_failure_among_skips_and_restores_is_told_with_every_count() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    fx.ta.write("x/taken.jpg", PAYLOAD);
    let taken = fx.trash_existing(fx.a, "x/taken.jpg", &batch, "user").await;
    fx.ta.write("x/taken.jpg", OTHER);
    let fine = fx.trash_into(fx.a, "x/fine.jpg", b"fine", &batch).await;
    let lost = fx.trash_into(fx.a, "x/lost.jpg", b"lost", &batch).await;
    std::fs::remove_file(fx.ta.path(lost.trash_path.as_str())).unwrap();
    let all = [taken.trash_item_id, fine.trash_item_id, lost.trash_item_id];

    let task = fx.run(RESTORE, ids_with(&all, "skip")).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.title, "Restored 1 of 3 items, 1 left in the trash");
    assert!(
        task.error
            .unwrap()
            .starts_with("Restored 1 of 3 items, 1 left in the trash (1 failed): x/lost.jpg"),
    );
    assert_eq!(
        fx.ids().await,
        vec![taken.trash_item_id, lost.trash_item_id]
    );
}

#[tokio::test]
async fn a_request_whose_only_volume_is_offline_is_refused_at_once() {
    let fx = TrashFx::new().await;
    let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
    let outage = fx.take_offline(fx.a).await;

    let (status, body) = fx.post(RESTORE, ids(&[item])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "volume_offline");
    let (status, body) = fx.post(RESTORE, deletion(fx.a, &TrashBatch::new(1))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "volume_offline");
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());

    fx.recover(outage).await;
    let task = fx.run(RESTORE, ids(&[item])).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(fx.files(fx.a)["x/IMG_1.jpg"], PAYLOAD);
}

#[tokio::test]
async fn an_offline_volume_fails_its_items_and_not_those_of_other_volumes() {
    let fx = TrashFx::new().await;
    let on_a = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
    let on_b = fx.trash(fx.b, "x/IMG_2.jpg", OTHER).await.trash_item_id;
    let outage = fx.take_offline(fx.a).await;

    let task = fx.run(RESTORE, ids(&[on_a, on_b])).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.title, "Restored 1 of 2 items");
    assert!(task.error.unwrap().contains("volume_offline"));
    assert_eq!(fx.files(fx.b)["x/IMG_2.jpg"], OTHER);
    assert_eq!(fx.ids().await, vec![on_a]);

    fx.recover(outage).await;
    let task = fx.run(RESTORE, ids(&[on_a])).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
}

#[tokio::test]
async fn a_read_only_volume_fails_its_items() {
    let fx = TrashFx::new().await;
    let on_a = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
    let on_b = fx.trash(fx.b, "x/IMG_2.jpg", OTHER).await.trash_item_id;
    let Some(outage) = fx.make_read_only(fx.a).await else {
        return;
    };

    let task = fx.run(RESTORE, ids(&[on_a, on_b])).await;

    assert_eq!(task.state, TaskState::Failed);
    assert!(task.error.unwrap().contains("volume_read_only"));
    assert_eq!(fx.files(fx.b)["x/IMG_2.jpg"], OTHER);
    assert_eq!(fx.ids().await, vec![on_a]);

    fx.recover(outage).await;
    let task = fx.run(RESTORE, ids(&[on_a])).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
}

#[tokio::test]
async fn a_restore_announces_the_folders_that_changed() {
    let fx = TrashFx::new().await;
    let item = fx.trash(fx.a, "x/IMG_1.jpg", PAYLOAD).await.trash_item_id;
    let mut events = fx.app.state.tasks.subscribe();

    fx.run(RESTORE, ids(&[item])).await;

    let dirs = within(async {
        loop {
            match events.recv().await.unwrap() {
                ServerEvent::FsChange { volume_id, dirs } if volume_id == fx.a => break dirs,
                _ => {}
            }
        }
    })
    .await;
    assert!(dirs.contains(&"x".to_owned()), "{dirs:?}");
}

#[tokio::test]
async fn invalid_requests_are_refused_with_the_field_at_fault() {
    let fx = TrashFx::new().await;

    let (status, body) = fx.post(RESTORE, ids(&[])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "validation_failed");
    assert_eq!(field_names(&body), ["target.ids"]);

    let many: Vec<i64> = (1..=10_001).collect();
    let (status, body) = fx.post(RESTORE, ids(&many)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(field_names(&body), ["target.ids"]);

    for batch in ["", "a/b", "..", "a/../b", "/abs"] {
        let body = json!({ "target": { "kind": "batch", "volume_id": fx.a, "batch": batch } });
        let (status, body) = fx.post(RESTORE, body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{batch:?}: {body}"
        );
        assert_eq!(field_names(&body), ["target.batch"], "{batch:?}");
    }

    // A legal folder name that is no batch of the volume is just not there.
    let body = json!({ "target": { "kind": "batch", "volume_id": fx.a, "batch": ".apm-trash" } });
    let (status, answer) = fx.post(RESTORE, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
    assert_eq!(error_code(&answer), "batch_not_found");

    for body in [
        json!({}),
        json!({ "target": { "kind": "everything" } }),
        json!({ "target": { "kind": "ids", "ids": [1] }, "conflict_policy": "explode" }),
        json!({ "target": { "kind": "ids", "ids": ["one"] } }),
    ] {
        let (status, answer) = fx.post(RESTORE, body.clone()).await;
        assert!(status.is_client_error(), "{body}: {status} {answer}");
        assert_eq!(error_code(&answer), "invalid_json", "{body}");
    }
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_restore_can_be_cancelled_between_two_items() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    let mut items = Vec::new();
    for n in 1..=3 {
        let path = format!("x/IMG_{n}.jpg");
        items.push(
            fx.trash_into(fx.a, &path, PAYLOAD, &batch)
                .await
                .trash_item_id,
        );
    }
    let parking = Parking::new();
    let service = fx.service_with_hook(parking.clone());

    let task_id = service
        .restore(TrashRestoreRequest {
            target: TrashTarget::Ids { ids: items.clone() },
            conflict_policy: None,
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
        left.ends_with(&items[1..]),
        "the items after the one in flight were never started: {left:?}"
    );
    for id in &items[1..] {
        assert!(left.contains(id));
    }
}
