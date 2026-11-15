//! The `Planner` trait: what the plan engine asks of each kind of plan, and the types they
//! exchange.
//!
//! The engine ([`PlanService`](super::PlanService)) owns everything generic: the HTTP endpoints,
//! the `plans` table and its lifecycle, time to live, concurrency, cancellation, the task that
//! executes a plan, the result, the selections. A planner owns only what is specific to its
//! kind, and nothing else: it must never touch HTTP, task wiring, expiry or the tables of another
//! kind.
//!
//! # Writing a planner
//!
//! 1. Declare `pub mod <kind>;` (already there for the three kinds) and implement [`Planner`] on a
//!    struct that holds the services it needs (clone a `Db`, `FsOps`, ... out of
//!    [`PlanDeps`](super::PlanDeps)).
//! 2. Register it on ONE line of `fn planners` in `plans/mod.rs`.
//! 3. Keep the rows of a plan in tables of your own, in your migration block (`02xx_*.sql`):
//!    `plan_id INTEGER NOT NULL REFERENCES plans (id) ON DELETE CASCADE`, an index that starts
//!    with `plan_id`, a plain rowid table (not `WITHOUT ROWID`), and list the table names in
//!    [`Planner::child_tables`] so the engine empties them in chunks before it deletes a plan.
//! 4. Fill them with [`bulk`](super::bulk) and account for the execution with
//!    [`ResultBuilder`](super::ResultBuilder): there is one implementation of each.
//!
//! # Build
//!
//! [`Planner::build`] runs inside the `POST /api/plans` request, after the engine inserted the
//! `building` row. The plan does not exist for clients until `build` returns, so it may take its
//! time (at most [`BUILD_TIMEOUT`](super::BUILD_TIMEOUT)), but it must watch
//! [`BuildContext::cancel`]: the engine cancels it when the client drops the request, when the
//! timeout hits and at shutdown, and then drops the future. Everything a build writes belongs to
//! `ctx.plan_id`, and the engine deletes it all when the build fails or is cancelled. Do not spawn
//! work that outlives the future.
//!
//! A build that needs a consistent view of the index reads it inside ONE read transaction
//! (`ctx.db.read().begin()`: a WAL snapshot) and writes the plan rows through [`bulk`]
//! (the single writer connection); keep that read transaction as short as the build allows,
//! because it pins the WAL. Reads use `ctx.db.read()`, never the writer.
//!
//! Return [`PlanError::Invalid`] for parameters the planner rejects (400), and
//! [`BuildContext::check_size`] while rows pile up: a plan above
//! [`MAX_PLAN_ITEMS`](super::MAX_PLAN_ITEMS) is refused with 422 `plan_too_large`.
//!
//! # Overlay
//!
//! The browse API calls [`Planner::item_flags`], [`Planner::ghost_items`] and
//! [`Planner::folder_overlay`] on every request that carries a plan, so they answer from the
//! planner's own tables and the index, never from the file system, and never take the writer
//! connection. The contract of the results is the one of `PlanOverlay` in `overlay.rs`. A planner
//! that has nothing to show for a `side` (a move plan seen from the comparison side) answers with
//! an empty result, not an error. The engine answers for a plan that was already executed (empty
//! overlays) and for an unknown or expired one (404), so these methods only ever see `ready` and
//! `executing` plans.
//!
//! # Execute: the safety contract
//!
//! [`Planner::execute`] runs inside a `plan_execute` task and may delete or move people's photos.
//! Every executor MUST:
//!
//! * **Re-validate each item immediately before acting.** Never trust what was true when the plan
//!   was built: read the live index row again AND check the file on disk (`FsOps::stat` with an
//!   `Expect` built from the row, or pass the `Expect` to the operation). A file that changed or
//!   vanished is a failure of that item (`source_changed`, `source_missing`), never an action.
//! * **Act through `FsOps` only.** No `std::fs` or `tokio::fs` mutation of a volume path, no
//!   shelling out. `FsOps` is journaled, sidecar-aware and keeps the index and Immich in step.
//! * **Never stop for one bad file.** Record the failure ([`ResultBuilder::fail`](super::ResultBuilder::fail)
//!   or [`fail_fs`](super::ResultBuilder::fail_fs)) and go on with the next item. Only a problem
//!   that makes every further item pointless (the volume went offline before the first one) is an
//!   `Err`: the task then fails and the plan records the message.
//! * **Check [`ExecContext::cancelled`] between items** and stop at once when it is set; return
//!   the partial result with `ResultBuilder::mark_cancelled`. The engine persists whatever you return.
//! * **Report progress in items AND bytes** through `ctx.task.progress`: declare the totals first
//!   ([`ExecContext::declare_totals`]), then add each finished item
//!   ([`ExecContext::item_processed`]). The byte counter of a move or copy is fed by the
//!   `OpContext` already.
//! * **Honour `only` or refuse it.** When [`ExecContext::only`] is set, only the files of that
//!   selection may be touched ([`ExecContext::only_ids`]); a planner that cannot restrict returns
//!   [`PlanError::Invalid`] from `prepare_execute` instead.
//! * **Trash through `ctx.batch`** so everything one run deletes lands in one trash batch and
//!   restores together.
//!
//! A per-item failure never fails the task: the task ends `succeeded` and `PlanResult::failed`
//! counts it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::http::StatusCode;
use futures_util::future::BoxFuture;
use opm_core::dto::{Overlay, PlanKind, PlanResult, PlanSide, PlanState, PlanWarning};
use opm_core::paths::{FileId, PlanId, SelectionId};
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::overlay::{FolderOverlay, GhostPage, GhostView, Window, plan_not_found};
use super::selections;
use crate::db::{Db, DbError};
use crate::error::AppError;
use crate::fsops::{FsOpError, FsOps, OpContext, TrashBatch};
use crate::indexer::VolumeRegistry;
use crate::query::QueryError;
use crate::settings::Settings;
use crate::tasks::{TaskCtx, TaskError};

/// What the engine asks of one kind of plan. See the module docs for the rules.
pub trait Planner: Send + Sync {
    /// The kind this planner serves; the registry is keyed by it.
    fn kind(&self) -> PlanKind;

    /// Parses and validates `params` (the planner owns its typed parameters), fills its own
    /// tables for `ctx.plan_id` and returns the item count, the statistics and the warnings.
    fn build<'a>(
        &'a self,
        ctx: &'a BuildContext,
        params: Value,
    ) -> BoxFuture<'a, Result<Built, PlanError>>;

    /// Flags of the REAL files `ids` seen from `side`: `matched` and `will_delete`. A file
    /// without flags is absent from the map; `planned` is never set on a real file.
    fn item_flags<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, Result<HashMap<FileId, Overlay>, PlanError>>;

    /// The ghost files directly in `view.dir`, seen from `side`: sorted, windowed and counted as
    /// `PlanOverlay::ghost_items` documents.
    fn ghost_items<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, Result<GhostPage, PlanError>>;

    /// The plan's view of the folder list of `view.dir`, as `PlanOverlay::folder_overlay`
    /// documents.
    fn folder_overlay<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, Result<FolderOverlay, PlanError>>;

    /// Ids of the REAL files the plan involves on `side` (files with a match, files that will
    /// move, files that will be deleted): the hand-off set of `POST /api/plans/{id}/selection`.
    /// Each id once; the engine deduplicates anyway.
    fn involved_ids<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
    ) -> BoxFuture<'a, Result<Vec<FileId>, PlanError>>;

    /// Cheap, synchronous validation of the execute options, so bad input is a 400 BEFORE a task
    /// exists and the plan stays `ready`. Also names the task and words its summary. Do not read
    /// the database or the disk here.
    fn prepare_execute(&self, plan: &Plan, options: &Value) -> Result<ExecutePrep, PlanError>;

    /// Carries the plan out. See the safety contract in the module docs. Return the result even
    /// when some items failed or the run was cancelled; return `Err` only for a failure of the
    /// whole run. `Err(PlanError::Cancelled)` is a cancellation with an empty result.
    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        plan: &'a Plan,
        options: Value,
    ) -> BoxFuture<'a, Result<PlanResult, PlanError>>;

    /// The tables of this planner that hold rows of a plan (`plan_id` column, rowid tables). The
    /// engine empties them in chunks ([`bulk::delete_plan_rows`](super::bulk::delete_plan_rows))
    /// before it deletes the plan row, so removing a plan of millions of rows never holds the
    /// writer for long. Listing none is correct, only slower for a big plan (the cascade deletes
    /// everything in one statement).
    fn child_tables(&self) -> &'static [&'static str] {
        &[]
    }
}

/// A plan as the engine loaded it, handed to every [`Planner`] method.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub id: PlanId,
    pub kind: PlanKind,
    /// The parameters the plan was built from, as the planner accepted them (a JSON object).
    pub params: Value,
    pub stats: BTreeMap<String, u64>,
    pub warnings: Vec<PlanWarning>,
    /// Never `building`: a plan is only handed out once it is built.
    pub state: PlanState,
    pub created_at: i64,
    pub expires_at: i64,
}

impl Plan {
    /// The parameters as the planner's own type. They were accepted by `build`, so a failure
    /// here is an internal error.
    pub fn params<T: DeserializeOwned>(&self) -> Result<T, PlanError> {
        serde_json::from_value(self.params.clone()).map_err(|err| {
            PlanError::Internal(anyhow::anyhow!(
                "the stored parameters of plan {} do not parse: {err}",
                self.id
            ))
        })
    }
}

/// What a planner receives to build a plan.
pub struct BuildContext {
    /// The plan being built: write rows for this id.
    pub plan_id: PlanId,
    pub db: Db,
    /// The settings as they were when the build started.
    pub settings: Arc<Settings>,
    pub volumes: VolumeRegistry,
    /// Cancelled when the client gave up, the build timed out or the server stops.
    pub cancel: CancellationToken,
    /// Epoch seconds, from the engine's clock.
    pub now: i64,
    /// The most items a plan may hold, see [`BuildContext::check_size`].
    pub max_items: u64,
}

impl BuildContext {
    /// `Err(PlanError::Cancelled)` once the build was cancelled. Call it between units of work.
    pub fn check_cancelled(&self) -> Result<(), PlanError> {
        if self.cancel.is_cancelled() {
            Err(PlanError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// `Err(PlanError::TooLarge)` when `items` exceeds the limit: call it as rows accumulate so
    /// an oversized plan is refused early instead of after it was written.
    pub fn check_size(&self, items: u64) -> Result<(), PlanError> {
        if items > self.max_items {
            Err(PlanError::TooLarge {
                items,
                max: self.max_items,
            })
        } else {
            Ok(())
        }
    }
}

/// What a build reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Built {
    /// How many items the plan holds; the engine refuses a plan above the limit and logs it.
    pub items: u64,
    /// Counters under stable snake_case keys the planner documents.
    pub stats: BTreeMap<String, u64>,
    pub warnings: Vec<PlanWarning>,
}

impl Built {
    pub fn new(items: u64) -> Self {
        Self {
            items,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn stat(mut self, key: &str, value: u64) -> Self {
        self.stats.insert(key.to_owned(), value);
        self
    }

    /// Adds a warning; one that concerns no item (`count == 0`) is dropped.
    #[must_use]
    pub fn warning(
        mut self,
        code: impl Into<String>,
        message: impl Into<String>,
        count: u64,
    ) -> Self {
        if count > 0 {
            self.warnings.push(PlanWarning {
                code: code.into(),
                message: message.into(),
                count,
            });
        }
        self
    }
}

/// What `prepare_execute` returns: the words of the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutePrep {
    /// The title of the queued task ("Delete 2,988 files").
    pub title: String,
    /// Past tense, for the summary the task shows once it ended ("Deleted").
    pub verb: String,
    pub singular: String,
    pub plural: String,
}

impl ExecutePrep {
    /// A task called `title`, summarised as "Processed 12 of 14 items".
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            verb: "Processed".to_owned(),
            singular: "item".to_owned(),
            plural: "items".to_owned(),
        }
    }

    /// Words the summary: "{verb} 2,988 of 3,000 {plural}, 12 failed".
    #[must_use]
    pub fn summary(
        mut self,
        verb: impl Into<String>,
        singular: impl Into<String>,
        plural: impl Into<String>,
    ) -> Self {
        self.verb = verb.into();
        self.singular = singular.into();
        self.plural = plural.into();
        self
    }
}

/// What a planner receives to execute a plan. Owned and cheap to clone: move it into whatever
/// the execution spawns.
#[derive(Clone)]
pub struct ExecContext {
    pub plan_id: PlanId,
    pub db: Db,
    pub fsops: FsOps,
    pub settings: Arc<Settings>,
    pub soft_exclusions: opm_core::exclusions::Exclusions,
    pub volumes: VolumeRegistry,
    /// The `plan_execute` task: its cancellation token and its progress.
    pub task: TaskCtx,
    /// Pass it to every `FsOps` call; it is wired to the task's cancellation and byte progress.
    pub op: OpContext,
    /// The ONE trash batch of this execution (clone of `op.batch()`).
    pub batch: TrashBatch,
    /// Restricts the execution to the files of this selection.
    pub only: Option<SelectionId>,
}

impl ExecContext {
    /// Whether the user cancelled the task or the server is stopping.
    pub fn cancelled(&self) -> bool {
        self.task.cancel.is_cancelled()
    }

    /// Declares the work to do, so the task shows a bar, a speed and an ETA.
    pub fn declare_totals(&self, items: u64, bytes: u64) {
        self.task.progress.set_totals(
            Some(i64::try_from(items).unwrap_or(i64::MAX)),
            Some(i64::try_from(bytes).unwrap_or(i64::MAX)),
        );
    }

    /// Counts one finished item (done, skipped or failed) in the task's progress.
    pub fn item_processed(&self) {
        self.task.progress.add(1, 0);
    }

    /// The ids of the files of the `only` selection, `None` when the run is not restricted.
    pub async fn only_ids(&self) -> Result<Option<HashSet<FileId>>, PlanError> {
        let checkpoint = crate::tasks::Checkpoint::new(&self.db, self.task.id).await?;
        if let Some(ids) = checkpoint.load("only-ids").await? {
            return Ok(ids);
        }
        let params: Option<String> =
            sqlx::query_scalar("SELECT params_json FROM tasks WHERE id = ? AND resumable = 1")
                .bind(self.task.id)
                .fetch_optional(self.db.read())
                .await?;
        if let Some(params) = params {
            let params: Value = serde_json::from_str(&params).map_err(anyhow::Error::from)?;
            if let Some(ids) = params.get("only_ids") {
                return serde_json::from_value(ids.clone())
                    .map_err(|error| PlanError::Internal(error.into()));
            }
        }
        let ids = match self.only {
            Some(selection) => Some(
                selections::file_ids(&self.db, selection)
                    .await?
                    .into_iter()
                    .collect(),
            ),
            None => None,
        };
        checkpoint.save("only-ids", &ids).await?;
        Ok(ids)
    }
}

/// Why a plan operation failed. Converts to [`AppError`] for the HTTP layer and to [`TaskError`]
/// for the task runner; a planner usually just uses `?`.
#[derive(Debug, Error)]
pub enum PlanError {
    /// No such plan (it expired, was deleted or never existed).
    #[error("plan {0} not found")]
    NotFound(PlanId),
    /// The parameters or options are not acceptable. `field` names the offending one.
    #[error("{message}")]
    Invalid {
        field: Option<String>,
        message: String,
    },
    /// The plan would hold more than the allowed number of items.
    #[error("the plan holds {items} items, more than the {max} allowed")]
    TooLarge { items: u64, max: u64 },
    /// The build or the execution was cancelled.
    #[error("the plan was cancelled")]
    Cancelled,
    /// The request clashes with the state of the plan: a stable `code` and a client-safe message.
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Db(#[from] DbError),
    /// An API error from a service the planner called; it passes through unchanged.
    #[error(transparent)]
    App(#[from] AppError),
    /// Anything else. Logged, never sent to a client.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl PlanError {
    /// The parameters are not acceptable.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            field: None,
            message: message.into(),
        }
    }

    /// The parameter `field` is not acceptable.
    pub fn invalid_field(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            field: Some(field.into()),
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for PlanError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(err))
    }
}

impl From<FsOpError> for PlanError {
    fn from(err: FsOpError) -> Self {
        Self::App(err.into())
    }
}

impl From<PlanError> for AppError {
    /// 404 `plan_not_found`, 400 `invalid_plan_params`, 422 `plan_too_large`, 503 `plan_cancelled`,
    /// 409 for a conflict with the state; internal causes are logged and never sent.
    fn from(err: PlanError) -> Self {
        match err {
            PlanError::NotFound(id) => plan_not_found(id),
            PlanError::Invalid { field, message } => {
                let message = match field {
                    Some(field) => format!("{field}: {message}"),
                    None => message,
                };
                Self::bad_request("invalid_plan_params", message)
            }
            PlanError::TooLarge { items, max } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "plan_too_large",
                format!("the plan would hold {items} items, more than the {max} allowed"),
            ),
            PlanError::Cancelled => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_cancelled",
                "the plan was cancelled",
            ),
            PlanError::Conflict { code, message } => Self::conflict(code, message),
            PlanError::Query(err) => err.into(),
            PlanError::Db(err) => err.into(),
            PlanError::App(err) => err,
            PlanError::Internal(err) => err.into(),
        }
    }
}

impl From<PlanError> for TaskError {
    fn from(err: PlanError) -> Self {
        match err {
            PlanError::Cancelled => Self::Cancelled,
            other => AppError::from(other).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[test]
    fn errors_map_to_the_documented_statuses() {
        let cases = [
            (
                PlanError::NotFound(4),
                StatusCode::NOT_FOUND,
                "plan_not_found",
            ),
            (
                PlanError::invalid("nope"),
                StatusCode::BAD_REQUEST,
                "invalid_plan_params",
            ),
            (
                PlanError::TooLarge { items: 5, max: 3 },
                StatusCode::UNPROCESSABLE_ENTITY,
                "plan_too_large",
            ),
            (
                PlanError::Cancelled,
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_cancelled",
            ),
            (
                PlanError::conflict("plan_executed", "already executed"),
                StatusCode::CONFLICT,
                "plan_executed",
            ),
        ];
        for (err, status, code) in cases {
            let app = AppError::from(err);
            assert_eq!((app.status(), app.code()), (status, code));
        }
    }

    #[test]
    fn an_invalid_field_is_named_in_the_message() {
        let app = AppError::from(PlanError::invalid_field("pattern", "is empty"));
        assert_eq!(app.message(), "pattern: is empty");
    }

    #[test]
    fn internal_causes_never_reach_the_client() {
        let app = AppError::from(PlanError::Internal(anyhow::anyhow!("secret detail")));
        assert_eq!(app.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!app.message().contains("secret"));
    }

    #[test]
    fn an_app_error_passes_through_unchanged() {
        let app = AppError::from(PlanError::from(AppError::forbidden("denied", "no")));
        assert_eq!(
            (app.status(), app.code()),
            (StatusCode::FORBIDDEN, "denied")
        );
    }

    #[test]
    fn a_cancelled_plan_cancels_the_task() {
        assert!(matches!(
            TaskError::from(PlanError::Cancelled),
            TaskError::Cancelled
        ));
        match TaskError::from(PlanError::invalid("bad")) {
            TaskError::Failed(message) => assert_eq!(message, "bad"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn built_drops_warnings_without_items() {
        let built = Built::new(3)
            .stat("pairs", 2)
            .warning("skipped", "two files were left out", 2)
            .warning("never", "nothing happened", 0);
        assert_eq!(built.items, 3);
        assert_eq!(built.stats.get("pairs"), Some(&2));
        assert_eq!(built.warnings.len(), 1);
        assert_eq!(built.warnings[0].code, "skipped");
    }

    #[test]
    fn plan_params_parse_into_the_planners_type() {
        #[derive(Deserialize)]
        struct Params {
            mode: String,
        }
        let mut plan = Plan {
            id: 1,
            kind: PlanKind::Move,
            params: json!({"mode": "copy"}),
            stats: BTreeMap::new(),
            warnings: Vec::new(),
            state: PlanState::Ready,
            created_at: 0,
            expires_at: 0,
        };
        assert_eq!(plan.params::<Params>().unwrap().mode, "copy");
        plan.params = json!({"mode": 3});
        assert!(matches!(
            plan.params::<Params>(),
            Err(PlanError::Internal(_))
        ));
    }

    #[tokio::test]
    async fn the_build_context_checks_size_and_cancellation() {
        let temp = crate::db::Db::open_temp().await.unwrap();
        let volumes = VolumeRegistry::load(temp.handle(), temp.dir().join("volumes"))
            .await
            .unwrap();
        let ctx = BuildContext {
            plan_id: 1,
            db: temp.handle(),
            settings: Arc::new(Settings::defaults_for(1, 1)),
            volumes,
            cancel: CancellationToken::new(),
            now: 0,
            max_items: 10,
        };
        assert!(ctx.check_size(10).is_ok());
        assert!(matches!(
            ctx.check_size(11),
            Err(PlanError::TooLarge { items: 11, max: 10 })
        ));
        assert!(ctx.check_cancelled().is_ok());
        ctx.cancel.cancel();
        assert!(matches!(ctx.check_cancelled(), Err(PlanError::Cancelled)));
    }
}
