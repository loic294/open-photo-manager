//! What a new server finds in the table of the previous one: unfinished tasks become interrupted
//! and old ones are pruned.

use std::time::Duration;

use opm_core::dto::TaskState;

use super::support::{
    Gate, Harness, QUIET, eventually, fast, finished, fs, immich, index, states, submit_instant,
};
use crate::tasks::{Retention, TaskOptions};

#[tokio::test]
async fn resumable_tasks_keep_their_id_and_checkpoint_across_repeated_restarts() {
    let h = Harness::new().await;
    let id = h.seed_running(&fs().resumable()).await;
    let checkpoint = crate::tasks::Checkpoint::new(&h.db, id).await.unwrap();
    checkpoint.save("next", &3_u64).await.unwrap();
    let second = h.restarted_with(fast()).await;
    assert_eq!(second.recoverable().await.unwrap()[0].0, id);
    let db = h.db.handle();
    second
        .resume(id, move |ctx| async move {
            let checkpoint = crate::tasks::Checkpoint::new(&db, ctx.id).await?;
            assert_eq!(checkpoint.load::<u64>("next").await?, Some(3));
            checkpoint.save("next", &4_u64).await?;
            Ok(serde_json::json!({"next": 4}))
        })
        .await
        .unwrap();
    assert_eq!(finished(&second, id).await.state, TaskState::Done);
    let third = h.restarted_with(fast()).await;
    assert!(third.recoverable().await.unwrap().is_empty());
    assert_eq!(h.stored_ids().await, [id]);
    assert_eq!(checkpoint.load::<u64>("next").await.unwrap(), Some(4));
}

#[tokio::test]
async fn cancelled_resumable_tasks_are_not_recovered_or_run() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let running = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    let cancelled = submit_instant(&h.engine, fs().resumable()).await;
    h.engine.cancel(cancelled).await.unwrap();
    gate.release(1);
    h.finished(running).await;
    let second = h.restarted_with(fast()).await;
    assert!(second.recoverable().await.unwrap().is_empty());
    second
        .resume(cancelled, |_| async { panic!("cancelled task ran") })
        .await
        .unwrap();
    assert_eq!(
        second.get(cancelled).await.unwrap().state,
        TaskState::Cancelled
    );
}

const DAY: i64 = 24 * 3600;

fn retention(retention: Retention) -> TaskOptions {
    TaskOptions {
        retention,
        ..fast()
    }
}

#[tokio::test]
async fn a_restart_interrupts_the_tasks_the_previous_run_left_unfinished() {
    let h = Harness::new().await;
    let queued = h.seed_queued(&fs()).await;
    let running = h.seed_running(&index()).await;
    let done = h.seed_finished(&immich(), 60).await;

    let second = h.restarted_with(fast()).await;

    for id in [queued, running] {
        let row = h.row(id).await;
        assert_eq!(row.state, "interrupted");
        assert_eq!(row.error.as_deref(), Some("interrupted by restart"));
        assert!(row.finished_at.is_some());

        let task = second.get(id).await.unwrap();
        assert_eq!(task.state, TaskState::Interrupted);
        assert_eq!(task.error.as_deref(), Some("interrupted by restart"));
        assert!(task.finished_at.is_some());
    }
    assert_eq!(h.row(queued).await.started_at, None);
    assert!(h.row(running).await.started_at.is_some());
    let untouched = h.row(done).await;
    assert_eq!(untouched.state, "succeeded");
    assert_eq!(untouched.error, None);

    // Nothing is active any more, and nothing runs again by itself.
    let listed = second.list(10).await.unwrap();
    assert_eq!(
        listed.iter().map(|task| task.id).collect::<Vec<_>>(),
        [running, queued, done]
    );
    assert_eq!(
        states(&listed),
        [
            TaskState::Interrupted,
            TaskState::Interrupted,
            TaskState::Done
        ]
    );
    assert_eq!(h.active_rows().await, 0);
}

#[tokio::test]
async fn a_new_task_after_a_restart_works_as_usual() {
    let h = Harness::new().await;
    let stale = h.seed_running(&fs()).await;
    let second = h.restarted_with(fast()).await;

    let id = submit_instant(&second, fs()).await;
    let task = finished(&second, id).await;
    assert_eq!(task.state, TaskState::Done);
    assert_ne!(id, stale);
    assert_eq!(h.row(stale).await.state, "interrupted");
    assert_eq!(h.row(id).await.state, "succeeded");
}

#[tokio::test]
async fn a_late_finish_of_the_previous_engine_does_not_revive_an_interrupted_row() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let id = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;

    let _second = h.restarted_with(fast()).await;
    assert_eq!(h.row(id).await.state, "interrupted");

    gate.release(1);
    h.finished(id).await;
    let row = h.row(id).await;
    assert_eq!(row.state, "interrupted");
    assert_eq!(row.error.as_deref(), Some("interrupted by restart"));
    assert_eq!(row.result_json, None);
}

#[tokio::test]
async fn finished_tasks_older_than_a_week_are_pruned_at_startup() {
    let h = Harness::new().await;
    let old = h.seed_finished(&fs(), 8 * DAY).await;
    let recent = h.seed_finished(&fs(), 3600).await;
    assert_eq!(h.stored_ids().await, [old, recent]);

    let _second = h.restarted_with(fast()).await;
    assert_eq!(h.stored_ids().await, [recent]);
}

#[tokio::test]
async fn the_maximum_age_can_be_changed() {
    let h = Harness::new().await;
    let old = h.seed_finished(&fs(), 3600).await;
    let recent = h.seed_finished(&fs(), 60).await;
    assert_eq!(h.stored_ids().await, [old, recent]);

    let _second = h
        .restarted_with(retention(Retention {
            max_age: Duration::from_secs(600),
            ..Retention::default()
        }))
        .await;
    assert_eq!(h.stored_ids().await, [recent]);
}

#[tokio::test]
async fn only_the_newest_finished_tasks_are_kept() {
    let h = Harness::new().await;
    let mut ids = Vec::new();
    for age in [600, 500, 400, 300, 200, 100] {
        ids.push(h.seed_finished(&fs(), age).await);
    }

    let _second = h
        .restarted_with(retention(Retention {
            keep: 3,
            ..Retention::default()
        }))
        .await;
    assert_eq!(h.stored_ids().await, ids[3..].to_vec());
}

#[tokio::test]
async fn retention_keeps_sweeping_while_the_engine_runs() {
    let h = Harness::with(retention(Retention {
        keep: 2,
        sweep_every: Duration::from_millis(30),
        ..Retention::default()
    }))
    .await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        let id = submit_instant(&h.engine, fs()).await;
        // The newest finished task is always among those kept, so it is still there to wait for.
        h.finished(id).await;
        ids.push(id);
    }

    let (h_ref, newest) = (&h, &ids[3..]);
    eventually("the oldest tasks to be pruned", || async move {
        (h_ref.stored_ids().await == newest).then_some(())
    })
    .await;
}

#[tokio::test]
async fn active_tasks_are_never_pruned() {
    let h = Harness::with(retention(Retention {
        keep: 0,
        sweep_every: Duration::from_millis(20),
        ..Retention::default()
    }))
    .await;
    let mut events = h.events();
    let mut gate = Gate::new();
    let running = h.engine.submit(fs(), gate.runner()).await.unwrap();
    gate.started().await;
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    // Several sweeps go by.
    tokio::time::sleep(QUIET).await;
    assert_eq!(h.stored_ids().await, [running, waiting]);
    assert_eq!(h.active_rows().await, 2);

    gate.release(2);
    events
        .wait_for_task(waiting, |task| task.state == TaskState::Done)
        .await;
    let h_ref = &h;
    eventually("the finished tasks to be pruned", || async move {
        h_ref.stored_ids().await.is_empty().then_some(())
    })
    .await;
}
