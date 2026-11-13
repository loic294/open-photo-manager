//! The scheduler of the outbox and the entry points of its routes.
//!
//! One loop DECIDES when a pass starts ([`schedule::should_start`]) and submits it as ONE
//! `ImmichCleanup` task; the pass itself is [`pass::run`]. The loop looks at the queue when the
//! hook enqueued something (after a short debounce, so a burst of moves is a few looks), when
//! `retry` asks, when a pass ends, every [`TICK`], and exactly when what it waits for ends: the
//! quiet period, a back-off, the due time of a row.
//!
//! - **Single flight.** A look holds a gate from the decision to the submission and counts a
//!   queued or running `ImmichCleanup` task as busy, so two looks never start two passes.
//! - **A failed pass backs off** (`failure_backoff_secs`); `retry` and a person's cancel decide:
//!   `retry` ends the back-off, a cancel starts one (the person did not want it running).
//! - **The stored copy of Immich's list catches up.** After a pass that changed what Immich
//!   shows, once no pending row is left, an incremental sync is queued: the new assets and the
//!   removed ones show in the browser without waiting for the sync's own schedule.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use opm_core::dto::{
    OUTBOX_MAX_IDS, OutboxDismissResult, OutboxPage, OutboxRetryResult, OutboxState, TaskKind,
    TaskState,
};
use opm_core::paths::TaskId;
use serde_json::Value;
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;

use super::consts::{TICK, WAKE_DEBOUNCE, failure_backoff_secs};
use super::pass::{self, Env, PassError, Report};
use super::prepare::ScanThrottle;
use super::schedule::{self, Situation};
use super::signal::OutboxSignal;
use super::store;
use crate::db::{Db, now_secs};
use crate::error::AppError;
use crate::immich::{ImmichError, ImmichService, ImmichSync, SyncRequest};
use crate::indexer::VolumeRegistry;
use crate::tasks::{Lane, TaskCtx, TaskEngine, TaskError, TaskSpec};

/// What the status says about a pass that a person cancelled.
const CANCELLED: &str = "The last pass was cancelled.";

/// The largest page `GET /api/immich/outbox` answers.
pub const MAX_PAGE: i64 = 200;

/// The scheduler of the outbox. Cheap to clone; every clone is the same scheduler.
#[derive(Clone)]
pub struct ImmichOutbox {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    immich: ImmichService,
    sync: ImmichSync,
    tasks: TaskEngine,
    volumes: VolumeRegistry,
    signal: OutboxSignal,
    quiet_secs: i64,
    /// When each library was last asked to scan (see `prepare`).
    throttle: ScanThrottle,
    /// A person asked for the next pass (retry): neither the quiet period nor a back-off applies.
    forced: AtomicBool,
    /// A pass changed what Immich shows and the stored copy of its list was not read since.
    dirty: AtomicBool,
    /// Held from the decision to start a pass to its submission.
    gate: Mutex<()>,
    direct_initialized: OnceCell<()>,
}

/// What one look at the queue decided.
struct Look {
    /// The pass it submitted.
    started: Option<TaskId>,
    /// How long the loop may sleep before it looks again.
    sleep: Duration,
}

impl fmt::Debug for ImmichOutbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmichOutbox").finish_non_exhaustive()
    }
}

impl crate::fsops::MoveGuard for ImmichOutbox {
    fn check<'a>(
        &'a self,
        src: &'a crate::fsops::Loc,
        dst: &'a crate::fsops::Loc,
        sidecars: &'a [(crate::fsops::Loc, crate::fsops::Loc)],
    ) -> futures_util::future::BoxFuture<'a, Result<(), crate::fsops::FsOpError>> {
        Box::pin(async move {
            self.preflight_move(src, dst, sidecars)
                .await
                .map(|_| ())
                .map_err(|error| crate::fsops::FsOpError::ImmichMoveRefused(error.refusal()))
        })
    }
    fn settled<'a>(
        &'a self,
        src: &'a crate::fsops::Loc,
        dst: &'a crate::fsops::Loc,
        identical: bool,
    ) -> futures_util::future::BoxFuture<'a, Result<(), crate::fsops::FsOpError>> {
        Box::pin(async move {
            super::direct::settled(&self.inner.db, src, dst, identical)
                .await
                .map_err(crate::fsops::FsOpError::from)
        })
    }
}

impl ImmichOutbox {
    /// Call from the move executor before any file is touched, with FINAL owner
    /// and per-asset linked sidecar destinations. No remote I/O in OpHook.
    pub async fn preflight_move(
        &self,
        src: &crate::fsops::Loc,
        dst: &crate::fsops::Loc,
        sidecars: &[(crate::fsops::Loc, crate::fsops::Loc)],
    ) -> Result<super::MovePreflight, crate::immich::direct_db::DbMoveError> {
        self.initialize_direct()
            .await
            .map_err(|_| crate::immich::direct_db::DbMoveError::Unavailable)?;
        super::direct::preflight(&self.inner.db, &self.inner.immich, src, dst, sidecars).await
    }

    async fn initialize_direct(&self) -> Result<(), crate::db::DbError> {
        self.inner
            .direct_initialized
            .get_or_try_init(|| async { super::direct::cleanup_at_start(&self.inner.db).await })
            .await?;
        Ok(())
    }

    /// Nothing is read or sent here; see [`ImmichOutbox::start_background`]. `signal` is the line
    /// the hook wakes the scheduler on; `quiet_secs` is `immich_quiet_secs`.
    pub fn new(
        db: Db,
        immich: ImmichService,
        sync: ImmichSync,
        tasks: TaskEngine,
        volumes: VolumeRegistry,
        signal: OutboxSignal,
        quiet_secs: u64,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                immich,
                sync,
                tasks,
                volumes,
                signal,
                quiet_secs: i64::try_from(quiet_secs).unwrap_or(i64::MAX),
                throttle: ScanThrottle::default(),
                forced: AtomicBool::new(false),
                dirty: AtomicBool::new(false),
                gate: Mutex::new(()),
                direct_initialized: OnceCell::new(),
            }),
        }
    }

    /// Starts the loop; it stops with `shutdown`.
    pub fn start_background(&self, shutdown: CancellationToken) {
        self.start_background_every(shutdown, TICK);
    }

    /// [`ImmichOutbox::start_background`] with another longest sleep, for tests.
    pub fn start_background_every(&self, shutdown: CancellationToken, tick: Duration) {
        let outbox = self.clone();
        tokio::spawn(async move { outbox.run_loop(shutdown, tick).await });
    }

    async fn run_loop(self, shutdown: CancellationToken, tick: Duration) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            let sleep = match self.look().await {
                Ok(look) => look.sleep.min(tick),
                Err(err) => {
                    tracing::warn!(error = %err, "the Immich outbox could not look at its queue");
                    tick
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(sleep) => {}
                () = self.inner.signal.woken() => {
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        () = tokio::time::sleep(WAKE_DEBOUNCE) => {}
                    }
                }
            }
        }
    }

    /// Starts a pass now when a row is due, whatever the quiet period and the back-off say; what
    /// `retry` makes the loop do. `None`: no row is due, Immich is not connected or a pass is
    /// already queued or running. For tests and tools.
    pub async fn submit_now(&self) -> Result<Option<TaskId>, AppError> {
        self.inner.forced.store(true, Ordering::Relaxed);
        Ok(self.look().await?.started)
    }

    /// The decision of the loop: submits a pass when the rule says so.
    async fn look(&self) -> Result<Look, AppError> {
        self.initialize_direct().await?;
        let inner = &self.inner;
        let _gate = inner.gate.lock().await;
        let situation = self.situation().await?;
        if situation.next_due.is_none() {
            inner.forced.store(false, Ordering::Relaxed);
        }
        if !schedule::should_start(&situation) {
            return Ok(Look {
                started: None,
                sleep: schedule::sleep_for(&situation),
            });
        }
        let task_id = self.submit_pass().await?;
        inner.forced.store(false, Ordering::Relaxed);
        Ok(Look {
            started: Some(task_id),
            sleep: TICK,
        })
    }

    async fn situation(&self) -> Result<Situation, AppError> {
        let inner = &self.inner;
        let connected = inner.immich.connection().await?.is_some();
        let active: Vec<_> = inner
            .tasks
            .list(0)
            .await?
            .into_iter()
            .filter(|task| matches!(task.state, TaskState::Queued | TaskState::Running))
            .collect();
        let state = store::pass_state(&inner.db).await?;
        let failure_backoff_until = match state.last_run_at {
            Some(at) if state.consecutive_failures > 0 => {
                at.saturating_add(failure_backoff_secs(state.consecutive_failures))
            }
            _ => 0,
        };
        Ok(Situation {
            now: now_secs(),
            connected,
            task_active: active
                .iter()
                .any(|task| task.kind == TaskKind::ImmichCleanup),
            failure_backoff_until,
            next_due: store::next_due(&inner.db).await?,
            last_enqueue: inner.signal.last_enqueue(),
            fs_busy: active
                .iter()
                .any(|task| Lane::for_kind(task.kind) == Lane::Fs),
            quiet: inner.quiet_secs,
            forced: inner.forced.load(Ordering::Relaxed),
        })
    }

    async fn submit_pass(&self) -> Result<TaskId, AppError> {
        let inner = &self.inner;
        let title = title(store::summary(&inner.db, false).await?.pending);
        let outbox = self.clone();
        let task_title = title.clone();
        inner
            .tasks
            .submit(
                TaskSpec::new(TaskKind::ImmichCleanup, title).resumable(),
                move |ctx| async move { outbox.run(ctx, task_title).await },
            )
            .await
    }

    pub(crate) async fn resume_task(&self, id: TaskId) -> Result<(), AppError> {
        let title = title(store::summary(&self.inner.db, false).await?.pending);
        let service = self.clone();
        self.inner
            .tasks
            .resume(id, move |ctx| async move { service.run(ctx, title).await })
            .await
    }

    /// The body of the task.
    async fn run(self, ctx: TaskCtx, title: String) -> Result<Value, TaskError> {
        let inner = &self.inner;
        let mut report = Report::default();
        let outcome = {
            let env = Env {
                db: &inner.db,
                immich: &inner.immich,
                sync: &inner.sync,
                volumes: &inner.volumes,
                throttle: &inner.throttle,
                ctx: &ctx,
            };
            pass::run(&env, &mut report).await
        };
        if report.changed() {
            inner.dirty.store(true, Ordering::Relaxed);
        }
        tracing::info!(
            rows = report.rows,
            scans = report.scans,
            carried = report.carried,
            deleted = report.deleted,
            waiting = report.waiting,
            retried = report.retried,
            attention = report.attention,
            ok = outcome.is_ok(),
            "an Immich outbox pass ended"
        );
        let result = self.settle(&ctx, outcome, &report, title).await;
        inner.signal.wake();
        self.refresh_when_drained().await;
        result
    }

    /// Records how the pass went and turns it into the result of the task.
    async fn settle(
        &self,
        ctx: &TaskCtx,
        outcome: Result<(), PassError>,
        report: &Report,
        title: String,
    ) -> Result<Value, TaskError> {
        let inner = &self.inner;
        match outcome {
            Ok(()) => {
                if let Err(err) = store::record_success(&inner.db, now_secs()).await {
                    tracing::warn!(error = %err, "the Immich outbox could not record a pass");
                }
                let rows = i64::try_from(report.rows).unwrap_or(i64::MAX);
                ctx.progress.phase(title, Some(rows), None);
                ctx.progress.add(rows, 0);
                Ok(serde_json::to_value(report).unwrap_or(Value::Null))
            }
            Err(PassError::Failed(message)) => {
                self.record_failure(&message).await;
                Err(TaskError::failed(message))
            }
            Err(PassError::Stopped) => {
                let by_a_person =
                    ctx.cancel.is_cancelled() && !inner.tasks.shutdown_token().is_cancelled();
                if by_a_person {
                    self.record_failure(CANCELLED).await;
                }
                Err(TaskError::Cancelled)
            }
        }
    }

    /// Starts the back-off of a pass that did not complete. Not after a disconnect: the state it
    /// would write belongs to a connection that is gone.
    async fn record_failure(&self, message: &str) {
        let inner = &self.inner;
        if !matches!(inner.immich.connection().await, Ok(Some(_))) {
            return;
        }
        if let Err(err) = store::record_failure(&inner.db, now_secs(), message).await {
            tracing::warn!(error = %err, "the Immich outbox could not record a failed pass");
        }
    }

    /// Reads what changed in Immich once nothing is left to mirror.
    async fn refresh_when_drained(&self) {
        let inner = &self.inner;
        if !inner.dirty.load(Ordering::Relaxed) || inner.tasks.shutdown_token().is_cancelled() {
            return;
        }
        match store::any_pending(&inner.db).await {
            Ok(false) => {}
            Ok(true) => return,
            Err(err) => {
                tracing::warn!(error = %err, "the Immich outbox could not look at its queue");
                return;
            }
        }
        inner.dirty.store(false, Ordering::Relaxed);
        if let Err(err) = inner.sync.submit(SyncRequest { full: false }).await {
            tracing::debug!(error = %err, "no sync after the outbox drained");
        }
    }

    // ----- the routes ---------------------------------------------------------------------------

    /// One page of queued changes, oldest first. Empty without a connection: the queue is
    /// cleared when Immich is disconnected.
    pub async fn list(
        &self,
        state: Option<OutboxState>,
        offset: i64,
        limit: i64,
    ) -> Result<OutboxPage, AppError> {
        if offset < 0 {
            return Err(AppError::bad_request(
                "bad_request",
                "offset must not be negative",
            ));
        }
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AppError::bad_request(
                "bad_request",
                format!("limit must be between 1 and {MAX_PAGE}"),
            ));
        }
        Ok(store::list(&self.inner.db, state, offset, limit).await?)
    }

    /// Makes `ids` (every change that needs attention or waits for its next try when `None`) due
    /// now, ends the back-off of failed passes, lets the next pass ask Immich to scan again even
    /// if it did a minute ago, and wakes the loop, which then skips the quiet period. 409
    /// `immich_not_connected` without a connection.
    pub async fn retry(&self, ids: Option<&[i64]>) -> Result<OutboxRetryResult, AppError> {
        self.ensure_connected().await?;
        if let Some(ids) = ids {
            check_ids(ids)?;
        }
        let inner = &self.inner;
        let retried = store::retry(&inner.db, ids, now_secs()).await?;
        store::reset_failures(&inner.db).await?;
        inner.throttle.forget();
        inner.forced.store(true, Ordering::Relaxed);
        inner.signal.wake();
        Ok(OutboxRetryResult {
            retried: count(retried),
        })
    }

    /// Forgets `ids` without touching Immich. 409 `immich_not_connected` without a connection.
    pub async fn dismiss(&self, ids: &[i64]) -> Result<OutboxDismissResult, AppError> {
        self.ensure_connected().await?;
        check_ids(ids)?;
        let dismissed = store::dismiss(&self.inner.db, ids).await?;
        Ok(OutboxDismissResult {
            dismissed: count(dismissed),
        })
    }

    async fn ensure_connected(&self) -> Result<(), AppError> {
        match self.inner.immich.connection().await? {
            Some(_) => Ok(()),
            None => Err(ImmichError::NotConnected.into()),
        }
    }
}

fn check_ids(ids: &[i64]) -> Result<(), AppError> {
    if ids.len() > OUTBOX_MAX_IDS {
        return Err(AppError::bad_request(
            "bad_request",
            format!("at most {OUTBOX_MAX_IDS} ids at a time"),
        ));
    }
    Ok(())
}

fn count(rows: u64) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}

fn title(pending: i64) -> String {
    if pending == 1 {
        "Updating Immich (1 change)".to_owned()
    } else {
        format!("Updating Immich ({pending} changes)")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_title_counts_the_changes() {
        assert_eq!(title(1), "Updating Immich (1 change)");
        assert_eq!(title(2), "Updating Immich (2 changes)");
        assert_eq!(title(10_000), "Updating Immich (10000 changes)");
    }

    #[test]
    fn a_request_may_name_at_most_the_documented_number_of_ids() {
        let ids: Vec<i64> = (0..i64::try_from(OUTBOX_MAX_IDS).unwrap()).collect();
        assert!(check_ids(&ids).is_ok());
        let too_many: Vec<i64> = (0..=i64::try_from(OUTBOX_MAX_IDS).unwrap()).collect();
        assert!(check_ids(&too_many).is_err());
    }
}
