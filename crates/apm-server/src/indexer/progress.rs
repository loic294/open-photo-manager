//! Progress reporting shared by every stage of an indexer run.
//!
//! A stage reports through a [`ProgressSink`]; the task wiring adapts the sink to the task
//! engine's progress (`TaskProgress::set_totals` / `add`). A report is a CUMULATIVE snapshot of
//! one phase (all volumes together), so the adapter can turn two consecutive reports into a delta
//! and the UI can show speed and ETA from `items_*` and `bytes_*`.
//!
//! [`PhaseProgress`] is the producer side: it aggregates the work of one phase (possibly done by
//! several volumes at once) and rate-limits the reports to about four per second, with one final
//! report when the phase ends. Every stage uses it instead of calling the sink directly.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

/// The part of a run a report belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPhase {
    /// Stage 1: listing directories and updating the index (the scanner).
    Walk,
    /// Stage 2: reading metadata.
    Extract,
    /// Stage 3: hashing file contents.
    Hash,
    /// Stage 4: pre-generating thumbnails.
    Thumbs,
}

/// A cumulative progress snapshot of one phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexProgress {
    pub phase: IndexPhase,
    /// Items (files) handled so far.
    pub items_done: u64,
    /// Items expected in total, when known. An estimate may turn out too low; it is never lower
    /// than `items_done`.
    pub items_total: Option<u64>,
    /// Bytes handled so far: listed sizes for the walk, bytes read for the phases that read content.
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// What is being worked on, for display (`volume/dir` or a file name). Never an absolute path.
    pub detail: Option<String>,
}

/// Receives the progress of a run. Called from the stage's own tasks: keep it quick and never
/// block in it.
pub trait ProgressSink: Send + Sync {
    fn report(&self, progress: IndexProgress);
}

/// Discards every report.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn report(&self, _progress: IndexProgress) {}
}

/// Keeps every report, for tests.
#[derive(Debug, Default)]
pub struct RecordingProgress {
    reports: Mutex<Vec<IndexProgress>>,
}

impl RecordingProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every report so far, oldest first.
    pub fn reports(&self) -> Vec<IndexProgress> {
        self.lock().clone()
    }

    pub fn last(&self) -> Option<IndexProgress> {
        self.lock().last().cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<IndexProgress>> {
        self.reports.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ProgressSink for RecordingProgress {
    fn report(&self, progress: IndexProgress) {
        self.lock().push(progress);
    }
}

/// Minimum time between two reports of one phase.
pub const REPORT_INTERVAL: Duration = Duration::from_millis(250);

/// Aggregates and throttles the progress of one phase. Cheap to share (`Arc`) between the
/// concurrent volume tasks of a stage.
pub struct PhaseProgress {
    sink: Arc<dyn ProgressSink>,
    phase: IndexPhase,
    interval: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    items_done: u64,
    items_total: Option<u64>,
    bytes_done: u64,
    bytes_total: Option<u64>,
    detail: Option<String>,
    last_report: Option<Instant>,
}

impl PhaseProgress {
    pub fn new(sink: Arc<dyn ProgressSink>, phase: IndexPhase) -> Self {
        Self::with_interval(sink, phase, REPORT_INTERVAL)
    }

    /// Like [`new`](Self::new) with another minimum interval (`Duration::ZERO` reports every
    /// change; tests use it).
    pub fn with_interval(
        sink: Arc<dyn ProgressSink>,
        phase: IndexPhase,
        interval: Duration,
    ) -> Self {
        Self {
            sink,
            phase,
            interval,
            state: Mutex::new(State::default()),
        }
    }

    /// Sets the expected totals (replaces the previous ones; `None` = unknown). Reported with the
    /// next report.
    pub fn set_totals(&self, items: Option<u64>, bytes: Option<u64>) {
        let mut state = self.lock();
        state.items_total = items;
        state.bytes_total = bytes;
    }

    /// Adds `items` to the expected total (volumes that run at the same time each add their own
    /// estimate).
    pub fn add_items_total(&self, items: u64) {
        let mut state = self.lock();
        state.items_total = Some(state.items_total.unwrap_or(0).saturating_add(items));
    }

    /// Records finished work and reports when the minimum interval has passed (always for the
    /// very first call).
    pub fn advance(&self, items: u64, bytes: u64, detail: Option<&str>) {
        let mut state = self.lock();
        state.items_done += items;
        state.bytes_done += bytes;
        if let Some(detail) = detail
            && state.detail.as_deref() != Some(detail)
        {
            state.detail = Some(detail.to_owned());
        }
        let due = state
            .last_report
            .is_none_or(|last| last.elapsed() >= self.interval);
        if due {
            self.emit(&mut state);
        }
    }

    /// Reports the final state of the phase, whatever the interval.
    pub fn finish(&self) {
        let mut state = self.lock();
        self.emit(&mut state);
    }

    /// Work recorded so far: `(items, bytes)`.
    pub fn done(&self) -> (u64, u64) {
        let state = self.lock();
        (state.items_done, state.bytes_done)
    }

    // The sink is called with the lock held, so reports reach it in order and never go backwards.
    fn emit(&self, state: &mut State) {
        state.last_report = Some(Instant::now());
        self.sink.report(IndexProgress {
            phase: self.phase,
            items_done: state.items_done,
            items_total: state.items_total.map(|total| total.max(state.items_done)),
            bytes_done: state.bytes_done,
            bytes_total: state.bytes_total.map(|total| total.max(state.bytes_done)),
            detail: state.detail.clone(),
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(interval: Duration) -> (Arc<RecordingProgress>, PhaseProgress) {
        let sink = Arc::new(RecordingProgress::new());
        let phase = PhaseProgress::with_interval(sink.clone(), IndexPhase::Walk, interval);
        (sink, phase)
    }

    #[test]
    fn reports_are_cumulative() {
        let (sink, phase) = progress(Duration::ZERO);
        phase.advance(3, 30, Some("photos/a"));
        phase.advance(2, 5, None);
        let reports = sink.reports();
        assert_eq!(reports.len(), 2);
        assert_eq!((reports[0].items_done, reports[0].bytes_done), (3, 30));
        assert_eq!((reports[1].items_done, reports[1].bytes_done), (5, 35));
        assert_eq!(
            reports[1].detail.as_deref(),
            Some("photos/a"),
            "detail sticks"
        );
        assert_eq!(reports[1].phase, IndexPhase::Walk);
        assert_eq!(phase.done(), (5, 35));
    }

    #[test]
    fn a_long_interval_leaves_the_first_and_the_final_report() {
        let (sink, phase) = progress(Duration::from_secs(3600));
        for _ in 0..100 {
            phase.advance(1, 1, None);
        }
        phase.finish();
        let reports = sink.reports();
        assert_eq!(reports.len(), 2, "first and final: {reports:?}");
        assert_eq!(reports[0].items_done, 1);
        assert_eq!(reports[1].items_done, 100);
    }

    #[test]
    fn a_total_is_never_below_the_work_done() {
        let (sink, phase) = progress(Duration::ZERO);
        phase.set_totals(Some(2), None);
        phase.advance(5, 9, None);
        let last = sink.last().unwrap();
        assert_eq!(last.items_total, Some(5));
        assert_eq!(last.bytes_total, None);

        phase.set_totals(Some(50), Some(100));
        phase.finish();
        let last = sink.last().unwrap();
        assert_eq!((last.items_total, last.bytes_total), (Some(50), Some(100)));
    }

    #[test]
    fn estimates_of_several_volumes_add_up() {
        let (sink, phase) = progress(Duration::ZERO);
        phase.add_items_total(10);
        phase.add_items_total(5);
        phase.advance(1, 0, None);
        assert_eq!(sink.last().unwrap().items_total, Some(15));
    }

    #[test]
    fn the_phase_is_part_of_every_report() {
        let sink = Arc::new(RecordingProgress::new());
        let phase = PhaseProgress::new(sink.clone(), IndexPhase::Hash);
        phase.finish();
        assert_eq!(sink.last().unwrap().phase, IndexPhase::Hash);
    }
}
