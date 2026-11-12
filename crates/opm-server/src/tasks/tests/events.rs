//! The event bus as the engine exposes it: custom events and coalesced file-system changes.

use std::time::Duration;

use opm_core::dto::{ServerEvent, TaskState};
use opm_core::paths::VolumeId;

use super::support::{Events, Harness, QUIET, fast, fs, states, submit_instant, task_events};
use crate::tasks::TaskOptions;

fn dirs(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn fs_change(volume_id: VolumeId, values: &[&str]) -> ServerEvent {
    ServerEvent::FsChange {
        volume_id,
        dirs: dirs(values),
    }
}

/// An engine that collects changes for `window` before it announces them.
async fn with_window(window: Duration) -> Harness {
    Harness::with(TaskOptions {
        fs_change_window: window,
        ..fast()
    })
    .await
}

async fn next_fs_change(events: &mut Events) -> ServerEvent {
    events
        .wait_for(|event| matches!(event, ServerEvent::FsChange { .. }).then(|| event.clone()))
        .await
}

#[tokio::test]
async fn changes_reported_close_together_become_one_event_per_volume() {
    let h = with_window(Duration::from_millis(400)).await;
    let mut events = h.events();

    h.engine.publish_fs_change(2, dirs(&["/z"]));
    h.engine.publish_fs_change(1, dirs(&["/b", "/a"]));
    h.engine.publish_fs_change(1, dirs(&["/a", "/c"]));

    assert_eq!(
        next_fs_change(&mut events).await,
        fs_change(1, &["/a", "/b", "/c"])
    );
    assert_eq!(next_fs_change(&mut events).await, fs_change(2, &["/z"]));
    tokio::time::sleep(QUIET).await;
    assert!(events.drain().is_empty(), "nothing is announced twice");
}

#[tokio::test]
async fn changes_keep_flowing_after_an_announcement() {
    let h = Harness::new().await;
    let mut events = h.events();

    h.engine.publish_fs_change(1, dirs(&["/a"]));
    assert_eq!(next_fs_change(&mut events).await, fs_change(1, &["/a"]));
    h.engine.publish_fs_change(1, dirs(&["/b"]));
    assert_eq!(next_fs_change(&mut events).await, fs_change(1, &["/b"]));
}

#[tokio::test]
async fn reporting_no_directory_announces_nothing() {
    let h = Harness::new().await;
    let mut events = h.events();

    h.engine.publish_fs_change(1, Vec::new());
    tokio::time::sleep(QUIET).await;
    assert!(events.drain().is_empty());
}

#[tokio::test]
async fn pending_changes_are_announced_when_the_engine_stops() {
    let h = with_window(Duration::from_secs(10)).await;
    let mut events = h.events();

    h.engine.publish_fs_change(1, dirs(&["/b", "/a"]));
    h.engine.shutdown().await;
    assert_eq!(events.drain(), [fs_change(1, &["/a", "/b"])]);
}

#[tokio::test]
async fn published_events_reach_every_subscriber() {
    let h = Harness::new().await;
    let mut first = h.events();
    let mut second = h.events();

    let event = fs_change(7, &["/x"]);
    h.engine.publish(event.clone());
    assert_eq!(next_fs_change(&mut first).await, event);
    assert_eq!(next_fs_change(&mut second).await, event);
}

#[tokio::test]
async fn task_and_fs_events_share_one_stream() {
    let h = Harness::new().await;
    let mut events = h.events();

    let id = submit_instant(&h.engine, fs()).await;
    h.engine.publish_fs_change(3, dirs(&["/moved"]));
    h.finished(id).await;
    tokio::time::sleep(QUIET).await;

    let seen = events.drain();
    assert!(seen.contains(&fs_change(3, &["/moved"])), "{seen:?}");
    assert_eq!(
        states(&task_events(&seen, id)),
        [TaskState::Queued, TaskState::Running, TaskState::Done]
    );
}
