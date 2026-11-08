//! `IndexJobs`: the one door to indexing work.
//!
//! Every trigger (the "Re-index" menu, a held-removals "Rescan", the startup scan, the periodic
//! rescan, the filesystem watcher) calls [`IndexJobs::submit`] with a [`ScanRequest`] and an
//! [`IndexOrigin`]. A request becomes, or joins, one task of kind `index` on the `index` lane. The
//! task re-reads the volumes and then runs [`Indexer::run`](super::Indexer::run) (scan, metadata,
//! hashes, thumbnails) with the task's cancel token, its progress and the `fs_change` events.
//! Nothing else in production starts index work, so ordering, de-duplication and progress are
//! decided here and nowhere else.
//!
//! # Coalescing
//!
//! The `index` lane runs one task at a time and is FIFO, so a busy library must not pile up
//! identical work. `submit` looks at the index tasks that are queued or running (a task that was
//! told to stop does not count) and applies the first rule that fits:
//!
//! 1. `Startup` and `Periodic` requests are dropped while any index task is queued or running: the
//!    oldest such task is returned with `coalesced = true`. Otherwise they queue normally.
//! 2. A request with `force = false` and `confirm_removals = false` merges into the oldest QUEUED
//!    job (not yet started) that is also not forced and not confirming: the targets are unioned, a
//!    target covered by another (same volume, the other recursive and an ancestor or equal) is
//!    dropped, and an empty list ("every volume") absorbs everything. The task id is the job's and
//!    `coalesced = true`. A running job never absorbs anything: the runner takes its request when
//!    it starts, under the same lock a merge needs.
//! 3. A `User` request identical to a queued or running job (the same normalised targets and the
//!    same flags) returns that task: a double click on "Re-index all assets" starts one job. A
//!    forced or confirming request is never merged into a job of another shape, only returned for
//!    an identical one. Watcher requests do not take this rule: what changed after a scan began
//!    must be scanned again.
//! 4. Anything else queues a new task.
//!
//! `confirm_removals` is the user's explicit OK to apply removals the scanner's guard held back; it
//! only counts for `User` requests and is cleared for every other origin.
//!
//! If the user cancels a queued job a request that merged into it is cancelled with it. A request
//! that arrives afterwards starts a new job (the cancelled one no longer counts).
//!
//! # The task
//!
//! The runner is a `TaskKind::Index` task. Its params are `{origin, targets, force,
//! confirm_removals}` with volume-relative directories only. The title says what the job does
//! ("Indexing", "Re-indexing all assets", "Indexing 3 folders", "Indexing changes" for the watcher)
//! and is recomputed from the request the runner actually took, because a queued task cannot be
//! edited after a merge. While it runs the title is the phase ("Indexing: hashing files") and the
//! counters restart with every phase ([`TaskProgressSink`]). The result is the [`IndexSummary`] as
//! JSON; a cancelled run is `cancelled`; every other error is a generic user-readable failure (the
//! cause is logged).
//!
//! Before it indexes, the runner re-reads the volumes, so a share that was mounted after the start
//! is picked up. When a volume has come back online the file operations that were waiting for it
//! are recovered again ([`FsOps::reconcile`]) and the folders that recovery asks to rescan join a
//! targeted request.
//!
//! # Startup and periodic scans
//!
//! [`IndexJobs::start_background`] (once, from `main`; never from `AppState::new`, so tests stay
//! quiet) queues a non-forced scan of every volume shortly after the start, then another one every
//! `index.rescan_interval_minutes` minutes (`0` disables the periodic ones). A change of the setting
//! applies at once, measured from the change; the interval is measured from the end of the previous
//! attempt, never back to back, and a scan that is skipped because the index is busy counts as an
//! attempt. [`SchedulerOptions`] shortens the delay and the minute for tests. The scheduler ends
//! with the shutdown token.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use apm_core::dto::{HeldRemovalsDto, IndexStatusDto, RescanRequest, TaskDto, TaskKind, TaskState};
use apm_core::paths::{RelPath, TaskId, VolumeId};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    IndexError, IndexPhase, IndexProgress, Indexer, ProgressSink, RunContext, ScanRequest,
    ScanTarget, pending,
};
use crate::error::{AppError, AppResult};
use crate::fsops::{FsOps, TaskChangeSink};
use crate::tasks::{TaskCtx, TaskEngine, TaskError, TaskProgress, TaskSpec};

/// How long after the start the first scan is queued, so the server answers requests first.
const STARTUP_DELAY: Duration = Duration::from_secs(2);
/// The unit of `index.rescan_interval_minutes`.
const MINUTE: Duration = Duration::from_secs(60);

/// What asked for a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOrigin {
    /// A person: a button, or an API client acting for one. The only origin whose
    /// `confirm_removals` counts.
    User,
    /// The scan shortly after the server started.
    Startup,
    /// The periodic rescan.
    Periodic,
    /// The filesystem watcher, with the folders it saw change.
    Watcher,
}

impl IndexOrigin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Startup => "startup",
            Self::Periodic => "periodic",
            Self::Watcher => "watcher",
        }
    }
}

/// What [`IndexJobs::submit`] did with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitOutcome {
    /// The task that will (or does) cover the request.
    pub task_id: TaskId,
    /// The request was merged into, or answered by, a task that already existed.
    pub coalesced: bool,
}

/// The clock of the scheduler, injectable so tests do not wait for minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerOptions {
    /// The wait before the startup scan.
    pub startup_delay: Duration,
    /// How long one minute of `index.rescan_interval_minutes` lasts.
    pub minute: Duration,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        Self {
            startup_delay: STARTUP_DELAY,
            minute: MINUTE,
        }
    }
}

/// A job that was accepted and has not ended: its request, as the task will run it.
struct Job {
    task_id: TaskId,
    origin: IndexOrigin,
    request: ScanRequest,
    /// The runner took the request: nothing can be merged into the job any more.
    taken: bool,
}

struct Inner {
    indexer: Indexer,
    tasks: TaskEngine,
    fsops: FsOps,
    shutdown: CancellationToken,
    options: SchedulerOptions,
    started: AtomicBool,
    /// The accepted jobs, oldest first. `submit` holds the lock for its whole duration (the task is
    /// queued inside it) and the runner takes its request under the same lock, so a request is
    /// either merged before the runner reads it or becomes a job of its own.
    jobs: tokio::sync::Mutex<Vec<Job>>,
}

/// Turns scan requests into index tasks. Cheap to clone; the clones share their state.
#[derive(Clone)]
pub struct IndexJobs {
    inner: Arc<Inner>,
}

impl fmt::Debug for IndexJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexJobs").finish_non_exhaustive()
    }
}

impl IndexJobs {
    /// Jobs that run on `indexer` as tasks of `tasks`. `shutdown` stops the scheduler.
    pub fn new(
        indexer: Indexer,
        tasks: TaskEngine,
        fsops: FsOps,
        shutdown: CancellationToken,
    ) -> Self {
        Self::with_options(indexer, tasks, fsops, shutdown, SchedulerOptions::default())
    }

    /// [`IndexJobs::new`] with another scheduler clock.
    pub fn with_options(
        indexer: Indexer,
        tasks: TaskEngine,
        fsops: FsOps,
        shutdown: CancellationToken,
        options: SchedulerOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                indexer,
                tasks,
                fsops,
                shutdown,
                options,
                started: AtomicBool::new(false),
                jobs: tokio::sync::Mutex::new(Vec::new()),
            }),
        }
    }

    /// Queues `request` as an index task, or merges it into one (see the module docs for the rules).
    ///
    /// An empty target list means every online volume, completely. The targets are not checked
    /// against the volumes here (the run copes with a volume that vanished): [`scan_request`]
    /// validates what a client sent.
    ///
    /// # Errors
    /// 503 `shutting_down` once the task engine stops, `internal` when its tasks cannot be read.
    ///
    /// [`scan_request`]: IndexJobs::scan_request
    pub async fn submit(
        &self,
        request: ScanRequest,
        origin: IndexOrigin,
    ) -> AppResult<SubmitOutcome> {
        let request = ScanRequest {
            targets: normalise_targets(&request.targets),
            confirm_removals: request.confirm_removals && origin == IndexOrigin::User,
            ..request
        };
        let mut jobs = self.inner.jobs.lock().await;
        let active = self.active_tasks().await?;
        jobs.retain(|job| active.iter().any(|task| task.id == job.task_id));

        if matches!(origin, IndexOrigin::Startup | IndexOrigin::Periodic) {
            let ids = active.iter().map(|task| task.id).collect();
            if let Some(task_id) = self.first_open(ids).await {
                return Ok(SubmitOutcome::joined(task_id));
            }
        } else {
            if !request.force && !request.confirm_removals {
                let ids = jobs
                    .iter()
                    .filter(|job| !job.taken && !job.request.force && !job.request.confirm_removals)
                    .map(|job| job.task_id)
                    .collect();
                if let Some(task_id) = self.first_open(ids).await
                    && let Some(job) = jobs.iter_mut().find(|job| job.task_id == task_id)
                {
                    job.request.targets = union(&job.request.targets, &request.targets);
                    return Ok(SubmitOutcome::joined(task_id));
                }
            }
            if origin == IndexOrigin::User {
                let ids = jobs
                    .iter()
                    .filter(|job| same_request(&job.request, &request))
                    .map(|job| job.task_id)
                    .collect();
                if let Some(task_id) = self.first_open(ids).await {
                    return Ok(SubmitOutcome::joined(task_id));
                }
            }
        }

        let spec = TaskSpec::new(TaskKind::Index, title(&request, origin))
            .with_params(params(&request, origin));
        let runner = self.clone();
        let task_id = self
            .inner
            .tasks
            .submit(spec, move |ctx| async move { runner.run_job(ctx).await })
            .await?;
        jobs.push(Job {
            task_id,
            origin,
            request,
            taken: false,
        });
        Ok(SubmitOutcome {
            task_id,
            coalesced: false,
        })
    }

    /// What `GET /api/index/status` answers: the removals the scanner's guard is holding, and the
    /// index task that is running, else the oldest one that is queued.
    ///
    /// # Errors
    /// `internal` when the database or the task list cannot be read.
    pub async fn status(&self) -> AppResult<IndexStatusDto> {
        let held_removals = self
            .inner
            .indexer
            .held_removals()
            .await?
            .into_iter()
            .map(|held| HeldRemovalsDto {
                volume_id: held.volume_id,
                volume_name: held.volume_name,
                files: held.files,
            })
            .collect();
        let active_task_id = self.active_tasks().await?.iter().map(|task| task.id).min();
        Ok(IndexStatusDto {
            held_removals,
            active_task_id,
        })
    }

    /// The one conversion from what a client sent to what the indexer runs, with the checks the
    /// API promises: a volume must exist (404 `not_found`) and be online (409 `volume_offline`),
    /// `dir` must be a volume-relative path (400 `invalid_path`), and a scope without a volume
    /// must have an empty `dir` (400 `bad_request`). A scope without a volume stands for every
    /// online volume. A directory that does not exist (any more) is allowed: the scan removes its
    /// entries.
    ///
    /// # Errors
    /// The ones above; 409 `volume_offline` too when scopes were sent and no volume is online.
    pub fn scan_request(&self, request: &RescanRequest) -> AppResult<ScanRequest> {
        let volumes = self.inner.indexer.volumes();
        let mut targets = Vec::with_capacity(request.targets.len());
        for scope in &request.targets {
            if let Some(volume_id) = scope.volume_id {
                let dir = RelPath::new(&scope.dir)
                    .map_err(|err| AppError::bad_request("invalid_path", err.to_string()))?;
                volumes.get(volume_id)?;
                targets.push(ScanTarget {
                    volume_id,
                    dir,
                    recursive: scope.recursive,
                });
            } else if scope.dir.is_empty() {
                targets.extend(volumes.online().into_iter().map(|volume| ScanTarget {
                    volume_id: volume.id,
                    dir: RelPath::root(),
                    recursive: scope.recursive,
                }));
            } else {
                return Err(AppError::bad_request(
                    "bad_request",
                    "dir must be empty unless volume_id is set",
                ));
            }
        }
        if !request.targets.is_empty() && targets.is_empty() {
            return Err(AppError::conflict("volume_offline", "no volume is online"));
        }
        Ok(ScanRequest {
            targets,
            force: request.force,
            confirm_removals: request.confirm_removals,
        })
    }

    /// Starts the startup scan and the periodic rescans (see the module docs). Call it once the
    /// server is up; a second call does nothing and returns `None`. The returned handle ends when
    /// the shutdown token is cancelled.
    pub fn start_background(&self) -> Option<JoinHandle<()>> {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return None;
        }
        let jobs = self.clone();
        Some(tokio::spawn(async move { jobs.schedule().await }))
    }

    /// The first of `ids` that is still queued or running and was not told to stop.
    async fn first_open(&self, ids: Vec<TaskId>) -> Option<TaskId> {
        for id in ids {
            if self.is_open(id).await {
                return Some(id);
            }
        }
        None
    }

    /// Whether a request given to the task would still run. The engine shows a task that was told
    /// to stop as running until its body returns, and a request merged into it would never run.
    /// The order of the two reads matters: a task that has ended reads as "not stopping", so its
    /// state is read second, and an ended task is not open.
    async fn is_open(&self, id: TaskId) -> bool {
        let tasks = &self.inner.tasks;
        !tasks.is_stopping(id)
            && matches!(
                tasks.get(id).await.map(|task| task.state),
                Ok(TaskState::Queued | TaskState::Running)
            )
    }

    /// The index tasks that are queued or running, running first, then by id.
    async fn active_tasks(&self) -> AppResult<Vec<TaskDto>> {
        Ok(self
            .inner
            .tasks
            .list(0)
            .await?
            .into_iter()
            .filter(|task| {
                task.kind == TaskKind::Index
                    && matches!(task.state, TaskState::Queued | TaskState::Running)
            })
            .collect())
    }

    /// The request of a job that starts now. It cannot grow afterwards.
    async fn take(&self, task_id: TaskId) -> Option<(ScanRequest, IndexOrigin)> {
        let mut jobs = self.inner.jobs.lock().await;
        let job = jobs.iter_mut().find(|job| job.task_id == task_id)?;
        job.taken = true;
        Some((job.request.clone(), job.origin))
    }

    /// The body of an index task.
    async fn run_job(&self, ctx: TaskCtx) -> Result<Value, TaskError> {
        let Some((mut request, origin)) = self.take(ctx.id).await else {
            tracing::error!(task = ctx.id, "an index task started without its request");
            return Err(TaskError::failed(
                "the indexing request was lost; please try again",
            ));
        };
        let heading = title(&request, origin);
        ctx.progress.set_title(heading.clone());
        let outcome = self.execute(&ctx, &mut request).await;
        ctx.progress.set_title(heading);
        outcome
    }

    async fn execute(&self, ctx: &TaskCtx, request: &mut ScanRequest) -> Result<Value, TaskError> {
        if ctx.cancel.is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let rescan = self.refresh_volumes().await.map_err(|err| {
            tracing::error!(error = %err, "could not read the volumes before indexing");
            TaskError::failed("could not read the list of volumes; see the server log")
        })?;
        if !request.targets.is_empty() && !rescan.is_empty() {
            request.targets = union(&request.targets, &rescan);
        }
        let run = RunContext::new(
            ctx.cancel.clone(),
            Arc::new(TaskProgressSink::new(ctx.progress.clone())),
            Arc::new(TaskChangeSink::new(self.inner.tasks.clone())),
        );
        match self.inner.indexer.run(request, &run).await {
            Ok(summary) => Ok(serde_json::to_value(summary).unwrap_or(Value::Null)),
            Err(IndexError::Cancelled) => Err(TaskError::Cancelled),
            Err(IndexError::UnknownVolume(_)) => Err(TaskError::failed(
                "a volume of this request does not exist any more",
            )),
            Err(err) => {
                tracing::error!(error = %err, "indexing failed");
                Err(TaskError::failed(
                    "indexing failed; see the server log for details",
                ))
            }
        }
    }

    /// Re-reads the volumes. When one has come online since the last read, the file operations
    /// that waited for it are recovered again; the folders recovery wants rescanned are returned.
    async fn refresh_volumes(&self) -> Result<Vec<ScanTarget>, crate::db::DbError> {
        let volumes = self.inner.indexer.volumes();
        let before = online_ids(volumes);
        volumes.refresh().await?;
        if !came_online(&before, &online_ids(volumes)) {
            return Ok(Vec::new());
        }
        match self.inner.fsops.reconcile().await {
            Ok(report) => {
                tracing::info!(
                    examined = report.examined,
                    completed = report.completed,
                    abandoned = report.abandoned,
                    still_pending = report.still_pending,
                    "file operation recovery after a volume came online"
                );
                Ok(normalise_targets(&report.rescan))
            }
            Err(err) => {
                tracing::warn!(error = %err, "file operation recovery after a volume came online failed");
                Ok(Vec::new())
            }
        }
    }

    /// The scheduler: the startup scan, then the periodic ones.
    async fn schedule(self) {
        let shutdown = self.inner.shutdown.clone();
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(self.inner.options.startup_delay) => {}
        }
        self.submit_scheduled(IndexOrigin::Startup).await;

        let mut settings = self.inner.indexer.settings().subscribe();
        let rescan_minutes = |settings: &tokio::sync::watch::Receiver<_>| {
            let current: Arc<crate::settings::Settings> = Arc::clone(&settings.borrow());
            current.index.rescan_interval_minutes
        };
        let mut minutes = rescan_minutes(&settings);
        let mut due = self.due(minutes);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = settings.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    // Any setting can change: only a new interval moves the next scan.
                    let now = rescan_minutes(&settings);
                    if now != minutes {
                        minutes = now;
                        due = self.due(minutes);
                    }
                }
                () = wait_until(due) => {
                    self.submit_scheduled(IndexOrigin::Periodic).await;
                    due = self.due(minutes);
                }
            }
        }
    }

    /// When the next periodic scan is due if `minutes` is the interval; `None` for never.
    fn due(&self, minutes: u32) -> Option<Instant> {
        (minutes > 0).then(|| Instant::now() + self.inner.options.minute.saturating_mul(minutes))
    }

    /// Queues a non-forced scan of every volume, if there is any volume to scan.
    async fn submit_scheduled(&self, origin: IndexOrigin) {
        if let Err(err) = self.refresh_volumes().await {
            tracing::warn!(error = %err, ?origin, "could not read the volumes for a scheduled scan");
            return;
        }
        if self.inner.indexer.volumes().list().is_empty() {
            tracing::debug!(?origin, "no volume to scan");
            return;
        }
        match self.submit(ScanRequest::default(), origin).await {
            Ok(outcome) if outcome.coalesced => {
                tracing::debug!(
                    ?origin,
                    task = outcome.task_id,
                    "scan skipped: indexing is busy"
                );
            }
            Ok(outcome) => {
                tracing::info!(?origin, task = outcome.task_id, "scheduled scan queued");
            }
            Err(err) => tracing::warn!(error = %err, ?origin, "could not queue a scheduled scan"),
        }
    }
}

impl SubmitOutcome {
    fn joined(task_id: TaskId) -> Self {
        Self {
            task_id,
            coalesced: true,
        }
    }
}

async fn wait_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

fn online_ids(volumes: &super::VolumeRegistry) -> HashSet<VolumeId> {
    volumes
        .online()
        .into_iter()
        .map(|volume| volume.id)
        .collect()
}

/// Is a volume online in `after` that was not in `before`?
fn came_online(before: &HashSet<VolumeId>, after: &HashSet<VolumeId>) -> bool {
    !after.is_subset(before)
}

/// The targets in the canonical form the stages plan with ([`pending::plan_targets`]): by volume,
/// a directory before what is below it, nothing a recursive target already covers. An empty list
/// stays empty: it means every volume.
fn normalise_targets(targets: &[ScanTarget]) -> Vec<ScanTarget> {
    pending::plan_targets(targets, &[])
        .into_iter()
        .flat_map(|plan| {
            let volume_id = plan.volume_id;
            plan.scopes.into_iter().map(move |scope| ScanTarget {
                volume_id,
                dir: scope.dir,
                recursive: scope.recursive,
            })
        })
        .collect()
}

/// What covers both lists. "Every volume" (an empty list) covers everything.
fn union(first: &[ScanTarget], second: &[ScanTarget]) -> Vec<ScanTarget> {
    if first.is_empty() || second.is_empty() {
        return Vec::new();
    }
    normalise_targets(&[first, second].concat())
}

/// Both requests are normalised.
fn same_request(first: &ScanRequest, second: &ScanRequest) -> bool {
    first.force == second.force
        && first.confirm_removals == second.confirm_removals
        && first.targets == second.targets
}

/// What the task panel shows for a job: the number of folders asked for, "all assets" for a forced
/// run of every volume, and "changes" for what the watcher saw.
fn title(request: &ScanRequest, origin: IndexOrigin) -> String {
    if origin == IndexOrigin::Watcher {
        return "Indexing changes".to_owned();
    }
    let verb = if request.force {
        "Re-indexing"
    } else {
        "Indexing"
    };
    match request.targets.len() {
        0 if request.force => "Re-indexing all assets".to_owned(),
        0 => "Indexing".to_owned(),
        1 => format!("{verb} 1 folder"),
        count => format!("{verb} {count} folders"),
    }
}

/// The parameters stored with the task: nothing secret, no absolute path.
fn params(request: &ScanRequest, origin: IndexOrigin) -> Value {
    let targets: Vec<Value> = request
        .targets
        .iter()
        .map(|target| {
            json!({
                "volume_id": target.volume_id,
                "dir": target.dir.as_str(),
                "recursive": target.recursive,
            })
        })
        .collect();
    json!({
        "origin": origin.as_str(),
        "targets": targets,
        "force": request.force,
        "confirm_removals": request.confirm_removals,
    })
}

/// Maps the cumulative reports of an indexer run onto a task's progress: a new phase starts a new
/// phase of the task (title, counters and rate), the rest moves the counters by the difference.
pub struct TaskProgressSink {
    progress: TaskProgress,
    state: Mutex<SinkState>,
}

#[derive(Default)]
struct SinkState {
    phase: Option<IndexPhase>,
    items_done: u64,
    bytes_done: u64,
    totals: (Option<u64>, Option<u64>),
}

impl TaskProgressSink {
    pub fn new(progress: TaskProgress) -> Self {
        Self {
            progress,
            state: Mutex::new(SinkState::default()),
        }
    }
}

impl ProgressSink for TaskProgressSink {
    fn report(&self, report: IndexProgress) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let totals = (report.items_total, report.bytes_total);
        if state.phase != Some(report.phase) {
            self.progress.phase(
                phase_title(report.phase),
                totals.0.map(saturating),
                totals.1.map(saturating),
            );
            *state = SinkState {
                phase: Some(report.phase),
                totals,
                ..SinkState::default()
            };
        } else if state.totals != totals {
            self.progress
                .set_totals(totals.0.map(saturating), totals.1.map(saturating));
            state.totals = totals;
        }
        let items = report.items_done.saturating_sub(state.items_done);
        let bytes = report.bytes_done.saturating_sub(state.bytes_done);
        self.progress.add(saturating(items), saturating(bytes));
        state.items_done = state.items_done.max(report.items_done);
        state.bytes_done = state.bytes_done.max(report.bytes_done);
    }
}

fn phase_title(phase: IndexPhase) -> &'static str {
    match phase {
        IndexPhase::Walk => "Indexing: scanning folders",
        IndexPhase::Extract => "Indexing: reading metadata",
        IndexPhase::Hash => "Indexing: hashing files",
        IndexPhase::Thumbs => "Indexing: creating thumbnails",
    }
}

fn saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
