//! Lanes: how many tasks run at once, and in which order they start.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use opm_core::dto::{ServerEvent, TaskKind, TaskState};
use opm_core::paths::TaskId;
use serde_json::json;
use tokio::sync::Semaphore;

use super::support::{
    Gate, Harness, Outcome, eventually, fast, fs, immich, index, states, submit_instant,
    task_events,
};
use crate::tasks::{Lane, LaneLimits, TaskCtx, TaskError, TaskOptions, TaskSpec};

async fn runs_three_at_a_time(spec: fn() -> TaskSpec) {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(h.engine.submit(spec(), gate.runner()).await.unwrap());
    }

    let mut started = [
        gate.started().await,
        gate.started().await,
        gate.started().await,
    ];
    started.sort_unstable();
    assert_eq!(started, ids[..3]);
    gate.assert_nothing_starts().await;
    assert_eq!(h.count_in("running").await, 3);
    assert_eq!(h.count_in("queued").await, 2);

    for next in &ids[3..] {
        gate.release(1);
        assert_eq!(gate.started().await, *next);
        assert_eq!(h.count_in("running").await, 3, "three slots, three tasks");
    }
    gate.release(3);
    for id in ids {
        assert_eq!(h.finished(id).await.state, TaskState::Done);
    }
}

#[tokio::test]
async fn the_fs_lane_runs_three_tasks_at_a_time() {
    runs_three_at_a_time(fs).await;
}

#[tokio::test]
async fn the_index_lane_runs_three_tasks_at_a_time() {
    runs_three_at_a_time(index).await;
}

#[tokio::test]
async fn the_immich_lane_runs_three_tasks_at_once() {
    runs_three_at_a_time(immich).await;
}

#[tokio::test]
async fn mixed_lanes_share_three_slots_and_cancellation_releases_one() {
    let h = Harness::new().await;
    let mut gate = Gate::new();
    let mut ids = Vec::new();
    for spec in [fs(), index(), immich(), immich(), index(), fs()] {
        ids.push(h.engine.submit(spec, gate.runner()).await.unwrap());
    }
    let mut first = [
        gate.started().await,
        gate.started().await,
        gate.started().await,
    ];
    first.sort_unstable();
    assert_eq!(first, ids[..3]);
    gate.assert_nothing_starts().await;
    assert_eq!(h.count_in("running").await, 3);
    assert_eq!(h.count_in("queued").await, 3);
    h.engine.cancel(ids[0]).await.unwrap();
    assert_eq!(h.finished(ids[0]).await.state, TaskState::Cancelled);
    assert_eq!(gate.started().await, ids[3]);
    assert_eq!(h.count_in("running").await, 3);
    h.engine.cancel(ids[5]).await.unwrap();
    assert_eq!(h.finished(ids[5]).await.started_at, None);
    gate.release(1);
    assert_eq!(gate.started().await, ids[4]);
    gate.assert_nothing_starts().await;
    assert_eq!(h.count_in("running").await, 3);
    gate.release(3);
    for id in &ids[1..5] {
        assert_eq!(h.finished(*id).await.state, TaskState::Done);
    }
}

#[tokio::test]
async fn lanes_do_not_wait_for_each_other() {
    let h = Harness::serial_fs().await;
    let mut gate = Gate::new();
    let blocked = h.engine.submit(fs(), gate.runner()).await.unwrap();
    assert_eq!(gate.started().await, blocked);
    let waiting = h.engine.submit(fs(), gate.runner()).await.unwrap();

    let elsewhere =
        TaskSpec::new(TaskKind::FsMove, "A move in another lane").with_lane(Lane::Immich);
    for spec in [index(), immich(), elsewhere] {
        let id = submit_instant(&h.engine, spec).await;
        assert_eq!(h.finished(id).await.state, TaskState::Done);
    }
    assert_eq!(h.row(blocked).await.state, "running");
    assert_eq!(h.row(waiting).await.state, "queued");

    gate.release(2);
    assert_eq!(h.finished(blocked).await.state, TaskState::Done);
    assert_eq!(h.finished(waiting).await.state, TaskState::Done);
}

#[tokio::test]
async fn the_limits_can_be_changed() {
    let options = TaskOptions {
        concurrency: LaneLimits {
            fs: 2,
            ..LaneLimits::default()
        },
        ..fast()
    };
    let h = Harness::with(options).await;
    let mut gate = Gate::new();
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(h.engine.submit(fs(), gate.runner()).await.unwrap());
    }

    let mut first_two = [gate.started().await, gate.started().await];
    first_two.sort_unstable();
    assert_eq!(first_two, [ids[0], ids[1]]);
    gate.assert_nothing_starts().await;

    gate.release(1);
    assert_eq!(gate.started().await, ids[2]);
    gate.release(4);
    for id in ids {
        h.finished(id).await;
    }
}

#[tokio::test]
async fn a_limit_of_zero_does_not_starve_the_lane() {
    let options = TaskOptions {
        concurrency: LaneLimits {
            fs: 0,
            index: 0,
            immich: 0,
        },
        ..fast()
    };
    let h = Harness::with(options).await;
    for spec in [fs(), index(), immich()] {
        let id = submit_instant(&h.engine, spec).await;
        assert_eq!(h.finished(id).await.state, TaskState::Done);
    }
}

/// How many runners of a lane are inside at once, how many were at most, and in which order they
/// went in.
#[derive(Default)]
struct Occupancy {
    inside: AtomicUsize,
    peak: AtomicUsize,
    entered: Mutex<Vec<TaskId>>,
}

impl Occupancy {
    fn enter(&self, id: TaskId) {
        let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        self.entered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }

    fn leave(&self) {
        self.inside.fetch_sub(1, Ordering::SeqCst);
    }

    fn inside(&self) -> usize {
        self.inside.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn entered(&self) -> Vec<TaskId> {
        self.entered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A runner that counts itself into `lane`, then waits for the cancellation or, when `hold` is
/// given, for one of its permits (without it a short nap), and ends `Ok` in that case.
fn counted(
    lane: &Arc<Occupancy>,
    hold: Option<&Arc<Semaphore>>,
) -> impl FnOnce(TaskCtx) -> BoxFuture<'static, Outcome> + Send + 'static {
    let (lane, hold) = (Arc::clone(lane), hold.cloned());
    move |ctx| {
        async move {
            lane.enter(ctx.id);
            let outcome = match hold {
                Some(hold) => tokio::select! {
                    () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                    permit = hold.acquire() => {
                        permit.expect("the semaphore is never closed").forget();
                        Ok(json!({}))
                    }
                },
                None => tokio::select! {
                    () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                    () = tokio::time::sleep(Duration::from_millis(3)) => Ok(json!({})),
                },
            };
            lane.leave();
            outcome
        }
        .boxed()
    }
}

#[tokio::test]
async fn sixty_tasks_respect_the_limits_and_their_order() {
    let h = Harness::with(TaskOptions {
        concurrency: LaneLimits {
            fs: 1,
            index: 1,
            immich: 2,
        },
        ..fast()
    })
    .await;
    let mut events = h.events();
    let (fs_lane, immich_lane) = (
        Arc::new(Occupancy::default()),
        Arc::new(Occupancy::default()),
    );
    let hold = Arc::new(Semaphore::new(0));
    let (mut fs_ids, mut immich_ids) = (Vec::new(), Vec::new());
    for n in 0..60 {
        if n % 2 == 0 {
            let run = counted(&fs_lane, Some(&hold));
            fs_ids.push(h.engine.submit(fs(), run).await.unwrap());
        } else {
            let run = counted(&immich_lane, Some(&hold));
            immich_ids.push(h.engine.submit(immich(), run).await.unwrap());
        }
    }

    // Everything is submitted and nothing can finish yet: the lanes are as full as they may be.
    eventually("the first runners to start", || {
        let (fs_lane, immich_lane) = (&fs_lane, &immich_lane);
        async move { (fs_lane.inside() == 1 && immich_lane.inside() == 2).then_some(()) }
    })
    .await;
    assert_eq!(h.count_in("running").await, 3);
    assert_eq!(h.count_in("queued").await, 57);

    hold.add_permits(60);
    for id in fs_ids.iter().chain(&immich_ids) {
        assert_eq!(h.finished(*id).await.state, TaskState::Done);
    }

    assert_eq!(fs_lane.peak(), 1);
    assert_eq!(immich_lane.peak(), 2);
    assert_eq!(
        fs_lane.entered(),
        fs_ids,
        "the fs lane is first in, first out"
    );

    let all = events.drain();
    for id in fs_ids.iter().chain(&immich_ids) {
        assert_eq!(
            states(&task_events(&all, *id)),
            [TaskState::Queued, TaskState::Running, TaskState::Done],
            "task {id}"
        );
    }
    let started: Vec<TaskId> = all
        .iter()
        .filter_map(|event| match event {
            ServerEvent::Task { task } if task.state == TaskState::Running => Some(task.id),
            _ => None,
        })
        .filter(|id| fs_ids.contains(id))
        .collect();
    assert_eq!(started, fs_ids, "announced as running in submission order");
}

/// A pseudo-random sequence, so that a failing run can be repeated.
fn lcg(state: &mut u64) -> usize {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    usize::try_from(*state >> 33).unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_while_tasks_come_and_go_never_breaks_the_lanes() {
    let h = Harness::new().await;
    let mut events = h.events();
    let lanes: [Arc<Occupancy>; 3] = std::array::from_fn(|_| Arc::new(Occupancy::default()));
    let hold = Arc::new(Semaphore::new(0));
    let specs: [fn() -> TaskSpec; 3] = [fs, index, immich];

    let mut ids = Vec::new();
    let mut seed = 7;
    for n in 0..120 {
        let lane = n % 3;
        // The first task of each lane waits until the end, so that a line forms behind it.
        let run = counted(&lanes[lane], (n < 3).then_some(&hold));
        ids.push(h.engine.submit(specs[lane](), run).await.unwrap());

        if n >= 6 {
            let target = ids[lcg(&mut seed) % ids.len()];
            match h.engine.cancel(target).await {
                Ok(_) => {}
                Err(err) => assert_eq!(err.code(), "conflict", "{err:?}"),
            }
        }
    }
    hold.add_permits(3);

    let (mut done, mut cancelled) = (0, 0);
    for id in &ids {
        match h.finished(*id).await.state {
            TaskState::Done => done += 1,
            TaskState::Cancelled => cancelled += 1,
            other => panic!("task {id} ended {other:?}"),
        }
    }
    assert!(
        cancelled > 0 && done > 0,
        "{done} done, {cancelled} cancelled"
    );

    assert!(lanes.iter().all(|lane| lane.peak() <= 3));
    assert!(lanes.iter().all(|lane| lane.inside() == 0));
    assert_eq!(h.active_rows().await, 0);
    assert!(h.engine.list(0).await.unwrap().is_empty());

    let all = events.drain();
    for id in &ids {
        let seen = states(&task_events(&all, *id));
        let allowed: [&[TaskState]; 3] = [
            &[TaskState::Queued, TaskState::Running, TaskState::Done],
            &[TaskState::Queued, TaskState::Running, TaskState::Cancelled],
            &[TaskState::Queued, TaskState::Cancelled],
        ];
        assert!(allowed.contains(&seen.as_slice()), "task {id}: {seen:?}");
    }
}
