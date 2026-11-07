//! Running one task: from the moment the scheduler starts it until its final state is stored and
//! announced.
//!
//! A runner executes in its own Tokio task, so a panic surfaces as a `JoinError` instead of taking
//! the lane down. The driver around it samples the progress a few times per second (events and
//! throttled persistence), waits for the runner and then settles the outcome.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use apm_core::dto::{ServerEvent, TaskDto, TaskKind, TaskState};
use apm_core::paths::TaskId;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinError;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::Inner;
use super::model::{INTERRUPTED_BY_RESTART, Lane, TaskCtx, TaskError, TaskSpec};
use super::progress::{Clock, ProgressView, TaskProgress};
use super::store::{self, Finish};
use crate::db::now_secs;
use crate::security::redact_urls_in;

/// What a task failed with when its runner panicked (the panic itself is only logged).
const PANIC_MESSAGE: &str = "task failed unexpectedly";

/// The shortest sampling period; `tokio::time::interval` rejects zero.
const MIN_INTERVAL: Duration = Duration::from_millis(1);

type RunFuture = BoxFuture<'static, Result<Value, TaskError>>;

/// The runner closure, boxed so that jobs of any kind share one scheduler.
pub(super) type RunFn = Box<dyn FnOnce(TaskCtx) -> RunFuture + Send>;

/// A submitted task waiting for its turn.
pub(super) struct Job {
    pub(super) live: Arc<LiveTask>,
    pub(super) run: RunFn,
}

#[derive(Clone, Copy)]
struct Status {
    state: TaskState,
    started_at: Option<i64>,
}

/// A task this process knows about and has not finished: queued or running.
pub(super) struct LiveTask {
    pub(super) id: TaskId,
    pub(super) kind: TaskKind,
    pub(super) lane: Lane,
    /// Handed to the runner; a child of the engine's stop token.
    pub(super) cancel: CancellationToken,
    /// Set when the user asked for the cancellation (as opposed to a shutdown).
    pub(super) user_cancelled: AtomicBool,
    pub(super) progress: TaskProgress,
    status: Mutex<Status>,
    /// Holds the final DTO once the task is over (for [`super::TaskEngine::wait_finished`]).
    pub(super) finished: watch::Sender<Option<TaskDto>>,
}

impl LiveTask {
    pub(super) fn new(
        id: TaskId,
        spec: &TaskSpec,
        cancel: CancellationToken,
        clock: Clock,
    ) -> Self {
        Self {
            id,
            kind: spec.kind,
            lane: spec.lane,
            cancel,
            user_cancelled: AtomicBool::new(false),
            progress: TaskProgress::with_clock(spec.title.clone(), clock),
            status: Mutex::new(Status {
                state: TaskState::Queued,
                started_at: None,
            }),
            finished: watch::channel(None).0,
        }
    }

    fn status(&self) -> Status {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn state(&self) -> TaskState {
        self.status().state
    }

    pub(super) fn started_at(&self) -> Option<i64> {
        self.status().started_at
    }

    pub(super) fn set_running(&self, started_at: i64) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = Status {
            state: TaskState::Running,
            started_at: Some(started_at),
        };
    }

    /// Asks the runner to stop because the user cancelled the task.
    pub(super) fn request_cancel(&self) {
        self.user_cancelled.store(true, Ordering::SeqCst);
        self.cancel.cancel();
    }

    /// The task as clients see it right now (queued or running).
    pub(super) fn dto(&self) -> TaskDto {
        self.dto_from(self.progress.view())
    }

    fn dto_from(&self, view: ProgressView) -> TaskDto {
        let status = self.status();
        let view = if status.state == TaskState::Running {
            view
        } else {
            view.without_rates()
        };
        self.build_dto(status, view, None, None)
    }

    fn final_dto(&self, resolved: &Resolved, view: ProgressView, finished_at: i64) -> TaskDto {
        let status = Status {
            state: resolved.state,
            started_at: self.started_at(),
        };
        self.build_dto(
            status,
            view.without_rates(),
            resolved.error.clone(),
            Some(finished_at),
        )
    }

    fn build_dto(
        &self,
        status: Status,
        view: ProgressView,
        error: Option<String>,
        finished_at: Option<i64>,
    ) -> TaskDto {
        TaskDto {
            id: self.id,
            kind: self.kind,
            title: view.title,
            state: status.state,
            items_done: view.items_done,
            items_total: view.items_total,
            bytes_done: view.bytes_done,
            bytes_total: view.bytes_total,
            speed: view.speed,
            eta_secs: view.eta_secs,
            error,
            started_at: status.started_at,
            finished_at,
        }
    }
}

/// How the runner ended.
enum RunEnd {
    /// Cancelled before the runner was started.
    NotRun,
    /// The grace period of a shutdown ran out and the runner was aborted.
    Aborted,
    Returned(Result<Result<Value, TaskError>, JoinError>),
}

/// The final state of a task, as stored and announced.
struct Resolved {
    state: TaskState,
    error: Option<String>,
    result: Option<Value>,
}

/// What the driver remembers between two samples.
struct Reporter {
    last_event: TaskDto,
    persisted_version: u64,
    persisted_at: Instant,
}

impl Inner {
    /// Starts every queued task that the lane limits allow. Called after a submission and after
    /// every completion; drivers are spawned under the lock so that a concurrent shutdown, which
    /// closes the engine under the same lock, either sees them in the tracker or keeps them out.
    pub(super) fn dispatch(self: &Arc<Self>) {
        let mut core = self.core();
        if core.closed || self.stop.is_cancelled() {
            return;
        }
        while let Some(job) = core.scheduler.start_next() {
            job.live.set_running(now_secs());
            let inner = Arc::clone(self);
            self.tracker.spawn(async move { inner.drive(job).await });
        }
    }

    async fn drive(self: Arc<Self>, job: Job) {
        let Job { live, run } = job;
        live.progress.start();
        let started_at = live.started_at().unwrap_or_else(now_secs);
        match store::mark_running(&self.db, live.id, started_at).await {
            Ok(true) => {}
            Ok(false) => tracing::debug!(task = live.id, "the task row was not queued any more"),
            Err(err) => {
                tracing::warn!(task = live.id, error = %err, "could not store that the task started");
            }
        }
        let first = live.dto();
        self.bus.publish(ServerEvent::Task {
            task: first.clone(),
        });

        let end = if live.cancel.is_cancelled() {
            RunEnd::NotRun
        } else {
            let reporter = Reporter {
                last_event: first,
                persisted_version: live.progress.sample().1,
                persisted_at: Instant::now(),
            };
            self.run_to_end(&live, run, reporter).await
        };
        // Judged when the runner returns: a runner that finishes after a cancellation counts as
        // cancelled even if it managed to produce a result.
        let resolved = resolve(
            end,
            live.id,
            live.cancel.is_cancelled(),
            live.user_cancelled.load(Ordering::SeqCst),
            self.stop.is_cancelled(),
        );
        self.finalize(&live, resolved, true).await;
        self.dispatch();
    }

    async fn run_to_end(&self, live: &Arc<LiveTask>, run: RunFn, mut reporter: Reporter) -> RunEnd {
        let ctx = TaskCtx {
            id: live.id,
            cancel: live.cancel.clone(),
            progress: live.progress.clone(),
        };
        let mut handle = tokio::spawn(async move { run(ctx).await });
        let mut ticker = tokio::time::interval(self.options.progress_interval.max(MIN_INTERVAL));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            tokio::select! {
                joined = &mut handle => return RunEnd::Returned(joined),
                () = self.hard_stop.cancelled() => {
                    handle.abort();
                    return RunEnd::Aborted;
                }
                _ = ticker.tick() => self.report(live, &mut reporter).await,
            }
        }
    }

    /// One sample of a running task: announce what changed and, now and then, save the counters.
    async fn report(&self, live: &LiveTask, reporter: &mut Reporter) {
        let (view, version) = live.progress.sample();
        let dto = live.dto_from(view.clone());
        if dto != reporter.last_event {
            self.bus.publish(ServerEvent::Task { task: dto.clone() });
            reporter.last_event = dto;
        }
        if version != reporter.persisted_version
            && reporter.persisted_at.elapsed() >= self.options.persist_interval
        {
            reporter.persisted_version = version;
            reporter.persisted_at = Instant::now();
            if let Err(err) = store::save_progress(&self.db, live.id, &view).await {
                tracing::warn!(task = live.id, error = %err, "could not save the task progress");
            }
        }
    }

    /// Stores and announces the final state of a task that is over. `ran` tells whether the task
    /// occupied a lane slot (it did unless it never started).
    async fn finalize(&self, live: &Arc<LiveTask>, resolved: Resolved, ran: bool) -> TaskDto {
        let finished_at = now_secs();
        let view = live.progress.view();
        let outcome = Finish {
            state: resolved.state,
            error: resolved.error.as_deref(),
            result: resolved.result.as_ref(),
            finished_at,
            progress: ran.then_some(&view),
        };
        match store::finish(&self.db, live.id, &outcome).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                task = live.id,
                "the task row was already finished, its final state was not stored"
            ),
            Err(err) => {
                tracing::error!(task = live.id, error = %err, "could not store the final state of a task");
            }
        }
        let dto = live.final_dto(&resolved, view, finished_at);
        {
            let mut core = self.core();
            core.live.remove(&live.id);
            if ran {
                core.scheduler.finish(live.lane);
            }
        }
        // Announced before the waiters are released: whoever waits for the task has already seen
        // its final event when it carries on.
        self.bus.publish(ServerEvent::Task { task: dto.clone() });
        live.finished.send_replace(Some(dto.clone()));
        dto
    }

    /// A task that never started because it was cancelled while queued or the server is stopping.
    pub(super) async fn finalize_unstarted(
        &self,
        live: &Arc<LiveTask>,
        state: TaskState,
    ) -> TaskDto {
        let error = (state == TaskState::Interrupted).then(|| INTERRUPTED_BY_RESTART.to_owned());
        let resolved = Resolved {
            state,
            error,
            result: None,
        };
        self.finalize(live, resolved, false).await
    }
}

/// Turns the way a runner ended into the final state. Pure, so the table of outcomes is tested
/// below.
///
/// * `cancelled`: the runner's token was cancelled when it returned.
/// * `user_cancelled`: the user asked for it (rather than a shutdown).
/// * `stopping`: the engine is shutting down.
fn resolve(
    end: RunEnd,
    id: TaskId,
    cancelled: bool,
    user_cancelled: bool,
    stopping: bool,
) -> Resolved {
    let interrupted = |error: Option<String>, result: Option<Value>| Resolved {
        state: TaskState::Interrupted,
        error: error.or_else(|| Some(INTERRUPTED_BY_RESTART.to_owned())),
        result,
    };
    // The runner stopped early: the user's cancellation wins over a shutdown.
    let stopped = |error: Option<String>, result: Option<Value>| {
        if !user_cancelled && stopping {
            interrupted(error, result)
        } else {
            Resolved {
                state: TaskState::Cancelled,
                error,
                result,
            }
        }
    };
    match end {
        RunEnd::Aborted => interrupted(None, None),
        RunEnd::NotRun => stopped(None, None),
        RunEnd::Returned(Err(err)) if err.is_panic() => {
            let message = panic_message(err.into_panic().as_ref());
            tracing::error!(task = id, panic = %message, "a task runner panicked");
            Resolved {
                state: TaskState::Failed,
                error: Some(PANIC_MESSAGE.to_owned()),
                result: None,
            }
        }
        RunEnd::Returned(Err(_)) => interrupted(None, None),
        RunEnd::Returned(Ok(Ok(value))) if cancelled => stopped(None, Some(value)),
        RunEnd::Returned(Ok(Ok(value))) => Resolved {
            state: TaskState::Done,
            error: None,
            result: Some(value),
        },
        RunEnd::Returned(Ok(Err(TaskError::Cancelled))) => stopped(None, None),
        RunEnd::Returned(Ok(Err(TaskError::Failed(message)))) => {
            let message = redact_urls_in(&message);
            if cancelled {
                stopped(Some(message), None)
            } else {
                Resolved {
                    state: TaskState::Failed,
                    error: Some(message),
                    result: None,
                }
            }
        }
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    redact_urls_in(&text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A `JoinError` for a task that was aborted.
    async fn aborted_join_error() -> JoinError {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        handle.await.unwrap_err()
    }

    async fn panicked_join_error() -> JoinError {
        tokio::spawn(async { panic!("boom") }).await.unwrap_err()
    }

    fn returned(result: Result<Value, TaskError>) -> RunEnd {
        RunEnd::Returned(Ok(result))
    }

    fn state_of(resolved: &Resolved) -> (TaskState, Option<&str>) {
        (resolved.state, resolved.error.as_deref())
    }

    #[test]
    fn a_result_without_cancellation_is_done_and_keeps_its_value() {
        let resolved = resolve(returned(Ok(json!({ "n": 1 }))), 1, false, false, false);
        assert_eq!(state_of(&resolved), (TaskState::Done, None));
        assert_eq!(resolved.result, Some(json!({ "n": 1 })));
    }

    #[test]
    fn a_failure_keeps_its_message_and_scrubs_urls() {
        let resolved = resolve(
            returned(Err(TaskError::failed(
                "no route to https://u:p@host/x?token=abc",
            ))),
            1,
            false,
            false,
            false,
        );
        assert_eq!(resolved.state, TaskState::Failed);
        let error = resolved.error.unwrap();
        assert!(!error.contains("abc") && !error.contains("u:p"), "{error}");
        assert!(resolved.result.is_none());
    }

    #[test]
    fn returning_cancelled_is_cancelled_even_without_a_token() {
        let resolved = resolve(returned(Err(TaskError::Cancelled)), 1, false, false, false);
        assert_eq!(state_of(&resolved), (TaskState::Cancelled, None));
    }

    #[test]
    fn finishing_while_the_token_is_set_counts_as_cancelled_and_keeps_the_result() {
        let resolved = resolve(returned(Ok(json!(7))), 1, true, true, false);
        assert_eq!(state_of(&resolved), (TaskState::Cancelled, None));
        assert_eq!(resolved.result, Some(json!(7)));

        let resolved = resolve(
            returned(Err(TaskError::failed("cleanup failed"))),
            1,
            true,
            true,
            false,
        );
        assert_eq!(
            state_of(&resolved),
            (TaskState::Cancelled, Some("cleanup failed"))
        );
    }

    #[test]
    fn a_shutdown_interrupts_but_a_user_cancellation_wins() {
        let resolved = resolve(returned(Err(TaskError::Cancelled)), 1, true, false, true);
        assert_eq!(
            state_of(&resolved),
            (TaskState::Interrupted, Some("interrupted by restart"))
        );
        let resolved = resolve(returned(Err(TaskError::Cancelled)), 1, true, true, true);
        assert_eq!(state_of(&resolved), (TaskState::Cancelled, None));
        let resolved = resolve(returned(Ok(json!(1))), 1, true, false, true);
        assert_eq!(resolved.state, TaskState::Interrupted);
        assert_eq!(resolved.result, Some(json!(1)));
    }

    #[test]
    fn a_task_that_never_ran_follows_the_stop_reason() {
        assert_eq!(
            resolve(RunEnd::NotRun, 1, true, true, false).state,
            TaskState::Cancelled
        );
        assert_eq!(
            resolve(RunEnd::NotRun, 1, true, false, true).state,
            TaskState::Interrupted
        );
    }

    #[tokio::test]
    async fn a_panic_fails_the_task_with_a_generic_message() {
        let end = RunEnd::Returned(Err(panicked_join_error().await));
        let resolved = resolve(end, 1, false, false, false);
        assert_eq!(
            state_of(&resolved),
            (TaskState::Failed, Some(PANIC_MESSAGE))
        );
    }

    #[tokio::test]
    async fn an_aborted_runner_is_interrupted() {
        let resolved = resolve(RunEnd::Aborted, 1, true, false, true);
        assert_eq!(
            state_of(&resolved),
            (TaskState::Interrupted, Some("interrupted by restart"))
        );
        let end = RunEnd::Returned(Err(aborted_join_error().await));
        assert_eq!(
            resolve(end, 1, false, false, false).state,
            TaskState::Interrupted
        );
    }

    #[test]
    fn panic_payloads_are_read_as_text_without_urls() {
        assert_eq!(panic_message(&"literal"), "literal");
        assert_eq!(panic_message(&String::from("owned")), "owned");
        assert_eq!(panic_message(&42_u8), "non-string panic payload");
        assert!(!panic_message(&"see https://h/x?token=secret").contains("secret"));
    }
}
