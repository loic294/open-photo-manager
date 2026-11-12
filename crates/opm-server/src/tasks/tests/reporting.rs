//! What a running task reports: progress events, saved counters, speed and ETA.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use opm_core::dto::TaskState;
use serde_json::json;

use super::support::{Harness, eventually, fast, fs, manual_clock, steered, task_events, within};
use crate::tasks::TaskOptions;

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("a value");
    assert!(
        (actual - expected).abs() <= expected * 0.02,
        "{actual} is not close to {expected}"
    );
}

#[tokio::test]
async fn progress_is_announced_at_most_once_per_interval() {
    let interval = Duration::from_millis(50);
    let h = Harness::with(TaskOptions {
        progress_interval: interval,
        ..fast()
    })
    .await;
    let mut events = h.events();
    let begun = Instant::now();
    let id = h
        .engine
        .submit(fs(), |ctx| async move {
            ctx.progress.set_totals(Some(30), None);
            for _ in 0..30 {
                ctx.progress.add(1, 0);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(json!({}))
        })
        .await
        .unwrap();
    let done = h.finished(id).await;
    let elapsed = begun.elapsed();
    assert_eq!(done.items_done, 30);

    let seen = task_events(&events.drain(), id);
    let progress = seen
        .iter()
        .filter(|task| task.state == TaskState::Running && task.items_done > 0)
        .count();
    let most = usize::try_from(elapsed.as_millis() / interval.as_millis()).unwrap() + 2;
    assert!(progress >= 1, "progress is announced while the task runs");
    assert!(
        progress <= most,
        "{progress} progress events within {elapsed:?} for a {interval:?} interval"
    );
    assert_eq!(seen.last().unwrap(), &done);
}

#[tokio::test]
async fn a_progress_change_reaches_the_subscribers() {
    let h = Harness::new().await;
    let mut events = h.events();
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(10), Some(1_000));
    steered.progress.add(3, 300);
    steered.progress.set_title("Moving 3 of 10");
    let seen = events
        .wait_for_task(id, |task| task.title == "Moving 3 of 10")
        .await;
    assert_eq!(seen.state, TaskState::Running);
    assert_eq!((seen.items_done, seen.items_total), (3, Some(10)));
    assert_eq!((seen.bytes_done, seen.bytes_total), (300, Some(1_000)));

    steered.end(Ok(json!({})));
    h.finished(id).await;
}

#[tokio::test]
async fn counters_and_title_are_saved_while_the_task_runs() {
    let h = Harness::new().await;
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(10), Some(1_000));
    steered.progress.add(4, 400);
    steered.progress.set_title("Moving 4 of 10");
    let h_ref = &h;
    let row = eventually("the counters to be saved", || async move {
        let row = h_ref.row(id).await;
        (row.items_done == 4).then_some(row)
    })
    .await;
    assert_eq!(row.state, "running");
    assert_eq!(row.title, "Moving 4 of 10");
    assert_eq!(
        (row.items_total, row.bytes_done, row.bytes_total),
        (Some(10), 400, Some(1_000))
    );

    let live = h.engine.get(id).await.unwrap();
    assert_eq!((live.items_done, live.items_total), (4, Some(10)));
    assert_eq!((live.bytes_done, live.bytes_total), (400, Some(1_000)));
    assert_eq!(live.title, "Moving 4 of 10");

    steered.end(Ok(json!({})));
    h.finished(id).await;
}

#[tokio::test]
async fn the_final_counters_are_stored_even_if_none_were_saved_before() {
    let h = Harness::with(TaskOptions {
        persist_interval: Duration::from_secs(3600),
        ..fast()
    })
    .await;
    let mut events = h.events();
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(5), Some(50));
    steered.progress.add(5, 50);
    steered.progress.set_title("Almost there");
    events
        .wait_for_task(id, |task| task.title == "Almost there")
        .await;
    assert_eq!(
        h.row(id).await.items_done,
        0,
        "counters are saved at most once per interval"
    );

    steered.end(Ok(json!({})));
    let done = h.finished(id).await;
    assert_eq!((done.items_done, done.bytes_done), (5, 50));
    let row = h.row(id).await;
    assert_eq!((row.items_done, row.items_total), (5, Some(5)));
    assert_eq!((row.bytes_done, row.bytes_total), (50, Some(50)));
    assert_eq!(row.title, "Almost there");
}

#[tokio::test]
async fn the_speed_is_in_bytes_per_second_when_a_byte_total_is_declared() {
    let (now, clock) = manual_clock();
    let h = Harness::with(TaskOptions { clock, ..fast() }).await;
    let mut events = h.events();
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(200), Some(20_000));
    for second in 1..=10u64 {
        steered.progress.add(10, 1_000);
        now.store(second * 1_000, Ordering::SeqCst);
    }

    let live = h.engine.get(id).await.unwrap();
    assert_close(live.speed, 1_000.0);
    assert_close(live.eta_secs, 10.0);
    let announced = events
        .wait_for_task(id, |task| task.items_done == 100 && task.speed.is_some())
        .await;
    assert_close(announced.speed, 1_000.0);
    assert_close(announced.eta_secs, 10.0);

    steered.end(Ok(json!({})));
    let done = h.finished(id).await;
    assert_eq!((done.items_done, done.bytes_done), (100, 10_000));
    assert_eq!(
        (done.speed, done.eta_secs),
        (None, None),
        "only a running task has a speed"
    );
}

#[tokio::test]
async fn the_speed_is_in_items_per_second_without_a_byte_total() {
    let (now, clock) = manual_clock();
    let h = Harness::with(TaskOptions { clock, ..fast() }).await;
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(1_000), None);
    for second in 1..=10u64 {
        steered.progress.add(20, 5_000_000);
        now.store(second * 1_000, Ordering::SeqCst);
    }

    let live = h.engine.get(id).await.unwrap();
    assert_close(live.speed, 20.0);
    assert_close(live.eta_secs, 40.0);

    steered.end(Ok(json!({})));
    h.finished(id).await;
}

#[tokio::test]
async fn there_is_no_speed_during_the_first_second() {
    let (now, clock) = manual_clock();
    let h = Harness::with(TaskOptions { clock, ..fast() }).await;
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(100), Some(1_000));
    steered.progress.add(10, 100);
    now.store(500, Ordering::SeqCst);
    let warming_up = h.engine.get(id).await.unwrap();
    assert_eq!((warming_up.speed, warming_up.eta_secs), (None, None));

    steered.progress.add(10, 100);
    now.store(2_000, Ordering::SeqCst);
    let warm = h.engine.get(id).await.unwrap();
    assert!(warm.speed.is_some_and(|speed| speed > 0.0), "{warm:?}");
    assert!(warm.eta_secs.is_some(), "{warm:?}");

    steered.end(Ok(json!({})));
    h.finished(id).await;
}

#[tokio::test]
async fn a_new_phase_restarts_the_counters_and_the_title() {
    let h = Harness::new().await;
    let mut events = h.events();
    let (run, steered) = steered();
    let id = h.engine.submit(fs(), run).await.unwrap();
    let steered = within(steered).await.unwrap();

    steered.progress.set_totals(Some(10), None);
    steered.progress.add(10, 0);
    steered.progress.phase("Hashing", Some(50), Some(5_000));

    let seen = events
        .wait_for_task(id, |task| task.title == "Hashing")
        .await;
    assert_eq!((seen.items_done, seen.items_total), (0, Some(50)));
    assert_eq!((seen.bytes_done, seen.bytes_total), (0, Some(5_000)));
    assert_eq!(h.engine.get(id).await.unwrap().title, "Hashing");

    steered.end(Ok(json!({})));
    h.finished(id).await;
}
