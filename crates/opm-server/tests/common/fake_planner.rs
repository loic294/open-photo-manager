//! A scripted [`Planner`] and a harness that runs the REAL plan engine (HTTP, tasks, storage)
//! around it. The engine tests, and later the tests of a planner that needs a generic plan, use
//! it to see how the engine behaves without a real kind of plan behind it.
//!
//! ```ignore
//! let h = Harness::new().await;
//! let (status, plan) = h.post_plan("cleanup", json!({ "matched": [1, 2], "filler": 3 })).await;
//! assert_eq!(status, StatusCode::CREATED);
//! let task = h.run(plan["id"].as_i64().unwrap(), json!({})).await;
//! assert_eq!(h.planner().processed(), vec![1, 2, 1_000_000, 1_000_001, 1_000_002]);
//! ```
//!
//! # What a plan of the fake holds
//!
//! The rows of a plan live in `fake_plan_items` (a table of its own, `ON DELETE CASCADE`, listed
//! in `child_tables`, written with `bulk::insert_all`), one row per file and side:
//!
//! | `flag` | `side` | Built from build parameter |
//! |---|---|---|
//! | `matched` | source | `matched` (ids of real files) |
//! | `matched` | source | `select`: `{scope, filter}`, the files the real `FileQuery` lists |
//! | `will_delete` | source | `will_delete` (ids of real files) |
//! | `matched` | comparison | `comparison_matched` |
//! | `ghost` | source | `ghosts`: `{id, volume_id, dir, name}`, the planned location |
//! | `filler` | source | `filler: N`: N rows with the ids 1_000_000 and up, for size and chunk tests |
//!
//! Other build parameters: `claim_items` (what `Built.items` reports, and no `check_size` call),
//! `stats`, `warnings`, `hold` (wait for [`FakePlanner::open_build_gate`] or cancellation after
//! the rows were written) and `fail` (`"invalid"` or `"internal"`, after the rows were written).
//!
//! Execute options: `reject` (field name; `prepare_execute` refuses it), `fail` / `skip` (ids),
//! `fail_all`, `bytes` (per done item), `hold` (wait for [`FakePlanner::open_exec_gate`] or
//! cancellation before the first item), `stop_after` (after that many items, wait for the task to
//! be cancelled and return the partial result) and `error` (`"invalid"`, `"internal"` or
//! `"cancelled"`, before the first item). The execution handles the `matched`, `will_delete`
//! and `filler` rows of the source side in insertion order, restricted to `only` when it is set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use futures_util::future::BoxFuture;
use opm_core::dto::{
    FileItem, Filter, Overlay, PlanKind, PlanResult, PlanSide, Scope, SortKey, SortOrder, TaskDto,
    TaskKind,
};
use opm_core::names::normalize_name;
use opm_core::paths::{FileId, PlanId, SelectionId, TaskId, VolumeId};
use opm_server::app;
use opm_server::db::{Db, now_secs};
use opm_server::plans::overlay::{FolderOverlay, GhostPage, GhostView, Window};
use opm_server::plans::{
    BuildContext, Built, ExecContext, ExecutePrep, Plan, PlanClock, PlanDeps, PlanError,
    PlanOptions, PlanService, Planner, ResultBuilder, bulk,
};
use opm_server::query::{FileQuery, QueryScope};
use opm_server::state::AppState;
use opm_server::tasks::{TaskCtx, TaskSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::TestApp;

/// How long a test waits for something that has to happen.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// The table that holds the rows of the plans of the fake.
pub const TABLE: &str = "fake_plan_items";

const COLUMNS: &[&str] = &[
    "plan_id",
    "side",
    "file_id",
    "flag",
    "volume_id",
    "dir",
    "name",
];

/// The id of the first `filler` row.
pub const FILLER_BASE: FileId = 1_000_000;

/// The message of the internal errors the fake raises; it must never reach a client.
pub const SECRET: &str = "secret detail: /volumes/x";

// ---- what the engine did to the planner ----

/// One call the engine made to the planner.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Build {
        plan_id: PlanId,
        params: Value,
    },
    ItemFlags {
        plan_id: PlanId,
        side: PlanSide,
        ids: Vec<FileId>,
    },
    GhostItems {
        plan_id: PlanId,
        side: PlanSide,
        view: GhostView,
        window: Window,
    },
    FolderOverlay {
        plan_id: PlanId,
        side: PlanSide,
        view: GhostView,
        real_children: Vec<String>,
    },
    InvolvedIds {
        plan_id: PlanId,
        side: PlanSide,
    },
    PrepareExecute {
        plan_id: PlanId,
        options: Value,
    },
    Execute {
        plan_id: PlanId,
        options: Value,
        only: Option<SelectionId>,
        /// The files of the `only` selection, sorted.
        only_ids: Option<Vec<FileId>>,
    },
}

/// What the planner has gone through so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub builds_started: usize,
    pub builds_running: usize,
    pub builds_max_running: usize,
    /// Builds that wrote their rows and reached `hold`.
    pub builds_held: usize,
    pub executions_started: usize,
    pub executions_finished: usize,
    pub items_processed: usize,
}

struct Shared {
    calls: Mutex<Vec<Call>>,
    counters: watch::Sender<Counters>,
    build_gate: watch::Sender<bool>,
    exec_gate: watch::Sender<bool>,
    tokens: Mutex<Vec<CancellationToken>>,
    processed: Mutex<Vec<FileId>>,
    folder_overlay: Mutex<FolderOverlay>,
}

impl Shared {
    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    fn count(&self, change: impl FnOnce(&mut Counters)) {
        self.counters.send_modify(change);
    }
}

/// Counts a build as running until the future of the build is dropped.
struct BuildRunning(Arc<Shared>);

impl Drop for BuildRunning {
    fn drop(&mut self) {
        self.0.count(|counters| counters.builds_running -= 1);
    }
}

/// Counts an execution as finished when its future ends or is dropped.
struct ExecutionFinished(Arc<Shared>);

impl Drop for ExecutionFinished {
    fn drop(&mut self) {
        self.0.count(|counters| counters.executions_finished += 1);
    }
}

/// Waits until `gate` is open; `false` when `cancel` fires first.
async fn pass(gate: &watch::Sender<bool>, cancel: &CancellationToken) -> bool {
    let mut open = gate.subscribe();
    let opened = async {
        loop {
            if *open.borrow_and_update() {
                return true;
            }
            if open.changed().await.is_err() {
                return false;
            }
        }
    };
    tokio::select! {
        () = cancel.cancelled() => false,
        opened = opened => opened,
    }
}

fn side_name(side: PlanSide) -> &'static str {
    match side {
        PlanSide::Source => "source",
        PlanSide::Comparison => "comparison",
    }
}

// ---- parameters ----

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BuildParams {
    matched: Vec<FileId>,
    select: Option<Select>,
    will_delete: Vec<FileId>,
    comparison_matched: Vec<FileId>,
    ghosts: Vec<GhostParam>,
    filler: u64,
    claim_items: Option<u64>,
    stats: BTreeMap<String, u64>,
    warnings: Vec<WarningParam>,
    hold: bool,
    fail: Option<String>,
}

/// What a real planner takes as its input: the scope and the filter of the pane (the filter may
/// name a selection), run through the one `FileQuery`.
#[derive(Debug, Deserialize)]
struct Select {
    scope: Scope,
    #[serde(default)]
    filter: Filter,
}

#[derive(Debug, Deserialize)]
struct GhostParam {
    id: FileId,
    volume_id: VolumeId,
    dir: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct WarningParam {
    code: String,
    message: String,
    #[serde(default)]
    count: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExecOptions {
    reject: Option<String>,
    fail: Vec<FileId>,
    skip: Vec<FileId>,
    fail_all: bool,
    bytes: u64,
    hold: bool,
    stop_after: Option<u64>,
    error: Option<String>,
}

fn parse_options(options: &Value) -> Result<ExecOptions, PlanError> {
    serde_json::from_value(options.clone()).map_err(|err| PlanError::invalid(err.to_string()))
}

/// One row of `fake_plan_items` to write.
struct Row {
    side: &'static str,
    file_id: FileId,
    flag: &'static str,
    volume_id: Option<VolumeId>,
    dir: Option<String>,
    name: Option<String>,
}

impl Row {
    fn flagged(side: &'static str, file_id: FileId, flag: &'static str) -> Self {
        Self {
            side,
            file_id,
            flag,
            volume_id: None,
            dir: None,
            name: None,
        }
    }
}

fn sorted(ids: &HashSet<FileId>) -> Vec<FileId> {
    let mut ids: Vec<FileId> = ids.iter().copied().collect();
    ids.sort_unstable();
    ids
}

// ---- the planner ----

/// A [`Planner`] whose plans, execution and failures are scripted by the parameters (see the
/// module docs). Clones share their state; register one clone with the engine and keep another
/// to look at what happened.
#[derive(Clone)]
pub struct FakePlanner {
    kind: PlanKind,
    db: Db,
    shared: Arc<Shared>,
}

impl FakePlanner {
    /// Creates the table of the fake (idempotent) and a planner for `kind`.
    pub async fn install(db: &Db, kind: PlanKind) -> Self {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS fake_plan_items (\
               plan_id INTEGER NOT NULL REFERENCES plans (id) ON DELETE CASCADE, \
               side TEXT NOT NULL CHECK (side IN ('source', 'comparison')), \
               file_id INTEGER NOT NULL, \
               flag TEXT NOT NULL CHECK (flag IN ('matched', 'will_delete', 'ghost', 'filler')), \
               volume_id INTEGER, \
               dir TEXT, \
               name TEXT)",
        )
        .execute(db.write())
        .await
        .unwrap();
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS fake_plan_items_plan \
             ON fake_plan_items (plan_id, side, flag)",
        )
        .execute(db.write())
        .await
        .unwrap();
        Self {
            kind,
            db: db.clone(),
            shared: Arc::new(Shared {
                calls: Mutex::default(),
                counters: watch::channel(Counters::default()).0,
                build_gate: watch::channel(false).0,
                exec_gate: watch::channel(false).0,
                tokens: Mutex::default(),
                processed: Mutex::default(),
                folder_overlay: Mutex::default(),
            }),
        }
    }

    /// Every call the engine made, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.shared.calls.lock().unwrap().clone()
    }

    /// The parameters of every build that started, in order.
    pub fn builds(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Build { params, .. } => Some(params),
                _ => None,
            })
            .collect()
    }

    /// The `Execute` calls, in order.
    pub fn executions(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Execute { .. }))
            .collect()
    }

    pub fn counters(&self) -> Counters {
        self.shared.counters.borrow().clone()
    }

    /// Waits until `ready` holds for the counters; panics after [`PATIENCE`].
    pub async fn wait_for(&self, what: &str, ready: impl Fn(&Counters) -> bool) {
        let mut counters = self.shared.counters.subscribe();
        let reached = tokio::time::timeout(PATIENCE, async {
            loop {
                if ready(&counters.borrow_and_update()) {
                    return true;
                }
                if counters.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await;
        assert!(
            matches!(reached, Ok(true)),
            "timed out waiting for {what}: {:?}",
            self.counters()
        );
    }

    /// The cancellation token of every build that started, in order. A token stays cancelled
    /// after its build is gone, so a test can tell the engine cancelled it.
    pub fn tokens(&self) -> Vec<CancellationToken> {
        self.shared.tokens.lock().unwrap().clone()
    }

    /// The ids of the files the executions handled, in order.
    pub fn processed(&self) -> Vec<FileId> {
        self.shared.processed.lock().unwrap().clone()
    }

    /// Lets the builds that wait on `hold` go on (and every later one pass at once).
    pub fn open_build_gate(&self) {
        self.shared.build_gate.send_replace(true);
    }

    /// Lets the executions that wait on `hold` go on (and every later one pass at once).
    pub fn open_exec_gate(&self) {
        self.shared.exec_gate.send_replace(true);
    }

    /// What `folder_overlay` answers from now on.
    pub fn set_folder_overlay(&self, overlay: FolderOverlay) {
        *self.shared.folder_overlay.lock().unwrap() = overlay;
    }

    async fn build_plan(&self, ctx: &BuildContext, params: Value) -> Result<Built, PlanError> {
        let shared = &self.shared;
        shared.record(Call::Build {
            plan_id: ctx.plan_id,
            params: params.clone(),
        });
        shared.tokens.lock().unwrap().push(ctx.cancel.clone());
        shared.count(|counters| {
            counters.builds_started += 1;
            counters.builds_running += 1;
            counters.builds_max_running = counters.builds_max_running.max(counters.builds_running);
        });
        let _running = BuildRunning(Arc::clone(shared));

        let params: BuildParams =
            serde_json::from_value(params).map_err(|err| PlanError::invalid(err.to_string()))?;
        let mut rows = Vec::new();
        for id in &params.matched {
            rows.push(Row::flagged("source", *id, "matched"));
        }
        if let Some(select) = &params.select {
            let scope = QueryScope::try_from(&select.scope)?;
            for id in FileQuery::new(scope, &select.filter)?.ids(&ctx.db).await? {
                rows.push(Row::flagged("source", id, "matched"));
            }
        }
        for id in &params.will_delete {
            rows.push(Row::flagged("source", *id, "will_delete"));
        }
        for id in &params.comparison_matched {
            rows.push(Row::flagged("comparison", *id, "matched"));
        }
        for ghost in &params.ghosts {
            rows.push(Row {
                volume_id: Some(ghost.volume_id),
                dir: Some(ghost.dir.clone()),
                name: Some(ghost.name.clone()),
                ..Row::flagged("source", ghost.id, "ghost")
            });
        }
        for index in 0..params.filler {
            let id = FILLER_BASE + i64::try_from(index).unwrap();
            rows.push(Row::flagged("source", id, "filler"));
        }
        let total = rows.len() as u64;
        if params.claim_items.is_none() {
            ctx.check_size(total)?;
        }

        let plan_id = ctx.plan_id;
        bulk::insert_all(
            &ctx.db,
            &ctx.cancel,
            TABLE,
            COLUMNS,
            rows,
            move |mut row, item: Row| {
                row.push_bind(plan_id)
                    .push_bind(item.side)
                    .push_bind(item.file_id)
                    .push_bind(item.flag)
                    .push_bind(item.volume_id)
                    .push_bind(item.dir)
                    .push_bind(item.name);
            },
        )
        .await?;

        if params.hold {
            shared.count(|counters| counters.builds_held += 1);
            if !pass(&shared.build_gate, &ctx.cancel).await {
                return Err(PlanError::Cancelled);
            }
        }
        match params.fail.as_deref() {
            Some("invalid") => {
                return Err(PlanError::invalid_field(
                    "fail",
                    "the fake rejects this plan",
                ));
            }
            Some("internal") => return Err(PlanError::Internal(anyhow::anyhow!(SECRET))),
            Some(other) => return Err(PlanError::invalid(format!("unknown failure {other}"))),
            None => {}
        }

        let mut built = Built::new(params.claim_items.unwrap_or(total));
        for (key, value) in &params.stats {
            built = built.stat(key, *value);
        }
        for warning in params.warnings {
            built = built.warning(warning.code, warning.message, warning.count);
        }
        Ok(built)
    }

    async fn flags(
        &self,
        plan: &Plan,
        side: PlanSide,
        ids: &[FileId],
    ) -> Result<HashMap<FileId, Overlay>, PlanError> {
        self.shared.record(Call::ItemFlags {
            plan_id: plan.id,
            side,
            ids: ids.to_vec(),
        });
        let asked: HashSet<FileId> = ids.iter().copied().collect();
        let rows: Vec<(FileId, String)> = sqlx::query_as(
            "SELECT file_id, flag FROM fake_plan_items \
             WHERE plan_id = ?1 AND side = ?2 AND flag IN ('matched', 'will_delete')",
        )
        .bind(plan.id)
        .bind(side_name(side))
        .fetch_all(self.db.read())
        .await?;
        let mut flags: HashMap<FileId, Overlay> = HashMap::new();
        for (id, flag) in rows.into_iter().filter(|(id, _)| asked.contains(id)) {
            let overlay = flags.entry(id).or_default();
            match flag.as_str() {
                "matched" => overlay.matched = true,
                _ => overlay.will_delete = true,
            }
        }
        Ok(flags)
    }

    async fn ghosts(
        &self,
        plan: &Plan,
        side: PlanSide,
        view: &GhostView,
        window: Window,
    ) -> Result<GhostPage, PlanError> {
        self.shared.record(Call::GhostItems {
            plan_id: plan.id,
            side,
            view: view.clone(),
            window,
        });
        let mut rows: Vec<(FileId, VolumeId, String, String)> = sqlx::query_as(
            "SELECT file_id, volume_id, dir, name FROM fake_plan_items \
             WHERE plan_id = ?1 AND side = ?2 AND flag = 'ghost' AND volume_id = ?3 AND dir = ?4",
        )
        .bind(plan.id)
        .bind(side_name(side))
        .bind(view.volume_id)
        .bind(view.dir.as_str())
        .fetch_all(self.db.read())
        .await?;
        // The fake knows no capture time and no size: only a sort by name has an order of its
        // own, the rest is decided by the ties (normalised name, then id, ascending).
        let reversed = view.sort == SortKey::Name && view.order == SortOrder::Desc;
        rows.sort_by(|a, b| {
            let by_name = normalize_name(&a.3).cmp(&normalize_name(&b.3));
            let by_name = if reversed { by_name.reverse() } else { by_name };
            by_name.then(a.0.cmp(&b.0))
        });
        let skip = usize::try_from(window.skip.max(0)).unwrap();
        let take = usize::try_from(window.take.max(0)).unwrap();
        let items = rows
            .iter()
            .skip(skip)
            .take(take)
            .map(|(id, volume_id, dir, name)| FileItem {
                id: *id,
                volume_id: *volume_id,
                dir: dir.clone(),
                name: name.clone(),
                overlay: Overlay {
                    planned: true,
                    ..Overlay::default()
                },
                ..FileItem::default()
            })
            .collect();
        Ok(GhostPage {
            total: i64::try_from(rows.len()).unwrap(),
            items,
        })
    }

    async fn involved(&self, plan: &Plan, side: PlanSide) -> Result<Vec<FileId>, PlanError> {
        self.shared.record(Call::InvolvedIds {
            plan_id: plan.id,
            side,
        });
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT file_id FROM fake_plan_items \
             WHERE plan_id = ?1 AND side = ?2 AND flag IN ('matched', 'will_delete', 'ghost') \
             ORDER BY file_id",
        )
        .bind(plan.id)
        .bind(side_name(side))
        .fetch_all(self.db.read())
        .await?)
    }

    async fn run(
        &self,
        ctx: ExecContext,
        plan: &Plan,
        options: Value,
    ) -> Result<PlanResult, PlanError> {
        let shared = &self.shared;
        let opts = parse_options(&options)?;
        let only_ids = ctx.only_ids().await?;
        shared.record(Call::Execute {
            plan_id: plan.id,
            options,
            only: ctx.only,
            only_ids: only_ids.as_ref().map(sorted),
        });
        shared.count(|counters| counters.executions_started += 1);
        let _finished = ExecutionFinished(Arc::clone(shared));

        let mut result = ResultBuilder::new();
        if opts.hold && !pass(&shared.exec_gate, &ctx.task.cancel).await {
            result.mark_cancelled();
            return Ok(result.finish());
        }
        match opts.error.as_deref() {
            Some("cancelled") => return Err(PlanError::Cancelled),
            Some("internal") => return Err(PlanError::Internal(anyhow::anyhow!(SECRET))),
            Some("invalid") => return Err(PlanError::invalid("the fake gave up")),
            Some(other) => return Err(PlanError::invalid(format!("unknown error {other}"))),
            None => {}
        }

        let ids: Vec<FileId> = sqlx::query_scalar(
            "SELECT file_id FROM fake_plan_items \
             WHERE plan_id = ?1 AND side = 'source' AND flag IN ('matched', 'will_delete', 'filler') \
             ORDER BY rowid",
        )
        .bind(plan.id)
        .fetch_all(self.db.read())
        .await?;
        let ids: Vec<FileId> = ids
            .into_iter()
            .filter(|id| only_ids.as_ref().is_none_or(|only| only.contains(id)))
            .collect();
        let count = ids.len() as u64;
        ctx.declare_totals(count, count.saturating_mul(opts.bytes));

        for (index, id) in ids.iter().enumerate() {
            if opts.stop_after.is_some_and(|stop| index as u64 >= stop) {
                ctx.task.cancel.cancelled().await;
            }
            if ctx.cancelled() {
                result.mark_cancelled();
                break;
            }
            let mut bytes = 0;
            if opts.fail_all || opts.fail.contains(id) {
                result.fail(
                    Some(*id),
                    format!("fake/{id}"),
                    "source_changed",
                    "the file changed",
                );
            } else if opts.skip.contains(id) {
                result.skip();
            } else {
                result.done(opts.bytes);
                bytes = i64::try_from(opts.bytes).unwrap_or(i64::MAX);
            }
            ctx.task.progress.add(1, bytes);
            shared.processed.lock().unwrap().push(*id);
            shared.count(|counters| counters.items_processed += 1);
            tokio::task::yield_now().await;
        }
        Ok(result.finish())
    }
}

impl Planner for FakePlanner {
    fn kind(&self) -> PlanKind {
        self.kind
    }

    fn build<'a>(
        &'a self,
        ctx: &'a BuildContext,
        params: Value,
    ) -> BoxFuture<'a, Result<Built, PlanError>> {
        Box::pin(self.build_plan(ctx, params))
    }

    fn item_flags<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, Result<HashMap<FileId, Overlay>, PlanError>> {
        Box::pin(self.flags(plan, side, ids))
    }

    fn ghost_items<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, Result<GhostPage, PlanError>> {
        Box::pin(self.ghosts(plan, side, view, window))
    }

    fn folder_overlay<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, Result<FolderOverlay, PlanError>> {
        self.shared.record(Call::FolderOverlay {
            plan_id: plan.id,
            side,
            view: view.clone(),
            real_children: real_children.to_vec(),
        });
        let overlay = self.shared.folder_overlay.lock().unwrap().clone();
        Box::pin(async move { Ok(overlay) })
    }

    fn involved_ids<'a>(
        &'a self,
        plan: &'a Plan,
        side: PlanSide,
    ) -> BoxFuture<'a, Result<Vec<FileId>, PlanError>> {
        Box::pin(self.involved(plan, side))
    }

    fn prepare_execute(&self, plan: &Plan, options: &Value) -> Result<ExecutePrep, PlanError> {
        self.shared.record(Call::PrepareExecute {
            plan_id: plan.id,
            options: options.clone(),
        });
        let opts = parse_options(options)?;
        if let Some(field) = opts.reject {
            return Err(PlanError::invalid_field(
                field,
                "the fake rejects this option",
            ));
        }
        Ok(ExecutePrep::new(format!("Run fake plan {}", plan.id))
            .summary("Handled", "item", "items"))
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        plan: &'a Plan,
        options: Value,
    ) -> BoxFuture<'a, Result<PlanResult, PlanError>> {
        Box::pin(self.run(ctx, plan, options))
    }

    fn child_tables(&self) -> &'static [&'static str] {
        &[TABLE]
    }
}

// ---- a clock the test moves ----

/// The clock of the engine in a test: frozen until the test moves it.
#[derive(Clone)]
pub struct TestClock(Arc<AtomicI64>);

impl TestClock {
    pub fn new() -> Self {
        Self(Arc::new(AtomicI64::new(now_secs())))
    }

    pub fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }

    pub fn plan_clock(&self) -> PlanClock {
        let clock = self.clone();
        PlanClock::new(move || clock.now())
    }
}

// ---- the harness ----

/// An engine with the fakes as planners, the clock of the test, and a periodic sweep that never
/// fires (tests call `PlanService::sweep` themselves).
async fn new_engine(
    state: &AppState,
    planners: &[FakePlanner],
    clock: &TestClock,
    tune: impl FnOnce(&mut PlanOptions),
) -> PlanService {
    let mut options = PlanOptions {
        clock: clock.plan_clock(),
        gc_interval: Duration::from_secs(24 * 60 * 60),
        ..PlanOptions::default()
    };
    tune(&mut options);
    PlanService::with_options(
        PlanDeps {
            db: state.db.clone(),
            fsops: state.fsops.clone(),
            settings: state.settings.clone(),
            shutdown: state.shutdown.clone(),
            tasks: state.tasks.clone(),
            volumes: state.volumes.clone(),
        },
        planners
            .iter()
            .map(|planner| Arc::new(planner.clone()) as Arc<dyn Planner>)
            .collect(),
        options,
    )
    .await
    .unwrap()
}

/// Keeps the file operations lane busy with a task that does nothing until it is released, so
/// a plan task submitted meanwhile stays `queued`.
pub struct LaneBlock {
    release: Option<oneshot::Sender<()>>,
    pub task_id: TaskId,
}

impl LaneBlock {
    pub fn release(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

/// The application of `TestApp` with the plan engine rebuilt around [`FakePlanner`]s (one per
/// kind it was given) and a clock the test controls. Everything else is the real application.
pub struct Harness {
    pub app: Arc<TestApp>,
    /// One planner per registered kind, in the order of the kinds.
    pub planners: Vec<FakePlanner>,
    pub clock: TestClock,
}

impl Harness {
    /// A fake planner for the kind `cleanup`, production options except that the periodic sweep
    /// never fires (tests call `PlanService::sweep` themselves).
    pub async fn new() -> Self {
        Self::with_kinds(&[PlanKind::Cleanup], |_| {}).await
    }

    /// Like [`Harness::new`], letting `tune` change the options of the engine.
    pub async fn tuned(tune: impl FnOnce(&mut PlanOptions)) -> Self {
        Self::with_kinds(&[PlanKind::Cleanup], tune).await
    }

    pub async fn with_kinds(kinds: &[PlanKind], tune: impl FnOnce(&mut PlanOptions)) -> Self {
        let mut app = TestApp::new().await;
        app.state.plans.shutdown().await;
        let mut planners = Vec::new();
        for kind in kinds {
            planners.push(FakePlanner::install(&app.state.db, *kind).await);
        }
        let clock = TestClock::new();
        let service = new_engine(&app.state, &planners, &clock, tune).await;
        service.settle().await;
        app.state.plans = service;
        app.router = app::router(app.state.clone());
        Self {
            app: Arc::new(app),
            planners,
            clock,
        }
    }

    /// Another engine over the same database, planners and clock, as a restart of the server
    /// would start it. The application keeps using its own engine; the caller drives this one
    /// and shuts it down.
    pub async fn engine(&self, tune: impl FnOnce(&mut PlanOptions)) -> PlanService {
        new_engine(&self.app.state, &self.planners, &self.clock, tune).await
    }

    /// The planner of the first kind.
    pub fn planner(&self) -> &FakePlanner {
        &self.planners[0]
    }

    pub fn db(&self) -> &Db {
        &self.app.state.db
    }

    /// Waits for the background jobs of the engine (removals, extensions). Call it only while no
    /// plan task is queued or running: the watcher of such a task is one of those jobs. Panics
    /// after [`PATIENCE`] rather than hanging the test.
    pub async fn settle(&self) {
        tokio::time::timeout(PATIENCE, self.app.state.plans.settle())
            .await
            .expect("settle timed out: a plan task is still queued or running");
    }

    // ---- requests ----

    /// `POST /api/plans` with the build parameters `params`.
    pub async fn post_plan(&self, kind: &str, params: Value) -> (StatusCode, Value) {
        self.app
            .call(
                Method::POST,
                "/api/plans",
                Some(json!({ "kind": kind, "params": params })),
            )
            .await
    }

    /// A plan of the kind `cleanup`, which must be built (201).
    pub async fn new_plan(&self, params: Value) -> PlanId {
        let (status, body) = self.post_plan("cleanup", params).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_i64().unwrap()
    }

    pub async fn get_plan(&self, id: PlanId) -> (StatusCode, Value) {
        self.app
            .call(Method::GET, &format!("/api/plans/{id}"), None)
            .await
    }

    pub async fn delete_plan(&self, id: PlanId) -> (StatusCode, Value) {
        self.app
            .call(Method::DELETE, &format!("/api/plans/{id}"), None)
            .await
    }

    /// `POST /api/plans/{id}/execute` with these options.
    pub async fn execute(&self, id: PlanId, options: Value) -> (StatusCode, Value) {
        self.app
            .call(
                Method::POST,
                &format!("/api/plans/{id}/execute"),
                Some(json!({ "options": options })),
            )
            .await
    }

    /// `POST /api/plans/{id}/execute` restricted to the selection `only`.
    pub async fn execute_only(
        &self,
        id: PlanId,
        options: Value,
        only: SelectionId,
    ) -> (StatusCode, Value) {
        self.app
            .call(
                Method::POST,
                &format!("/api/plans/{id}/execute"),
                Some(json!({ "options": options, "only": only })),
            )
            .await
    }

    /// Executes the plan (which must be accepted, 202) and returns the task, not waited for.
    pub async fn start(&self, id: PlanId, options: Value) -> TaskDto {
        let (status, body) = self.execute(id, options).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        serde_json::from_value(body).unwrap()
    }

    /// Waits until the task is over and returns it.
    pub async fn finish(&self, task_id: TaskId) -> TaskDto {
        tokio::time::timeout(PATIENCE, self.app.state.tasks.wait_finished(task_id))
            .await
            .expect("timed out waiting for the task")
            .unwrap()
    }

    /// The task as it is now.
    pub async fn task(&self, task_id: TaskId) -> TaskDto {
        self.app.state.tasks.get(task_id).await.unwrap()
    }

    /// Executes the plan and waits until its task is over; the plan is `executed` by then
    /// unless the task never ran.
    pub async fn run(&self, id: PlanId, options: Value) -> TaskDto {
        let task = self.start(id, options).await;
        self.finish(task.id).await
    }

    /// Polls the plan until it is in `state`, returns its body.
    pub async fn wait_state(&self, id: PlanId, state: &str) -> Value {
        let poll = async {
            loop {
                let (status, body) = self.get_plan(id).await;
                if status == StatusCode::OK && body["state"] == state {
                    return body;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(PATIENCE, poll)
            .await
            .unwrap_or_else(|_| panic!("plan {id} never reached {state}"))
    }

    /// `POST /api/tasks/{id}/cancel`.
    pub async fn cancel_task(&self, task_id: TaskId) {
        let (status, body) = self
            .app
            .call(Method::POST, &format!("/api/tasks/{task_id}/cancel"), None)
            .await;
        assert!(status.is_success(), "{status}: {body}");
    }

    /// Starts `POST /api/plans`, waits until `ready` holds for the counters of the first planner
    /// and drops the request, as a client that gives up does.
    pub async fn abandon_plan(
        &self,
        kind: &str,
        params: Value,
        what: &str,
        ready: impl Fn(&Counters) -> bool,
    ) {
        let request = self.post_plan(kind, params);
        tokio::pin!(request);
        tokio::select! {
            outcome = &mut request => panic!("the request ended before it was abandoned: {outcome:?}"),
            () = self.planner().wait_for(what, ready) => {}
        }
    }

    /// Occupies the lane `plan_execute` tasks run in, until the block is released.
    pub async fn block_lane(&self) -> LaneBlock {
        let (release, released) = oneshot::channel::<()>();
        let (started, running) = oneshot::channel::<()>();
        let task_id = self
            .app
            .state
            .tasks
            .submit(
                TaskSpec::new(TaskKind::FsMove, "Block the lane"),
                move |ctx: TaskCtx| async move {
                    let _ = started.send(());
                    tokio::select! {
                        _ = released => {}
                        () = ctx.cancel.cancelled() => {}
                    }
                    Ok(json!({}))
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(PATIENCE, running)
            .await
            .expect("the blocker never started")
            .unwrap();
        LaneBlock {
            release: Some(release),
            task_id,
        }
    }

    // ---- the database ----

    /// Rows of the `plans` table, whatever their state.
    pub async fn plan_rows(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM plans")
            .fetch_one(self.db().read())
            .await
            .unwrap()
    }

    /// Rows of the fake's table, all plans or one.
    pub async fn fake_rows(&self, plan_id: Option<PlanId>) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM fake_plan_items WHERE ?1 IS NULL OR plan_id = ?1")
            .bind(plan_id)
            .fetch_one(self.db().read())
            .await
            .unwrap()
    }

    /// The raw `state` column of a plan, `None` when the row does not exist.
    pub async fn raw_state(&self, id: PlanId) -> Option<String> {
        sqlx::query_scalar("SELECT state FROM plans WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.db().read())
            .await
            .unwrap()
    }

    /// The raw `expires_at` column of a plan.
    pub async fn raw_expires_at(&self, id: PlanId) -> i64 {
        sqlx::query_scalar("SELECT expires_at FROM plans WHERE id = ?1")
            .bind(id)
            .fetch_one(self.db().read())
            .await
            .unwrap()
    }

    /// Inserts a plan row the way a previous run of the server could have left it.
    pub async fn insert_raw(
        &self,
        kind: &str,
        state: &str,
        created_at: i64,
        expires_at: i64,
    ) -> PlanId {
        sqlx::query_scalar(
            "INSERT INTO plans (kind, state, params_json, created_at, expires_at) \
             VALUES (?1, ?2, '{}', ?3, ?4) RETURNING id",
        )
        .bind(kind)
        .bind(state)
        .bind(created_at)
        .bind(expires_at)
        .fetch_one(self.db().write())
        .await
        .unwrap()
    }
}
