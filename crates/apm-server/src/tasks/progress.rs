//! Progress reporting for a running task: [`TaskProgress`] is what a runner holds, the engine
//! reads it to build `TaskDto`s.
//!
//! # Units
//!
//! A task counts `items` (files, assets, ...) and `bytes`. Either total may be unknown (`None`).
//! The speed in `TaskDto.speed` is bytes per second **when `bytes_total` is set** and items per
//! second otherwise: the rule of `apm_core::progress` and the doc of the DTO field, so a client can
//! label the unit from `bytes_total` alone. A task that wants a byte rate therefore declares a byte
//! total as soon as it knows one, even an estimate; byte progress reported without a total shows
//! the items rate. The ETA divides what remains of the declared total (bytes first, then items) by
//! that speed.
//!
//! # Costs
//!
//! Reporting is a short critical section on a `std::sync::Mutex` (two additions), never an
//! `await`, so it is safe from any thread, including `spawn_blocking` closures and parallel
//! workers. The rate estimator is fed when somebody *reads* the progress (the engine does so a few
//! times per second while the task runs), not on every report; the estimate depends on elapsed
//! time, not on the number of samples (see `apm_core::progress`).

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use apm_core::progress::ProgressTracker;

/// A monotonic millisecond clock for the rate estimator. The default counts from its creation; tests
/// pass their own to make speeds and ETAs deterministic.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> u64 + Send + Sync>);

impl Clock {
    /// Milliseconds elapsed since this clock was created.
    pub fn system() -> Self {
        let origin = Instant::now();
        Self::new(move || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// A clock reading `now_ms` (milliseconds on any monotonic scale).
    pub fn new(now_ms: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self(Arc::new(now_ms))
    }

    pub fn now_ms(&self) -> u64 {
        (self.0)()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clock").finish_non_exhaustive()
    }
}

/// The progress of a task at one moment, with the rates the estimator derives from it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressView {
    pub title: String,
    pub items_done: i64,
    pub items_total: Option<i64>,
    pub bytes_done: i64,
    pub bytes_total: Option<i64>,
    /// Bytes per second when `bytes_total` is set, else items per second; `None` while warming up
    /// (the first second of a phase).
    pub speed: Option<f64>,
    pub eta_secs: Option<f64>,
}

impl ProgressView {
    /// The same counters without speed and ETA (a task that is not running has none).
    pub(super) fn without_rates(mut self) -> Self {
        self.speed = None;
        self.eta_secs = None;
        self
    }
}

/// Reports the progress of one task. Cheap to clone; every clone updates the same counters.
#[derive(Clone)]
pub struct TaskProgress {
    shared: Arc<Shared>,
}

struct Shared {
    clock: Clock,
    state: Mutex<State>,
}

struct State {
    title: String,
    items_done: i64,
    bytes_done: i64,
    items_total: Option<i64>,
    bytes_total: Option<i64>,
    tracker: ProgressTracker,
    /// Bumped by every change a reader may want to publish or persist.
    version: u64,
}

impl State {
    fn sync_tracker_totals(&mut self) {
        self.tracker.set_totals(
            self.items_total.map(non_negative),
            self.bytes_total.map(non_negative),
        );
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

impl TaskProgress {
    /// A progress that belongs to no task, with the system clock. Meant for the unit tests of code
    /// that reports progress (read the result with [`TaskProgress::view`]).
    pub fn new(title: impl Into<String>) -> Self {
        Self::with_clock(title, Clock::system())
    }

    pub(super) fn with_clock(title: impl Into<String>, clock: Clock) -> Self {
        let start = clock.now_ms();
        Self {
            shared: Arc::new(Shared {
                clock,
                state: Mutex::new(State {
                    title: title.into(),
                    items_done: 0,
                    bytes_done: 0,
                    items_total: None,
                    bytes_total: None,
                    tracker: ProgressTracker::new(start),
                    version: 0,
                }),
            }),
        }
    }

    /// Declares how much work there is (`None`: unknown) without touching the done counters.
    /// Negative totals count as zero. Declare `bytes` whenever you report bytes (see the module
    /// docs for the speed unit).
    pub fn set_totals(&self, items: Option<i64>, bytes: Option<i64>) {
        let mut state = self.lock();
        state.items_total = items.map(|total| total.max(0));
        state.bytes_total = bytes.map(|total| total.max(0));
        state.sync_tracker_totals();
        state.bump();
    }

    /// Adds finished work. Negative amounts are ignored: progress only moves forward.
    pub fn add(&self, items: i64, bytes: i64) {
        let (items, bytes) = (items.max(0), bytes.max(0));
        if items == 0 && bytes == 0 {
            return;
        }
        let mut state = self.lock();
        state.items_done = state.items_done.saturating_add(items);
        state.bytes_done = state.bytes_done.saturating_add(bytes);
        state.bump();
    }

    /// Replaces the title shown for the task.
    pub fn set_title(&self, title: impl Into<String>) {
        let title = title.into();
        let mut state = self.lock();
        state.title = title;
        state.bump();
    }

    /// Starts a new phase of a multi-step task (scan, metadata, hashing, thumbnails). In ONE step,
    /// under one lock: the done counters go back to zero, the rate estimator starts over, the totals
    /// are replaced and the title is set, so the bar, the speed and the ETA restart cleanly and a
    /// reader never sees a mix of two phases.
    pub fn phase(
        &self,
        title: impl Into<String>,
        items_total: Option<i64>,
        bytes_total: Option<i64>,
    ) {
        let title = title.into();
        let now = self.shared.clock.now_ms();
        let mut state = self.lock();
        state.title = title;
        state.items_done = 0;
        state.bytes_done = 0;
        state.items_total = items_total.map(|total| total.max(0));
        state.bytes_total = bytes_total.map(|total| total.max(0));
        state.tracker = ProgressTracker::new(now);
        state.sync_tracker_totals();
        state.bump();
    }

    /// The current counters, title, speed and ETA.
    pub fn view(&self) -> ProgressView {
        self.sample().0
    }

    /// Feeds the estimator with the counters as of now and reads the result, together with the
    /// version of the state it reflects.
    pub(super) fn sample(&self) -> (ProgressView, u64) {
        let now = self.shared.clock.now_ms();
        let mut state = self.lock();
        let (bytes, items) = (
            non_negative(state.bytes_done),
            non_negative(state.items_done),
        );
        state.tracker.record(now, bytes, items);
        let snapshot = state.tracker.snapshot(now);
        let view = ProgressView {
            title: state.title.clone(),
            items_done: state.items_done,
            items_total: state.items_total,
            bytes_done: state.bytes_done,
            bytes_total: state.bytes_total,
            speed: snapshot.primary_speed(),
            eta_secs: snapshot.eta_secs,
        };
        (view, state.version)
    }

    /// Restarts the rate estimator at the moment the task starts running, so time spent in the
    /// queue neither counts as a stall nor skips the warm-up. The counters stay as they are.
    pub(super) fn start(&self) {
        let now = self.shared.clock.now_ms();
        let mut state = self.lock();
        state.tracker = ProgressTracker::new(now);
        state.sync_tracker_totals();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for TaskProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        f.debug_struct("TaskProgress")
            .field("title", &state.title)
            .field("items_done", &state.items_done)
            .field("bytes_done", &state.bytes_done)
            .finish_non_exhaustive()
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A clock the test moves by hand.
    fn manual() -> (Arc<AtomicU64>, Clock) {
        let now = Arc::new(AtomicU64::new(0));
        let clock = Clock::new({
            let now = Arc::clone(&now);
            move || now.load(Ordering::SeqCst)
        });
        (now, clock)
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("a value");
        assert!(
            (actual - expected).abs() <= expected.abs() * 0.01 + 1e-6,
            "{actual} is not close to {expected}"
        );
    }

    #[test]
    fn counters_accumulate_and_ignore_negative_amounts() {
        let progress = TaskProgress::new("Moving");
        progress.add(2, 100);
        progress.add(-5, -1);
        progress.add(0, 0);
        progress.add(1, 50);
        let view = progress.view();
        assert_eq!((view.items_done, view.bytes_done), (3, 150));
        assert_eq!(view.title, "Moving");
        assert_eq!((view.items_total, view.bytes_total), (None, None));

        progress.set_totals(Some(10), Some(-4));
        let view = progress.view();
        assert_eq!((view.items_total, view.bytes_total), (Some(10), Some(0)));
        assert_eq!(
            (view.items_done, view.bytes_done),
            (3, 150),
            "counters stay"
        );
    }

    #[test]
    fn counters_saturate_instead_of_overflowing() {
        let progress = TaskProgress::new("Big");
        progress.add(i64::MAX, i64::MAX);
        progress.add(i64::MAX, i64::MAX);
        let view = progress.view();
        assert_eq!((view.items_done, view.bytes_done), (i64::MAX, i64::MAX));
    }

    #[test]
    fn speed_and_eta_follow_a_steady_byte_rate() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Copy", clock);
        progress.set_totals(Some(100), Some(20_000));
        for second in 1..=10u64 {
            now.store(second * 1_000, Ordering::SeqCst);
            progress.add(10, 1_000);
            progress.view();
        }
        let view = progress.view();
        assert_close(view.speed, 1_000.0);
        assert_close(view.eta_secs, 10.0);
        assert_eq!((view.items_done, view.bytes_done), (100, 10_000));
    }

    #[test]
    fn the_speed_is_in_items_when_no_byte_total_is_declared() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Scan", clock);
        progress.set_totals(Some(1_000), None);
        for second in 1..=10u64 {
            now.store(second * 1_000, Ordering::SeqCst);
            progress.add(20, 5_000_000);
            progress.view();
        }
        let view = progress.view();
        assert_close(view.speed, 20.0);
        assert_close(view.eta_secs, 40.0);
    }

    #[test]
    fn there_is_no_speed_while_warming_up_nor_an_eta_without_a_total() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Scan", clock);
        now.store(500, Ordering::SeqCst);
        progress.add(5, 0);
        let view = progress.view();
        assert_eq!((view.speed, view.eta_secs), (None, None));

        now.store(3_000, Ordering::SeqCst);
        progress.add(25, 0);
        let view = progress.view();
        assert!(view.speed.is_some_and(|speed| speed > 0.0));
        assert_eq!(view.eta_secs, None, "no total, no ETA");
    }

    #[test]
    fn the_speed_decays_while_nothing_is_reported() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Copy", clock);
        progress.set_totals(None, Some(1_000_000));
        for second in 1..=5u64 {
            now.store(second * 1_000, Ordering::SeqCst);
            progress.add(0, 10_000);
            progress.view();
        }
        let moving = progress.view().speed.unwrap();
        now.store(15_000, Ordering::SeqCst);
        let stalled = progress.view().speed.unwrap();
        assert!(
            stalled < moving / 2.0,
            "{stalled} should have decayed from {moving}"
        );
    }

    #[test]
    fn a_phase_resets_counters_totals_title_and_estimator_in_one_step() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Scanning", clock);
        progress.set_totals(Some(50), Some(5_000));
        for second in 1..=5u64 {
            now.store(second * 1_000, Ordering::SeqCst);
            progress.add(10, 1_000);
            progress.view();
        }
        let before = progress.view();
        assert!(before.speed.is_some());
        assert_eq!(before.items_done, 50);

        progress.phase("Hashing", Some(7), Some(700));
        let after = progress.view();
        assert_eq!(after.title, "Hashing");
        assert_eq!((after.items_done, after.bytes_done), (0, 0));
        assert_eq!((after.items_total, after.bytes_total), (Some(7), Some(700)));
        assert_eq!(
            (after.speed, after.eta_secs),
            (None, None),
            "warming up again"
        );

        now.store(7_000, Ordering::SeqCst);
        progress.add(2, 200);
        let later = progress.view();
        assert_eq!((later.items_done, later.bytes_done), (2, 200));
        assert_close(later.speed, 100.0);
        assert_close(later.eta_secs, 5.0);
    }

    #[test]
    fn a_phase_without_totals_clears_the_previous_ones() {
        let progress = TaskProgress::new("A");
        progress.set_totals(Some(5), Some(5));
        progress.phase("B", None, None);
        let view = progress.view();
        assert_eq!((view.items_total, view.bytes_total), (None, None));
    }

    #[test]
    fn starting_the_clock_skips_the_time_spent_queued() {
        let (now, clock) = manual();
        let progress = TaskProgress::with_clock("Queued", clock);
        progress.set_totals(None, Some(10_000));
        now.store(60_000, Ordering::SeqCst);
        progress.start();
        now.store(60_500, Ordering::SeqCst);
        progress.add(0, 500);
        assert_eq!(progress.view().speed, None, "still warming up after 0.5 s");
        now.store(62_000, Ordering::SeqCst);
        progress.add(0, 1_500);
        assert_close(progress.view().speed, 1_000.0);
    }

    #[test]
    fn the_version_moves_with_every_change() {
        let progress = TaskProgress::new("v");
        let (_, v0) = progress.sample();
        assert_eq!(progress.sample().1, v0, "reading does not change it");
        progress.add(1, 0);
        let (_, v1) = progress.sample();
        progress.set_title("w");
        let (_, v2) = progress.sample();
        progress.set_totals(Some(1), None);
        let (_, v3) = progress.sample();
        progress.phase("x", None, None);
        let (_, v4) = progress.sample();
        assert!(v0 < v1 && v1 < v2 && v2 < v3 && v3 < v4);
        progress.add(0, 0);
        assert_eq!(progress.sample().1, v4, "adding nothing is not a change");
    }

    #[test]
    fn clones_share_one_state_and_threads_do_not_lose_updates() {
        let progress = TaskProgress::new("Parallel");
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let progress = progress.clone();
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        progress.add(1, 3);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let view = progress.view();
        assert_eq!((view.items_done, view.bytes_done), (8_000, 24_000));
    }

    #[test]
    fn a_view_without_rates_keeps_the_counters() {
        let view = ProgressView {
            title: "t".to_owned(),
            items_done: 1,
            items_total: Some(2),
            bytes_done: 3,
            bytes_total: Some(4),
            speed: Some(5.0),
            eta_secs: Some(6.0),
        };
        let bare = view.clone().without_rates();
        assert_eq!((bare.speed, bare.eta_secs), (None, None));
        assert_eq!(
            (bare.items_done, bare.bytes_total),
            (view.items_done, view.bytes_total)
        );
    }
}
