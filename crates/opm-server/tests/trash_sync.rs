//! `POST /api/trash/sync` over the real application: the rows of the trash come to agree with what
//! `.opm-trash` holds, whether the database was lost or somebody cleaned the trash by hand, and a
//! trash that looks unmounted is never wiped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use axum::http::StatusCode;
use common::fsops::permissions_are_enforced;
use common::trash::{Row, TrashFx};
use opm_core::dto::{TaskDto, TaskKind, TaskState};
use opm_core::paths::VolumeId;
use opm_server::db::now_secs;
use opm_server::fsops::TrashBatch;
use serde_json::{Value, json};

const SYNC: &str = "/api/trash/sync";
/// A batch name with a stamp (2024-03-15 10:15:00 UTC) and the task that made it.
const TASK_BATCH: &str = "20240315-101500-t7";
const TASK_BATCH_AT: i64 = 1_710_497_700;
/// A batch name with a stamp and no task.
const ADHOC_BATCH: &str = "20240315-101500-r0a1b2c3d";

/// Runs a sync to its end and returns the task and what it reported.
async fn sync(fx: &TrashFx) -> (TaskDto, Value) {
    let id = fx.queue_sync().await;
    let task = fx.finished(id).await;
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    let result = fx.result_of(id).await.expect("a sync reports its counts");
    (task, result)
}

/// The counters of a sync result, in the order they are documented.
fn counts(result: &Value) -> [u64; 4] {
    ["adopted", "removed", "held", "unreadable"].map(|key| result[key].as_u64().unwrap())
}

/// A file in the trash that a row covers, as `FsOps` would have left it.
async fn kept(fx: &TrashFx, volume: VolumeId, batch: &str, name: &str) {
    fx.tree(volume)
        .write(&format!(".opm-trash/{batch}/{name}"), name.as_bytes());
    fx.insert(Row::new(volume, batch, name).size(name.len() as i64))
        .await;
}

/// A row whose file is not there.
async fn ghost(fx: &TrashFx, volume: VolumeId, batch: &str, name: &str) {
    fx.insert(Row::new(volume, batch, name)).await;
}

/// `count` rows of a volume whose files are gone and `kept_count` whose files are there.
async fn trash_with_gaps(fx: &TrashFx, volume: VolumeId, count: usize, kept_count: usize) {
    for n in 0..count {
        ghost(
            fx,
            volume,
            "20240101-000000-r00000001",
            &format!("ghost-{n}.jpg"),
        )
        .await;
    }
    for n in 0..kept_count {
        kept(
            fx,
            volume,
            "20240101-000000-r00000002",
            &format!("kept-{n}.jpg"),
        )
        .await;
    }
}

/// The listing's items by name.
async fn items_by_name(fx: &TrashFx) -> BTreeMap<String, Value> {
    let listing = fx.list("limit=200").await;
    listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["name"].as_str().unwrap().to_owned(), item.clone()))
        .collect()
}

#[tokio::test]
async fn a_sync_is_a_task_on_the_file_lane_that_waits_for_its_turn() {
    let fx = TrashFx::new().await;
    let gate = fx.hold_the_queue().await;

    let (status, queued) = fx.app.call(axum::http::Method::POST, SYNC, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let id = queued["id"].as_i64().unwrap();
    assert_eq!(queued["kind"], "fs_trash");
    assert_eq!(
        queued["state"], "queued",
        "it waits behind the file operation"
    );
    assert_eq!(queued["title"], "Checking the trash");
    let params: String = sqlx::query_scalar("SELECT params_json FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&params).unwrap(),
        json!({ "op": "trash_sync" })
    );

    gate.open();
    let task = fx.finished(id).await;
    assert_eq!(task.kind, TaskKind::FsTrash);
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(task.title, "The trash matches the disk");
    assert_eq!(task.error, None);
    assert_eq!(
        fx.result_of(id).await.unwrap(),
        json!({
            "adopted": 0, "removed": 0, "held": 0, "unreadable": 0, "volumes": 2, "offline": 0,
        })
    );
}

#[tokio::test]
async fn a_queued_sync_is_not_an_index_scan() {
    let fx = TrashFx::new().await;
    let gate = fx.hold_the_queue().await;
    let id = fx.queue_sync().await;

    let (status, index) = fx
        .app
        .call(axum::http::Method::GET, "/api/index/status", None)
        .await;

    assert_eq!(status, StatusCode::OK, "{index}");
    assert_eq!(
        index["active_task_id"],
        Value::Null,
        "IndexJobs counts every queued `index` task as a scan: {index}"
    );
    gate.open();
    fx.finished(id).await;
}

#[tokio::test]
async fn a_sync_needs_the_request_header_like_every_state_change() {
    let fx = TrashFx::new().await;

    let status = fx.post_unmarked(SYNC, &json!({})).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(fx.app.state.tasks.list(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn files_the_database_does_not_know_are_adopted_with_what_their_batch_says() {
    let fx = TrashFx::new().await;
    fx.ta.write(
        &format!(".opm-trash/{TASK_BATCH}/Trips/2019/IMG_1.jpg"),
        b"abc",
    );
    fx.ta.write(
        &format!(".opm-trash/{ADHOC_BATCH}/Other/IMG_2.jpg"),
        b"defgh",
    );
    fx.tb.write(".opm-trash/mystuff/IMG_3.jpg", b"i");
    let before = now_secs();

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [3, 0, 0, 0]);
    assert_eq!(
        task.title,
        "Checked the trash: added 3 items found on the disk"
    );
    let items = items_by_name(&fx).await;
    assert_eq!(items.len(), 3, "{items:?}");

    let stamped = &items["IMG_1.jpg"];
    assert_eq!(stamped["volume"], "a");
    assert_eq!(stamped["dir"], "Trips/2019");
    assert_eq!(stamped["is_dir"], false);
    assert_eq!(stamped["size"], 3);
    assert_eq!(stamped["batch"], TASK_BATCH);
    assert_eq!(
        stamped["task_id"], 7,
        "the batch name says which task made it"
    );
    assert_eq!(stamped["trashed_at"], TASK_BATCH_AT);
    assert_eq!(stamped["reason"], Value::Null);
    assert_eq!(stamped["has_snapshot"], false);

    let adhoc = &items["IMG_2.jpg"];
    assert_eq!(adhoc["dir"], "Other");
    assert_eq!(adhoc["size"], 5);
    assert_eq!(adhoc["task_id"], Value::Null, "an `r` batch has no task");
    assert_eq!(adhoc["trashed_at"], TASK_BATCH_AT);

    let unnamed = &items["IMG_3.jpg"];
    assert_eq!(unnamed["volume"], "b");
    assert_eq!(unnamed["dir"], "");
    assert_eq!(unnamed["batch"], "mystuff");
    assert_eq!(unnamed["task_id"], Value::Null);
    let at = unnamed["trashed_at"].as_i64().unwrap();
    assert!(
        (before..=now_secs()).contains(&at),
        "a batch without a stamp is as old as the sync, not as old as the file: {at}"
    );

    let summary = &fx.list("").await["summary"];
    assert_eq!(summary["items"], 3);
    assert_eq!(summary["bytes"], 9);
    assert_eq!(summary["max_id"], fx.ids().await.last().copied().unwrap());
    assert_eq!(summary["volumes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_second_sync_finds_nothing_new() {
    let fx = TrashFx::new().await;
    fx.ta
        .write(&format!(".opm-trash/{TASK_BATCH}/x/IMG_1.jpg"), b"abc");
    let first = sync(&fx).await;
    assert_eq!(counts(&first.1), [1, 0, 0, 0]);
    let ids = fx.ids().await;

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 0, 0, 0]);
    assert_eq!(task.title, "The trash matches the disk");
    assert_eq!(fx.ids().await, ids, "the rows were not touched");
}

#[tokio::test]
async fn a_sync_recounts_totals_that_drifted() {
    let fx = TrashFx::new().await;
    kept(&fx, fx.a, TASK_BATCH, "one.jpg").await;
    kept(&fx, fx.a, TASK_BATCH, "three.jpg").await;
    kept(&fx, fx.b, TASK_BATCH, "five5.jpg").await;
    let before = fx.list("").await["summary"].clone();
    assert_eq!(before["items"], 3);
    assert_eq!(before["bytes"], 7 + 9 + 9);

    // A writer that got around the triggers (an `INSERT OR REPLACE`, an edited database).
    let db = fx.app.state.db.write();
    sqlx::query("UPDATE trash_volume_totals SET items = 40, bytes = 1 WHERE volume_id = ?")
        .bind(fx.a)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM trash_volume_totals WHERE volume_id = ?")
        .bind(fx.b)
        .execute(db)
        .await
        .unwrap();
    let drifted = fx.list("").await["summary"].clone();
    assert_eq!(
        drifted["items"], 40,
        "the summary shows what the totals say"
    );

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 0, 0, 0]);
    assert_eq!(task.title, "The trash matches the disk");
    let after = fx.list("").await["summary"].clone();
    assert_eq!(after, before);
}

#[tokio::test]
async fn an_adopted_item_comes_back_to_where_it_was_and_can_be_emptied() {
    let fx = TrashFx::new().await;
    fx.ta.write(
        &format!(".opm-trash/{TASK_BATCH}/Trips/2019/IMG_1.jpg"),
        b"abc",
    );
    fx.tb.write(".opm-trash/mystuff/IMG_9.jpg", b"nine");
    sync(&fx).await;
    let items = items_by_name(&fx).await;

    let restore = fx
        .run(
            "/api/trash/restore",
            json!({ "target": { "kind": "ids", "ids": [items["IMG_1.jpg"]["id"]] } }),
        )
        .await;

    assert_eq!(restore.state, TaskState::Done, "{restore:?}");
    assert_eq!(restore.title, "Restored 1 item");
    assert_eq!(fx.files(fx.a)["Trips/2019/IMG_1.jpg"], b"abc");
    assert!(fx.trashed_files(fx.a).is_empty());

    let summary = fx.list("").await["summary"].clone();
    let emptied = fx
        .run(
            "/api/trash/empty",
            json!({ "volume_id": null, "up_to_id": summary["max_id"] }),
        )
        .await;

    assert_eq!(emptied.state, TaskState::Done, "{emptied:?}");
    assert_eq!(emptied.title, "Deleted 1 item");
    assert!(
        fx.trashed_files(fx.b).is_empty(),
        "the adopted file is deleted for good"
    );
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn a_row_whose_file_was_removed_by_hand_goes() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::adhoc();
    let one = fx.trash_into(fx.a, "x/IMG_1.jpg", b"1", &batch).await;
    let two = fx.trash_into(fx.a, "x/IMG_2.jpg", b"22", &batch).await;
    std::fs::remove_file(fx.ta.path(one.trash_path.as_str())).unwrap();

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 1, 0, 0]);
    assert_eq!(
        task.title,
        "Checked the trash: removed 1 item whose file is gone"
    );
    assert_eq!(fx.ids().await, vec![two.trash_item_id]);
}

#[tokio::test]
async fn a_batch_folder_removed_by_hand_takes_its_rows_with_it() {
    let fx = TrashFx::new().await;
    let batch = TrashBatch::new(5);
    for name in ["x/IMG_1.jpg", "x/IMG_2.jpg", "y/IMG_3.jpg"] {
        fx.trash_into(fx.a, name, b"data", &batch).await;
    }
    let other = fx.trash(fx.b, "x/IMG_4.jpg", b"data").await;
    std::fs::remove_dir_all(fx.ta.path(&format!(".opm-trash/{batch}"))).unwrap();

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 3, 0, 0]);
    assert_eq!(
        task.title,
        "Checked the trash: removed 3 items whose file is gone"
    );
    assert_eq!(fx.ids().await, vec![other.trash_item_id]);
    assert_eq!(fx.trashed_files(fx.b).len(), 1);
}

#[tokio::test]
async fn a_trash_that_looks_unmounted_is_left_alone() {
    let fx = TrashFx::new().await;
    trash_with_gaps(&fx, fx.a, 60, 20).await;

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 0, 60, 0]);
    assert_eq!(
        task.title,
        "Checked the trash: kept 60 items whose file is missing (is a share unmounted?)"
    );
    assert_eq!(fx.ids().await.len(), 80, "no row was removed");
}

#[tokio::test]
async fn a_few_missing_files_are_not_mistaken_for_an_unmounted_share() {
    let fx = TrashFx::new().await;
    trash_with_gaps(&fx, fx.a, 49, 51).await;

    let (_, result) = sync(&fx).await;

    assert_eq!(
        counts(&result),
        [0, 49, 0, 0],
        "49 is below the guard's floor of 50"
    );
    assert_eq!(fx.ids().await.len(), 51);
}

#[tokio::test]
async fn a_small_share_of_a_big_trash_is_cleaned() {
    let fx = TrashFx::new().await;
    trash_with_gaps(&fx, fx.a, 50, 350).await;

    let (_, result) = sync(&fx).await;

    assert_eq!(
        counts(&result),
        [0, 50, 0, 0],
        "50 of 400 is 12.5 %, below the guard's fifth"
    );
    assert_eq!(fx.ids().await.len(), 350);
}

#[tokio::test]
async fn the_guard_judges_every_volume_on_its_own() {
    let fx = TrashFx::new().await;
    trash_with_gaps(&fx, fx.a, 60, 20).await;
    trash_with_gaps(&fx, fx.b, 3, 5).await;

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [0, 3, 60, 0]);
    assert_eq!(
        task.title,
        "Checked the trash: removed 3 items whose file is gone, kept 60 items whose file is \
         missing (is a share unmounted?)"
    );
    let rows = fx.rows().await;
    assert_eq!(
        rows.iter().filter(|(volume, _)| *volume == fx.a).count(),
        80
    );
    assert_eq!(rows.iter().filter(|(volume, _)| *volume == fx.b).count(), 5);
}

#[tokio::test]
async fn the_trash_folder_is_read_with_care() {
    let fx = TrashFx::new().await;
    // A file straight below the trash folder belongs to no deletion.
    fx.ta.write(".opm-trash/stray.jpg", b"stray");
    // A folder item covers everything below it...
    let batch = TrashBatch::adhoc();
    fx.ta.write("Trips/2019/a.jpg", b"aa");
    fx.ta.write("Trips/2019/b.jpg", b"bbb");
    let folder = fx.trash_existing(fx.a, "Trips/2019", &batch, "user").await;
    assert!(folder.is_dir);
    // ...but a file beside it in the same deletion is not covered, and junk is a file like any.
    fx.ta
        .write(&format!(".opm-trash/{batch}/Extra/c.jpg"), b"c");
    fx.ta
        .write(&format!(".opm-trash/{batch}/.DS_Store"), b"junk");
    // Links are never followed, never adopted.
    fx.ta.write("outside/real.jpg", b"real");
    std::os::unix::fs::symlink(
        fx.ta.path("outside/real.jpg"),
        fx.ta.path(&format!(".opm-trash/{batch}/link.jpg")),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        fx.ta.path("outside"),
        fx.ta.path(&format!(".opm-trash/{batch}/dirlink")),
    )
    .unwrap();

    let (_, result) = sync(&fx).await;

    assert_eq!(counts(&result), [2, 0, 0, 0], "{result}");
    let items = items_by_name(&fx).await;
    let mut names: Vec<&str> = items.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, vec![".DS_Store", "2019", "c.jpg"]);
    assert!(items["2019"]["is_dir"].as_bool().unwrap());
    assert_eq!(items["c.jpg"]["dir"], "Extra");
    assert_eq!(items[".DS_Store"]["size"], 4);
    assert!(
        fx.trashed_files(fx.a).contains_key("stray.jpg"),
        "the stray file was left where it is"
    );
}

#[tokio::test]
async fn an_offline_volume_is_not_looked_at() {
    let fx = TrashFx::new().await;
    fx.ta
        .write(&format!(".opm-trash/{TASK_BATCH}/x/A.jpg"), b"a");
    fx.tb
        .write(&format!(".opm-trash/{ADHOC_BATCH}/y/B.jpg"), b"b");
    let ghost_row = fx
        .insert(Row::new(fx.b, "20240101-000000-r00000001", "g.jpg"))
        .await;
    let outage = fx.take_offline(fx.b).await;

    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [1, 0, 0, 0]);
    assert_eq!(
        (result["volumes"].as_u64(), result["offline"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(
        task.title,
        "Checked the trash: added 1 item found on the disk, 1 volume offline"
    );
    assert!(
        fx.ids().await.contains(&ghost_row),
        "what an offline volume holds is not judged"
    );

    fx.recover(outage).await;
    let (task, result) = sync(&fx).await;

    assert_eq!(counts(&result), [1, 1, 0, 0]);
    assert_eq!(
        (result["volumes"].as_u64(), result["offline"].as_u64()),
        (Some(2), Some(0))
    );
    assert_eq!(
        task.title,
        "Checked the trash: added 1 item found on the disk, removed 1 item whose file is gone"
    );
    assert!(!fx.ids().await.contains(&ghost_row));
}

#[tokio::test]
async fn a_folder_that_cannot_be_read_is_counted_and_the_rest_is_still_checked() {
    let fx = TrashFx::new().await;
    if !permissions_are_enforced(fx.ta.root()) {
        return;
    }
    fx.ta
        .write(&format!(".opm-trash/{TASK_BATCH}/open/A.jpg"), b"a");
    fx.ta
        .write(&format!(".opm-trash/{ADHOC_BATCH}/locked/B.jpg"), b"b");
    let locked = fx.ta.path(&format!(".opm-trash/{ADHOC_BATCH}/locked"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (task, result) = {
        let id = fx.queue_sync().await;
        let task = fx.finished(id).await;
        let result = fx.result_of(id).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        (task, result.expect("a sync reports its counts"))
    };

    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(counts(&result), [1, 0, 0, 1]);
    assert_eq!(
        task.title,
        "Checked the trash: added 1 item found on the disk, skipped 1 entry that could not be read"
    );
    let (_, result) = sync(&fx).await;
    assert_eq!(
        counts(&result),
        [1, 0, 0, 0],
        "readable again, so adopted now"
    );
}

#[tokio::test]
async fn a_name_that_is_not_text_is_counted_and_skipped() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let fx = TrashFx::new().await;
    fx.ta
        .write(&format!(".opm-trash/{TASK_BATCH}/x/fine.jpg"), b"fine");
    let odd = fx
        .ta
        .path(&format!(".opm-trash/{TASK_BATCH}/x"))
        .join(OsString::from_vec(b"odd-\xff.jpg".to_vec()));
    if std::fs::write(odd, b"odd").is_err() {
        // The file system refuses names that are not valid text (macOS does).
        return;
    }

    let (_, result) = sync(&fx).await;

    assert_eq!(counts(&result), [1, 0, 0, 1]);
}

#[tokio::test]
async fn a_queued_sync_can_be_cancelled_and_changes_nothing() {
    let fx = TrashFx::new().await;
    fx.ta
        .write(&format!(".opm-trash/{TASK_BATCH}/x/A.jpg"), b"a");
    ghost(&fx, fx.b, "20240101-000000-r00000001", "g.jpg").await;
    let gate = fx.hold_the_queue().await;
    let id = fx.queue_sync().await;

    let cancelled = fx.app.state.tasks.cancel(id).await.unwrap();
    gate.open();
    let task = fx.finished(id).await;

    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    assert_eq!(
        fx.ids().await.len(),
        1,
        "neither adopted nor removed anything"
    );
}

#[tokio::test]
async fn a_trash_that_lost_its_database_comes_back_from_the_disk() {
    let fx = TrashFx::new().await;
    let task_batch = TrashBatch::new(42);
    fx.ta.write("Trips/2019/a.jpg", b"aa");
    fx.ta.write("Trips/2019/b.jpg", b"bbb");
    fx.trash_as_task(fx.a, "Trips/2019", &task_batch, "user", 42)
        .await;
    fx.trash(fx.b, "Docs/IMG_9.jpg", b"nine").await;
    let before = items_by_name(&fx).await;
    assert_eq!(before.len(), 2, "a folder item and a file item: {before:?}");
    sqlx::query("DELETE FROM trash_items")
        .execute(fx.app.state.db.write())
        .await
        .unwrap();
    assert!(fx.list("").await["items"].as_array().unwrap().is_empty());

    let (_, result) = sync(&fx).await;

    assert_eq!(
        counts(&result),
        [3, 0, 0, 0],
        "the folder comes back as its two files"
    );
    let after = items_by_name(&fx).await;
    let names: Vec<&str> = after.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["IMG_9.jpg", "a.jpg", "b.jpg"]);
    assert_eq!(after["a.jpg"]["dir"], "Trips/2019");
    assert_eq!(after["a.jpg"]["task_id"], 42);
    assert_eq!(after["IMG_9.jpg"]["dir"], "Docs");
    assert_eq!(after["IMG_9.jpg"]["task_id"], Value::Null);
    let was = before["IMG_9.jpg"]["trashed_at"].as_i64().unwrap();
    let is = after["IMG_9.jpg"]["trashed_at"].as_i64().unwrap();
    assert!(
        (was - is).abs() <= 3,
        "the batch name keeps the time: was {was}, is {is}"
    );

    let restored = fx
        .run(
            "/api/trash/restore",
            json!({ "target": { "kind": "batch", "volume_id": fx.a, "batch": task_batch.name() } }),
        )
        .await;
    assert_eq!(restored.state, TaskState::Done, "{restored:?}");
    assert_eq!(restored.title, "Restored 2 items");
    let files = fx.files(fx.a);
    assert_eq!(files["Trips/2019/a.jpg"], b"aa");
    assert_eq!(files["Trips/2019/b.jpg"], b"bbb");
    let restored = fx
        .run(
            "/api/trash/restore",
            json!({ "target": { "kind": "ids", "ids": [after["IMG_9.jpg"]["id"]] } }),
        )
        .await;
    assert_eq!(restored.state, TaskState::Done, "{restored:?}");
    assert_eq!(fx.files(fx.b)["Docs/IMG_9.jpg"], b"nine");
    assert!(fx.ids().await.is_empty());
}
