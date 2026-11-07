//! Fixtures shared by the engine tests.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use apm_core::dto::{ServerEvent, TaskDto, TaskKind, TaskState};
use apm_core::paths::TaskId;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::db::{Db, TempDb, now_secs};
use crate::tasks::{
    Clock, TaskCtx, TaskEngine, TaskError, TaskOptions, TaskProgress, TaskSpec, store,
};

/// How long a test waits for something that has to happen.
pub(super) const PATIENCE: Duration = Duration::from_secs(10);

/// How long a test watches for something that must NOT happen. Such a check can only fail when the
/// engine does what it must not, so a slow machine never makes it flaky.
pub(super) const QUIET: Duration = Duration::from_millis(150);

pub(super) type Outcome = Result<Value, TaskError>;

/// The options of the server with every interval shortened, so that tests need not wait for the
/// production ones.
pub(super) fn fast() -> TaskOptions {
    TaskOptions {
        progress_interval: Duration::from_millis(10),
        persist_interval: Duration::from_millis(20),
        fs_change_window: Duration::from_millis(20),
        shutdown_grace: Duration::from_millis(500),
        ..TaskOptions::default()
    }
}

/// A task of the `fs` lane.
pub(super) fn fs() -> TaskSpec {
    TaskSpec::new(TaskKind::FsMove, "Move files")
}

/// A task of the `index` lane.
pub(super) fn index() -> TaskSpec {
    TaskSpec::new(TaskKind::Index, "Index photos")
}

/// A task of the `immich` lane.
pub(super) fn immich() -> TaskSpec {
    TaskSpec::new(TaskKind::ImmichSync, "Sync Immich")
}

/// An engine on a private database.
pub(super) struct Harness {
    // Declared before `db`: the engine goes away first.
    pub(super) engine: TaskEngine,
    /// The app-wide token the engine was given.
    pub(super) shutdown: CancellationToken,
    pub(super) db: TempDb,
}

impl Harness {
    pub(super) async fn new() -> Self {
        Self::with(fast()).await
    }

    pub(super) async fn with(options: TaskOptions) -> Self {
        let db = Db::open_temp().await.unwrap();
        let shutdown = CancellationToken::new();
        let engine = TaskEngine::with_options(db.handle(), shutdown.clone(), options)
            .await
            .unwrap();
        Self {
            engine,
            shutdown,
            db,
        }
    }

    /// Another engine on the same database, as a restarted server builds it.
    pub(super) async fn restarted_with(&self, options: TaskOptions) -> TaskEngine {
        TaskEngine::with_options(self.db.handle(), CancellationToken::new(), options)
            .await
            .unwrap()
    }

    /// A subscription to the event stream.
    pub(super) fn events(&self) -> Events {
        Events::new(&self.engine)
    }

    /// The stored row of a task.
    pub(super) async fn row(&self, id: TaskId) -> Row {
        sqlx::query_as::<_, Row>(
            "SELECT state, lane, title, params_json, error, result_json, items_done, items_total, \
             bytes_done, bytes_total, started_at, finished_at FROM tasks WHERE id = ?",
        )
        .bind(id)
        .fetch_one(self.db.read())
        .await
        .unwrap()
    }

    /// The ids of every stored task, oldest first.
    pub(super) async fn stored_ids(&self) -> Vec<TaskId> {
        sqlx::query_scalar("SELECT id FROM tasks ORDER BY id")
            .fetch_all(self.db.read())
            .await
            .unwrap()
    }

    /// How many rows are `queued` or `running`.
    pub(super) async fn active_rows(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE state IN ('queued', 'running')")
            .fetch_one(self.db.read())
            .await
            .unwrap()
    }

    /// How many rows are in `state`, spelled as the table does (`queued`, `succeeded`, ...).
    pub(super) async fn count_in(&self, state: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE state = ?")
            .bind(state)
            .fetch_one(self.db.read())
            .await
            .unwrap()
    }

    pub(super) async fn finished(&self, id: TaskId) -> TaskDto {
        finished(&self.engine, id).await
    }

    /// Stores a task that was waiting when its server died.
    pub(super) async fn seed_queued(&self, spec: &TaskSpec) -> TaskId {
        store::insert_queued(&self.db, spec, now_secs())
            .await
            .unwrap()
    }

    /// Stores a task that was running when its server died.
    pub(super) async fn seed_running(&self, spec: &TaskSpec) -> TaskId {
        let id = self.seed_queued(spec).await;
        assert!(store::mark_running(&self.db, id, now_secs()).await.unwrap());
        id
    }

    /// Stores a task that succeeded `age_secs` ago, as an earlier run left it.
    pub(super) async fn seed_finished(&self, spec: &TaskSpec, age_secs: i64) -> TaskId {
        let at = now_secs() - age_secs;
        let id = store::insert_queued(&self.db, spec, at).await.unwrap();
        let outcome = store::Finish {
            state: TaskState::Done,
            error: None,
            result: None,
            finished_at: at,
            progress: None,
        };
        assert!(store::finish(&self.db, id, &outcome).await.unwrap());
        id
    }
}

/// A row of the `tasks` table as the engine left it.
#[derive(Debug, sqlx::FromRow)]
pub(super) struct Row {
    pub(super) state: String,
    pub(super) lane: String,
    pub(super) title: String,
    pub(super) params_json: String,
    pub(super) error: Option<String>,
    pub(super) result_json: Option<String>,
    pub(super) items_done: i64,
    pub(super) items_total: Option<i64>,
    pub(super) bytes_done: i64,
    pub(super) bytes_total: Option<i64>,
    pub(super) started_at: Option<i64>,
    pub(super) finished_at: Option<i64>,
}

/// Waits for a task to be over.
pub(super) async fn finished(engine: &TaskEngine, id: TaskId) -> TaskDto {
    within(engine.wait_finished(id)).await.unwrap()
}

/// Submits a task that returns at once.
pub(super) async fn submit_instant(engine: &TaskEngine, spec: TaskSpec) -> TaskId {
    engine
        .submit(spec, |_ctx| async { Ok(json!({})) })
        .await
        .unwrap()
}

pub(super) async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

/// Polls `check` until it returns a value, and panics after [`PATIENCE`].
pub(super) async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = check().await {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A subscription to [`TaskEngine::subscribe`].
pub(super) struct Events {
    receiver: broadcast::Receiver<ServerEvent>,
}

impl Events {
    pub(super) fn new(engine: &TaskEngine) -> Self {
        Self {
            receiver: engine.subscribe(),
        }
    }

    /// Everything that arrived since the last call, without waiting.
    pub(super) fn drain(&mut self) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Closed) => return events,
                Err(TryRecvError::Lagged(missed)) => panic!("the test lagged by {missed} events"),
            }
        }
    }

    /// Waits for the first event `pick` accepts.
    pub(super) async fn wait_for<T>(
        &mut self,
        mut pick: impl FnMut(&ServerEvent) -> Option<T>,
    ) -> T {
        within(async {
            loop {
                let event = self.receiver.recv().await.expect("the stream stays open");
                if let Some(found) = pick(&event) {
                    return found;
                }
            }
        })
        .await
    }

    /// Waits for the first `task` event of `id` that `accept` likes.
    pub(super) async fn wait_for_task(
        &mut self,
        id: TaskId,
        mut accept: impl FnMut(&TaskDto) -> bool,
    ) -> TaskDto {
        self.wait_for(|event| match event {
            ServerEvent::Task { task } if task.id == id && accept(task) => Some(task.clone()),
            _ => None,
        })
        .await
    }
}

/// The `task` events about `id`, in order.
pub(super) fn task_events(events: &[ServerEvent], id: TaskId) -> Vec<TaskDto> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::Task { task } if task.id == id => Some(task.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn states(tasks: &[TaskDto]) -> Vec<TaskState> {
    tasks.iter().map(|task| task.state).collect()
}

/// Holds runners and lets a test release them one by one.
pub(super) struct Gate {
    permits: Arc<Semaphore>,
    announce: mpsc::UnboundedSender<TaskId>,
    started: mpsc::UnboundedReceiver<TaskId>,
}

impl Gate {
    pub(super) fn new() -> Self {
        let (announce, started) = mpsc::unbounded_channel();
        Self {
            permits: Arc::new(Semaphore::new(0)),
            announce,
            started,
        }
    }

    /// A runner that announces its start, then ends `Ok` once [`Gate::release`] gives it a permit
    /// and `Cancelled` when its token fires.
    pub(super) fn runner(
        &self,
    ) -> impl FnOnce(TaskCtx) -> BoxFuture<'static, Outcome> + Send + 'static {
        let (permits, announce) = (Arc::clone(&self.permits), self.announce.clone());
        move |ctx| {
            async move {
                announce.send(ctx.id).ok();
                tokio::select! {
                    () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                    permit = permits.acquire() => {
                        permit.expect("the gate is never closed").forget();
                        Ok(json!({ "task": ctx.id }))
                    }
                }
            }
            .boxed()
        }
    }

    /// Lets `count` held runners finish.
    pub(super) fn release(&self, count: usize) {
        self.permits.add_permits(count);
    }

    /// The id of the next runner that started.
    pub(super) async fn started(&mut self) -> TaskId {
        within(self.started.recv())
            .await
            .expect("the gate keeps its sender")
    }

    /// Fails if a runner starts within [`QUIET`].
    pub(super) async fn assert_nothing_starts(&mut self) {
        if let Ok(started) = tokio::time::timeout(QUIET, self.started.recv()).await {
            panic!("task {started:?} started although its lane is full");
        }
    }
}

/// A clock the test moves by hand, for speeds and ETAs that do not depend on the machine.
pub(super) fn manual_clock() -> (Arc<AtomicU64>, Clock) {
    let now = Arc::new(AtomicU64::new(0));
    let clock = Clock::new({
        let now = Arc::clone(&now);
        move || now.load(Ordering::SeqCst)
    });
    (now, clock)
}

/// A running task whose progress and end the test steers.
pub(super) struct Steered {
    /// The progress handle of the runner.
    pub(super) progress: TaskProgress,
    end: oneshot::Sender<Outcome>,
}

impl Steered {
    /// Makes the runner return `outcome`.
    pub(super) fn end(self, outcome: Outcome) {
        self.end.send(outcome).ok();
    }
}

/// A runner that hands its progress to the test and then waits for [`Steered::end`] (or for the
/// cancellation, which ends it `Cancelled`).
pub(super) fn steered() -> (
    impl FnOnce(TaskCtx) -> BoxFuture<'static, Outcome> + Send + 'static,
    oneshot::Receiver<Steered>,
) {
    let (hand_over, receiver) = oneshot::channel();
    let run = move |ctx: TaskCtx| {
        async move {
            let (end, ended) = oneshot::channel();
            hand_over
                .send(Steered {
                    progress: ctx.progress.clone(),
                    end,
                })
                .ok();
            tokio::select! {
                () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                outcome = ended => outcome.unwrap_or(Err(TaskError::Cancelled)),
            }
        }
        .boxed()
    };
    (run, receiver)
}
