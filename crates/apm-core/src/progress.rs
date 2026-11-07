//! Rate and ETA estimator behind task progress.
//!
//! [`ProgressTracker`] turns a stream of cumulative counters (`done_bytes`,
//! `done_items`) into a [`ProgressSnapshot`]: a smoothed speed, the estimated time
//! remaining and a percentage. There is no clock in here. The caller passes the
//! current time as milliseconds on any monotonic scale (for example milliseconds
//! since the task started), so tests are fully deterministic. The snapshot is
//! plain Rust and is not exported to TypeScript: the task engine copies it into
//! the `TaskDto` fields.
//!
//! # Semantics other code relies on
//!
//! - **Smoothing.** The speed is the average rate over roughly the last
//!   [`DEFAULT_WINDOW_MS`] (5 s). Bytes (or items) and elapsed time are both
//!   weighted by `exp(-age / window)` and divided, so the estimate depends on time
//!   and not on the number of samples: how often the caller reports does not
//!   matter, a burst of bytes within a few milliseconds is averaged over the
//!   window instead of being extrapolated to a huge instantaneous speed, and the
//!   speed of a task that reports at a steady rate is exactly that rate.
//! - **Stalls.** [`ProgressTracker::snapshot`] folds the idle time since the last
//!   sample into the estimate without changing the tracker, so the speed decays
//!   while nothing is reported (and never rises during a stall). A speed below
//!   [`MIN_SPEED`] is reported as `0.0`.
//! - **Warm-up.** Speeds and the ETA are `None` until a fifth of the window (1 s
//!   by default) has elapsed since the tracker started.
//! - **Speed unit.** [`ProgressSnapshot::primary_speed`] is what goes into
//!   `TaskDto.speed`: bytes per second when a byte total is known, items per
//!   second otherwise. Set a byte total only when the task reports byte progress.
//! - **ETA.** `remaining / primary speed`. `None` when no total is known, while
//!   warming up, when the speed is zero or when the estimate exceeds
//!   [`MAX_ETA_SECS`] (30 days). `Some(0.0)` as soon as the total is reached,
//!   whatever the speed.
//! - **Percent.** Based on the byte total when it is greater than zero, else on
//!   the item total; clamped to `0..=100`; a known total of zero is complete.
//! - **Counters** are cumulative since the tracker started (use
//!   [`ProgressTracker::with_initial`] when a resumed task starts above zero).
//!   Counters that go down contribute no progress, and time that goes backwards
//!   is treated as "no time has passed". Every output is finite and not negative,
//!   and nothing here can panic or divide by zero.

/// Default smoothing window in milliseconds (see the module docs).
pub const DEFAULT_WINDOW_MS: u64 = 5_000;

/// Remaining-time estimates above this many seconds (30 days) are reported as
/// unknown.
pub const MAX_ETA_SECS: f64 = 30.0 * 24.0 * 3600.0;

/// A speed below this many units per second counts as stalled and is reported as
/// `0.0`.
pub const MIN_SPEED: f64 = 1e-3;

/// Smallest accumulated duration, in seconds, a speed is derived from.
const MIN_SPAN_SECS: f64 = 1e-6;

/// Exponentially decayed totals behind the speed: bytes, items and seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sums {
    bytes: f64,
    items: f64,
    secs: f64,
}

impl Sums {
    const ZERO: Sums = Sums {
        bytes: 0.0,
        items: 0.0,
        secs: 0.0,
    };
}

/// Smoothed speed and ETA estimator fed with cumulative counters.
///
/// ```
/// use apm_core::progress::ProgressTracker;
///
/// let mut tracker = ProgressTracker::new(0);
/// tracker.set_totals(Some(100), Some(2_000_000));
/// for second in 1..=10u64 {
///     tracker.record(second * 1_000, second * 100_000, second * 4);
/// }
/// let snapshot = tracker.snapshot(10_000);
/// assert_eq!(snapshot.bytes_per_sec.map(f64::round), Some(100_000.0));
/// assert_eq!(snapshot.eta_secs.map(f64::round), Some(10.0));
/// assert_eq!(snapshot.percent().map(f64::round), Some(50.0));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressTracker {
    window_ms: u64,
    start_ms: u64,
    total_items: Option<u64>,
    total_bytes: Option<u64>,
    /// Time of the last folded sample and the counters at that time.
    base_ms: u64,
    base_bytes: u64,
    base_items: u64,
    /// Latest counters reported. They can run ahead of the base counters when
    /// several samples share one millisecond.
    done_bytes: u64,
    done_items: u64,
    sums: Sums,
}

impl ProgressTracker {
    /// Starts tracking at `start_ms` with both counters at zero and the default
    /// window.
    pub fn new(start_ms: u64) -> Self {
        Self {
            window_ms: DEFAULT_WINDOW_MS,
            start_ms,
            total_items: None,
            total_bytes: None,
            base_ms: start_ms,
            base_bytes: 0,
            base_items: 0,
            done_bytes: 0,
            done_items: 0,
            sums: Sums::ZERO,
        }
    }

    /// Sets the smoothing window (at least 1 ms). A shorter window follows speed
    /// changes faster and is noisier.
    #[must_use]
    pub fn with_window_ms(mut self, window_ms: u64) -> Self {
        self.window_ms = window_ms.max(1);
        self
    }

    /// Declares that the counters already stand at these values at the start
    /// (a resumed task), so that they are not counted as fresh progress.
    #[must_use]
    pub fn with_initial(mut self, done_bytes: u64, done_items: u64) -> Self {
        self.base_bytes = done_bytes;
        self.base_items = done_items;
        self.done_bytes = done_bytes;
        self.done_items = done_items;
        self
    }

    /// Sets or clears the totals (they may be learned or revised while the task
    /// runs). `None` means unknown.
    pub fn set_totals(&mut self, total_items: Option<u64>, total_bytes: Option<u64>) {
        self.total_items = total_items;
        self.total_bytes = total_bytes;
    }

    /// Records the cumulative counters at time `now_ms`.
    ///
    /// Samples may be as frequent or as sparse as convenient. A timestamp earlier
    /// than the previous sample is treated as equal to it.
    pub fn record(&mut self, now_ms: u64, done_bytes: u64, done_items: u64) {
        let now_ms = now_ms.max(self.base_ms);
        self.done_bytes = done_bytes;
        self.done_items = done_items;
        if now_ms > self.base_ms {
            self.sums = self.folded(now_ms);
            self.base_ms = now_ms;
            self.base_bytes = done_bytes;
            self.base_items = done_items;
        }
    }

    /// The current state at time `now_ms`. Does not change the tracker, so it can
    /// be called as often as needed, including long after the last sample (the
    /// speed then reflects the stall).
    pub fn snapshot(&self, now_ms: u64) -> ProgressSnapshot {
        let now_ms = now_ms.max(self.base_ms);
        let sums = self.folded(now_ms);
        let warmed_up = now_ms.saturating_sub(self.start_ms) >= (self.window_ms / 5).max(1);
        let (bytes_per_sec, items_per_sec) = if warmed_up && sums.secs >= MIN_SPAN_SECS {
            (
                per_second(sums.bytes, sums.secs),
                per_second(sums.items, sums.secs),
            )
        } else {
            (None, None)
        };
        let eta_secs = match (self.total_bytes, self.total_items) {
            (Some(total), _) => eta(total, self.done_bytes, bytes_per_sec),
            (None, Some(total)) => eta(total, self.done_items, items_per_sec),
            (None, None) => None,
        };
        ProgressSnapshot {
            done_items: self.done_items,
            total_items: self.total_items,
            done_bytes: self.done_bytes,
            total_bytes: self.total_bytes,
            bytes_per_sec,
            items_per_sec,
            eta_secs,
        }
    }

    /// The decayed sums after folding the interval from the last sample up to
    /// `now_ms`, with the counters reported since then credited to it.
    fn folded(&self, now_ms: u64) -> Sums {
        let elapsed_ms = now_ms.saturating_sub(self.base_ms);
        if elapsed_ms == 0 {
            return self.sums;
        }
        let elapsed_ms = elapsed_ms as f64;
        let decay = (-elapsed_ms / self.window_ms as f64).exp();
        Sums {
            bytes: self.sums.bytes * decay + self.done_bytes.saturating_sub(self.base_bytes) as f64,
            items: self.sums.items * decay + self.done_items.saturating_sub(self.base_items) as f64,
            secs: self.sums.secs * decay + elapsed_ms / 1000.0,
        }
    }
}

/// A point-in-time view of a task's progress. Plain data, not exported to
/// TypeScript.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProgressSnapshot {
    /// Items completed so far.
    pub done_items: u64,
    /// Items in total, when known.
    pub total_items: Option<u64>,
    /// Bytes processed so far.
    pub done_bytes: u64,
    /// Bytes in total, when known.
    pub total_bytes: Option<u64>,
    /// Smoothed bytes per second; `None` while warming up.
    pub bytes_per_sec: Option<f64>,
    /// Smoothed items per second; `None` while warming up.
    pub items_per_sec: Option<f64>,
    /// Estimated seconds remaining; see the module docs for when it is `None`.
    pub eta_secs: Option<f64>,
}

impl ProgressSnapshot {
    /// Completion between 0 and 100: byte-based when the byte total is greater
    /// than zero, else item-based. A known total of zero is complete (100) and no
    /// known total gives `None`.
    pub fn percent(&self) -> Option<f64> {
        match (self.total_bytes, self.total_items) {
            (Some(total), _) if total > 0 => Some(percent_of(self.done_bytes, total)),
            (_, Some(total)) if total > 0 => Some(percent_of(self.done_items, total)),
            (None, None) => None,
            _ => Some(100.0),
        }
    }

    /// The speed the task panel shows (`TaskDto.speed`): bytes per second when a
    /// byte total is known, items per second otherwise.
    pub fn primary_speed(&self) -> Option<f64> {
        if self.total_bytes.is_some() {
            self.bytes_per_sec
        } else {
            self.items_per_sec
        }
    }
}

fn per_second(sum: f64, secs: f64) -> Option<f64> {
    let rate = sum / secs;
    if !rate.is_finite() {
        return None;
    }
    Some(if rate < MIN_SPEED { 0.0 } else { rate })
}

fn eta(total: u64, done: u64, speed: Option<f64>) -> Option<f64> {
    let remaining = total.saturating_sub(done);
    if remaining == 0 {
        return Some(0.0);
    }
    let secs = remaining as f64 / speed.filter(|speed| *speed > 0.0)?;
    (secs.is_finite() && secs <= MAX_ETA_SECS).then_some(secs)
}

fn percent_of(done: u64, total: u64) -> f64 {
    (done as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MB: u64 = 1_000_000;

    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() <= tolerance * expected.abs()
    }

    /// A tracker with a virtual clock and cumulative counters.
    struct Sim {
        tracker: ProgressTracker,
        now: u64,
        bytes: u64,
        items: u64,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                tracker: ProgressTracker::new(0),
                now: 0,
                bytes: 0,
                items: 0,
            }
        }

        fn step(&mut self, ms: u64, bytes: u64, items: u64) {
            self.now += ms;
            self.bytes += bytes;
            self.items += items;
            self.tracker.record(self.now, self.bytes, self.items);
        }

        /// Runs `secs` seconds in 100 ms ticks at the given per-second rates.
        fn run(&mut self, secs: u64, bytes_per_sec: u64, items_per_sec: u64) {
            for _ in 0..secs * 10 {
                self.step(100, bytes_per_sec / 10, items_per_sec / 10);
            }
        }

        fn snap(&self) -> ProgressSnapshot {
            self.tracker.snapshot(self.now)
        }

        fn speed_after_idle(&self, secs: u64) -> f64 {
            let snapshot = self.tracker.snapshot(self.now + secs * 1000);
            snapshot.bytes_per_sec.unwrap()
        }
    }

    fn check_invariants(snapshot: &ProgressSnapshot) -> Result<(), TestCaseError> {
        for speed in [snapshot.bytes_per_sec, snapshot.items_per_sec]
            .into_iter()
            .flatten()
        {
            prop_assert!(speed.is_finite() && speed >= 0.0, "speed {speed}");
        }
        if let Some(eta) = snapshot.eta_secs {
            prop_assert!(eta.is_finite() && (0.0..=MAX_ETA_SECS).contains(&eta));
        }
        if let Some(percent) = snapshot.percent() {
            prop_assert!((0.0..=100.0).contains(&percent), "percent {percent}");
        }
        Ok(())
    }

    #[test]
    fn a_steady_rate_is_reported_exactly() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(Some(1_000), Some(200 * MB));
        sim.run(10, 10 * MB, 20);
        let snapshot = sim.snap();
        assert_eq!(snapshot.done_bytes, 100 * MB);
        assert_eq!(snapshot.done_items, 200);
        let ten_mb = 10.0 * MB as f64;
        assert!(close(snapshot.bytes_per_sec.unwrap(), ten_mb, 1e-9));
        assert!(close(snapshot.items_per_sec.unwrap(), 20.0, 1e-9));
        assert!(close(snapshot.eta_secs.unwrap(), 10.0, 1e-9));
        assert!(close(snapshot.percent().unwrap(), 50.0, 1e-9));
        assert_eq!(snapshot.primary_speed(), snapshot.bytes_per_sec);
    }

    #[test]
    fn the_documented_formula_is_what_is_computed() {
        let mut tracker = ProgressTracker::new(0);
        tracker.record(2_000, 4_000, 8);
        tracker.record(5_000, 10_000, 20);
        let decay = (-3_000.0f64 / 5_000.0).exp();
        let secs = 2.0 * decay + 3.0;
        let snapshot = tracker.snapshot(5_000);
        let bytes = snapshot.bytes_per_sec.unwrap();
        let items = snapshot.items_per_sec.unwrap();
        assert!(close(bytes, (4_000.0 * decay + 6_000.0) / secs, 1e-12));
        assert!(close(items, (8.0 * decay + 12.0) / secs, 1e-12));
    }

    #[test]
    fn nothing_is_estimated_during_the_warm_up() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(None, Some(100 * MB));
        sim.step(500, 5 * MB, 5);
        let snapshot = sim.snap();
        assert_eq!(snapshot.bytes_per_sec, None);
        assert_eq!(snapshot.items_per_sec, None);
        assert_eq!(snapshot.eta_secs, None);
        assert_eq!(snapshot.done_bytes, 5 * MB);
        assert!(close(snapshot.percent().unwrap(), 5.0, 1e-9));

        sim.step(500, 5 * MB, 5);
        let snapshot = sim.snap();
        let ten_mb = 10.0 * MB as f64;
        assert!(close(snapshot.bytes_per_sec.unwrap(), ten_mb, 1e-9));
        assert!(snapshot.eta_secs.is_some());
    }

    #[test]
    fn a_stall_makes_the_speed_decay_without_new_samples() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(None, Some(10_000 * MB));
        sim.run(10, 10 * MB, 0);
        let steady = sim.speed_after_idle(0);
        let (after_5, after_10, after_60) = (
            sim.speed_after_idle(5),
            sim.speed_after_idle(10),
            sim.speed_after_idle(60),
        );
        assert!(steady > after_5 && after_5 > after_10 && after_10 > after_60);
        assert!(after_5 < 0.5 * steady, "{after_5} vs {steady}");
        assert!(after_10 < 0.1 * steady, "{after_10} vs {steady}");
        assert_eq!(sim.speed_after_idle(600), 0.0);

        let eta_5 = sim.tracker.snapshot(sim.now + 5_000).eta_secs.unwrap();
        let eta_10 = sim.tracker.snapshot(sim.now + 10_000).eta_secs.unwrap();
        assert!(eta_10 > eta_5);
        assert_eq!(sim.tracker.snapshot(sim.now + 600_000).eta_secs, None);
    }

    #[test]
    fn the_speed_recovers_after_a_stall() {
        let mut sim = Sim::new();
        sim.run(10, 10 * MB, 0);
        sim.step(20_000, 0, 0);
        let stalled = sim.snap().bytes_per_sec.unwrap();
        assert!(stalled < 0.5 * MB as f64, "{stalled}");
        sim.run(10, 10 * MB, 0);
        let resumed = sim.snap().bytes_per_sec.unwrap();
        assert!(resumed > 5.0 * MB as f64, "{resumed}");
        sim.run(30, 10 * MB, 0);
        assert!(sim.snap().bytes_per_sec.unwrap() > 9.0 * MB as f64);
    }

    #[test]
    fn a_burst_is_averaged_over_the_window_not_extrapolated() {
        let mut sim = Sim::new();
        sim.run(20, MB, 0);
        sim.step(10, 50 * MB, 0);
        let burst = sim.snap().bytes_per_sec.unwrap();
        assert!(burst > 5.0 * MB as f64, "{burst}");
        // The instantaneous speed would be 50 MB / 10 ms = 5 GB/s.
        assert!(burst < 15.0 * MB as f64, "{burst}");
        sim.run(30, MB, 0);
        assert!(close(sim.snap().bytes_per_sec.unwrap(), MB as f64, 0.05));
    }

    #[test]
    fn the_estimate_does_not_depend_on_how_often_samples_arrive() {
        // 2 MB/s for 10 s, then 8 MB/s for 5 s, sampled at different rates.
        let speed_with_ticks_of = |tick_ms: u64| {
            let mut tracker = ProgressTracker::new(0);
            let mut bytes = 0;
            let mut now = 0;
            while now < 15_000 {
                let rate = if now < 10_000 { 2 * MB } else { 8 * MB };
                now += tick_ms;
                bytes += rate * tick_ms / 1000;
                tracker.record(now, bytes, 0);
            }
            tracker.snapshot(now).bytes_per_sec.unwrap()
        };
        let fine = speed_with_ticks_of(50);
        assert!(close(speed_with_ticks_of(500), fine, 0.02));
        assert!(close(speed_with_ticks_of(1_000), fine, 0.04));
    }

    #[test]
    fn without_totals_there_is_a_speed_but_no_eta_or_percent() {
        let mut sim = Sim::new();
        sim.run(5, MB, 30);
        let snapshot = sim.snap();
        assert!(snapshot.bytes_per_sec.is_some());
        assert!(close(snapshot.items_per_sec.unwrap(), 30.0, 1e-9));
        assert_eq!(snapshot.total_items, None);
        assert_eq!(snapshot.total_bytes, None);
        assert_eq!(snapshot.eta_secs, None);
        assert_eq!(snapshot.percent(), None);
        assert_eq!(snapshot.primary_speed(), snapshot.items_per_sec);
    }

    #[test]
    fn items_drive_the_eta_when_there_is_no_byte_total() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(Some(1_000), None);
        sim.run(10, 0, 50);
        let snapshot = sim.snap();
        assert!(close(snapshot.eta_secs.unwrap(), 10.0, 1e-9));
        assert!(close(snapshot.percent().unwrap(), 50.0, 1e-9));
        assert!(close(snapshot.primary_speed().unwrap(), 50.0, 1e-9));
    }

    #[test]
    fn bytes_win_over_items_when_both_totals_are_known() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(Some(200), Some(110 * MB));
        sim.run(10, 10 * MB, 10);
        let snapshot = sim.snap();
        // 100 MB of 110 MB done at 10 MB/s.
        assert!(close(snapshot.eta_secs.unwrap(), 1.0, 1e-9));
        let percent = snapshot.percent().unwrap();
        assert!(close(percent, 100.0 * 100.0 / 110.0, 1e-9));
        let speed = snapshot.primary_speed().unwrap();
        assert!(close(speed, 10.0 * MB as f64, 1e-9));
    }

    #[test]
    fn totals_can_be_learned_and_revised_while_running() {
        let mut sim = Sim::new();
        sim.run(10, MB, 10);
        assert_eq!(sim.snap().eta_secs, None);
        sim.tracker.set_totals(Some(1_000), None);
        assert!(close(sim.snap().eta_secs.unwrap(), 90.0, 1e-9));
        sim.tracker.set_totals(Some(500), None);
        assert!(close(sim.snap().eta_secs.unwrap(), 40.0, 1e-9));
        sim.tracker.set_totals(None, None);
        assert_eq!(sim.snap().eta_secs, None);
    }

    #[test]
    fn reaching_the_total_means_nothing_remains_even_without_a_speed() {
        let mut tracker = ProgressTracker::new(0);
        tracker.set_totals(Some(3), Some(30));
        tracker.record(10, 30, 3);
        let snapshot = tracker.snapshot(10);
        assert_eq!(snapshot.bytes_per_sec, None);
        assert_eq!(snapshot.eta_secs, Some(0.0));
        assert_eq!(snapshot.percent(), Some(100.0));
    }

    #[test]
    fn overshooting_the_total_is_clamped() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(Some(10), None);
        sim.run(5, 0, 30);
        let snapshot = sim.snap();
        assert_eq!(snapshot.done_items, 150);
        assert_eq!(snapshot.percent(), Some(100.0));
        assert_eq!(snapshot.eta_secs, Some(0.0));
    }

    #[test]
    fn a_known_total_of_zero_is_complete() {
        let mut snapshot = ProgressSnapshot {
            total_items: Some(0),
            ..ProgressSnapshot::default()
        };
        assert_eq!(snapshot.percent(), Some(100.0));
        snapshot.total_bytes = Some(0);
        assert_eq!(snapshot.percent(), Some(100.0));
        snapshot.total_items = None;
        assert_eq!(snapshot.percent(), Some(100.0));

        // Empty files: no bytes to move, but items still count.
        let snapshot = ProgressSnapshot {
            total_bytes: Some(0),
            total_items: Some(4),
            done_items: 1,
            ..ProgressSnapshot::default()
        };
        assert_eq!(snapshot.percent(), Some(25.0));

        let mut tracker = ProgressTracker::new(0);
        tracker.set_totals(Some(0), None);
        assert_eq!(tracker.snapshot(0).eta_secs, Some(0.0));
    }

    #[test]
    fn percent_uses_bytes_first_then_items() {
        let snapshot = ProgressSnapshot {
            done_bytes: 25,
            total_bytes: Some(100),
            done_items: 9,
            total_items: Some(10),
            ..ProgressSnapshot::default()
        };
        assert_eq!(snapshot.percent(), Some(25.0));
        let snapshot = ProgressSnapshot {
            total_bytes: None,
            ..snapshot
        };
        assert_eq!(snapshot.percent(), Some(90.0));
        assert_eq!(ProgressSnapshot::default().percent(), None);
    }

    #[test]
    fn no_progress_means_no_eta_and_no_division_by_zero() {
        let mut sim = Sim::new();
        sim.tracker.set_totals(Some(10), Some(100));
        sim.run(30, 0, 0);
        let snapshot = sim.snap();
        assert_eq!(snapshot.bytes_per_sec, Some(0.0));
        assert_eq!(snapshot.items_per_sec, Some(0.0));
        assert_eq!(snapshot.eta_secs, None);
        assert_eq!(snapshot.percent(), Some(0.0));
    }

    #[test]
    fn samples_sharing_an_instant_never_divide_by_zero() {
        let mut tracker = ProgressTracker::new(1_000);
        let snapshot = tracker.snapshot(1_000);
        assert_eq!(snapshot.bytes_per_sec, None);
        assert_eq!(snapshot.eta_secs, None);

        tracker.record(1_000, 10, 1);
        tracker.record(1_000, 20, 2);
        let snapshot = tracker.snapshot(1_000);
        assert_eq!((snapshot.done_bytes, snapshot.done_items), (20, 2));
        assert_eq!(snapshot.bytes_per_sec, None);

        // Everything reported since the start counts in the next interval.
        tracker.record(2_000, 120, 12);
        let snapshot = tracker.snapshot(2_000);
        assert!(close(snapshot.bytes_per_sec.unwrap(), 120.0, 1e-9));
        assert!(close(snapshot.items_per_sec.unwrap(), 12.0, 1e-9));
    }

    #[test]
    fn time_never_runs_backwards() {
        let mut tracker = ProgressTracker::new(0);
        tracker.record(5_000, 5_000, 0);
        let at_last_sample = tracker.snapshot(5_000);
        assert_eq!(tracker.snapshot(1_000), at_last_sample);

        // An earlier timestamp only updates the counters.
        tracker.record(2_000, 6_000, 0);
        let snapshot = tracker.snapshot(5_000);
        assert_eq!(snapshot.done_bytes, 6_000);
        assert_eq!(snapshot.bytes_per_sec, at_last_sample.bytes_per_sec);

        // The counters reported meanwhile are credited to the next interval.
        tracker.record(6_000, 7_000, 0);
        let decay = (-1_000.0f64 / 5_000.0).exp();
        let expected = (5_000.0 * decay + 2_000.0) / (5.0 * decay + 1.0);
        let speed = tracker.snapshot(6_000).bytes_per_sec.unwrap();
        assert!(close(speed, expected, 1e-12), "{speed} vs {expected}");
    }

    #[test]
    fn counters_that_go_down_add_no_progress_and_never_underflow() {
        let mut tracker = ProgressTracker::new(0);
        tracker.record(1_000, MB, 100);
        tracker.record(2_000, 0, 0);
        let snapshot = tracker.snapshot(2_000);
        assert_eq!((snapshot.done_bytes, snapshot.done_items), (0, 0));
        let decay = (-1_000.0f64 / 5_000.0).exp();
        let after_drop = MB as f64 * decay / (decay + 1.0);
        assert!(close(snapshot.bytes_per_sec.unwrap(), after_drop, 1e-12));

        // Progress after the reset counts again.
        tracker.record(3_000, MB / 2, 50);
        let secs = (decay + 1.0) * decay + 1.0;
        let bytes = MB as f64 * decay * decay + (MB / 2) as f64;
        let speed = tracker.snapshot(3_000).bytes_per_sec.unwrap();
        assert!(close(speed, bytes / secs, 1e-12));
    }

    #[test]
    fn resumed_counters_are_not_counted_as_fresh_progress() {
        let mut tracker = ProgressTracker::new(0).with_initial(500 * MB, 50);
        tracker.set_totals(Some(100), Some(1_000 * MB));
        assert_eq!(tracker.snapshot(0).done_bytes, 500 * MB);
        tracker.record(10_000, 600 * MB, 60);
        let snapshot = tracker.snapshot(10_000);
        let ten_mb = 10.0 * MB as f64;
        assert!(close(snapshot.bytes_per_sec.unwrap(), ten_mb, 1e-9));
        assert!(close(snapshot.eta_secs.unwrap(), 40.0, 1e-9));
        assert!(close(snapshot.percent().unwrap(), 60.0, 1e-9));
    }

    #[test]
    fn a_shorter_window_reacts_faster() {
        let speed_with_window = |window_ms: u64| {
            let mut tracker = ProgressTracker::new(0).with_window_ms(window_ms);
            let mut bytes = 0;
            for tick in 1..=130u64 {
                bytes += if tick <= 100 { 100_000 } else { MB };
                tracker.record(tick * 100, bytes, 0);
            }
            tracker.snapshot(13_000).bytes_per_sec.unwrap()
        };
        let fast = speed_with_window(1_000);
        let slow = speed_with_window(10_000);
        assert!(fast > slow);
        assert!(fast > 8.0 * MB as f64, "{fast}");
        assert!(slow < 5.0 * MB as f64, "{slow}");
    }

    #[test]
    fn a_zero_window_is_clamped_instead_of_dividing_by_zero() {
        let mut tracker = ProgressTracker::new(0).with_window_ms(0);
        tracker.set_totals(Some(10), Some(10));
        tracker.record(1, 1, 1);
        tracker.record(2, 2, 2);
        let snapshot = tracker.snapshot(3);
        check_invariants(&snapshot).unwrap();
        assert!(snapshot.bytes_per_sec.is_some());
    }

    #[test]
    fn extreme_values_stay_finite() {
        let mut tracker = ProgressTracker::new(0);
        tracker.set_totals(Some(u64::MAX), Some(u64::MAX));
        tracker.record(1, u64::MAX, u64::MAX);
        check_invariants(&tracker.snapshot(1)).unwrap();
        tracker.record(u64::MAX, 0, u64::MAX);
        check_invariants(&tracker.snapshot(u64::MAX)).unwrap();
        let mut late = ProgressTracker::new(u64::MAX - 1);
        late.record(u64::MAX, 10, 10);
        check_invariants(&late.snapshot(u64::MAX)).unwrap();
    }

    #[test]
    fn a_tracker_that_never_started_reports_nothing_but_the_totals() {
        let mut tracker = ProgressTracker::new(42);
        tracker.set_totals(Some(7), Some(9));
        let snapshot = tracker.snapshot(0);
        assert_eq!(snapshot.total_items, Some(7));
        assert_eq!(snapshot.total_bytes, Some(9));
        assert_eq!(snapshot.percent(), Some(0.0));
        assert_eq!(snapshot.eta_secs, None);
    }

    proptest! {
        #[test]
        fn outputs_are_always_finite_and_in_range(
            steps in proptest::collection::vec((0u64..120_000, any::<u64>(), any::<u64>()), 0..40),
            total_items in proptest::option::of(any::<u64>()),
            total_bytes in proptest::option::of(any::<u64>()),
            window_ms in 0u64..60_000,
            idle_ms in 0u64..1_000_000,
        ) {
            let mut tracker = ProgressTracker::new(0).with_window_ms(window_ms);
            tracker.set_totals(total_items, total_bytes);
            let mut now = 0u64;
            for (elapsed, bytes, items) in steps {
                now = now.saturating_add(elapsed);
                tracker.record(now, bytes, items);
                check_invariants(&tracker.snapshot(now))?;
                check_invariants(&tracker.snapshot(now.saturating_add(idle_ms)))?;
            }
        }

        #[test]
        fn a_steady_rate_is_exact_for_any_sampling_interval(
            tick_ms in 1u64..3_000,
            kilobytes_per_sec in 1u64..2_000_000,
            ticks in 2u64..200,
        ) {
            let rate = kilobytes_per_sec * 1_000;
            let mut tracker = ProgressTracker::new(0);
            for tick in 1..=ticks {
                tracker.record(tick * tick_ms, rate * tick * tick_ms / 1000, 0);
            }
            let now = ticks * tick_ms;
            let snapshot = tracker.snapshot(now);
            if now >= DEFAULT_WINDOW_MS / 5 {
                let speed = snapshot.bytes_per_sec.unwrap();
                prop_assert!(close(speed, rate as f64, 1e-9), "{speed} vs {rate}");
            } else {
                prop_assert_eq!(snapshot.bytes_per_sec, None);
            }
        }

        #[test]
        fn a_stall_never_raises_the_speed(
            steps in proptest::collection::vec((1u64..30_000, 0u64..1_000_000_000, 0u64..10_000), 1..30),
            first_idle_ms in 0u64..100_000,
            more_idle_ms in 0u64..100_000,
            window_ms in 1u64..60_000,
        ) {
            let mut tracker = ProgressTracker::new(0).with_window_ms(window_ms);
            let (mut now, mut bytes, mut items) = (0u64, 0u64, 0u64);
            for (elapsed, byte_step, item_step) in steps {
                now += elapsed;
                bytes += byte_step;
                items += item_step;
                tracker.record(now, bytes, items);
            }
            let earlier = tracker.snapshot(now + first_idle_ms);
            let later = tracker.snapshot(now + first_idle_ms + more_idle_ms);
            for (a, b) in [
                (earlier.bytes_per_sec, later.bytes_per_sec),
                (earlier.items_per_sec, later.items_per_sec),
            ] {
                if let (Some(a), Some(b)) = (a, b) {
                    prop_assert!(b <= a * (1.0 + 1e-9) + 1e-9, "{b} > {a}");
                }
            }
        }
    }
}
