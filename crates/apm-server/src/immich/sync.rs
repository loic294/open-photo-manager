//! The read-only sync of the assets of Immich into `immich_assets` (see `assets`), and the loop
//! that starts it.
//!
//! The sync only reads: its requests are `POST /search/metadata` (through
//! [`ImmichClient::search_metadata`], which hides the request styles of the server generations)
//! and `GET /libraries`. Nothing is ever changed in Immich, and nothing is changed on disk.
//!
//! # Scopes and passes
//!
//! [`assets::plan`] splits the mappings into scopes: with an administrator account one scope per
//! mapped library (several volumes may share one), otherwise one per mapped path prefix (the
//! search cannot name a library then). A *pass* reads one scope completely, page by page (1000
//! assets), and stores every page in one write transaction as it arrives, so the whole library is
//! never held in memory:
//!
//! - **Full**: no lower bound. After it, the rows the pass did not touch are deleted (those assets
//!   are gone from Immich) and the fingerprint of the scope is stored. A scope without state, whose
//!   fingerprint differs from the mappings or whose last full pass is a day old gets one.
//! - **Incremental**: only assets updated strictly after `high_water_ms` minus five minutes of
//!   skew. It sees changes, not deletions; the daily full pass bounds how long the row of an asset
//!   that was deleted from Immich for good can mislead.
//!
//! Every pass asks for trashed and offline assets too: Immich trashes an asset when its file goes
//! missing and flags it offline in the same moment, so without them a deletion is invisible. Both
//! flags are stored; "live" means neither (`ImmichAsset::is_live`).
//!
//! The server generations differ in what "everything" is: the typed filter of Immich 3.2 and later
//! returns every visibility the account can see, the flat form of older servers only archived and
//! timeline assets, and Live Photo motion videos are `hidden`. A pass therefore reads a second
//! stream of hidden assets when the server may be an older one: the version is below 3.2, or
//! unknown (the client may have fallen back to the flat form without saying so), or a page token of
//! the flat style came back. Locked assets are never returned to an API key and are out of scope,
//! like the assets that were uploaded to Immich instead of being read from a library.
//!
//! The high-water mark is the newest `updatedAt` seen, moved back by how long the pass took and
//! never backwards past the stored mark: an asset changed while a long pass was running can sit
//! earlier in `updatedAt` than the newest one read, and the next incremental pass must still see
//! it. Immich cannot order by update time, so the mark moves only after every page was read; a
//! cancelled or failed pass leaves it where it was (the rows it stored are valid anyway). An asset
//! without `updatedAt` is stored with `0` and never moves the mark.
//!
//! # Triggers
//!
//! No new settings. [`ImmichSync::start_background`] runs one loop: the first tick 30 s after the
//! start (the startup scan settles first), then every 30 s. A tick only decides and submits:
//!
//! - not connected, the key cannot be read (the status says `needs_reconnect`), or a sync already
//!   running: nothing;
//! - nothing mapped: the data of scopes that no longer exist is dropped, nothing else;
//! - a scope is due when it has no state, its fingerprint differs from the current mappings (first
//!   connection, edited mappings, another account), its last completed pass is 15 minutes old, or
//!   its last full pass a day;
//! - a failed scope backs off: `30 s * 2^failures` since its last attempt, at least 120 s and at
//!   most an hour. The minimum also keeps a cancelled sync from starting again at once.
//!
//! Besides the loop, `POST /api/immich/sync` submits a manual sync (all scopes, `full` forces full
//! passes). **The settings UI calls it after saving mappings, and the outbox calls
//! [`ImmichSync::submit`] when its queue drains**; both rely on coalescing.
//!
//! # Coalescing
//!
//! One sync runs at a time (the lane of the task engine would allow two), and at most one waits.
//! A request that arrives while one waits is merged into it (`full` is OR-ed, a manual request
//! makes it manual) and gets the same task id with `coalesced`. A request that arrives while one
//! runs queues exactly one follow-up, which later requests merge into. The task takes its request
//! when it starts running. A waiting task that was told to stop (the engine calls it running
//! until its body returns, see `TaskEngine::is_stopping`) takes no more requests: the next one
//! queues a new task, so a request is never lost to a task that will not run it.
//!
//! # Failure and cancellation
//!
//! An Immich error that every scope would meet (unreachable, key rejected, forbidden, redirected)
//! stops the run; any other error fails only its scope and the others still run. A mapping whose
//! library no longer exists fails its scope with a message that says so. A failed scope stores
//! `last_error` and raises `consecutive_failures`, the task fails with the first message, and the
//! stored data stays as it was. Cancelling the task, or losing the connection (see
//! [`assets::begin_guarded`]), ends the run as cancelled without recording anything more.
//!
//! # The task
//!
//! While it reads, the task is titled "Reading Immich" and counts the assets read, without a total
//! (the response of the search has none; its `total` is deprecated). A finished task is titled
//! "Syncing with Immich" and shows everything it read as both its done and its total count. Its
//! result is the [`SyncSummary`](apm_core::dto::SyncSummary).
//!
//! A key that Immich rejects is not special-cased: [`ImmichSync::submit`] still queues the sync,
//! the pass fails with the client's message, and the health tracking of the client turns the status
//! into `needs_reconnect`. The loop goes on at the pace of the failure backoff, an hour at most,
//! until the person connects again; connecting lifts the backoff (see `connect`). Only a key that
//! cannot be read at all (the key file was lost) is refused up front, with 409
//! `immich_needs_reconnect`; a client that was built before the loss keeps working until the
//! stored connection changes (see `ImmichService::client`).
//!
//! # Numbers
//!
//! From `tests/immich_sync_perf.rs` (run by hand, in a release build; see the docs of that file),
//! on an Apple M1 Max: a library of 220,000 files in 1,100 folders, 200,000 assets in Immich (2% of
//! them trashed or offline), the real router, task engine and database, and the fake Immich over a
//! loopback socket. The fake answers each page by scanning all its assets, so part of every sync
//! time is the fake's.
//!
//! | what                                 | time     | read    | searches |
//! |--------------------------------------|----------|---------|----------|
//! | first sync, everything is new        | 2.5 s    | 200,000 | 200      |
//! | incremental, nothing changed         | 0.12 s   | 200     | 1        |
//! | incremental, 1,000 assets changed    | 0.16 s   | 1,200   | 2        |
//! | full pass, 1,000 assets gone         | 2.9 s    | 199,000 | 199      |
//!
//! - The target of the plan was a first sync under 60 s: it takes a twentieth of that, about 80,000
//!   assets a second, so what the sync costs is the network and Immich, not this code.
//! - Item pages asked every 100 ms during a sync stay at 2 ms (3 ms at most): a write transaction
//!   carries 1000 rows at most and readers do not wait for the writer.
//! - The 200,000 rows add 67 MiB to the database (about 350 bytes an asset, indexes included).
//! - The `in_immich` badge adds 0.4 ms to a page of 200 files with all 200,000 assets stored (target:
//!   20 ms): see `query::items`.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apm_core::dto::{ImmichSyncRequest, TaskKind, TaskState};
use apm_core::paths::TaskId;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::ImmichService;
use super::assets::{self, Completion, Resolved, Scope, ScopeKind, ScopeState};
use super::client::ImmichClient;
use super::error::ImmichError;
use super::models::{AssetVisibility, MetadataSearch, OfflineFilter, PageToken, SortOrder};
use super::paths::PathMap;
use crate::db::{Db, DbError, now_secs};
use crate::error::AppError;
use crate::tasks::{TaskCtx, TaskEngine, TaskError, TaskSpec};

const TASK_TITLE: &str = "Syncing with Immich";

/// The loop starts looking this long after the server started.
const FIRST_DELAY: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_secs(30);

/// A scope is read again when its last completed pass is this old...
const INCREMENTAL_EVERY_SECS: i64 = 15 * 60;
/// ...and in full when its last full pass is this old.
const FULL_EVERY_SECS: i64 = 24 * 60 * 60;

/// After an attempt, a scope waits `BACKOFF_BASE_SECS * 2^failures`, between these limits.
const BACKOFF_BASE_SECS: i64 = 30;
const MIN_RETRY_GAP_SECS: i64 = 120;
const MAX_BACKOFF_SECS: i64 = 60 * 60;

/// Taken off the high-water mark of an incremental pass: clock skew between the machines and
/// writes that were in flight when the previous pass read.
const SKEW_MS: i64 = 5 * 60 * 1000;

const PAGE_SIZE: u32 = 1000;

/// A stream that is still going after this many pages is a server that never says it is done.
const MAX_PAGES: u32 = 20_000;

/// What `last_error` says when the database failed (the details are in the server log).
const STORAGE_FAILURE: &str = "The sync could not store what it read; see the server log.";

/// A request for a sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncRequest {
    /// Read every scope in full instead of only what changed since its last pass.
    pub full: bool,
}

impl From<ImmichSyncRequest> for SyncRequest {
    fn from(request: ImmichSyncRequest) -> Self {
        Self { full: request.full }
    }
}

/// The task a request ended up in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncSubmitted {
    pub task_id: TaskId,
    /// The request was merged into a sync that had not started yet.
    pub coalesced: bool,
}

/// The asset sync. Cheap to clone; every clone is the same sync.
#[derive(Clone)]
pub struct ImmichSync {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    immich: ImmichService,
    tasks: TaskEngine,
    /// Held by the body of a running sync.
    running: Mutex<()>,
    /// The request of the one sync that waits for its turn.
    pending: Mutex<Option<Pending>>,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    task_id: TaskId,
    full: bool,
    /// Asked for by a person (or the outbox): every scope is read, due or not.
    manual: bool,
}

impl fmt::Debug for ImmichSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmichSync").finish_non_exhaustive()
    }
}

impl ImmichSync {
    /// Nothing is read or sent here; see [`ImmichSync::start_background`] and
    /// [`ImmichSync::submit`].
    pub fn new(db: Db, immich: ImmichService, tasks: TaskEngine) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                immich,
                tasks,
                running: Mutex::new(()),
                pending: Mutex::new(None),
            }),
        }
    }

    /// Queues a sync of every scope (see the module docs for the coalescing). 409
    /// `immich_not_connected` without a connection, 409 `immich_needs_reconnect` when the stored
    /// key cannot be used, 409 `immich_not_mapped` without any mapping.
    pub async fn submit(&self, request: SyncRequest) -> Result<SyncSubmitted, AppError> {
        let immich = &self.inner.immich;
        immich.stored().await?;
        if immich.mappings().await?.is_empty() {
            return Err(AppError::conflict(
                "immich_not_mapped",
                "Map a volume to Immich before syncing.",
            ));
        }
        immich.client().await?;
        self.enqueue(request.full, true).await
    }

    /// What the loop does on every tick: queues a sync when a scope is due, and does nothing when
    /// not connected, when the key is unusable, when nothing is mapped or when a sync is running.
    pub async fn submit_if_due(&self) -> Result<Option<SyncSubmitted>, AppError> {
        let inner = &self.inner;
        let stored = match inner.immich.stored().await {
            Ok(stored) => stored,
            Err(ImmichError::NotConnected) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let mappings = inner.immich.mappings().await?;
        let scopes = assets::plan(&stored, &mappings);
        if scopes.is_empty() {
            assets::retain_scopes(&inner.db, &stored.api_key_id, &scopes).await?;
            return Ok(None);
        }
        match inner.immich.client().await {
            Ok(_) => {}
            Err(ImmichError::NotConnected | ImmichError::NeedsReconnect) => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        if inner.running.try_lock().is_err() {
            return Ok(None);
        }
        let states = assets::states(&inner.db).await?;
        let now = now_secs();
        if !scopes
            .iter()
            .any(|scope| is_due(scope, states.get(&scope.key), now))
        {
            return Ok(None);
        }
        self.enqueue(false, false).await.map(Some)
    }

    /// Starts the loop: the first look 30 s from now, then one every 30 s, until `shutdown`.
    pub fn start_background(&self, shutdown: CancellationToken) {
        self.start_background_every(shutdown, FIRST_DELAY, TICK);
    }

    /// [`ImmichSync::start_background`] with other timings, for tests.
    pub fn start_background_every(
        &self,
        shutdown: CancellationToken,
        first_delay: Duration,
        tick: Duration,
    ) {
        let sync = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(first_delay) => {}
            }
            let mut ticks = tokio::time::interval(tick);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    _ = ticks.tick() => {}
                }
                if let Err(err) = sync.submit_if_due().await {
                    tracing::warn!(error = %err, "the Immich sync could not be started");
                }
            }
        });
    }

    /// Queues a sync, or merges the request into the one that waits.
    async fn enqueue(&self, full: bool, manual: bool) -> Result<SyncSubmitted, AppError> {
        let inner = &self.inner;
        let mut pending = inner.pending.lock().await;
        if let Some(waiting) = pending.as_mut()
            && inner.is_active(waiting.task_id).await
        {
            waiting.full |= full;
            waiting.manual |= manual;
            return Ok(SyncSubmitted {
                task_id: waiting.task_id,
                coalesced: true,
            });
        }
        let sync = self.clone();
        let task_id = inner
            .tasks
            .submit(
                TaskSpec::new(TaskKind::ImmichSync, TASK_TITLE),
                move |ctx| async move { sync.run_task(ctx).await },
            )
            .await?;
        *pending = Some(Pending {
            task_id,
            full,
            manual,
        });
        Ok(SyncSubmitted {
            task_id,
            coalesced: false,
        })
    }

    /// The body of the task: waits for its turn, takes its request, runs.
    async fn run_task(self, ctx: TaskCtx) -> Result<Value, TaskError> {
        let inner = &self.inner;
        let turn = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => {
                inner.discard_pending(ctx.id).await;
                return Err(TaskError::Cancelled);
            }
            turn = inner.running.lock() => turn,
        };
        let request = inner.take_pending(ctx.id).await;
        if ctx.cancel.is_cancelled() {
            return Err(TaskError::Cancelled);
        }
        let outcome = self.run(&ctx, request).await;
        drop(turn);
        outcome
    }

    async fn run(&self, ctx: &TaskCtx, request: Pending) -> Result<Value, TaskError> {
        let inner = &self.inner;
        let db = &inner.db;
        let stored = match inner.immich.stored().await {
            Ok(stored) => stored,
            Err(ImmichError::NotConnected) => return Err(TaskError::Cancelled),
            Err(err) => return Err(immich_failed(&err)),
        };
        let api_key_id = stored.api_key_id.as_str();
        let mappings = inner
            .immich
            .mappings()
            .await
            .map_err(|err| immich_failed(&err))?;
        let scopes = assets::plan(&stored, &mappings);
        if !assets::retain_scopes(db, api_key_id, &scopes)
            .await
            .map_err(storage_failed)?
        {
            return Err(TaskError::Cancelled);
        }
        if scopes.is_empty() {
            return Err(TaskError::failed("No volume is mapped to Immich."));
        }
        // Read after the mappings: a mapping edited in between leaves rows that a newer map
        // resolved under an older fingerprint, which the next plan replaces with a full pass.
        let map = inner
            .immich
            .path_map()
            .await
            .map_err(|err| immich_failed(&err))?;
        let client = match inner.immich.client().await {
            Ok(client) => client,
            Err(ImmichError::NotConnected) => return Err(TaskError::Cancelled),
            Err(err) => return Err(immich_failed(&err)),
        };

        let states = assets::states(db).await.map_err(storage_failed)?;
        let now = now_secs();
        let due: Vec<&Scope> = scopes
            .iter()
            .filter(|scope| request.manual || is_due(scope, states.get(&scope.key), now))
            .collect();

        let env = PassEnv {
            db,
            client: &client,
            api_key_id,
            ctx,
            map: &map,
        };
        let libraries = Libraries::read(&client, &due).await;
        let mut failures: Vec<String> = Vec::new();
        let mut halted: Option<String> = None;
        let mut completed = false;
        let mut read = 0_i64;
        for (index, scope) in due.iter().enumerate() {
            if let Some(message) = &halted {
                env.record_failure(scope, message).await;
                failures.push(message.clone());
                continue;
            }
            ctx.progress
                .phase(reading_title(index, due.len()), None, None);
            let outcome = match libraries.check(scope) {
                Ok(()) => pass(&env, scope, states.get(&scope.key), request.full).await,
                Err(err) => Err(err),
            };
            match outcome {
                Ok(count) => {
                    completed = true;
                    read = read.saturating_add(count);
                }
                Err(PassError::Stopped) => return Err(TaskError::Cancelled),
                Err(PassError::Immich { message, fatal }) => {
                    env.record_failure(scope, &message).await;
                    if fatal {
                        halted = Some(message.clone());
                    }
                    failures.push(message);
                }
                Err(PassError::Db(err)) => {
                    tracing::error!(error = %err, scope = %scope.key, "the Immich sync could not store its data");
                    env.record_failure(scope, STORAGE_FAILURE).await;
                    halted = Some(STORAGE_FAILURE.to_owned());
                    failures.push(STORAGE_FAILURE.to_owned());
                }
            }
        }
        if completed && let Err(err) = db.optimize().await {
            tracing::debug!(error = %err, "PRAGMA optimize skipped");
        }
        if let Some(first) = failures.first() {
            return Err(TaskError::failed(match failures.len() - 1 {
                0 => first.clone(),
                more => format!("{first} (and {more} more)"),
            }));
        }
        let summary = assets::status_summary(db, &stored, &mappings)
            .await
            .map_err(storage_failed)?;
        // What a finished task shows: its own title and everything it read, not the last phase.
        ctx.progress.phase(TASK_TITLE, Some(read), None);
        ctx.progress.add(read, 0);
        Ok(serde_json::to_value(&summary).unwrap_or(Value::Null))
    }
}

impl Inner {
    /// Whether the task still waits for its turn or has it, and was not told to stop. The engine
    /// shows a cancelled task as running until its body returns (and a body that was cancelled
    /// before it started never returns anything to wait for), so a request merged into it would
    /// never run.
    async fn is_active(&self, id: TaskId) -> bool {
        !self.tasks.is_stopping(id)
            && matches!(
                self.tasks.get(id).await.map(|task| task.state),
                Ok(TaskState::Queued | TaskState::Running)
            )
    }

    /// The request of task `id`; one that was lost (a task that was told to stop has its entry
    /// replaced by the next request) reads as a manual incremental sync.
    async fn take_pending(&self, id: TaskId) -> Pending {
        let mut pending = self.pending.lock().await;
        match pending.take_if(|waiting| waiting.task_id == id) {
            Some(request) => request,
            None => Pending {
                task_id: id,
                full: false,
                manual: true,
            },
        }
    }

    async fn discard_pending(&self, id: TaskId) {
        let mut pending = self.pending.lock().await;
        pending.take_if(|waiting| waiting.task_id == id);
    }
}

fn immich_failed(err: &ImmichError) -> TaskError {
    TaskError::failed(err.summary())
}

fn storage_failed(err: DbError) -> TaskError {
    tracing::error!(error = %err, "the Immich sync could not store its data");
    TaskError::failed(STORAGE_FAILURE)
}

// ----- deciding what to read -------------------------------------------------------------------

/// How long a scope waits after an attempt before it is tried again.
fn retry_gap_secs(failures: i64) -> i64 {
    let doublings = u32::try_from(failures.clamp(0, 7)).unwrap_or(7);
    (BACKOFF_BASE_SECS << doublings).clamp(MIN_RETRY_GAP_SECS, MAX_BACKOFF_SECS)
}

/// `at` is unknown, or at least `age` seconds ago. A time in the future (the clock went back)
/// counts as old rather than blocking the scope.
fn older_than(at: Option<i64>, age: i64, now: i64) -> bool {
    at.is_none_or(|at| at > now || now - at >= age)
}

fn is_due(scope: &Scope, state: Option<&ScopeState>, now: i64) -> bool {
    let Some(state) = state else {
        return true;
    };
    if let Some(attempt) = state.last_attempt_at
        && (0..retry_gap_secs(state.consecutive_failures)).contains(&now.saturating_sub(attempt))
    {
        return false;
    }
    state.fingerprint != scope.fingerprint
        || older_than(state.last_sync_at, INCREMENTAL_EVERY_SECS, now)
        || older_than(state.last_full_at, FULL_EVERY_SECS, now)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassKind {
    Full,
    Incremental { updated_after_ms: i64 },
}

/// What kind of pass `scope` calls for. Incremental only on top of a complete full pass of the same
/// mappings, with a mark to start from and a full pass that is not a day old.
fn pass_kind(scope: &Scope, state: Option<&ScopeState>, force_full: bool, now: i64) -> PassKind {
    match state {
        Some(state)
            if !force_full
                && state.fingerprint == scope.fingerprint
                && state.high_water_ms > SKEW_MS
                && !older_than(state.last_full_at, FULL_EVERY_SECS, now) =>
        {
            PassKind::Incremental {
                updated_after_ms: state.high_water_ms - SKEW_MS,
            }
        }
        _ => PassKind::Full,
    }
}

// ----- reading one scope -----------------------------------------------------------------------

/// Why a pass did not complete.
enum PassError {
    /// The task was cancelled or the connection is gone: write nothing more.
    Stopped,
    /// Immich failed; `fatal` when every other scope would fail the same way.
    Immich {
        message: String,
        fatal: bool,
    },
    Db(DbError),
}

impl From<ImmichError> for PassError {
    fn from(err: ImmichError) -> Self {
        match err {
            ImmichError::Db(err) => Self::Db(err),
            other => Self::Immich {
                message: other.summary(),
                fatal: other.affects_every_call(),
            },
        }
    }
}

impl From<DbError> for PassError {
    fn from(err: DbError) -> Self {
        Self::Db(err)
    }
}

/// What a pass works with.
struct PassEnv<'a> {
    db: &'a Db,
    client: &'a ImmichClient,
    api_key_id: &'a str,
    ctx: &'a TaskCtx,
    map: &'a PathMap,
}

impl PassEnv<'_> {
    /// Notes the failure of `scope`. Failing to note it is only logged: the run already fails.
    async fn record_failure(&self, scope: &Scope, message: &str) {
        if let Err(err) =
            assets::record_failure(self.db, self.api_key_id, &scope.key, message, now_secs()).await
        {
            tracing::warn!(error = %err, scope = %scope.key, "could not store the failure of the Immich sync");
        }
    }
}

/// The libraries Immich has, read once per run when a library scope is part of it.
enum Libraries {
    NotNeeded,
    Known(Vec<String>),
    Failed { message: String, fatal: bool },
}

impl Libraries {
    async fn read(client: &ImmichClient, due: &[&Scope]) -> Self {
        if !due
            .iter()
            .any(|scope| matches!(scope.kind, ScopeKind::Library { .. }))
        {
            return Self::NotNeeded;
        }
        match client.libraries().await {
            Ok(libraries) => Self::Known(libraries.into_iter().map(|library| library.id).collect()),
            Err(err) => Self::Failed {
                message: err.summary(),
                fatal: err.affects_every_call(),
            },
        }
    }

    /// A mapped library that Immich no longer has fails its scope: a search for it would find
    /// nothing and a full pass would then empty the scope as if every asset had been deleted.
    fn check(&self, scope: &Scope) -> Result<(), PassError> {
        let ScopeKind::Library { id, name } = &scope.kind else {
            return Ok(());
        };
        match self {
            Self::NotNeeded => Ok(()),
            Self::Failed { message, fatal } => Err(PassError::Immich {
                message: message.clone(),
                fatal: *fatal,
            }),
            Self::Known(known) if known.iter().any(|have| have.eq_ignore_ascii_case(id)) => Ok(()),
            Self::Known(_) => {
                let label = if name.is_empty() { id } else { name };
                Err(PassError::Immich {
                    message: format!(
                        "The Immich library \"{label}\" no longer exists; map the volume to another library."
                    ),
                    fatal: false,
                })
            }
        }
    }
}

fn reading_title(index: usize, count: usize) -> String {
    if count > 1 {
        format!("Reading Immich ({}/{count})", index + 1)
    } else {
        "Reading Immich".to_owned()
    }
}

/// The request for the first page of `scope`.
fn search_for(scope: &Scope, updated_after_ms: Option<i64>) -> MetadataSearch {
    let mut search = MetadataSearch {
        updated_after_ms,
        offline: OfflineFilter::Any,
        include_trashed: true,
        order: SortOrder::Asc,
        with_exif: false,
        size: PAGE_SIZE,
        ..MetadataSearch::default()
    };
    match &scope.kind {
        ScopeKind::Library { id, .. } => search.library_id = Some(id.clone()),
        ScopeKind::Prefix { prefix } => {
            search.original_path_prefix = Some(directory_prefix(prefix));
        }
    }
    search
}

/// A trailing slash selects the directory (`/mnt/photo` also starts `/mnt/photos`); the root
/// prefix `/` is one already.
fn directory_prefix(prefix: &str) -> String {
    if prefix.ends_with('/') {
        prefix.to_owned()
    } else {
        format!("{prefix}/")
    }
}

/// What the streams of one pass showed.
#[derive(Default)]
struct Seen {
    /// How many assets the streams returned.
    read: i64,
    /// The newest `updatedAt` read, epoch milliseconds; `0` when there was none.
    updated_ms: i64,
    /// A page token of the flat style of older servers came back.
    flat_paging: bool,
}

/// Whether the server may hide the hidden assets from a search without a visibility: see the
/// module docs.
fn needs_hidden_pass(client: &ImmichClient, seen: &Seen) -> bool {
    seen.flat_paging || client.server_version().is_none() || !client.supports_search_filter()
}

/// Reads `scope` once, completely, and records the result; returns how many assets it read.
async fn pass(
    env: &PassEnv<'_>,
    scope: &Scope,
    state: Option<&ScopeState>,
    force_full: bool,
) -> Result<i64, PassError> {
    let started = Instant::now();
    let planned = pass_kind(scope, state, force_full, now_secs());
    let Some(start) = assets::begin_pass(env.db, env.api_key_id, scope, now_secs()).await? else {
        return Err(PassError::Stopped);
    };
    // The rows of a scope that was just wiped have nothing to be incremental on.
    let (full, updated_after_ms) = match planned {
        PassKind::Incremental { updated_after_ms } if !start.wiped => {
            (false, Some(updated_after_ms))
        }
        _ => (true, None),
    };

    let base = search_for(scope, updated_after_ms);
    let mut seen = Seen::default();
    read_stream(env, scope, start.run, base.clone(), &mut seen).await?;
    if needs_hidden_pass(env.client, &seen) {
        let hidden = MetadataSearch {
            visibility: Some(AssetVisibility::Hidden),
            ..base
        };
        read_stream(env, scope, start.run, hidden, &mut seen).await?;
    }

    env.ctx.progress.set_title("Matching files");
    let reading_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let completion = Completion {
        scope,
        run: start.run,
        full,
        high_water_ms: seen.updated_ms.saturating_sub(reading_ms),
        now: now_secs(),
    };
    if assets::complete(env.db, env.api_key_id, &completion).await? {
        Ok(seen.read)
    } else {
        Err(PassError::Stopped)
    }
}

/// Follows one search from `search` to its last page, storing every page as it comes.
async fn read_stream(
    env: &PassEnv<'_>,
    scope: &Scope,
    run: i64,
    mut search: MetadataSearch,
    seen: &mut Seen,
) -> Result<(), PassError> {
    for _ in 0..MAX_PAGES {
        let page = tokio::select! {
            () = env.ctx.cancel.cancelled() => return Err(PassError::Stopped),
            page = env.client.search_metadata(&search) => page?,
        };
        let rows: Vec<Resolved> = page
            .items
            .iter()
            .map(|asset| assets::resolve(asset, env.map, &scope.volumes))
            .collect();
        seen.updated_ms = rows
            .iter()
            .map(|row| row.record.updated_at_ms)
            .fold(seen.updated_ms, i64::max);
        if !assets::upsert_page(env.db, env.api_key_id, &scope.key, run, &rows).await? {
            return Err(PassError::Stopped);
        }
        let count = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        seen.read = seen.read.saturating_add(count);
        env.ctx.progress.add(count, 0);
        // A page can be short or empty without being the last one: only the end of the token
        // chain ends the stream.
        match page.next {
            Some(token) => {
                seen.flat_paging |= matches!(token, PageToken::Page(_));
                search.after = Some(token);
            }
            None => return Ok(()),
        }
    }
    Err(PassError::Immich {
        message: format!("Immich kept sending pages after {MAX_PAGES} of them; the sync gave up."),
        fatal: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TempDb;

    const NOW: i64 = 1_800_000_000;

    fn library_scope(fingerprint: &str) -> Scope {
        Scope {
            key: "lib-1".to_owned(),
            kind: ScopeKind::Library {
                id: "lib-1".to_owned(),
                name: "Photos".to_owned(),
            },
            volumes: vec![1],
            fingerprint: fingerprint.to_owned(),
        }
    }

    fn prefix_scope(prefix: &str) -> Scope {
        Scope {
            key: format!("path:{prefix}"),
            kind: ScopeKind::Prefix {
                prefix: prefix.to_owned(),
            },
            volumes: vec![1],
            fingerprint: "a".to_owned(),
        }
    }

    /// A scope whose last pass completed 20 minutes ago and whose last full pass is an hour old.
    fn settled(fingerprint: &str) -> ScopeState {
        ScopeState {
            scope: "lib-1".to_owned(),
            fingerprint: fingerprint.to_owned(),
            high_water_ms: 1_799_999_000_000,
            run: 4,
            last_attempt_at: Some(NOW - 20 * 60),
            last_sync_at: Some(NOW - 20 * 60),
            last_full_at: Some(NOW - 3600),
            ..ScopeState::default()
        }
    }

    #[test]
    fn a_scope_without_state_is_due_and_read_in_full() {
        let scope = library_scope("a");
        assert!(is_due(&scope, None, NOW));
        assert_eq!(pass_kind(&scope, None, false, NOW), PassKind::Full);
    }

    #[test]
    fn a_settled_scope_is_due_after_fifteen_minutes_and_read_incrementally() {
        let scope = library_scope("a");
        let mut state = settled("a");
        assert!(is_due(&scope, Some(&state), NOW));
        assert_eq!(
            pass_kind(&scope, Some(&state), false, NOW),
            PassKind::Incremental {
                updated_after_ms: 1_799_999_000_000 - SKEW_MS
            }
        );

        state.last_sync_at = Some(NOW - 14 * 60);
        state.last_attempt_at = Some(NOW - 14 * 60);
        assert!(!is_due(&scope, Some(&state), NOW));
    }

    #[test]
    fn a_forced_full_pass_and_an_old_full_pass_read_everything() {
        let scope = library_scope("a");
        assert_eq!(
            pass_kind(&scope, Some(&settled("a")), true, NOW),
            PassKind::Full
        );

        let recent = ScopeState {
            last_attempt_at: Some(NOW - 300),
            last_sync_at: Some(NOW - 300),
            ..settled("a")
        };
        assert!(
            !is_due(&scope, Some(&recent), NOW),
            "a pass five minutes ago and a full pass today wait"
        );

        let old = ScopeState {
            last_full_at: Some(NOW - FULL_EVERY_SECS),
            ..recent
        };
        assert!(
            is_due(&scope, Some(&old), NOW),
            "a day without a full pass is due"
        );
        assert_eq!(pass_kind(&scope, Some(&old), false, NOW), PassKind::Full);
    }

    #[test]
    fn another_fingerprint_is_due_and_read_in_full() {
        let scope = library_scope("b");
        let state = ScopeState {
            last_attempt_at: Some(NOW - 600),
            last_sync_at: Some(NOW - 60),
            ..settled("a")
        };
        assert!(is_due(&scope, Some(&state), NOW));
        assert_eq!(pass_kind(&scope, Some(&state), false, NOW), PassKind::Full);

        let wiped = ScopeState {
            fingerprint: String::new(),
            high_water_ms: 0,
            ..settled("a")
        };
        assert_eq!(
            pass_kind(&library_scope("a"), Some(&wiped), false, NOW),
            PassKind::Full
        );
    }

    #[test]
    fn a_mark_that_cannot_hold_the_skew_reads_in_full() {
        let scope = library_scope("a");
        for high_water_ms in [0, SKEW_MS] {
            let state = ScopeState {
                high_water_ms,
                ..settled("a")
            };
            assert_eq!(pass_kind(&scope, Some(&state), false, NOW), PassKind::Full);
        }
    }

    #[test]
    fn failures_back_off_between_two_minutes_and_an_hour() {
        let gaps: Vec<i64> = [0, 1, 2, 3, 4, 5, 6, 7, 8, 100, -3]
            .into_iter()
            .map(retry_gap_secs)
            .collect();
        assert_eq!(
            gaps,
            [120, 120, 120, 240, 480, 960, 1920, 3600, 3600, 3600, 120]
        );

        let scope = library_scope("a");
        let failing = |failures: i64, ago: i64| ScopeState {
            last_attempt_at: Some(NOW - ago),
            consecutive_failures: failures,
            ..ScopeState::default()
        };
        assert!(!is_due(&scope, Some(&failing(1, 119)), NOW));
        assert!(is_due(&scope, Some(&failing(1, 120)), NOW));
        assert!(!is_due(&scope, Some(&failing(5, 959)), NOW));
        assert!(is_due(&scope, Some(&failing(5, 960)), NOW));
        assert!(!is_due(&scope, Some(&failing(50, 3599)), NOW));
        assert!(is_due(&scope, Some(&failing(50, 3600)), NOW));
    }

    #[test]
    fn a_cancelled_attempt_waits_two_minutes_before_the_loop_tries_again() {
        let scope = library_scope("a");
        let cancelled = ScopeState {
            last_attempt_at: Some(NOW - 30),
            ..ScopeState::default()
        };
        assert!(!is_due(&scope, Some(&cancelled), NOW));
        assert!(is_due(&scope, Some(&cancelled), NOW + 120));
    }

    #[test]
    fn times_in_the_future_do_not_block_a_scope() {
        let scope = library_scope("a");
        let state = ScopeState {
            fingerprint: "a".to_owned(),
            last_attempt_at: Some(NOW + 3 * 3600),
            last_sync_at: Some(NOW + 3 * 3600),
            last_full_at: Some(NOW + 3 * 3600),
            ..ScopeState::default()
        };
        assert!(is_due(&scope, Some(&state), NOW));
    }

    #[test]
    fn a_library_is_searched_by_id_and_a_prefix_by_its_directory() {
        let search = search_for(&library_scope("a"), Some(5));
        assert_eq!(search.library_id.as_deref(), Some("lib-1"));
        assert_eq!(search.original_path_prefix, None);
        assert_eq!(search.updated_after_ms, Some(5));
        assert!(search.include_trashed);
        assert_eq!(search.offline, OfflineFilter::Any);
        assert_eq!(search.order, SortOrder::Asc);
        assert_eq!(
            (search.size, search.visibility, search.after),
            (PAGE_SIZE, None, None)
        );
        assert!(!search.with_exif);

        let search = search_for(&prefix_scope("/mnt/photo"), None);
        assert_eq!(search.library_id, None);
        assert_eq!(search.original_path_prefix.as_deref(), Some("/mnt/photo/"));
        assert_eq!(search.updated_after_ms, None);
        assert_eq!(directory_prefix("/"), "/");
    }

    #[test]
    fn the_progress_title_counts_the_scopes_only_when_there_are_several() {
        assert_eq!(reading_title(0, 1), "Reading Immich");
        assert_eq!(reading_title(1, 3), "Reading Immich (2/3)");
    }

    #[test]
    fn a_library_immich_no_longer_has_fails_its_scope_only() {
        let scope = library_scope("a");
        let known = Libraries::Known(vec!["LIB-1".to_owned()]);
        assert!(
            known.check(&scope).is_ok(),
            "ids compare without regard to case"
        );
        assert!(Libraries::NotNeeded.check(&scope).is_ok());

        let missing = Libraries::Known(vec!["other".to_owned()]);
        let Err(PassError::Immich { message, fatal }) = missing.check(&scope) else {
            panic!("the missing library must fail its scope");
        };
        assert!(message.contains("\"Photos\""), "{message}");
        assert!(!fatal);

        let prefix = prefix_scope("/mnt/photo");
        assert!(
            missing.check(&prefix).is_ok(),
            "a prefix scope has no library to miss"
        );

        let failed = Libraries::Failed {
            message: "Immich did not answer.".to_owned(),
            fatal: true,
        };
        assert!(matches!(
            failed.check(&scope),
            Err(PassError::Immich { fatal: true, .. })
        ));
        assert!(failed.check(&prefix).is_ok());
    }

    /// The sync with nothing connected: a task that gets its turn ends as cancelled at once.
    async fn unconnected_sync() -> (ImmichSync, TaskEngine, TempDb) {
        let db = Db::open_temp().await.unwrap();
        let tasks = TaskEngine::new(db.handle(), CancellationToken::new())
            .await
            .unwrap();
        let immich = ImmichService::new(db.handle(), db.dir().to_path_buf());
        (
            ImmichSync::new(db.handle(), immich, tasks.clone()),
            tasks,
            db,
        )
    }

    /// The task is cancelled right away (the engine calls it running, its body has not looked at
    /// the cancellation yet) or once its body had time to wait for its turn (the engine still
    /// calls it running until the body returns); the next request must not merge into it.
    #[tokio::test]
    async fn requests_merge_into_the_task_that_waits_and_not_into_one_that_was_cancelled() {
        for settle in [None, Some(Duration::from_millis(100))] {
            let (sync, tasks, _db) = unconnected_sync().await;
            // A sync that is running: the tasks below wait for their turn.
            let turn = sync.inner.running.lock().await;

            let first = sync.enqueue(false, false).await.unwrap();
            assert!(!first.coalesced);
            let second = sync.enqueue(true, true).await.unwrap();
            assert_eq!(
                (second.task_id, second.coalesced),
                (first.task_id, true),
                "a request merges into the task that waits"
            );
            let merged = (*sync.inner.pending.lock().await).unwrap();
            assert_eq!((merged.full, merged.manual), (true, true));

            if let Some(settle) = settle {
                tokio::time::sleep(settle).await;
            }
            tasks.cancel(first.task_id).await.unwrap();
            let third = sync.enqueue(false, false).await.unwrap();
            assert!(!third.coalesced, "settled for {settle:?}");
            assert_ne!(third.task_id, first.task_id);

            drop(turn);
            let cancelled = tasks.wait_finished(first.task_id).await.unwrap();
            assert_eq!(cancelled.state, TaskState::Cancelled);
            let last = tasks.wait_finished(third.task_id).await.unwrap();
            assert_eq!(
                last.state,
                TaskState::Cancelled,
                "without a connection the sync ends as cancelled"
            );
        }
    }
}
