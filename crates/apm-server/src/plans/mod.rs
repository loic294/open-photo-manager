//! Plans: materialized proposals (duplicate resolution, moves, cleanup) that the user reviews as
//! an overlay in the panes before executing them as a task.
//!
//! The generic layer (storage, lifecycle, HTTP, overlay dispatch, execution, results, selections,
//! garbage collection) is owned by todo `building-plan-engine` and exists ONCE. Each kind of plan
//! is a [`Planner`] in its own submodule, owned by its own todo: it knows how to build, show and
//! carry out its plan and nothing else (no HTTP, no task wiring, no expiry, no table of another
//! kind).
//!
//! | Module | Concern |
//! |---|---|
//! | [`planner`] | the [`Planner`] trait and the types it exchanges with the engine: read it first |
//! | [`engine`] | [`PlanService`]: create, get, delete, execute, hand-off, overlay dispatch, sweeps |
//! | [`store`] | every SQL statement on the `plans` table (migration `0200_plans.sql`) |
//! | [`selections`] | every SQL statement on `selections` and `selection_items` |
//! | [`overlay`] | `PlanOverlay`, the seam toward the browse API, implemented by [`PlanService`] |
//! | [`bulk`] | chunked bulk inserts and deletes for the tables of a planner |
//! | [`result`] | [`ResultBuilder`]: the one accounting of an execution (counts, 200 failure details) |
//! | [`cleanup`], [`duplicates`], [`moves`] | the planners |
//!
//! # Lifecycle
//!
//! ```text
//! POST /api/plans                 building (invisible) -> ready        expires 2 h after its last use
//! POST /api/plans/{id}/execute    ready -> executing -> executed       the result stays for 2 h
//! DELETE, `replaces`, expiry      hidden at once, its rows removed in chunks in the background
//! ```
//!
//! # Endpoints
//!
//! | Request | Answer |
//! |---|---|
//! | `POST /api/plans` | 201 `PlanDto`; 400 `invalid_plan_params`; 422 `plan_too_large`; 501 `not_implemented` (no planner for the kind); 503 `plan_build_timeout` or `plan_cancelled` |
//! | `GET /api/plans/{id}` | `PlanDto`; 404 `plan_not_found` (unknown, expired, still building) |
//! | `DELETE /api/plans/{id}` | 204, idempotent; 409 `plan_executing` |
//! | `POST /api/plans/{id}/execute` | 202 `TaskDto`; 400 `invalid_plan_options`; 404 `plan_not_found` or `selection_not_found`; 409 `plan_not_ready` or `plan_executed` |
//! | `POST /api/plans/{id}/selection` | 201 `SelectionDto`; 404; 409 `plan_executed`; 422 `selection_too_large` |
//! | `POST /api/selections` | 201 `SelectionDto`; 400 `invalid_selection`; 422 `selection_too_large` |
//! | `GET /api/selections/{id}` | `SelectionDto`; 404 `selection_not_found` |
//!
//! # Registering a planner
//!
//! 1. Write `impl Planner for YourPlanner` in your own submodule (the module is declared below
//!    already) and your tables in your migration block (`0210`-`0219` duplicates, `0220`-`0229`
//!    move, `0230`-`0239` cleanup).
//! 2. Add ONE line to `fn planners` below, in alphabetical order: `Arc::new(cleanup::CleanupPlanner::new(deps)),`.
//! 3. The engine does everything else: the endpoints, the build slot and timeout, the expiry, the
//!    removal of your rows (list your tables in [`Planner::child_tables`]), the task and the result.

pub mod bulk;
pub mod cleanup;
pub mod duplicates;
pub mod engine;
pub(crate) mod file_identity;
pub mod moves;
pub mod overlay;
pub mod planner;
pub mod result;
pub mod selections;
pub mod store;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub use self::bulk::{BulkWriter, delete_plan_rows, insert_all};
pub use self::engine::{PlanService, Swept};
pub use self::planner::{BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanError, Planner};
pub use self::result::{MAX_FAILURE_DETAILS, ResultBuilder, display_path, summary_title};
use crate::db::{self, Db};
use crate::fsops::FsOps;
use crate::indexer::VolumeRegistry;
use crate::settings::SettingsService;
use crate::tasks::TaskEngine;

/// How long `POST /api/plans` may take, waiting for a build slot included. A build that takes
/// longer is cancelled and answers 503 `plan_build_timeout`.
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(120);

/// Builds that run at the same time; the other requests wait for a slot.
pub const MAX_CONCURRENT_BUILDS: usize = 2;

/// The most items a plan may hold; a bigger one is refused with 422 `plan_too_large`.
pub const MAX_PLAN_ITEMS: u64 = 2_000_000;

/// The most distinct ids a selection may hold; a bigger one is refused with 422
/// `selection_too_large`.
pub const MAX_SELECTION_IDS: usize = 1_000_000;

/// How long a plan lives after its last use (a get, an execute, a browse request that carries it).
pub const PLAN_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a selection lives after its last use.
pub const SELECTION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// A plan or selection is written back at most this often, however often it is used: reads do not
/// write.
pub const EXTEND_THROTTLE: Duration = Duration::from_secs(60);

/// How often the sweep removes expired plans and selections.
pub const GC_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// A `building` row older than this is the leftover of a crashed build and is removed. It must
/// stay above [`BUILD_TIMEOUT`].
pub const BUILDING_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// How long the engine trusts a plan record it loaded for the overlay.
pub const CACHE_TTL: Duration = Duration::from_secs(5);

/// The source of "now" of the engine, in epoch seconds. Tests inject a clock they can move.
#[derive(Clone)]
pub struct PlanClock(Arc<dyn Fn() -> i64 + Send + Sync>);

impl PlanClock {
    /// The wall clock.
    pub fn system() -> Self {
        Self(Arc::new(db::now_secs))
    }

    pub fn new(now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    pub fn now(&self) -> i64 {
        (self.0)()
    }
}

impl fmt::Debug for PlanClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlanClock")
    }
}

/// The limits and timings of the engine; [`PlanOptions::default`] is production.
#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub build_timeout: Duration,
    pub max_items: u64,
    pub max_selection_ids: usize,
    pub max_concurrent_builds: usize,
    pub plan_ttl: Duration,
    pub selection_ttl: Duration,
    pub extend_throttle: Duration,
    pub gc_interval: Duration,
    pub building_max_age: Duration,
    pub cache_ttl: Duration,
    pub clock: PlanClock,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            build_timeout: BUILD_TIMEOUT,
            max_items: MAX_PLAN_ITEMS,
            max_selection_ids: MAX_SELECTION_IDS,
            max_concurrent_builds: MAX_CONCURRENT_BUILDS,
            plan_ttl: PLAN_TTL,
            selection_ttl: SELECTION_TTL,
            extend_throttle: EXTEND_THROTTLE,
            gc_interval: GC_INTERVAL,
            building_max_age: BUILDING_MAX_AGE,
            cache_ttl: CACHE_TTL,
            clock: PlanClock::system(),
        }
    }
}

/// The services the engine and the planners are built from.
#[derive(Clone)]
pub struct PlanDeps {
    pub db: Db,
    pub fsops: FsOps,
    pub settings: SettingsService,
    /// Cancelled by `AppState::begin_shutdown`: the engine's loops and builds stop on it.
    pub shutdown: CancellationToken,
    pub tasks: TaskEngine,
    pub volumes: VolumeRegistry,
}

/// The planners of the application, one registration per line, alphabetical by module.
///
/// Every planner todo adds its own line (and nothing else here); rename `_deps` to `deps` with it.
fn planners(deps: &PlanDeps) -> Vec<Arc<dyn Planner>> {
    vec![
        Arc::new(cleanup::CleanupPlanner::new(deps)),
        Arc::new(duplicates::DuplicatesPlanner::new(deps)),
        // Arc::new(moves::MovePlanner::new(deps)),
    ]
}
