//! Background task engine: queueing, progress, cancellation, persistence of task state and the
//! live event stream behind `GET /api/events`.
//!
//! Every long-running action (indexing, file moves, trash, plan execution, Immich sync) runs as a
//! task through this engine; callers only write the runner closure. Nobody else spawns detached
//! background work for a user-visible operation.
//!
//! # Public API
//!
//! * [`TaskEngine::submit`] queues a [`TaskSpec`] (kind, title, [`Lane`], JSON params) with a runner
//!   `FnOnce(TaskCtx) -> impl Future<Output = Result<serde_json::Value, TaskError>>` and returns
//!   the [`TaskId`] at once. The `Ok` value is stored as the task's `result_json`.
//! * [`TaskCtx`] gives the runner its `id`, a `cancel` token and a [`TaskProgress`].
//! * [`TaskEngine::list`], [`get`](TaskEngine::get), [`cancel`](TaskEngine::cancel) and
//!   [`wait_finished`](TaskEngine::wait_finished) read and steer tasks;
//!   [`subscribe`](TaskEngine::subscribe), [`publish`](TaskEngine::publish) and
//!   [`publish_fs_change`](TaskEngine::publish_fs_change) feed the event stream.
//! * [`TaskEngine::shutdown`] stops everything (see *Shutdown*).
//!
//! ```no_run
//! use apm_core::dto::TaskKind;
//! use apm_server::error::AppError;
//! use apm_server::tasks::{TaskEngine, TaskError, TaskSpec};
//! use serde_json::json;
//!
//! async fn trash_files(tasks: &TaskEngine, sizes: Vec<i64>) -> Result<i64, AppError> {
//!     let spec = TaskSpec::new(TaskKind::FsTrash, format!("Trash {} files", sizes.len()))
//!         .with_params(json!({ "files": sizes.len() }));
//!     tasks
//!         .submit(spec, move |ctx| async move {
//!             let total: i64 = sizes.iter().sum();
//!             ctx.progress.set_totals(i64::try_from(sizes.len()).ok(), Some(total));
//!             for size in &sizes {
//!                 if ctx.cancel.is_cancelled() {
//!                     return Err(TaskError::Cancelled);
//!                 }
//!                 // ... trash one file (never block the runtime here) ...
//!                 ctx.progress.add(1, *size);
//!             }
//!             Ok(json!({ "trashed": sizes.len() }))
//!         })
//!         .await
//! }
//! ```
//!
//! # Lifecycle
//!
//! `queued -> running -> done | failed | cancelled`, and `interrupted` for a task cut short by a
//! restart. The wire calls the success state `done`; the table calls it `succeeded` (one mapping
//! function in `model.rs`, unit-tested).
//!
//! * Lanes: `fs` (moves, trash, rmdir, plan execution, trash emptying) runs 1 task at a time,
//!   `index` 1, `immich` 2. Tasks of one lane start in submission order; lanes never wait for each
//!   other. [`Lane::for_kind`] picks the default lane, [`TaskSpec::with_lane`] overrides it.
//! * A runner returning `Ok(value)` makes the task `done`, `Err(TaskError::Failed(message))` makes
//!   it `failed` (the message is shown to the user: keep secrets out of it), and
//!   `Err(TaskError::Cancelled)` makes it `cancelled`. A runner that finishes while its `cancel`
//!   token is set is `cancelled` whatever it returned (its `Ok` value is still stored).
//! * A runner that panics ends `failed` with a generic message (the detail is logged); the lane
//!   keeps working. Runners must not block the async runtime: use `spawn_blocking` for blocking
//!   I/O, and check `cancel` between units of work.
//! * `queued`, `running` and the final state are written at once (final: with `finished_at`,
//!   `error` and `result_json`), the counters at most every 2 s. Params are stored as JSON, so
//!   never put a secret in them (`Redacted<T>` has no `Serialize`, which keeps a derived params
//!   struct from holding one by accident).
//! * Startup recovery ([`TaskEngine::new`]): every row left `queued` or `running` becomes
//!   `interrupted` with the error `interrupted by restart`. Reconciling the file system from
//!   `fs_journal` is the job of the file-operation service, not of the engine.
//! * Retention: finished tasks older than 7 days, or beyond the newest 500, are deleted at startup
//!   and then hourly.
//!
//! # Cancellation
//!
//! [`TaskEngine::cancel`]: a `queued` task is removed from its lane and is `cancelled` at once
//! **without its closure ever running** (whoever must clean up after a task that never ran, such
//! as the plan engine, has to watch for the terminal `task` event or use
//! [`TaskEngine::wait_finished`]). A `running` task gets its token cancelled and ends when its
//! runner returns. Cancelling a cancelled task answers with it again; cancelling a task that ended
//! any other way is a 409 `conflict`; an unknown id is a 404 `not_found`.
//!
//! # Progress, speed and ETA
//!
//! A runner reports through [`TaskProgress`]: `set_totals`, `add`, `set_title`, and `phase` to start
//! a new phase of a multi-step task (done counters, rate estimator, totals and title restart in one
//! step). It is safe and cheap to call from any thread.
//!
//! **Unit of `TaskDto.speed`**: bytes per second when the task declared a byte total
//! (`bytes_total` is set), items per second otherwise, so a client labels the unit from
//! `bytes_total` alone. A task that reports bytes must therefore declare an (estimated) byte total.
//! `eta_secs` is `None` until both a rate and a total are known. The rate comes from
//! `apm_core::progress` (EMA over about 5 s, nothing before the first second of a phase).
//!
//! # Events
//!
//! A `task` event is published when a task is queued, when it starts, at most every 200 ms while
//! it runs and something changed (counters, title, speed, ETA), and once more when it ends.
//! [`TaskEngine::publish_fs_change`] merges the directories reported within about 250 ms into one
//! `fs_change` event per volume (sorted, deduplicated, at most 500 directories per event).
//! Subscribers get no history: a client that connects reads `GET /api/tasks` first. A subscriber
//! that falls more than 1024 events behind gets `RecvError::Lagged`; the SSE route then closes its
//! stream so that the browser reconnects and resynchronises.
//!
//! # Shutdown
//!
//! `AppState` owns the app-wide `shutdown` token. Services built in `AppState::new` receive
//! `shutdown.clone()` (or a `child_token()`) from their constructor and stop when it is cancelled;
//! afterwards they read `state.shutdown`. Code that only holds a [`TaskEngine`] uses
//! [`TaskEngine::shutdown_token`], which fires on the same signal and also when the engine itself
//! is shut down. `AppState::begin_shutdown` (on SIGTERM, before the HTTP server drains) cancels the
//! token: runner tokens are its children, so runners are asked to stop at once, and open SSE
//! streams end. `AppState::shutdown` then awaits [`TaskEngine::shutdown`]: queued tasks become
//! `interrupted`, runners get a grace period (8 s by default) to return, stragglers are aborted and
//! marked `interrupted`, and only then is the database closed. After that [`TaskEngine::submit`]
//! answers 503 `shutting_down`.

mod events;
mod lanes;
mod model;
mod progress;
mod runner;
mod store;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use apm_core::dto::{ServerEvent, TaskDto, TaskState};
use apm_core::paths::{TaskId, VolumeId};
use axum::http::StatusCode;
use futures_util::FutureExt;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use self::events::{EventBus, FsChanges};
pub use self::lanes::LaneLimits;
use self::lanes::Scheduler;
pub use self::model::{Lane, TaskCtx, TaskError, TaskSpec};
pub use self::progress::{Clock, ProgressView, TaskProgress};
use self::runner::{Job, LiveTask};
use crate::db::{Db, DbError, now_secs};
use crate::error::AppError;

/// The most finished tasks [`TaskEngine::list`] returns (the retention keeps 500).
const MAX_FINISHED_LIMIT: i64 = 500;

/// How long [`TaskEngine::shutdown`] waits for aborted runners to wind down.
const ABORT_WAIT: Duration = Duration::from_secs(2);

/// How long finished tasks are kept, and how many.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// Finished tasks that ended longer ago than this are deleted (7 days).
    pub max_age: Duration,
    /// Only the newest this many finished tasks are kept (500).
    pub keep: usize,
    /// Pause between two sweeps after the one at startup (1 hour).
    pub sweep_every: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(7 * 24 * 3600),
            keep: 500,
            sweep_every: Duration::from_secs(3600),
        }
    }
}

/// Knobs of the engine; the defaults are what the server runs with, tests shorten the intervals.
#[derive(Clone, Debug)]
pub struct TaskOptions {
    /// Tasks running at once per lane (fs 1, index 1, immich 2).
    pub concurrency: LaneLimits,
    /// Pause between two samples of a running task, hence the shortest gap between two of its
    /// progress events (200 ms).
    pub progress_interval: Duration,
    /// Shortest gap between two saves of the counters of a running task (2 s).
    pub persist_interval: Duration,
    /// How long file-system changes are collected before one event is sent (250 ms).
    pub fs_change_window: Duration,
    /// How long a shutdown waits for runners before aborting them (8 s).
    pub shutdown_grace: Duration,
    pub retention: Retention,
    /// Time source of the rate estimator.
    pub clock: Clock,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            concurrency: LaneLimits::default(),
            progress_interval: Duration::from_millis(200),
            persist_interval: Duration::from_secs(2),
            fs_change_window: Duration::from_millis(250),
            shutdown_grace: Duration::from_secs(8),
            retention: Retention::default(),
            clock: Clock::system(),
        }
    }
}

/// The task engine. Cheap to clone; every clone is the same engine.
#[derive(Clone)]
pub struct TaskEngine {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    options: TaskOptions,
    /// Cancelled when the app starts shutting down or the engine is shut down. Runner tokens are
    /// its children.
    stop: CancellationToken,
    /// Cancelled when the grace period of a shutdown is over: runners still going are aborted.
    hard_stop: CancellationToken,
    bus: EventBus,
    fs_changes: Arc<FsChanges>,
    /// Tracks every driver and background loop so that a shutdown can wait for them.
    tracker: TaskTracker,
    core: Mutex<Core>,
}

/// What the lock protects: the waiting lines and the tasks in flight. Never held across an
/// `.await`.
struct Core {
    closed: bool,
    scheduler: Scheduler<Job>,
    live: HashMap<TaskId, Arc<LiveTask>>,
}

impl Inner {
    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives a row that was inserted but never queued its final state.
    async fn abandon(&self, id: TaskId) {
        let outcome = store::Finish {
            state: TaskState::Interrupted,
            error: Some(model::INTERRUPTED_BY_RESTART),
            result: None,
            finished_at: now_secs(),
            progress: None,
        };
        if let Err(err) = store::finish(&self.db, id, &outcome).await {
            tracing::warn!(task = id, error = %err, "could not close a task that was never queued");
        }
    }
}

impl Drop for Inner {
    /// Background loops hold no reference to the engine: stop them when the last handle is gone.
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl fmt::Debug for TaskEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskEngine").finish_non_exhaustive()
    }
}

impl TaskEngine {
    /// Starts an engine on `db` with the default [`TaskOptions`]. Tasks left `queued` or
    /// `running` by a previous run become `interrupted`, old finished tasks are pruned, and the
    /// background loops start. `shutdown` is the app-wide token: cancelling it stops the engine
    /// and every runner.
    pub async fn new(db: Db, shutdown: CancellationToken) -> Result<Self, DbError> {
        Self::with_options(db, shutdown, TaskOptions::default()).await
    }

    /// [`TaskEngine::new`] with explicit options.
    pub async fn with_options(
        db: Db,
        shutdown: CancellationToken,
        options: TaskOptions,
    ) -> Result<Self, DbError> {
        let recovered = store::recover_interrupted(&db, now_secs()).await?;
        if recovered > 0 {
            tracing::warn!(
                recovered,
                "tasks left unfinished by the previous run were marked interrupted"
            );
        }
        prune_once(&db, &options.retention).await;

        let stop = shutdown.child_token();
        let bus = EventBus::new();
        let fs_changes = Arc::new(FsChanges::default());
        let tracker = TaskTracker::new();
        tracker.spawn(retention_loop(db.clone(), options.retention, stop.clone()));
        tracker.spawn({
            let (changes, bus, stop) = (Arc::clone(&fs_changes), bus.clone(), stop.clone());
            let window = options.fs_change_window;
            async move { changes.run(&bus, window, &stop).await }
        });

        let core = Core {
            closed: false,
            scheduler: Scheduler::new(options.concurrency),
            live: HashMap::new(),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                options,
                stop,
                hard_stop: CancellationToken::new(),
                bus,
                fs_changes,
                tracker,
                core: Mutex::new(core),
            }),
        })
    }

    /// Queues a task and returns its id without waiting for it to start. `run` receives the
    /// [`TaskCtx`] when the task's lane has a free slot; its `Ok` value is stored as `result_json`.
    ///
    /// Fails with 503 `shutting_down` once the engine is stopping.
    pub async fn submit<F, Fut>(&self, spec: TaskSpec, run: F) -> Result<TaskId, AppError>
    where
        F: FnOnce(TaskCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, TaskError>> + Send + 'static,
    {
        let inner = &self.inner;
        if inner.stop.is_cancelled() || inner.core().closed {
            return Err(shutting_down());
        }
        let id = store::insert_queued(&inner.db, &spec, now_secs()).await?;
        let live = Arc::new(LiveTask::new(
            id,
            &spec,
            inner.stop.child_token(),
            inner.options.clock.clone(),
        ));
        let job = Job {
            live: Arc::clone(&live),
            run: Box::new(move |ctx| run(ctx).boxed()),
        };
        let accepted = {
            let mut core = inner.core();
            if core.closed || inner.stop.is_cancelled() {
                false
            } else {
                core.live.insert(id, Arc::clone(&live));
                core.scheduler.enqueue(spec.lane, id, job);
                // Published under the lock: the `queued` event is out before any driver can
                // announce `running`.
                inner.bus.publish(ServerEvent::Task { task: live.dto() });
                true
            }
        };
        if !accepted {
            // The engine stopped while the row was being written: nobody will ever run it.
            inner.abandon(id).await;
            return Err(shutting_down());
        }
        inner.dispatch();
        Ok(id)
    }

    /// The active tasks (running first, then queued, each by id) followed by the newest
    /// `finished_limit` finished ones (clamped to 0..=500), newest first.
    pub async fn list(&self, finished_limit: i64) -> Result<Vec<TaskDto>, AppError> {
        let limit = finished_limit.clamp(0, MAX_FINISHED_LIMIT);
        let mut active: Vec<TaskDto> = {
            let core = self.inner.core();
            core.live.values().map(|live| live.dto()).collect()
        };
        // The live tasks are read before the table: one that ends in between shows up in both,
        // and the stored (final) row wins.
        let finished = store::list_finished(&self.inner.db, limit).await?;
        let stored: HashSet<TaskId> = finished.iter().map(|task| task.id).collect();
        active.retain(|task| !stored.contains(&task.id));
        active.sort_by_key(|task| (task.state != TaskState::Running, task.id));
        active.extend(finished);
        Ok(active)
    }

    /// One task. Unknown ids are a 404 `not_found`.
    pub async fn get(&self, id: TaskId) -> Result<TaskDto, AppError> {
        let live = self.inner.core().live.get(&id).cloned();
        if let Some(live) = live {
            return Ok(live.dto());
        }
        store::get(&self.inner.db, id)
            .await?
            .ok_or_else(|| unknown_task(id))
    }

    /// Cancels a task and returns it as it is now (see the module docs for what that means for
    /// each state).
    pub async fn cancel(&self, id: TaskId) -> Result<TaskDto, AppError> {
        enum Step {
            Queued(Job),
            Running(TaskDto),
            Settling(Arc<LiveTask>),
            Stored,
        }
        let step = {
            let mut core = self.inner.core();
            match core.live.get(&id).cloned() {
                None => Step::Stored,
                Some(live) => match core.scheduler.remove_queued(id) {
                    Some(job) => Step::Queued(job),
                    None if live.state() == TaskState::Running => {
                        live.request_cancel();
                        Step::Running(live.dto())
                    }
                    // Taken out of its lane by somebody else and being finalised right now.
                    None => Step::Settling(live),
                },
            }
        };
        match step {
            Step::Queued(job) => Ok(self
                .inner
                .finalize_unstarted(&job.live, TaskState::Cancelled)
                .await),
            Step::Running(dto) => Ok(dto),
            Step::Settling(live) => cancel_outcome(finished_dto(&live).await),
            Step::Stored => match store::get(&self.inner.db, id).await? {
                Some(dto) => cancel_outcome(dto),
                None => Err(unknown_task(id)),
            },
        }
    }

    /// Resolves with the final state of the task once it is over (at once if it already is).
    /// Unknown ids are a 404 `not_found`.
    pub async fn wait_finished(&self, id: TaskId) -> Result<TaskDto, AppError> {
        let live = self.inner.core().live.get(&id).cloned();
        match live {
            Some(live) => Ok(finished_dto(&live).await),
            None => self.get(id).await,
        }
    }

    /// A receiver of every event published from now on (no history). A receiver that falls too far
    /// behind gets `RecvError::Lagged`.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.inner.bus.subscribe()
    }

    /// Sends an event to every subscriber.
    pub fn publish(&self, event: ServerEvent) {
        self.inner.bus.publish(event);
    }

    /// Reports directories whose content changed. Changes of the same volume reported within about
    /// 250 ms become one `fs_change` event, directories sorted and deduplicated. Never blocks.
    pub fn publish_fs_change(&self, volume_id: VolumeId, dirs: impl IntoIterator<Item = String>) {
        self.inner.fs_changes.push(volume_id, dirs);
    }

    /// A token cancelled when the app starts shutting down (the token given to
    /// [`TaskEngine::new`]) or when this engine is shut down. Long-lived services that hold a
    /// `TaskEngine` use it to stop their loops.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.inner.stop.clone()
    }

    /// Stops the engine: queued tasks become `interrupted` and never run, runners are cancelled
    /// and given [`TaskOptions::shutdown_grace`] to return, stragglers are aborted and marked
    /// `interrupted`. Idempotent. Call it before closing the database.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        let waiting = {
            let mut core = inner.core();
            core.closed = true;
            core.scheduler.drain_queued()
        };
        for job in waiting {
            inner
                .finalize_unstarted(&job.live, TaskState::Interrupted)
                .await;
        }
        inner.stop.cancel();
        inner.tracker.close();
        if tokio::time::timeout(inner.options.shutdown_grace, inner.tracker.wait())
            .await
            .is_ok()
        {
            return;
        }
        let running = inner.core().live.len();
        tracing::warn!(running, "tasks did not stop in time, aborting them");
        inner.hard_stop.cancel();
        if tokio::time::timeout(ABORT_WAIT, inner.tracker.wait())
            .await
            .is_err()
        {
            tracing::error!("some tasks are still running after being aborted");
        }
    }
}

fn shutting_down() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "shutting_down",
        "the server is shutting down",
    )
}

fn unknown_task(id: TaskId) -> AppError {
    AppError::not_found("not_found", format!("task {id} does not exist"))
}

/// What cancelling a task that is already over answers: a cancelled task stays cancelled (the call
/// is idempotent), any other final state is a conflict.
fn cancel_outcome(dto: TaskDto) -> Result<TaskDto, AppError> {
    if dto.state == TaskState::Cancelled {
        Ok(dto)
    } else {
        Err(AppError::conflict(
            "conflict",
            format!("task {} is not running", dto.id),
        ))
    }
}

/// Waits for a live task to reach its final state and returns it.
async fn finished_dto(live: &LiveTask) -> TaskDto {
    let mut receiver = live.finished.subscribe();
    loop {
        if let Some(dto) = receiver.borrow_and_update().clone() {
            return dto;
        }
        // The sender lives in `live`, which the caller keeps alive, so the channel cannot close.
        if receiver.changed().await.is_err() {
            return live.dto();
        }
    }
}

/// Deletes finished tasks past their retention. A failure is logged and retried at the next sweep.
async fn prune_once(db: &Db, retention: &Retention) {
    let max_age = i64::try_from(retention.max_age.as_secs()).unwrap_or(i64::MAX);
    let keep = i64::try_from(retention.keep).unwrap_or(i64::MAX);
    match store::prune(db, now_secs().saturating_sub(max_age), keep).await {
        Ok(0) => {}
        Ok(removed) => tracing::debug!(removed, "pruned finished tasks"),
        Err(err) => tracing::warn!(error = %err, "could not prune finished tasks"),
    }
}

/// Prunes every `retention.sweep_every` until `stop` fires.
async fn retention_loop(db: Db, retention: Retention, stop: CancellationToken) {
    let period = retention.sweep_every.max(Duration::from_millis(1));
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        prune_once(&db, &retention).await;
    }
}
