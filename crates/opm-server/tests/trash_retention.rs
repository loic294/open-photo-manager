//! The retention sweeper over the real application: what it deletes, when it looks and how it
//! stays out of the way of the user's own "Empty trash".

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::trash::{Parking, TrashFx, within};
use opm_core::dto::{ConflictPolicy, TaskDto, TaskKind, TaskState};
use opm_core::paths::{TaskId, TrashItemId};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// What time the sweeps of most tests think it is: far from the wall clock on purpose.
const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const PAYLOAD: &[u8] = b"a trashed photo";

/// Trashes a file of volume `a` and makes it look as if that happened at `trashed_at`.
async fn trash_aged(fx: &TrashFx, path: &str, trashed_at: i64) -> TrashItemId {
    let id = fx.trash(fx.a, path, PAYLOAD).await.trash_item_id;
    fx.age(id, trashed_at).await;
    id
}

async fn purge_tasks(fx: &TrashFx) -> Vec<TaskDto> {
    let mut tasks: Vec<TaskDto> = fx
        .app
        .state
        .tasks
        .list(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|task| task.kind == TaskKind::TrashEmpty)
        .collect();
    tasks.sort_by_key(|task| task.id);
    tasks
}

/// Waits until `count` purge tasks exist and the newest one is over.
async fn purge_tasks_after_waiting(fx: &TrashFx, count: usize) -> Vec<TaskDto> {
    within(async {
        loop {
            let tasks = purge_tasks(fx).await;
            if let Some(last) = tasks.last().filter(|_| tasks.len() >= count) {
                fx.finished(last.id).await;
                return purge_tasks(fx).await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

async fn params_of(fx: &TrashFx, task: TaskId) -> Value {
    let text: String = sqlx::query_scalar("SELECT params_json FROM tasks WHERE id = ?")
        .bind(task)
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The names the files in the trash of a volume had before they were trashed, sorted.
fn trashed_names(fx: &TrashFx, volume: i64) -> Vec<String> {
    let mut names: Vec<String> = fx
        .trashed_files(volume)
        .into_keys()
        .map(|path| path.split_once('/').unwrap().1.to_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn nothing_is_removed_while_retention_is_off() {
    let fx = TrashFx::new().await;
    let id = trash_aged(&fx, "x/old.jpg", 1).await;
    fx.configure(|settings| settings.trash_retention_days = None)
        .await;
    let service = fx.service_with_clock(|| NOW);

    assert_eq!(service.sweep_retention().await.unwrap(), None);

    assert!(purge_tasks(&fx).await.is_empty());
    assert_eq!(fx.ids().await, vec![id]);
    assert_eq!(trashed_names(&fx, fx.a), ["x/old.jpg"]);
}

#[tokio::test]
async fn only_what_is_older_than_the_retention_goes() {
    let fx = TrashFx::new().await;
    let cutoff = NOW - 7 * DAY;
    trash_aged(&fx, "x/older.jpg", cutoff - 1).await;
    let on_the_edge = trash_aged(&fx, "x/edge.jpg", cutoff).await;
    let newer = trash_aged(&fx, "y/newer.jpg", cutoff + 1).await;
    let fresh = trash_aged(&fx, "y/fresh.jpg", NOW).await;
    let on_b = fx.trash(fx.b, "z/on-b.jpg", PAYLOAD).await.trash_item_id;
    fx.age(on_b, cutoff - 100).await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);

    let task = service
        .sweep_retention()
        .await
        .unwrap()
        .expect("two items are older than seven days");
    let task = fx.finished(task).await;

    assert_eq!(task.kind, TaskKind::TrashEmpty);
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 2 items");
    let result = fx.result_of(task.id).await.unwrap();
    assert_eq!(result["deleted"], 2);
    assert_eq!(result["failed"], 0);
    // An item trashed exactly `retention` ago is not older than it yet.
    assert_eq!(fx.ids().await, vec![on_the_edge, newer, fresh]);
    assert_eq!(
        trashed_names(&fx, fx.a),
        ["x/edge.jpg", "y/fresh.jpg", "y/newer.jpg"]
    );
    assert!(trashed_names(&fx, fx.b).is_empty());
}

#[tokio::test]
async fn nothing_due_queues_no_task() {
    let fx = TrashFx::new().await;
    trash_aged(&fx, "x/edge.jpg", NOW - 7 * DAY).await;
    trash_aged(&fx, "x/fresh.jpg", NOW).await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);

    assert_eq!(service.sweep_retention().await.unwrap(), None);

    assert!(purge_tasks(&fx).await.is_empty());
    assert_eq!(fx.ids().await.len(), 2);
}

#[tokio::test]
async fn the_task_says_what_it_removes() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(30))
        .await;
    let service = fx.service_with_clock(|| NOW);

    trash_aged(&fx, "x/old.jpg", NOW - 40 * DAY).await;
    let gate = fx.hold_the_queue().await;
    let id = service.sweep_retention().await.unwrap().unwrap();
    let queued = fx.app.state.tasks.get(id).await.unwrap();
    assert_eq!(queued.kind, TaskKind::TrashEmpty);
    assert_eq!(queued.state, TaskState::Queued);
    assert_eq!(queued.title, "Removing trash older than 30 days");
    assert_eq!(
        params_of(&fx, id).await,
        json!({
            "op": "trash_retention",
            "retention_days": 30,
            "cutoff": NOW - 30 * DAY,
            "selector": { "older_than": { "cutoff": NOW - 30 * DAY } },
            "title": "Removing trash older than 30 days",
        })
    );
    gate.open();
    let done = fx.finished(id).await;
    assert_eq!(done.state, TaskState::Done, "{:?}", done.error);
    assert_eq!(done.title, "Deleted 1 item");

    fx.configure(|settings| settings.trash_retention_days = Some(1))
        .await;
    trash_aged(&fx, "x/yesterday.jpg", NOW - 2 * DAY).await;
    let gate = fx.hold_the_queue().await;
    let id = service.sweep_retention().await.unwrap().unwrap();
    let queued = fx.app.state.tasks.get(id).await.unwrap();
    assert_eq!(queued.title, "Removing trash older than 1 day");
    gate.open();
    assert_eq!(fx.finished(id).await.state, TaskState::Done);
}

#[tokio::test]
async fn a_sweep_never_deletes_what_is_newer_than_its_cutoff_even_after_waiting() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);
    trash_aged(&fx, "x/old.jpg", NOW - 8 * DAY).await;
    let gate = fx.hold_the_queue().await;
    let id = service.sweep_retention().await.unwrap().unwrap();

    // The queue holds the sweep: what arrives meanwhile is selected again when it starts.
    let late = trash_aged(&fx, "x/late.jpg", NOW).await;
    let also_old = trash_aged(&fx, "x/also-old.jpg", NOW - 9 * DAY).await;
    gate.open();

    let task = fx.finished(id).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 2 items");
    assert_eq!(fx.ids().await, vec![late]);
    assert!(!fx.ids().await.contains(&also_old));
}

#[tokio::test]
async fn no_second_task_starts_while_one_is_queued() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);

    // The user's own "Empty trash" is waiting in the queue.
    trash_aged(&fx, "x/old.jpg", NOW - 8 * DAY).await;
    let gate = fx.hold_the_queue().await;
    let up_to_id = fx.ids().await.into_iter().max().unwrap();
    let emptying = fx
        .queue(
            "/api/trash/empty",
            json!({ "volume_id": null, "up_to_id": up_to_id }),
        )
        .await;
    assert_eq!(service.sweep_retention().await.unwrap(), None);
    gate.open();
    assert_eq!(fx.finished(emptying).await.state, TaskState::Done);

    // A sweep that is queued blocks the next one.
    trash_aged(&fx, "x/old2.jpg", NOW - 8 * DAY).await;
    let gate = fx.hold_the_queue().await;
    let first = service.sweep_retention().await.unwrap().unwrap();
    assert_eq!(service.sweep_retention().await.unwrap(), None);
    gate.open();
    assert_eq!(fx.finished(first).await.state, TaskState::Done);
    assert_eq!(purge_tasks(&fx).await.len(), 2);

    // Once it is over, the next sweep is free to start.
    trash_aged(&fx, "x/old3.jpg", NOW - 8 * DAY).await;
    let next = service.sweep_retention().await.unwrap().unwrap();
    assert_ne!(next, first);
    assert_eq!(fx.finished(next).await.state, TaskState::Done);
    assert!(fx.ids().await.is_empty());
}

#[tokio::test]
async fn no_second_task_starts_while_one_is_running() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let parking = Parking::new();
    let service = fx.service_with_hook(parking.clone()).with_clock(|| NOW);
    trash_aged(&fx, "x/old1.jpg", NOW - 8 * DAY).await;
    trash_aged(&fx, "x/old2.jpg", NOW - 8 * DAY).await;

    let first = service.sweep_retention().await.unwrap().unwrap();
    parking.arrived().await;
    assert_eq!(
        fx.app.state.tasks.get(first).await.unwrap().state,
        TaskState::Running
    );
    // The sweep is half way and something is still due.
    assert_eq!(service.sweep_retention().await.unwrap(), None);
    assert_eq!(service.clone().sweep_retention().await.unwrap(), None);
    parking.release();

    let task = fx.finished(first).await;
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.title, "Deleted 2 items");
    assert_eq!(purge_tasks(&fx).await.len(), 1);
}

#[tokio::test]
async fn a_sweep_that_failed_is_tried_again_by_the_next_one() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);
    trash_aged(&fx, "x/on-a.jpg", NOW - 8 * DAY).await;
    let on_b = fx.trash(fx.b, "z/on-b.jpg", PAYLOAD).await.trash_item_id;
    fx.age(on_b, NOW - 8 * DAY).await;
    let outage = fx.take_offline(fx.b).await;

    let first = service.sweep_retention().await.unwrap().unwrap();
    let first = fx.finished(first).await;

    assert_eq!(first.state, TaskState::Failed);
    assert_eq!(first.title, "Deleted 1 of 2 items");
    let error = first.error.unwrap();
    assert!(error.contains("volume_offline"), "{error}");
    assert_eq!(
        fx.ids().await,
        vec![on_b],
        "only the offline volume keeps its item"
    );

    fx.recover(outage).await;
    let again = service
        .sweep_retention()
        .await
        .unwrap()
        .expect("the item is still due");
    assert_eq!(fx.finished(again).await.state, TaskState::Done);
    assert!(fx.ids().await.is_empty());
    assert!(trashed_names(&fx, fx.b).is_empty());
}

#[tokio::test]
async fn the_wall_clock_ages_the_trash_unless_a_test_replaces_it() {
    let fx = TrashFx::new().await;
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    trash_aged(&fx, "x/old.jpg", now - 3 * DAY).await;
    let fresh = fx.trash(fx.a, "x/fresh.jpg", PAYLOAD).await.trash_item_id;
    fx.configure(|settings| settings.trash_retention_days = Some(2))
        .await;

    let task = fx
        .app
        .state
        .trash
        .sweep_retention()
        .await
        .unwrap()
        .expect("one item is three days old");

    assert_eq!(fx.finished(task).await.state, TaskState::Done);
    assert_eq!(fx.ids().await, vec![fresh]);
}

#[tokio::test]
async fn the_sweeper_looks_on_its_own_schedule_and_stops_with_the_server() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let service = fx.service_with_clock(|| NOW);
    let shutdown = CancellationToken::new();
    service.start_background_every(
        shutdown.clone(),
        Duration::from_millis(30),
        Duration::from_millis(60),
    );

    trash_aged(&fx, "x/old.jpg", NOW - 8 * DAY).await;
    let tasks = purge_tasks_after_waiting(&fx, 1).await;
    assert_eq!(tasks[0].state, TaskState::Done, "{:?}", tasks[0].error);
    assert!(fx.ids().await.is_empty());

    // The next round finds what became due since.
    trash_aged(&fx, "x/older.jpg", NOW - 9 * DAY).await;
    let tasks = purge_tasks_after_waiting(&fx, 2).await;
    assert_eq!(tasks.len(), 2);
    assert!(fx.ids().await.is_empty());

    shutdown.cancel();
    tokio::time::sleep(Duration::from_millis(150)).await;
    trash_aged(&fx, "x/third.jpg", NOW - 9 * DAY).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        purge_tasks(&fx).await.len(),
        2,
        "no round after the shutdown"
    );
    assert_eq!(fx.ids().await.len(), 1);
}

#[tokio::test]
async fn changing_the_retention_sweeps_at_once_and_other_settings_do_not() {
    let fx = TrashFx::new().await;
    fx.configure(|settings| settings.trash_retention_days = None)
        .await;
    let service = fx.service_with_clock(|| NOW);
    let shutdown = CancellationToken::new();
    // The schedule is an hour: only a change of the setting can wake the sweeper in this test.
    service.start_background_every(
        shutdown.clone(),
        Duration::from_secs(3_600),
        Duration::from_secs(3_600),
    );
    trash_aged(&fx, "x/old.jpg", NOW - 40 * DAY).await;

    fx.configure(|settings| settings.trash_retention_days = Some(30))
        .await;

    let tasks = purge_tasks_after_waiting(&fx, 1).await;
    assert_eq!(tasks[0].state, TaskState::Done, "{:?}", tasks[0].error);
    assert!(fx.ids().await.is_empty());

    trash_aged(&fx, "x/old2.jpg", NOW - 40 * DAY).await;
    fx.configure(|settings| settings.default_conflict_policy = ConflictPolicy::Skip)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        purge_tasks(&fx).await.len(),
        1,
        "the retention did not change"
    );
    assert_eq!(fx.ids().await.len(), 1);

    // A shorter retention makes more of the trash due, and takes effect without waiting.
    trash_aged(&fx, "x/recent.jpg", NOW - 10 * DAY).await;
    fx.configure(|settings| settings.trash_retention_days = Some(7))
        .await;
    let tasks = purge_tasks_after_waiting(&fx, 2).await;
    assert_eq!(tasks.len(), 2);
    assert!(fx.ids().await.is_empty());
    shutdown.cancel();
}
