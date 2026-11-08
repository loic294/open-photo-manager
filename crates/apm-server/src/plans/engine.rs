//! [`PlanService`]: the ONE generic plan layer. Everything that is the same for every kind of plan
//! lives here; a [`Planner`](super::Planner) only supplies what is specific to its kind.
//!
//! # States
//!
//! `building` (invisible, the build runs inside `POST /api/plans`) -> `ready` -> `executing` ->
//! `executed`. A plan is visible to clients unless it is `building` or expired; an `executing`
//! plan stays visible until its task ends. Every transition is one conditional `UPDATE` in
//! [`store`](super::store), so two racing requests cannot both win.
//!
//! # Crash and cancellation safety
//!
//! * A build is dropped when the client gives up, the timeout hits or the server stops. A
//!   `BuildGuard` then cancels the build and removes everything it wrote, in the background.
//!   Any `building` row that survives (a crash) is removed at startup and by the sweep.
//! * Executing is handed to a tracked job, so a client that drops its request between claiming
//!   the plan and creating the task cannot leave an `executing` plan without a task. A watcher
//!   settles the plan if its task ends without the runner having recorded a result (cancelled
//!   while queued, interrupted, panicked), and startup settles whatever a crash left `executing`.
//! * Removing a plan (delete, `replaces`, expiry) first hides it (`expires_at = 0`: invisible and
//!   impossible to claim), then deletes its rows in chunks in the background.
//!
//! # Reads do not write
//!
//! Using a plan or a selection extends its life, but at most once per
//! [`EXTEND_THROTTLE`](super::EXTEND_THROTTLE), by a fire-and-forget job: a `GET` or a browse request
//! never waits for a write. The overlay answers from a short-lived in-memory copy of the plan
//! record; `execute` and the hand-off read the database, so their conflict answers are exact.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use apm_core::dto::{
    CreatePlanRequest, CreateSelectionRequest, ExecutePlanRequest, PlanDto, PlanKind, PlanResult,
    PlanSelectionRequest, PlanState, SelectionDto, TaskDto, TaskKind, TaskState,
};
use apm_core::paths::{PlanId, SelectionId, TaskId};
use axum::http::StatusCode;
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::overlay::plan_not_found;
use super::planner::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanError, Planner};
use super::result::summary_title;
use super::{PlanDeps, PlanOptions, bulk, selections, store};
use crate::db::DbError;
use crate::error::{AppError, AppResult};
use crate::fsops::OpContext;
use crate::tasks::{TaskCtx, TaskError, TaskSpec};

/// Plans looked at per sweep step.
const SWEEP_BATCH: i64 = 100;

/// Plan records the overlay cache keeps.
const CACHE_CAP: usize = 64;

/// Subjects the throttle remembers before it drops the old ones.
const THROTTLE_CAP: usize = 1_024;

/// How long `shutdown` waits for the background jobs.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

/// The longest label of a selection, in characters.
const MAX_LABEL_CHARS: usize = 200;

const NOTE_INTERRUPTED: &str = "interrupted by restart";

/// What a sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    /// Expired plans (ready or executed) and their rows.
    pub plans: u64,
    /// Leftovers of builds that never ended.
    pub building: u64,
    /// Expired selections.
    pub selections: u64,
}

/// The plan engine. Cheap to clone: every clone shares one state.
#[derive(Clone)]
pub struct PlanService(Arc<Inner>);

struct Inner {
    deps: PlanDeps,
    planners: HashMap<PlanKind, Arc<dyn Planner>>,
    options: PlanOptions,
    /// Build slots; closed at shutdown.
    builds: Semaphore,
    cache: std::sync::Mutex<HashMap<PlanId, Cached>>,
    throttle: std::sync::Mutex<Throttle>,
    /// Fire-and-forget work: removals, extensions, watchers, the start of an execution.
    jobs: TaskTracker,
    /// The sweep loop. Kept out of `jobs` so that `settle` can wait for the jobs alone.
    gc: std::sync::Mutex<Option<JoinHandle<()>>>,
    stop: CancellationToken,
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn secs(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn shutting_down() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "shutting_down",
        "the server is shutting down",
    )
}

fn selection_not_found(id: SelectionId) -> AppError {
    AppError::not_found("selection_not_found", format!("selection {id} not found"))
}

fn selection_too_large(count: usize, max: usize) -> AppError {
    AppError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "selection_too_large",
        format!("the selection would hold {count} files, more than the {max} allowed"),
    )
}

/// A plan record the overlay trusts for a few seconds.
struct Cached {
    plan: Arc<Plan>,
    loaded_at: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Subject {
    Plan(PlanId),
    Selection(SelectionId),
}

/// When each plan or selection was last written back, so that a burst of requests that all find
/// the old expiry cause one write.
#[derive(Default)]
struct Throttle {
    last: HashMap<Subject, i64>,
}

impl Throttle {
    /// Whether a write for `subject` is due at `now`; remembers it when it is.
    fn allow(&mut self, subject: Subject, now: i64, every: i64) -> bool {
        if self
            .last
            .get(&subject)
            .is_some_and(|last| now.saturating_sub(*last) < every)
        {
            return false;
        }
        if self.last.len() >= THROTTLE_CAP {
            self.last
                .retain(|_, last| now.saturating_sub(*last) < every);
        }
        self.last.insert(subject, now);
        true
    }
}

/// A plan and the planner that serves it, for the overlay.
pub(super) struct Target {
    pub(super) plan: Arc<Plan>,
    pub(super) planner: Arc<dyn Planner>,
}

/// Everything an execution needs, moved into the start job and then into the task.
struct Run {
    plan: Arc<Plan>,
    planner: Arc<dyn Planner>,
    prep: ExecutePrep,
    options: Value,
    only: Option<SelectionId>,
}

/// Removes what an abandoned build wrote. Armed from the moment the `building` row exists until
/// the plan is `ready`; dropping it while armed (an error, a timeout, a client that went away)
/// cancels the build and removes the plan in the background.
struct BuildGuard {
    service: PlanService,
    id: PlanId,
    kind: PlanKind,
    cancel: CancellationToken,
    armed: bool,
}

impl BuildGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel.cancel();
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let service = self.service.clone();
        let (id, kind) = (self.id, self.kind);
        self.service.0.jobs.spawn(async move {
            service.discard(id, kind).await;
        });
    }
}

impl fmt::Debug for PlanService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanService")
            .field("planners", &self.0.planners.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl PlanService {
    /// The engine with the planners of the application (`fn planners` in `plans/mod.rs`) and the
    /// production limits.
    pub async fn new(deps: PlanDeps) -> Result<Self, DbError> {
        let planners = super::planners(&deps);
        Self::with_options(deps, planners, PlanOptions::default()).await
    }

    /// The engine with these planners and options (tests inject a fake planner, a clock and small
    /// limits). Settles the plans a crash left `executing` and starts the sweep.
    pub async fn with_options(
        deps: PlanDeps,
        planners: Vec<Arc<dyn Planner>>,
        options: PlanOptions,
    ) -> Result<Self, DbError> {
        let service = Self(Arc::new(Inner {
            builds: Semaphore::new(options.max_concurrent_builds.max(1)),
            stop: deps.shutdown.child_token(),
            planners: planners
                .into_iter()
                .map(|planner| (planner.kind(), planner))
                .collect(),
            cache: std::sync::Mutex::default(),
            throttle: std::sync::Mutex::default(),
            jobs: TaskTracker::new(),
            gc: std::sync::Mutex::new(None),
            options,
            deps,
        }));
        service.recover().await?;
        // Listed before the service is handed out: a build that starts later is never taken for
        // the leftover of a crash.
        let leftovers = store::list_building(service.0.deps.db.read(), None).await?;
        service.start_background(leftovers);
        Ok(service)
    }

    /// Stops the background work and waits for it. Call it after the task engine stopped and
    /// before the database closes.
    pub async fn shutdown(&self) {
        let inner = &self.0;
        inner.stop.cancel();
        inner.builds.close();
        inner.jobs.close();
        if tokio::time::timeout(SHUTDOWN_WAIT, inner.jobs.wait())
            .await
            .is_err()
        {
            tracing::warn!("plan jobs are still running at shutdown");
        }
        let gc = lock(&inner.gc).take();
        if let Some(gc) = gc {
            let _ = gc.await;
        }
    }

    /// Waits until every fire-and-forget job that is running now has ended (removals,
    /// extensions, the start of an execution). It also waits for the watchers of executions, so
    /// call it only when no plan task is still queued or running. For tests.
    #[doc(hidden)]
    pub async fn settle(&self) {
        self.0.jobs.close();
        self.0.jobs.wait().await;
        self.0.jobs.reopen();
    }

    fn now(&self) -> i64 {
        self.0.options.clock.now()
    }

    fn plan_deadline(&self, now: i64) -> i64 {
        now.saturating_add(secs(self.0.options.plan_ttl))
    }

    fn selection_deadline(&self, now: i64) -> i64 {
        now.saturating_add(secs(self.0.options.selection_ttl))
    }

    fn planner(&self, kind: PlanKind) -> AppResult<Arc<dyn Planner>> {
        self.0.planners.get(&kind).cloned().ok_or_else(|| {
            AppError::not_implemented(match kind {
                PlanKind::Duplicates => "implementing-duplicate-matching",
                PlanKind::Move => "implementing-move-planner",
                PlanKind::Cleanup => "implementing-empty-folder-cleanup",
            })
        })
    }

    // ---- startup, sweep ----

    /// A restart killed every task: the plans that were executing end as interrupted.
    async fn recover(&self) -> Result<(), DbError> {
        let result = PlanResult {
            cancelled: true,
            note: Some(NOTE_INTERRUPTED.to_owned()),
            ..PlanResult::default()
        };
        let now = self.now();
        let settled =
            store::settle_interrupted(&self.0.deps.db, &result, self.plan_deadline(now)).await?;
        if settled > 0 {
            tracing::warn!(
                plans = settled,
                "plans that were executing when the server stopped are now marked as interrupted"
            );
        }
        Ok(())
    }

    /// Removes the leftovers of a crash and sweeps once, then sweeps every `gc_interval`. The
    /// first pass is a tracked job, so `settle` waits for it.
    fn start_background(&self, leftovers: Vec<(PlanId, PlanKind)>) {
        let this = self.clone();
        self.0.jobs.spawn(async move {
            if let Err(err) = this.remove_building(leftovers).await {
                tracing::warn!(error = %err, "could not remove the plans a crash left half built");
            }
            if let Err(err) = this.sweep().await {
                tracing::warn!(error = %err, "the plan sweep failed");
            }
        });
        let handle = tokio::spawn(sweep_loop(
            Arc::downgrade(&self.0),
            self.0.stop.clone(),
            self.0.options.gc_interval,
        ));
        *lock(&self.0.gc) = Some(handle);
    }

    /// One garbage-collection pass: removes the plans that expired, the leftovers of builds that
    /// never ended and the selections that expired. Chunked, so the writer is never held for
    /// long, and stopped by `shutdown`; what it leaves is found by the next pass.
    pub async fn sweep(&self) -> Result<Swept, PlanError> {
        let inner = &self.0;
        let now = self.now();
        let mut swept = Swept::default();
        loop {
            let batch = store::list_expired(inner.deps.db.read(), now, SWEEP_BATCH).await?;
            if batch.is_empty() {
                break;
            }
            let mut removed_any = false;
            for (id, kind) in batch {
                if inner.stop.is_cancelled() {
                    return Ok(swept);
                }
                // Skips a plan that was used since it was listed.
                if store::hide(&inner.deps.db, id, Some(now)).await?.is_none() {
                    continue;
                }
                if self.remove_plan(id, kind).await? {
                    swept.plans += 1;
                    removed_any = true;
                }
            }
            if !removed_any {
                break;
            }
        }
        let cutoff = now.saturating_sub(secs(inner.options.building_max_age));
        swept.building = self.purge_building(cutoff).await?;
        swept.selections = selections::purge_expired(&inner.deps.db, &inner.stop, now).await?;
        if swept != Swept::default() {
            tracing::info!(
                plans = swept.plans,
                building = swept.building,
                selections = swept.selections,
                "expired plans and selections removed"
            );
        }
        Ok(swept)
    }

    /// Removes `building` rows created at or before `created_by`.
    async fn purge_building(&self, created_by: i64) -> Result<u64, PlanError> {
        let rows = store::list_building(self.0.deps.db.read(), Some(created_by)).await?;
        self.remove_building(rows).await
    }

    async fn remove_building(&self, rows: Vec<(PlanId, PlanKind)>) -> Result<u64, PlanError> {
        let mut purged = 0;
        for (id, kind) in rows {
            if self.0.stop.is_cancelled() {
                break;
            }
            if self.remove_plan(id, kind).await? {
                purged += 1;
            }
        }
        Ok(purged)
    }

    // ---- removal ----

    /// Deletes a plan's rows, the planner's tables first in chunks, then the plan row itself.
    /// `false` for a plan that is executing (never removed) or already gone.
    async fn remove_plan(&self, id: PlanId, kind: PlanKind) -> Result<bool, PlanError> {
        let inner = &self.0;
        self.forget(id);
        if store::raw_state(inner.deps.db.read(), id).await?.as_deref() == Some("executing") {
            return Ok(false);
        }
        if let Some(planner) = inner.planners.get(&kind) {
            for table in planner.child_tables() {
                bulk::delete_plan_rows(&inner.deps.db, &inner.stop, table, id).await?;
            }
        }
        Ok(store::delete_row(&inner.deps.db, id).await?)
    }

    /// Hides a plan at once and removes its rows in the background. `None` when there is no such
    /// plan, or it is building or executing.
    async fn retire(&self, id: PlanId) -> Result<Option<PlanKind>, DbError> {
        let Some(kind) = store::hide(&self.0.deps.db, id, None).await? else {
            return Ok(None);
        };
        self.forget(id);
        let this = self.clone();
        self.0.jobs.spawn(async move {
            match this.remove_plan(id, kind).await {
                Ok(_) | Err(PlanError::Cancelled) => {}
                Err(err) => tracing::warn!(
                    plan_id = id,
                    error = %err,
                    "could not remove a plan; the next sweep retries"
                ),
            }
        });
        Ok(Some(kind))
    }

    /// Removes a plan whose build did not end well.
    async fn discard(&self, id: PlanId, kind: PlanKind) {
        let removed: Result<bool, PlanError> = async {
            // A build that reached `ready` just before it was abandoned.
            store::hide(&self.0.deps.db, id, None).await?;
            self.remove_plan(id, kind).await
        }
        .await;
        if let Err(err) = removed
            && !matches!(err, PlanError::Cancelled)
        {
            tracing::warn!(
                plan_id = id,
                error = %err,
                "could not remove an abandoned plan; the next sweep retries"
            );
        }
    }

    // ---- create, get, delete ----

    /// Builds a plan (`POST /api/plans`). The whole request, the wait for a build slot included,
    /// is bounded by `build_timeout`.
    pub async fn create(&self, request: CreatePlanRequest) -> AppResult<PlanDto> {
        let inner = &self.0;
        if inner.stop.is_cancelled() {
            return Err(shutting_down());
        }
        let planner = self.planner(request.kind)?;
        let params = match request.params {
            Value::Null => Value::Object(Map::new()),
            params @ Value::Object(_) => params,
            _ => {
                return Err(AppError::bad_request(
                    "invalid_plan_params",
                    "params must be a JSON object",
                ));
            }
        };
        let limit = inner.options.build_timeout;
        match tokio::time::timeout(
            limit,
            self.build_plan(planner, request.kind, params, request.replaces),
        )
        .await
        {
            Ok(created) => created,
            Err(_) => Err(AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_build_timeout",
                format!(
                    "building the plan took longer than {} seconds",
                    limit.as_secs()
                ),
            )),
        }
    }

    async fn build_plan(
        &self,
        planner: Arc<dyn Planner>,
        kind: PlanKind,
        params: Value,
        replaces: Option<PlanId>,
    ) -> AppResult<PlanDto> {
        let inner = &self.0;
        let _slot = inner.builds.acquire().await.map_err(|_| shutting_down())?;
        let started = Instant::now();
        let now = self.now();
        let id = store::insert_building(&inner.deps.db, kind, &params, now, now).await?;
        let cancel = inner.stop.child_token();
        let mut guard = BuildGuard {
            service: self.clone(),
            id,
            kind,
            cancel: cancel.clone(),
            armed: true,
        };
        let ctx = BuildContext {
            plan_id: id,
            db: inner.deps.db.clone(),
            settings: inner.deps.settings.get(),
            volumes: inner.deps.volumes.clone(),
            cancel,
            now,
            max_items: inner.options.max_items,
        };
        tracing::debug!(plan_id = id, kind = store::kind_to_db(kind), %params, "building a plan");
        let Built {
            items,
            stats,
            warnings,
        } = planner.build(&ctx, params).await?;
        if items > inner.options.max_items {
            return Err(PlanError::TooLarge {
                items,
                max: inner.options.max_items,
            }
            .into());
        }
        let ready_at = self.now();
        let expires_at = self.plan_deadline(ready_at);
        let ready = store::mark_ready(&inner.deps.db, id, &stats, &warnings, expires_at).await?;
        // No await between `mark_ready` and here: the plan is either ready or gone, never in
        // between, whatever happens to this future.
        guard.disarm();
        if !ready {
            return Err(PlanError::Cancelled.into());
        }
        if let Some(old) = replaces.filter(|old| *old != id)
            && let Err(err) = self.retire(old).await
        {
            tracing::warn!(plan_id = old, error = %err, "could not retire the plan this one replaces");
        }
        tracing::info!(
            plan_id = id,
            kind = store::kind_to_db(kind),
            items,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "plan built"
        );
        Ok(PlanDto {
            id,
            kind,
            state: PlanState::Ready,
            stats,
            warnings,
            result: None,
            task_id: None,
            created_at: now,
            expires_at,
        })
    }

    /// One plan, as `GET /api/plans/{id}` answers it. Unknown, expired and still-building plans
    /// are 404 `plan_not_found`.
    pub async fn get(&self, id: PlanId) -> AppResult<PlanDto> {
        let now = self.now();
        let row = store::get_visible(self.0.deps.db.read(), id, now)
            .await?
            .ok_or_else(|| plan_not_found(id))?;
        let dto = row.dto()?;
        self.touch_plan(id, dto.state, dto.expires_at, now);
        Ok(dto)
    }

    /// Removes a plan (`DELETE /api/plans/{id}`): invisible at once, its rows go in the
    /// background. Idempotent; a plan that is executing is a 409 `plan_executing`.
    pub async fn delete(&self, id: PlanId) -> AppResult<()> {
        if self.retire(id).await?.is_some() {
            return Ok(());
        }
        match store::raw_state(self.0.deps.db.read(), id)
            .await?
            .as_deref()
        {
            Some("executing") => Err(AppError::conflict(
                "plan_executing",
                "the plan is being executed: cancel its task first",
            )),
            _ => Ok(()),
        }
    }

    // ---- cache and expiry ----

    fn cached(&self, id: PlanId, now: i64) -> Option<Arc<Plan>> {
        let cache = lock(&self.0.cache);
        let entry = cache.get(&id)?;
        let fresh = now.saturating_sub(entry.loaded_at) < secs(self.0.options.cache_ttl);
        let alive = entry.plan.state == PlanState::Executing || entry.plan.expires_at > now;
        (fresh && alive).then(|| Arc::clone(&entry.plan))
    }

    fn remember(&self, plan: Arc<Plan>, now: i64) {
        let ttl = secs(self.0.options.cache_ttl);
        let mut cache = lock(&self.0.cache);
        if cache.len() >= CACHE_CAP {
            cache.retain(|_, entry| now.saturating_sub(entry.loaded_at) < ttl);
            if cache.len() >= CACHE_CAP {
                cache.clear();
            }
        }
        cache.insert(
            plan.id,
            Cached {
                plan,
                loaded_at: now,
            },
        );
    }

    fn forget(&self, id: PlanId) {
        lock(&self.0.cache).remove(&id);
    }

    /// The plan a client may see, from the database.
    async fn load_fresh(&self, id: PlanId) -> AppResult<Arc<Plan>> {
        let row = store::get_visible(self.0.deps.db.read(), id, self.now())
            .await?
            .ok_or_else(|| plan_not_found(id))?;
        Ok(Arc::new(row.plan()?))
    }

    /// The plan a client may see, from the cache when it is recent.
    async fn load(&self, id: PlanId) -> AppResult<Arc<Plan>> {
        let now = self.now();
        if let Some(plan) = self.cached(id, now) {
            return Ok(plan);
        }
        let plan = self.load_fresh(id).await?;
        self.remember(Arc::clone(&plan), now);
        Ok(plan)
    }

    /// Extends the life of a plan that was just used, unless that was done a moment ago. Never
    /// waits for the write.
    fn touch_plan(&self, id: PlanId, state: PlanState, expires_at: i64, now: i64) {
        if state == PlanState::Executing {
            return;
        }
        let inner = &self.0;
        let every = secs(inner.options.extend_throttle);
        let wanted = self.plan_deadline(now);
        if expires_at >= wanted.saturating_sub(every)
            || !lock(&inner.throttle).allow(Subject::Plan(id), now, every)
        {
            return;
        }
        let this = self.clone();
        inner.jobs.spawn(async move {
            match store::extend(&this.0.deps.db, id, now, wanted).await {
                Ok(_) => this.forget(id),
                Err(err) => tracing::warn!(plan_id = id, error = %err, "could not extend a plan"),
            }
        });
    }

    fn touch_selection(&self, id: SelectionId, expires_at: i64, now: i64) {
        let inner = &self.0;
        let every = secs(inner.options.extend_throttle);
        let wanted = self.selection_deadline(now);
        if expires_at >= wanted.saturating_sub(every)
            || !lock(&inner.throttle).allow(Subject::Selection(id), now, every)
        {
            return;
        }
        let this = self.clone();
        inner.jobs.spawn(async move {
            if let Err(err) = selections::extend(&this.0.deps.db, id, now, wanted).await {
                tracing::warn!(selection_id = id, error = %err, "could not extend a selection");
            }
        });
    }

    // ---- overlay ----

    /// The plan and planner behind a browse request. Unknown, expired and unplanned plans are 404
    /// `plan_not_found`; an executed plan has nothing left to show: `None`.
    pub(super) async fn overlay_target(&self, plan_id: PlanId) -> AppResult<Option<Target>> {
        let plan = self.load(plan_id).await?;
        let planner = self
            .0
            .planners
            .get(&plan.kind)
            .cloned()
            .ok_or_else(|| plan_not_found(plan_id))?;
        self.touch_plan(plan.id, plan.state, plan.expires_at, self.now());
        if plan.state == PlanState::Executed {
            return Ok(None);
        }
        Ok(Some(Target { plan, planner }))
    }

    // ---- execute ----

    /// Starts the execution of a plan (`POST /api/plans/{id}/execute`) as a `plan_execute` task
    /// and returns the task. Everything that can be refused is refused before the plan is
    /// claimed, so a bad request leaves the plan `ready` and creates no task.
    pub async fn execute(&self, id: PlanId, request: ExecutePlanRequest) -> AppResult<TaskDto> {
        let inner = &self.0;
        let plan = self.load_fresh(id).await?;
        match plan.state {
            PlanState::Ready => {}
            PlanState::Executing => {
                return Err(AppError::conflict(
                    "plan_not_ready",
                    "the plan is already being executed",
                ));
            }
            PlanState::Executed => {
                return Err(AppError::conflict(
                    "plan_executed",
                    "the plan was already executed",
                ));
            }
        }
        let planner = self.planner(plan.kind)?;
        let options = match request.options {
            Value::Null => Value::Object(Map::new()),
            options @ Value::Object(_) => options,
            _ => {
                return Err(AppError::bad_request(
                    "invalid_plan_options",
                    "options must be a JSON object",
                ));
            }
        };
        let prep = planner
            .prepare_execute(&plan, &options)
            .map_err(options_error)?;
        if let Some(only) = request.only {
            let now = self.now();
            match selections::expiry(inner.deps.db.read(), only, now).await? {
                Some(expires_at) => self.touch_selection(only, expires_at, now),
                None => return Err(selection_not_found(only)),
            }
        }
        let run = Run {
            plan,
            planner,
            prep,
            options,
            only: request.only,
        };
        // A tracked job, not this future: a client that drops the request after the plan was
        // claimed must not leave it `executing` without a task.
        let this = self.clone();
        match inner.jobs.spawn(async move { this.start(run).await }).await {
            Ok(started) => started,
            Err(err) => Err(AppError::internal(err)),
        }
    }

    async fn start(&self, run: Run) -> AppResult<TaskDto> {
        let inner = &self.0;
        let id = run.plan.id;
        let now = self.now();
        if !store::claim(&inner.deps.db, id, now, self.plan_deadline(now)).await? {
            return Err(AppError::conflict(
                "plan_not_ready",
                "the plan is not ready: it is being executed, was executed or has expired",
            ));
        }
        self.forget(id);
        let spec =
            TaskSpec::new(TaskKind::PlanExecute, run.prep.title.clone()).with_params(json!({
                "plan_id": id,
                "kind": store::kind_to_db(run.plan.kind),
                "only": run.only,
                "options": run.options,
            }));
        let service = self.clone();
        let submitted = inner
            .deps
            .tasks
            .submit(spec, move |ctx| service.run_task(ctx, run))
            .await;
        let task_id = match submitted {
            Ok(task_id) => task_id,
            Err(err) => {
                if let Err(release) = store::release_claim(&inner.deps.db, id).await {
                    tracing::error!(plan_id = id, error = %release, "could not give a plan back after its task was refused");
                }
                self.forget(id);
                return Err(err);
            }
        };
        if let Err(err) = store::set_task(&inner.deps.db, id, task_id).await {
            tracing::warn!(plan_id = id, task_id, error = %err, "could not link a plan to its task");
        }
        tracing::info!(plan_id = id, task_id, "plan execution queued");
        self.watch(id, task_id);
        inner.deps.tasks.get(task_id).await
    }

    /// The body of the `plan_execute` task.
    async fn run_task(self, ctx: TaskCtx, run: Run) -> Result<Value, TaskError> {
        let inner = &self.0;
        let Run {
            plan,
            planner,
            prep,
            options,
            only,
        } = run;
        let plan = Plan {
            state: PlanState::Executing,
            ..Plan::clone(&plan)
        };
        let op = OpContext::from_task(&ctx, &inner.deps.tasks);
        let batch = op.batch().clone();
        let exec = ExecContext {
            plan_id: plan.id,
            db: inner.deps.db.clone(),
            fsops: inner.deps.fsops.clone(),
            settings: inner.deps.settings.get(),
            volumes: inner.deps.volumes.clone(),
            task: ctx.clone(),
            op,
            batch,
            only,
        };
        match planner.execute(exec, &plan, options).await {
            Ok(result) => {
                let total = ctx.progress.view().items_total;
                ctx.progress.set_title(summary_title(&prep, &result, total));
                self.settle_plan(plan.id, ctx.id, &result).await;
                tracing::info!(
                    plan_id = plan.id,
                    done = result.done,
                    skipped = result.skipped,
                    failed = result.failed,
                    cancelled = result.cancelled,
                    "plan executed"
                );
                serde_json::to_value(&result).map_err(|err| TaskError::Failed(err.to_string()))
            }
            Err(PlanError::Cancelled) => {
                let result = PlanResult {
                    cancelled: true,
                    ..PlanResult::default()
                };
                self.settle_plan(plan.id, ctx.id, &result).await;
                Err(TaskError::Cancelled)
            }
            Err(err) => {
                // Logs an internal cause and keeps it from the client-facing message.
                let message = AppError::from(err).message().to_owned();
                let result = PlanResult {
                    note: Some(message.clone()),
                    ..PlanResult::default()
                };
                self.settle_plan(plan.id, ctx.id, &result).await;
                Err(TaskError::Failed(message))
            }
        }
    }

    /// Records the end of an execution. The first writer wins, so the runner and the watcher can
    /// both call it.
    async fn settle_plan(&self, plan_id: PlanId, task_id: TaskId, result: &PlanResult) {
        let now = self.now();
        let finished = store::finish(
            &self.0.deps.db,
            plan_id,
            Some(task_id),
            result,
            self.plan_deadline(now),
        )
        .await;
        if let Err(err) = finished {
            tracing::error!(plan_id, task_id, error = %err, "could not record the result of a plan");
        }
        self.forget(plan_id);
    }

    /// Settles the plan if its task ends without the runner having recorded a result: cancelled
    /// while queued, interrupted by a shutdown, panicked.
    fn watch(&self, plan_id: PlanId, task_id: TaskId) {
        let this = self.clone();
        self.0.jobs.spawn(async move {
            let task = match this.0.deps.tasks.wait_finished(task_id).await {
                Ok(task) => task,
                Err(err) => {
                    tracing::warn!(plan_id, task_id, error = %err, "could not wait for the task of a plan");
                    return;
                }
            };
            let result = match task.state {
                TaskState::Done => PlanResult {
                    note: Some("the result of the run could not be recorded".to_owned()),
                    ..PlanResult::default()
                },
                TaskState::Cancelled => PlanResult {
                    cancelled: true,
                    note: Some("cancelled before it ran".to_owned()),
                    ..PlanResult::default()
                },
                TaskState::Interrupted => PlanResult {
                    cancelled: true,
                    note: Some(NOTE_INTERRUPTED.to_owned()),
                    ..PlanResult::default()
                },
                TaskState::Failed | TaskState::Queued | TaskState::Running => PlanResult {
                    note: Some(
                        task.error
                            .unwrap_or_else(|| "the task did not finish".to_owned()),
                    ),
                    ..PlanResult::default()
                },
            };
            this.settle_plan(plan_id, task_id, &result).await;
        });
    }

    // ---- selections ----

    /// Stores a selection of files (`POST /api/selections`): the ids that exist, each once.
    pub async fn create_selection(
        &self,
        request: CreateSelectionRequest,
    ) -> AppResult<SelectionDto> {
        let CreateSelectionRequest { mut ids, label } = request;
        let label = clean_label(label)?;
        ids.sort_unstable();
        ids.dedup();
        self.store_selection(&ids, label.as_deref()).await
    }

    /// One selection (`GET /api/selections/{id}`), counted from the files that are in it now.
    pub async fn get_selection(&self, id: SelectionId) -> AppResult<SelectionDto> {
        let now = self.now();
        let dto = selections::get(self.0.deps.db.read(), id, now)
            .await?
            .ok_or_else(|| selection_not_found(id))?;
        self.touch_selection(id, dto.expires_at, now);
        Ok(dto)
    }

    /// Hands the files a plan involves over as a selection
    /// (`POST /api/plans/{id}/selection`).
    pub async fn selection_from_plan(
        &self,
        id: PlanId,
        request: PlanSelectionRequest,
    ) -> AppResult<SelectionDto> {
        let plan = self.load_fresh(id).await?;
        if plan.state == PlanState::Executed {
            return Err(AppError::conflict(
                "plan_executed",
                "the plan was already executed: it involves no files any more",
            ));
        }
        let planner = self.planner(plan.kind)?;
        let mut ids = planner.involved_ids(&plan, request.side).await?;
        if let Some(restrict) = request.restrict_ids {
            let allowed: HashSet<_> = restrict.into_iter().collect();
            ids.retain(|id| allowed.contains(id));
        }
        ids.sort_unstable();
        ids.dedup();
        self.touch_plan(plan.id, plan.state, plan.expires_at, self.now());
        let label = format!("{} plan {}", store::kind_to_db(plan.kind), plan.id);
        self.store_selection(&ids, Some(&label)).await
    }

    /// `ids` must be sorted and unique.
    async fn store_selection(
        &self,
        ids: &[apm_core::paths::FileId],
        label: Option<&str>,
    ) -> AppResult<SelectionDto> {
        let inner = &self.0;
        let max = inner.options.max_selection_ids;
        if ids.len() > max {
            return Err(selection_too_large(ids.len(), max));
        }
        let now = self.now();
        let id = selections::create(
            &inner.deps.db,
            ids,
            label,
            now,
            self.selection_deadline(now),
        )
        .await?;
        selections::get(inner.deps.db.read(), id, now)
            .await?
            .ok_or_else(|| selection_not_found(id))
    }
}

/// `prepare_execute` rejected the options: a 400 of its own code, before any task exists.
fn options_error(err: PlanError) -> AppError {
    match err {
        PlanError::Invalid { field, message } => AppError::bad_request(
            "invalid_plan_options",
            match field {
                Some(field) => format!("{field}: {message}"),
                None => message,
            },
        ),
        other => other.into(),
    }
}

fn clean_label(label: Option<String>) -> AppResult<Option<String>> {
    let Some(label) = label else {
        return Ok(None);
    };
    let label = label.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(AppError::bad_request(
            "invalid_selection",
            format!("the label is longer than {MAX_LABEL_CHARS} characters"),
        ));
    }
    Ok(Some(label.to_owned()))
}

/// Sweeps now and then until the engine stops or is gone. It holds the engine only while a pass
/// runs, so a dropped engine does not live on through its own loop.
async fn sweep_loop(inner: Weak<Inner>, stop: CancellationToken, every: Duration) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(every) => {}
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let service = PlanService(inner);
        if let Err(err) = service.sweep().await {
            tracing::warn!(error = %err, "the plan sweep failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_throttle_allows_one_write_per_period() {
        let mut throttle = Throttle::default();
        let plan = Subject::Plan(1);
        assert!(throttle.allow(plan, 1_000, 60));
        assert!(!throttle.allow(plan, 1_000, 60));
        assert!(!throttle.allow(plan, 1_059, 60));
        assert!(throttle.allow(plan, 1_060, 60));
        assert!(
            throttle.allow(Subject::Selection(1), 1_061, 60),
            "a selection is not a plan with the same id"
        );
    }

    #[test]
    fn the_throttle_forgets_old_subjects_when_it_is_full() {
        let mut throttle = Throttle::default();
        for id in 0..i64::try_from(THROTTLE_CAP).unwrap() {
            assert!(throttle.allow(Subject::Plan(id), 1_000, 60));
        }
        assert!(throttle.allow(Subject::Plan(-1), 2_000, 60));
        assert_eq!(throttle.last.len(), 1);
    }

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(clean_label(None).unwrap(), None);
        assert_eq!(clean_label(Some("   ".to_owned())).unwrap(), None);
        assert_eq!(
            clean_label(Some("  keep  ".to_owned())).unwrap().as_deref(),
            Some("keep")
        );
        assert!(clean_label(Some("x".repeat(MAX_LABEL_CHARS))).is_ok());
        let err = clean_label(Some("x".repeat(MAX_LABEL_CHARS + 1))).unwrap_err();
        assert_eq!(err.code(), "invalid_selection");
    }

    #[test]
    fn rejected_options_have_their_own_code() {
        let err = options_error(PlanError::invalid_field("mode", "is unknown"));
        assert_eq!(
            (err.status(), err.code(), err.message()),
            (
                StatusCode::BAD_REQUEST,
                "invalid_plan_options",
                "mode: is unknown"
            )
        );
        let err = options_error(PlanError::NotFound(3));
        assert_eq!(err.code(), "plan_not_found");
    }

    #[test]
    fn durations_become_seconds_without_overflow() {
        assert_eq!(secs(Duration::from_secs(90)), 90);
        assert_eq!(secs(Duration::MAX), i64::MAX);
    }
}
