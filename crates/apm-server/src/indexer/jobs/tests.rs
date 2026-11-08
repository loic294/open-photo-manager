//! Tests of the jobs: coalescing, the task a job becomes, the progress mapping and the scheduler.
//!
//! The crate's Tokio has no `test-util` feature, so the clock is never paused: the scheduler runs
//! with a millisecond "minute", waits poll against a deadline, and a check that something does NOT
//! happen can only fail when the code does what it must not. The index lane is kept busy by a task
//! that waits for a signal (`Fixture::block_lane`), and a job is kept `running` by holding the lock
//! of its volume.

use std::fs;
use std::future::Future;
use std::path::PathBuf;

use apm_core::dto::Scope;
use tokio::sync::{OwnedMutexGuard, oneshot};

use super::*;
use crate::config::Config;
use crate::db::{Db, TempDb};
use crate::indexer::test_support::touch;
use crate::indexer::{FakeExtractor, VolumeRegistry};
use crate::settings::SettingsService;
use crate::tasks::ProgressView;

/// How long a test waits for something that has to happen.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long a test watches for something that must NOT happen.
const QUIET: Duration = Duration::from_millis(250);

fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap()
}

/// The files of `path` only.
fn dir(volume: VolumeId, path: &str) -> ScanTarget {
    ScanTarget::dir(volume, rel(path))
}

/// `path` and everything below it.
fn subtree(volume: VolumeId, path: &str) -> ScanTarget {
    ScanTarget::subtree(volume, rel(path))
}

fn plain(targets: Vec<ScanTarget>) -> ScanRequest {
    ScanRequest {
        targets,
        ..ScanRequest::default()
    }
}

fn forced(targets: Vec<ScanTarget>) -> ScanRequest {
    ScanRequest {
        targets,
        force: true,
        ..ScanRequest::default()
    }
}

fn confirming(targets: Vec<ScanTarget>) -> ScanRequest {
    ScanRequest {
        targets,
        confirm_removals: true,
        ..ScanRequest::default()
    }
}

fn scope(volume_id: Option<VolumeId>, dir: &str, recursive: bool) -> Scope {
    Scope {
        volume_id,
        dir: dir.to_owned(),
        recursive,
    }
}

fn rescan(targets: Vec<Scope>) -> RescanRequest {
    RescanRequest {
        targets,
        ..RescanRequest::default()
    }
}

fn fast_scheduler() -> SchedulerOptions {
    SchedulerOptions {
        startup_delay: Duration::from_millis(30),
        minute: Duration::from_millis(10),
    }
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .expect("timed out waiting")
}

/// Polls `check` until it returns a value, and panics after [`PATIENCE`].
async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
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

/// A task that is not one of the jobs and holds the index lane until it is released.
struct Blocker {
    id: TaskId,
    release: oneshot::Sender<()>,
}

impl Blocker {
    fn release(self) {
        self.release.send(()).ok();
    }
}

/// The jobs over a private database and volumes directory.
struct Fixture {
    jobs: IndexJobs,
    tasks: TaskEngine,
    indexer: Indexer,
    volumes: VolumeRegistry,
    settings: SettingsService,
    shutdown: CancellationToken,
    volumes_dir: PathBuf,
    // Last: the directory is removed after everything that uses it.
    temp: TempDb,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::with_options(SchedulerOptions::default()).await
    }

    async fn with_options(options: SchedulerOptions) -> Self {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let config = Config::for_tests(temp.dir());
        fs::create_dir_all(&config.volumes_dir).unwrap();
        let settings = SettingsService::load(db.clone(), &config).await.unwrap();
        let volumes = VolumeRegistry::load(db.clone(), config.volumes_dir.clone())
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let tasks = TaskEngine::new(db.clone(), shutdown.clone()).await.unwrap();
        let indexer = Indexer::new(db.clone(), settings.clone(), volumes.clone())
            .with_extractor(Arc::new(FakeExtractor::unavailable("tests")));
        let fsops = FsOps::new(db, volumes.clone(), settings.clone(), indexer.clone());
        let jobs = IndexJobs::with_options(
            indexer.clone(),
            tasks.clone(),
            fsops,
            shutdown.clone(),
            options,
        );
        Self {
            jobs,
            tasks,
            indexer,
            volumes,
            settings,
            shutdown,
            volumes_dir: config.volumes_dir,
            temp,
        }
    }

    /// A volume directory called `name`, known to the registry and online. Ids follow the order of
    /// creation.
    async fn volume(&self, name: &str) -> VolumeId {
        fs::create_dir_all(self.volumes_dir.join(name)).unwrap();
        self.volumes.refresh().await.unwrap();
        self.volumes
            .online()
            .into_iter()
            .find(|volume| volume.name == name)
            .unwrap()
            .id
    }

    fn put(&self, volume: &str, path: &str, bytes: &[u8]) {
        touch(&self.volumes_dir.join(volume), path, bytes);
    }

    async fn set_rescan_minutes(&self, minutes: u32) {
        let mut settings = (*self.settings.get()).clone();
        settings.index.rescan_interval_minutes = minutes;
        self.settings.update(settings).await.unwrap();
    }

    /// Holds the scans of `volume` back: a job for it is `running` and waits for the lock.
    async fn hold(&self, volume: VolumeId) -> OwnedMutexGuard<()> {
        self.indexer.volume_lock(volume).lock_owned().await
    }

    /// Occupies the index lane with a task that is not one of the jobs.
    async fn block_lane(&self) -> Blocker {
        let (release, released) = oneshot::channel::<()>();
        let id = self
            .tasks
            .submit(
                TaskSpec::new(TaskKind::Index, "A task in the way"),
                move |ctx| async move {
                    tokio::select! {
                        _ = released => Ok(json!({})),
                        () = ctx.cancel.cancelled() => Err(TaskError::Cancelled),
                    }
                },
            )
            .await
            .unwrap();
        self.wait_state(id, TaskState::Running).await;
        Blocker { id, release }
    }

    /// A running task that ignores a cancellation until it is released. It is returned once its
    /// body runs: a task cancelled before that never runs its body and ends at once.
    async fn stubborn_task(&self) -> (TaskId, oneshot::Sender<()>) {
        let (release, released) = oneshot::channel::<()>();
        let (started, running) = oneshot::channel::<()>();
        let id = self
            .tasks
            .submit(
                TaskSpec::new(TaskKind::Index, "A task that stops late"),
                move |ctx| async move {
                    started.send(()).ok();
                    released.await.ok();
                    if ctx.cancel.is_cancelled() {
                        Err(TaskError::Cancelled)
                    } else {
                        Ok(json!({}))
                    }
                },
            )
            .await
            .unwrap();
        within(running).await.unwrap();
        (id, release)
    }

    async fn wait_state(&self, id: TaskId, state: TaskState) {
        eventually("a task state", || async move {
            (self.tasks.get(id).await.unwrap().state == state).then_some(())
        })
        .await;
    }

    /// The runner has taken the request: nothing can be merged into the job any more.
    async fn wait_taken(&self, id: TaskId) {
        eventually("the runner to take the request", || async move {
            let jobs = self.jobs.inner.jobs.lock().await;
            jobs.iter()
                .any(|job| job.task_id == id && job.taken)
                .then_some(())
        })
        .await;
        self.wait_state(id, TaskState::Running).await;
    }

    async fn wait_for_tasks(&self, count: i64) {
        eventually("index tasks", || async move {
            (self.task_count().await >= count).then_some(())
        })
        .await;
    }

    async fn wait_for_origin(&self, origin: &str) {
        eventually("a task of an origin", || async move {
            self.origins()
                .await
                .iter()
                .any(|seen| seen == origin)
                .then_some(())
        })
        .await;
    }

    async fn wait_for_files(&self, count: usize) {
        eventually("files to be indexed", || async move {
            (self.indexed().await.len() >= count).then_some(())
        })
        .await;
    }

    async fn finished(&self, id: TaskId) -> TaskDto {
        within(self.tasks.wait_finished(id)).await.unwrap()
    }

    /// The request of a job as it stands now.
    async fn request_of(&self, id: TaskId) -> ScanRequest {
        let jobs = self.jobs.inner.jobs.lock().await;
        jobs.iter()
            .find(|job| job.task_id == id)
            .map(|job| job.request.clone())
            .expect("the job is registered")
    }

    async fn registered(&self) -> usize {
        self.jobs.inner.jobs.lock().await.len()
    }

    /// The index tasks that are queued or running, by id.
    async fn active_ids(&self) -> Vec<TaskId> {
        let mut ids: Vec<TaskId> = self
            .jobs
            .active_tasks()
            .await
            .unwrap()
            .into_iter()
            .map(|task| task.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// How many index tasks exist, whatever their state.
    async fn task_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE kind = 'index'")
            .fetch_one(self.temp.read())
            .await
            .unwrap()
    }

    /// The `origin` parameter of every index task that has one, oldest first.
    async fn origins(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT json_extract(params_json, '$.origin') FROM tasks \
             WHERE kind = 'index' AND json_extract(params_json, '$.origin') IS NOT NULL \
             ORDER BY id",
        )
        .fetch_all(self.temp.read())
        .await
        .unwrap()
    }

    async fn first_task(&self) -> TaskId {
        sqlx::query_scalar("SELECT MIN(id) FROM tasks WHERE kind = 'index'")
            .fetch_one(self.temp.read())
            .await
            .unwrap()
    }

    async fn params_of(&self, id: TaskId) -> Value {
        let text: String = sqlx::query_scalar("SELECT params_json FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_one(self.temp.read())
            .await
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    async fn result_of(&self, id: TaskId) -> Value {
        let text: Option<String> = sqlx::query_scalar("SELECT result_json FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_one(self.temp.read())
            .await
            .unwrap();
        serde_json::from_str(&text.expect("the task stored a result")).unwrap()
    }

    /// Every indexed file as `volume/dir/name`, sorted.
    async fn indexed(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT v.name || '/' || CASE WHEN f.dir_path = '' THEN '' ELSE f.dir_path || '/' END \
             || f.name FROM files f JOIN volumes v ON v.id = f.volume_id ORDER BY 1",
        )
        .fetch_all(self.temp.read())
        .await
        .unwrap()
    }

    /// Stops the scheduler through the shutdown token and waits for it to end.
    async fn stop(&self, scheduler: JoinHandle<()>) {
        self.shutdown.cancel();
        within(scheduler).await.unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers without state

#[test]
fn normalising_groups_by_volume_and_drops_what_is_covered() {
    let targets = [
        dir(2, "x"),
        subtree(1, "a/b"),
        subtree(1, "a"),
        dir(1, "a/c"),
        dir(2, "x"),
        dir(1, "z"),
    ];
    assert_eq!(
        normalise_targets(&targets),
        vec![subtree(1, "a"), dir(1, "z"), dir(2, "x")]
    );
    assert!(normalise_targets(&[]).is_empty(), "every volume stays so");
}

#[test]
fn a_recursive_scope_covers_the_same_directory_without_recursion() {
    let targets = [dir(1, "a"), subtree(1, "a"), dir(1, "a")];
    assert_eq!(normalise_targets(&targets), vec![subtree(1, "a")]);
}

#[test]
fn a_union_with_every_volume_is_every_volume() {
    assert!(union(&[], &[dir(1, "a")]).is_empty());
    assert!(union(&[dir(1, "a")], &[]).is_empty());
    assert!(union(&[], &[]).is_empty());
    assert_eq!(
        union(&[dir(1, "a")], &[subtree(1, "a"), dir(2, "b")]),
        vec![subtree(1, "a"), dir(2, "b")]
    );
}

#[test]
fn identical_requests_have_the_same_targets_and_flags() {
    let base = plain(vec![dir(1, "a")]);
    assert!(same_request(&base, &plain(vec![dir(1, "a")])));
    assert!(!same_request(&base, &plain(vec![subtree(1, "a")])));
    assert!(!same_request(&base, &plain(vec![dir(1, "b")])));
    assert!(!same_request(&base, &forced(vec![dir(1, "a")])));
    assert!(!same_request(&base, &confirming(vec![dir(1, "a")])));
    assert!(same_request(&plain(vec![]), &plain(vec![])));
}

#[test]
fn a_volume_that_appears_has_come_online() {
    let set = |ids: &[VolumeId]| ids.iter().copied().collect::<HashSet<_>>();
    assert!(came_online(&set(&[1]), &set(&[1, 2])));
    assert!(came_online(&set(&[]), &set(&[3])));
    assert!(came_online(&set(&[1, 2]), &set(&[1, 3])));
    assert!(!came_online(&set(&[1, 2]), &set(&[1])), "gone offline");
    assert!(!came_online(&set(&[1]), &set(&[1])));
    assert!(!came_online(&set(&[]), &set(&[])));
}

#[test]
fn the_task_params_list_the_request_without_absolute_paths() {
    let request = ScanRequest {
        targets: vec![subtree(2, "2024/trip"), ScanTarget::volume(1)],
        force: true,
        confirm_removals: false,
    };
    assert_eq!(
        params(&request, IndexOrigin::User),
        json!({
            "origin": "user",
            "targets": [
                {"volume_id": 2, "dir": "2024/trip", "recursive": true},
                {"volume_id": 1, "dir": "", "recursive": true},
            ],
            "force": true,
            "confirm_removals": false,
        })
    );
    let origins = [
        (IndexOrigin::User, "user"),
        (IndexOrigin::Startup, "startup"),
        (IndexOrigin::Periodic, "periodic"),
        (IndexOrigin::Watcher, "watcher"),
    ];
    for (origin, name) in origins {
        assert_eq!(params(&plain(vec![]), origin)["origin"], json!(name));
    }
}

// ---------------------------------------------------------------------------------------------
// The progress of a run on the progress of its task

fn report(
    phase: IndexPhase,
    done: u64,
    total: Option<u64>,
    bytes: u64,
    bytes_total: Option<u64>,
) -> IndexProgress {
    IndexProgress {
        phase,
        items_done: done,
        items_total: total,
        bytes_done: bytes,
        bytes_total,
        detail: None,
    }
}

fn shown(view: &ProgressView) -> (&str, i64, Option<i64>, i64, Option<i64>) {
    (
        view.title.as_str(),
        view.items_done,
        view.items_total,
        view.bytes_done,
        view.bytes_total,
    )
}

#[test]
fn a_new_phase_restarts_the_counters_under_its_own_title() {
    let progress = TaskProgress::new("Indexing");
    let sink = TaskProgressSink::new(progress.clone());

    sink.report(report(IndexPhase::Walk, 5, Some(10), 500, Some(1000)));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: scanning folders", 5, Some(10), 500, Some(1000))
    );
    sink.report(report(IndexPhase::Walk, 8, Some(10), 800, Some(1000)));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: scanning folders", 8, Some(10), 800, Some(1000))
    );

    sink.report(report(IndexPhase::Extract, 1, Some(4), 0, None));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: reading metadata", 1, Some(4), 0, None)
    );
    sink.report(report(IndexPhase::Hash, 0, None, 0, None));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: hashing files", 0, None, 0, None)
    );
    sink.report(report(IndexPhase::Thumbs, 2, Some(2), 0, None));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: creating thumbnails", 2, Some(2), 0, None)
    );
}

#[test]
fn the_counters_never_run_backwards_and_follow_changing_totals() {
    let progress = TaskProgress::new("Indexing");
    let sink = TaskProgressSink::new(progress.clone());

    sink.report(report(IndexPhase::Walk, 10, Some(20), 100, Some(200)));
    sink.report(report(IndexPhase::Walk, 7, Some(20), 70, Some(200)));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: scanning folders", 10, Some(20), 100, Some(200)),
        "a late report is ignored"
    );

    sink.report(report(IndexPhase::Walk, 12, Some(30), 120, Some(300)));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: scanning folders", 12, Some(30), 120, Some(300)),
        "the estimate grew: the totals follow, the counters keep what was done"
    );

    sink.report(report(IndexPhase::Walk, 11, None, 110, None));
    assert_eq!(
        shown(&progress.view()),
        ("Indexing: scanning folders", 12, None, 120, None),
        "an unknown total is shown as unknown"
    );
}

#[test]
fn huge_reports_saturate_instead_of_wrapping() {
    let progress = TaskProgress::new("Indexing");
    let sink = TaskProgressSink::new(progress.clone());
    for _ in 0..2 {
        sink.report(report(
            IndexPhase::Walk,
            u64::MAX,
            Some(u64::MAX),
            u64::MAX,
            Some(u64::MAX),
        ));
    }
    assert_eq!(
        shown(&progress.view()),
        (
            "Indexing: scanning folders",
            i64::MAX,
            Some(i64::MAX),
            i64::MAX,
            Some(i64::MAX)
        )
    );
}

// ---------------------------------------------------------------------------------------------
// From a request of a client to a request of the indexer

fn failure(err: &AppError) -> (u16, &str) {
    (err.status().as_u16(), err.code())
}

#[tokio::test]
async fn an_empty_rescan_request_means_every_volume_and_keeps_its_flags() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    let request = fx
        .jobs
        .scan_request(&RescanRequest {
            targets: Vec::new(),
            force: true,
            confirm_removals: true,
        })
        .unwrap();
    assert!(request.targets.is_empty());
    assert!(request.force && request.confirm_removals);

    let request = fx.jobs.scan_request(&RescanRequest::default()).unwrap();
    assert!(request.targets.is_empty() && !request.force && !request.confirm_removals);
}

#[tokio::test]
async fn scopes_become_targets_with_normalised_directories() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let request = fx
        .jobs
        .scan_request(&rescan(vec![
            scope(Some(photos), "2024//Trip/", true),
            scope(Some(photos), "", false),
            scope(Some(photos), "not/there/yet", true),
        ]))
        .unwrap();
    assert_eq!(
        request.targets,
        vec![
            subtree(photos, "2024/Trip"),
            dir(photos, ""),
            subtree(photos, "not/there/yet"),
        ],
        "a directory that does not exist on the disk is allowed: the scan removes its entries"
    );
}

#[tokio::test]
async fn a_scope_without_a_volume_stands_for_every_online_volume() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let scans = fx.volume("scans").await;
    fx.volume("archive").await;
    fs::remove_dir(fx.volumes_dir.join("archive")).unwrap();
    fx.volumes.refresh().await.unwrap();

    let all = fx
        .jobs
        .scan_request(&rescan(vec![scope(None, "", true)]))
        .unwrap();
    assert_eq!(
        all.targets,
        vec![ScanTarget::volume(photos), ScanTarget::volume(scans)],
        "the offline volume is left out"
    );

    let shallow = fx
        .jobs
        .scan_request(&rescan(vec![scope(None, "", false)]))
        .unwrap();
    assert_eq!(shallow.targets, vec![dir(photos, ""), dir(scans, "")]);
}

#[tokio::test]
async fn a_scope_without_a_volume_needs_a_volume_to_be_online() {
    let fx = Fixture::new().await;
    let err = fx
        .jobs
        .scan_request(&rescan(vec![scope(None, "", true)]))
        .unwrap_err();
    assert_eq!(failure(&err), (409, "volume_offline"));
}

#[tokio::test]
async fn a_directory_without_a_volume_is_a_bad_request() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    let err = fx
        .jobs
        .scan_request(&rescan(vec![scope(None, "2024", true)]))
        .unwrap_err();
    assert_eq!(failure(&err), (400, "bad_request"));
}

#[tokio::test]
async fn a_volume_that_does_not_exist_is_not_found() {
    let fx = Fixture::new().await;
    let err = fx
        .jobs
        .scan_request(&rescan(vec![scope(Some(4242), "", true)]))
        .unwrap_err();
    assert_eq!(failure(&err), (404, "not_found"));
}

#[tokio::test]
async fn a_volume_that_is_offline_is_a_conflict() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fs::remove_dir(fx.volumes_dir.join("photos")).unwrap();
    fx.volumes.refresh().await.unwrap();
    let err = fx
        .jobs
        .scan_request(&rescan(vec![scope(Some(photos), "", true)]))
        .unwrap_err();
    assert_eq!(failure(&err), (409, "volume_offline"));
}

#[tokio::test]
async fn a_directory_that_is_not_relative_is_an_invalid_path() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    for bad in ["../outside", "/absolute", "a/../../b", "nul\0byte"] {
        let err = fx
            .jobs
            .scan_request(&rescan(vec![scope(Some(photos), bad, true)]))
            .unwrap_err();
        assert_eq!(failure(&err), (400, "invalid_path"), "{bad:?}");
    }
}

#[tokio::test]
async fn one_bad_scope_rejects_the_whole_request() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let err = fx
        .jobs
        .scan_request(&rescan(vec![
            scope(Some(photos), "ok", true),
            scope(Some(4242), "", true),
        ]))
        .unwrap_err();
    assert_eq!(failure(&err), (404, "not_found"));
}

// ---------------------------------------------------------------------------------------------
// Titles

#[test]
fn titles_say_what_the_job_does() {
    let user = IndexOrigin::User;
    let folders = |count: usize| -> Vec<ScanTarget> {
        (0..count).map(|n| subtree(1, &format!("d{n}"))).collect()
    };

    assert_eq!(title(&plain(vec![]), user), "Indexing");
    assert_eq!(title(&plain(vec![]), IndexOrigin::Startup), "Indexing");
    assert_eq!(title(&plain(vec![]), IndexOrigin::Periodic), "Indexing");
    assert_eq!(title(&forced(vec![]), user), "Re-indexing all assets");
    assert_eq!(title(&plain(folders(1)), user), "Indexing 1 folder");
    assert_eq!(title(&plain(folders(2)), user), "Indexing 2 folders");
    assert_eq!(
        title(&plain(vec![ScanTarget::volume(1)]), user),
        "Indexing 1 folder",
        "a whole volume is one folder to the panel"
    );
    assert_eq!(title(&forced(folders(1)), user), "Re-indexing 1 folder");
    assert_eq!(title(&forced(folders(3)), user), "Re-indexing 3 folders");
    assert_eq!(
        title(&plain(folders(2)), IndexOrigin::Watcher),
        "Indexing changes"
    );
    assert_eq!(
        title(&forced(vec![]), IndexOrigin::Watcher),
        "Indexing changes"
    );
}

// ---------------------------------------------------------------------------------------------
// Coalescing

#[tokio::test]
async fn a_request_merges_into_the_queued_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let scans = fx.volume("scans").await;
    let blocker = fx.block_lane().await;

    let first = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!first.coalesced);
    let second = fx
        .jobs
        .submit(plain(vec![subtree(scans, "b")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    assert_eq!(
        second,
        SubmitOutcome {
            task_id: first.task_id,
            coalesced: true
        }
    );

    assert_eq!(
        fx.request_of(first.task_id).await.targets,
        vec![dir(photos, "a"), subtree(scans, "b")],
        "the targets of both requests"
    );
    assert_eq!(
        fx.active_ids().await,
        vec![blocker.id, first.task_id],
        "no second task was queued"
    );
    blocker.release();
}

#[tokio::test]
async fn a_target_that_another_covers_is_dropped_when_requests_merge() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let _blocker = fx.block_lane().await;

    let job = fx
        .jobs
        .submit(plain(vec![dir(photos, "a/b")]), IndexOrigin::User)
        .await
        .unwrap()
        .task_id;
    fx.jobs
        .submit(plain(vec![subtree(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(
        fx.request_of(job).await.targets,
        vec![subtree(photos, "a")],
        "a/b is inside a recursive a"
    );

    fx.jobs
        .submit(
            plain(vec![dir(photos, "a/c"), dir(photos, "z")]),
            IndexOrigin::Watcher,
        )
        .await
        .unwrap();
    assert_eq!(
        fx.request_of(job).await.targets,
        vec![subtree(photos, "a"), dir(photos, "z")]
    );
}

#[tokio::test]
async fn a_request_for_every_volume_absorbs_the_others() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let scans = fx.volume("scans").await;
    let _blocker = fx.block_lane().await;

    let job = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap()
        .task_id;
    let all = fx
        .jobs
        .submit(plain(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(all.task_id, job);
    assert!(all.coalesced);
    assert!(fx.request_of(job).await.targets.is_empty(), "every volume");

    fx.jobs
        .submit(plain(vec![dir(scans, "x")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    assert!(
        fx.request_of(job).await.targets.is_empty(),
        "and it stays every volume"
    );
}

#[tokio::test]
async fn a_forced_request_gets_a_job_of_its_own() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let _blocker = fx.block_lane().await;

    let queued = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    let forced_job = fx
        .jobs
        .submit(forced(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!forced_job.coalesced);
    assert_ne!(forced_job.task_id, queued.task_id);
    assert!(fx.request_of(forced_job.task_id).await.force);

    let later = fx
        .jobs
        .submit(plain(vec![dir(photos, "b")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    assert_eq!(
        later.task_id, queued.task_id,
        "a plain request goes to the plain job"
    );
    assert_eq!(
        fx.request_of(forced_job.task_id).await.targets,
        vec![dir(photos, "a")],
        "the forced job did not grow"
    );

    let again = fx
        .jobs
        .submit(forced(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(
        again,
        SubmitOutcome {
            task_id: forced_job.task_id,
            coalesced: true
        },
        "the same forced request is answered by the job that has it"
    );
    let other = fx
        .jobs
        .submit(forced(vec![dir(photos, "b")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(
        !other.coalesced,
        "a different forced request is a job of its own"
    );
}

#[tokio::test]
async fn only_a_user_can_confirm_removals() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let _blocker = fx.block_lane().await;

    let watcher = fx
        .jobs
        .submit(confirming(vec![dir(photos, "a")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    assert!(!fx.request_of(watcher.task_id).await.confirm_removals);
    assert_eq!(
        fx.params_of(watcher.task_id).await["confirm_removals"],
        json!(false)
    );

    let user = fx
        .jobs
        .submit(confirming(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!user.coalesced, "a confirming request is never merged");
    assert_ne!(user.task_id, watcher.task_id);
    assert!(fx.request_of(user.task_id).await.confirm_removals);
    assert_eq!(
        fx.params_of(user.task_id).await["confirm_removals"],
        json!(true)
    );

    let more = fx
        .jobs
        .submit(plain(vec![dir(photos, "b")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    assert_eq!(
        more.task_id, watcher.task_id,
        "nothing merges into a confirming job"
    );
    assert_eq!(
        fx.request_of(user.task_id).await.targets,
        vec![dir(photos, "a")]
    );
}

#[tokio::test]
async fn an_identical_user_request_is_answered_by_the_queued_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let _blocker = fx.block_lane().await;

    let first = fx
        .jobs
        .submit(forced(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    let again = fx
        .jobs
        .submit(forced(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(
        again,
        SubmitOutcome {
            task_id: first.task_id,
            coalesced: true
        }
    );
    let other = fx
        .jobs
        .submit(forced(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!other.coalesced, "another set of targets is another job");
    assert_eq!(fx.registered().await, 2);
}

#[tokio::test]
async fn an_identical_user_request_is_answered_by_the_running_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let held = fx.hold(photos).await;

    let first = fx
        .jobs
        .submit(forced(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    fx.wait_taken(first.task_id).await;

    let again = fx
        .jobs
        .submit(forced(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(
        again,
        SubmitOutcome {
            task_id: first.task_id,
            coalesced: true
        },
        "a double click starts one job"
    );
    assert_eq!(fx.active_ids().await, vec![first.task_id]);

    drop(held);
    assert_eq!(fx.finished(first.task_id).await.state, TaskState::Done);
}

#[tokio::test]
async fn a_running_job_absorbs_nothing() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let held = fx.hold(photos).await;

    let running = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    fx.wait_taken(running.task_id).await;

    let next = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!next.coalesced);
    assert_ne!(next.task_id, running.task_id);
    assert_eq!(
        fx.request_of(running.task_id).await.targets,
        vec![ScanTarget::volume(photos)],
        "the running job keeps what it took"
    );
    assert_eq!(
        fx.request_of(next.task_id).await.targets,
        vec![dir(photos, "a")]
    );

    drop(held);
    fx.finished(running.task_id).await;
    fx.finished(next.task_id).await;
}

#[tokio::test]
async fn the_watcher_never_joins_a_running_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let held = fx.hold(photos).await;

    let running = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    fx.wait_taken(running.task_id).await;

    let changed = fx
        .jobs
        .submit(
            plain(vec![ScanTarget::volume(photos)]),
            IndexOrigin::Watcher,
        )
        .await
        .unwrap();
    assert!(
        !changed.coalesced,
        "what changed while a scan ran must be scanned again"
    );
    assert_ne!(changed.task_id, running.task_id);

    drop(held);
    fx.finished(running.task_id).await;
    fx.finished(changed.task_id).await;
}

#[tokio::test]
async fn scheduled_scans_are_dropped_while_the_index_is_busy() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    let blocker = fx.block_lane().await;

    for origin in [IndexOrigin::Startup, IndexOrigin::Periodic] {
        let outcome = fx
            .jobs
            .submit(ScanRequest::default(), origin)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SubmitOutcome {
                task_id: blocker.id,
                coalesced: true
            },
            "{origin:?}"
        );
    }
    assert_eq!(fx.task_count().await, 1, "nothing was queued");

    let queued = fx
        .jobs
        .submit(plain(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!queued.coalesced);
    let outcome = fx
        .jobs
        .submit(ScanRequest::default(), IndexOrigin::Periodic)
        .await
        .unwrap();
    assert!(outcome.coalesced, "a queued task counts as well");
    assert_eq!(fx.task_count().await, 2);
    blocker.release();
}

#[tokio::test]
async fn a_scheduled_scan_queues_when_the_index_is_idle() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;

    let first = fx
        .jobs
        .submit(ScanRequest::default(), IndexOrigin::Periodic)
        .await
        .unwrap();
    assert!(!first.coalesced);
    assert_eq!(fx.finished(first.task_id).await.state, TaskState::Done);

    let next = fx
        .jobs
        .submit(ScanRequest::default(), IndexOrigin::Startup)
        .await
        .unwrap();
    assert!(!next.coalesced, "the first job is over");
    assert_ne!(next.task_id, first.task_id);
    assert_eq!(fx.finished(next.task_id).await.state, TaskState::Done);
    assert_eq!(fx.origins().await, ["periodic", "startup"]);
}

#[tokio::test]
async fn a_cancelled_job_is_not_a_target_for_merging() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let _blocker = fx.block_lane().await;

    let queued = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    let cancelled = fx.tasks.cancel(queued.task_id).await.unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);

    let next = fx
        .jobs
        .submit(plain(vec![dir(photos, "b")]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!next.coalesced);
    assert_ne!(next.task_id, queued.task_id);
    assert_eq!(
        fx.request_of(next.task_id).await.targets,
        vec![dir(photos, "b")]
    );
    assert_eq!(
        fx.registered().await,
        1,
        "the cancelled job left the registry"
    );
}

#[tokio::test]
async fn only_a_task_that_waits_or_runs_and_was_not_told_to_stop_is_open() {
    let fx = Fixture::new().await;
    let blocker = fx.block_lane().await;
    let queued = fx
        .jobs
        .submit(plain(vec![]), IndexOrigin::User)
        .await
        .unwrap()
        .task_id;
    assert!(fx.jobs.is_open(blocker.id).await, "running");
    assert!(fx.jobs.is_open(queued).await, "queued");
    assert!(!fx.jobs.is_open(4242).await, "unknown");

    fx.tasks.cancel(queued).await.unwrap();
    assert!(!fx.jobs.is_open(queued).await, "cancelled while it waited");

    let id = blocker.id;
    blocker.release();
    fx.finished(id).await;
    assert!(
        !fx.jobs.is_open(id).await,
        "an ended task reads as not stopping"
    );

    let (stubborn, release) = fx.stubborn_task().await;
    assert!(fx.jobs.is_open(stubborn).await);
    fx.tasks.cancel(stubborn).await.unwrap();
    assert!(
        !fx.jobs.is_open(stubborn).await,
        "told to stop, though it still reads as running"
    );
    release.send(()).ok();
    fx.finished(stubborn).await;
}

#[tokio::test]
async fn a_task_that_is_stopping_does_not_hold_back_a_scheduled_scan() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    let (stubborn, release) = fx.stubborn_task().await;
    fx.tasks.cancel(stubborn).await.unwrap();
    assert!(fx.tasks.is_stopping(stubborn));
    assert_eq!(
        fx.tasks.get(stubborn).await.unwrap().state,
        TaskState::Running,
        "it still occupies the lane"
    );

    let outcome = fx
        .jobs
        .submit(ScanRequest::default(), IndexOrigin::Periodic)
        .await
        .unwrap();
    assert!(
        !outcome.coalesced,
        "a task that was told to stop does not count"
    );

    release.send(()).ok();
    assert_eq!(fx.finished(stubborn).await.state, TaskState::Cancelled);
    assert_eq!(fx.finished(outcome.task_id).await.state, TaskState::Done);
}

#[tokio::test]
async fn a_job_that_ended_is_forgotten() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;

    let first = fx
        .jobs
        .submit(forced(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    fx.finished(first.task_id).await;

    let second = fx
        .jobs
        .submit(forced(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(
        !second.coalesced,
        "the identical job is over: this is a new request"
    );
    assert_ne!(second.task_id, first.task_id);
    assert_eq!(fx.registered().await, 1);
    fx.finished(second.task_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_share_one_queued_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    let blocker = fx.block_lane().await;

    let submissions: Vec<_> = (0..20)
        .map(|index| {
            let jobs = fx.jobs.clone();
            tokio::spawn(async move {
                let request = plain(vec![dir(photos, &format!("d{index:02}"))]);
                jobs.submit(request, IndexOrigin::Watcher).await.unwrap()
            })
        })
        .collect();
    let mut outcomes = Vec::new();
    for submission in submissions {
        outcomes.push(submission.await.unwrap());
    }

    let ids: HashSet<TaskId> = outcomes.iter().map(|outcome| outcome.task_id).collect();
    assert_eq!(ids.len(), 1, "one job for all of them");
    assert_eq!(
        outcomes.iter().filter(|outcome| !outcome.coalesced).count(),
        1
    );
    let job = outcomes[0].task_id;
    assert_eq!(fx.request_of(job).await.targets.len(), 20);
    assert_eq!(fx.active_ids().await, vec![blocker.id, job]);
    blocker.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_is_never_lost_between_a_merge_and_the_start_of_the_job() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    for index in 0..30 {
        fx.put("photos", &format!("d{index:02}/f.jpg"), b"x");
    }
    let blocker = fx.block_lane().await;

    let first = fx
        .jobs
        .submit(plain(vec![dir(photos, "d00")]), IndexOrigin::Watcher)
        .await
        .unwrap();
    // The lane opens while the other requests arrive: each one either joins the job before it
    // starts or becomes a job of its own, and none of them may be dropped.
    let submissions: Vec<_> = (1..30)
        .map(|index| {
            let jobs = fx.jobs.clone();
            tokio::spawn(async move {
                let request = plain(vec![dir(photos, &format!("d{index:02}"))]);
                jobs.submit(request, IndexOrigin::Watcher).await.unwrap()
            })
        })
        .collect();
    blocker.release();
    let mut ids = HashSet::from([first.task_id]);
    for submission in submissions {
        ids.insert(submission.await.unwrap().task_id);
    }
    for id in ids {
        assert_eq!(fx.finished(id).await.state, TaskState::Done);
    }

    let indexed = fx.indexed().await;
    assert_eq!(
        indexed.len(),
        30,
        "every directory was indexed: {indexed:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The task

#[tokio::test]
async fn a_job_indexes_only_its_targets_and_stores_the_summary() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fx.put("photos", "a/one.jpg", b"1");
    fx.put("photos", "b/two.jpg", b"22");

    let outcome = fx
        .jobs
        .submit(plain(vec![subtree(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    let task = fx.finished(outcome.task_id).await;

    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.kind, TaskKind::Index);
    assert_eq!(task.title, "Indexing 1 folder");
    assert_eq!(fx.indexed().await, ["photos/a/one.jpg"]);
    let summary = fx.result_of(task.id).await;
    assert_eq!(summary["added"], json!(1));
    assert_eq!(summary["removed"], json!(0));
    assert_eq!(
        fx.params_of(task.id).await,
        json!({
            "origin": "user",
            "targets": [{"volume_id": photos, "dir": "a", "recursive": true}],
            "force": false,
            "confirm_removals": false,
        })
    );
}

#[tokio::test]
async fn a_job_for_every_volume_indexes_all_of_them() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    fx.volume("scans").await;
    fx.put("photos", "a.jpg", b"1");
    fx.put("scans", "deep/b.jpg", b"2");

    let outcome = fx
        .jobs
        .submit(plain(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    let task = fx.finished(outcome.task_id).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.title, "Indexing");
    assert_eq!(fx.indexed().await, ["photos/a.jpg", "scans/deep/b.jpg"]);
}

#[tokio::test]
async fn the_runner_reads_the_volumes_again_before_it_indexes() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    // Mounted after the server started: the registry does not know it yet.
    fx.put("later", "x.jpg", b"1");

    let outcome = fx
        .jobs
        .submit(plain(vec![]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(fx.finished(outcome.task_id).await.state, TaskState::Done);
    assert_eq!(fx.indexed().await, ["later/x.jpg"]);
}

#[tokio::test]
async fn refreshing_the_volumes_picks_up_a_new_share_without_asking_for_a_rescan() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    fs::create_dir_all(fx.volumes_dir.join("later")).unwrap();
    assert_eq!(fx.volumes.online().len(), 1);

    let rescan = fx.jobs.refresh_volumes().await.unwrap();
    assert!(rescan.is_empty(), "no file operation was waiting");
    assert_eq!(fx.volumes.online().len(), 2);
}

#[tokio::test]
async fn the_title_follows_the_request_that_runs() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fx.put("photos", "a/x.jpg", b"1");
    fx.put("photos", "b/y.jpg", b"2");
    let blocker = fx.block_lane().await;

    let first = fx
        .jobs
        .submit(plain(vec![dir(photos, "a")]), IndexOrigin::User)
        .await
        .unwrap();
    fx.jobs
        .submit(plain(vec![dir(photos, "b")]), IndexOrigin::User)
        .await
        .unwrap();
    assert_eq!(
        fx.tasks.get(first.task_id).await.unwrap().title,
        "Indexing 1 folder",
        "a queued task cannot be renamed"
    );

    blocker.release();
    let task = fx.finished(first.task_id).await;
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.title, "Indexing 2 folders");
    assert_eq!(fx.indexed().await, ["photos/a/x.jpg", "photos/b/y.jpg"]);
}

#[tokio::test]
async fn a_job_for_a_volume_that_does_not_exist_fails_with_a_readable_error() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;

    let outcome = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(4242)]), IndexOrigin::User)
        .await
        .unwrap();
    let task = fx.finished(outcome.task_id).await;
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(
        task.error.as_deref(),
        Some("a volume of this request does not exist any more")
    );
}

#[tokio::test]
async fn cancelling_a_queued_job_means_it_never_runs() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fx.put("photos", "a.jpg", b"1");
    let blocker = fx.block_lane().await;

    let queued = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    let cancelled = fx.tasks.cancel(queued.task_id).await.unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);

    let id = blocker.id;
    blocker.release();
    fx.finished(id).await;
    tokio::time::sleep(QUIET).await;
    assert!(fx.indexed().await.is_empty(), "the job did not run");
}

#[tokio::test]
async fn cancelling_a_running_job_stops_it_and_a_later_job_completes() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fx.put("photos", "a.jpg", b"1");
    let held = fx.hold(photos).await;

    let running = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    fx.wait_taken(running.task_id).await;
    fx.tasks.cancel(running.task_id).await.unwrap();
    let task = fx.finished(running.task_id).await;
    assert_eq!(task.state, TaskState::Cancelled);
    assert!(fx.indexed().await.is_empty(), "it stopped before it wrote");

    drop(held);
    let next = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    assert!(!next.coalesced, "a cancelled job takes no request");
    assert_eq!(fx.finished(next.task_id).await.state, TaskState::Done);
    assert_eq!(fx.indexed().await, ["photos/a.jpg"]);
}

#[tokio::test]
async fn a_job_that_was_cancelled_before_it_started_does_not_scan() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    fx.put("photos", "a.jpg", b"1");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = TaskCtx {
        id: 1,
        cancel,
        progress: TaskProgress::new("Indexing"),
    };
    fx.jobs.inner.jobs.lock().await.push(Job {
        task_id: 1,
        origin: IndexOrigin::User,
        request: plain(vec![ScanTarget::volume(photos)]),
        taken: false,
    });
    let outcome = fx.jobs.run_job(ctx).await;
    assert!(matches!(outcome, Err(TaskError::Cancelled)), "{outcome:?}");
    assert!(fx.indexed().await.is_empty());
}

#[tokio::test]
async fn a_task_that_lost_its_request_fails_instead_of_scanning_everything() {
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    fx.put("photos", "a.jpg", b"1");
    let ctx = TaskCtx {
        id: 77,
        cancel: CancellationToken::new(),
        progress: TaskProgress::new("Indexing"),
    };
    let outcome = fx.jobs.run_job(ctx).await;
    assert!(matches!(outcome, Err(TaskError::Failed(_))), "{outcome:?}");
    assert!(fx.indexed().await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The status

#[tokio::test]
async fn the_status_names_the_active_task() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    assert_eq!(
        fx.jobs.status().await.unwrap(),
        IndexStatusDto::default(),
        "idle"
    );

    let blocker = fx.block_lane().await;
    let queued = fx
        .jobs
        .submit(plain(vec![ScanTarget::volume(photos)]), IndexOrigin::User)
        .await
        .unwrap();
    let busy = fx.jobs.status().await.unwrap();
    assert_eq!(busy.active_task_id, Some(blocker.id), "the running one");
    assert!(busy.held_removals.is_empty());

    blocker.release();
    fx.finished(queued.task_id).await;
    assert_eq!(fx.jobs.status().await.unwrap().active_task_id, None);
}

#[tokio::test]
async fn the_status_lists_the_removals_the_guard_holds() {
    let fx = Fixture::new().await;
    let photos = fx.volume("photos").await;
    sqlx::query("UPDATE volumes SET held_removals = 12345 WHERE id = ?")
        .bind(photos)
        .execute(fx.temp.write())
        .await
        .unwrap();
    assert_eq!(
        fx.jobs.status().await.unwrap().held_removals,
        vec![HeldRemovalsDto {
            volume_id: photos,
            volume_name: "photos".to_owned(),
            files: 12_345,
        }]
    );
}

// ---------------------------------------------------------------------------------------------
// The scheduler

#[tokio::test]
async fn the_startup_scan_runs_once_after_the_delay() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    fx.set_rescan_minutes(0).await;

    let scheduler = fx.jobs.start_background().expect("the first start");
    assert!(
        fx.jobs.start_background().is_none(),
        "a second start does nothing"
    );
    fx.wait_for_tasks(1).await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        fx.origins().await,
        ["startup"],
        "once, and no periodic scan"
    );
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn the_startup_scan_is_a_plain_scan_of_every_volume() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    fx.put("photos", "a.jpg", b"1");
    fx.set_rescan_minutes(0).await;

    let scheduler = fx.jobs.start_background().unwrap();
    fx.wait_for_files(1).await;
    assert_eq!(
        fx.params_of(fx.first_task().await).await,
        json!({"origin": "startup", "targets": [], "force": false, "confirm_removals": false})
    );
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn without_a_volume_there_is_nothing_to_scan() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    let scheduler = fx.jobs.start_background().unwrap();
    tokio::time::sleep(fast_scheduler().startup_delay + QUIET).await;
    assert_eq!(fx.task_count().await, 0);
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn the_periodic_scan_follows_the_interval_of_the_settings() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    fx.set_rescan_minutes(5).await;

    let scheduler = fx.jobs.start_background().unwrap();
    fx.wait_for_origin("periodic").await;
    assert_eq!(
        fx.origins().await.first().map(String::as_str),
        Some("startup")
    );
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn a_new_interval_applies_at_once() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    fx.set_rescan_minutes(0).await;

    let scheduler = fx.jobs.start_background().unwrap();
    fx.wait_for_tasks(1).await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(fx.origins().await, ["startup"], "0 never rescans");

    fx.set_rescan_minutes(5).await;
    fx.wait_for_origin("periodic").await;
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn a_busy_index_skips_the_scheduled_scan() {
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    fx.set_rescan_minutes(0).await;
    let blocker = fx.block_lane().await;

    let scheduler = fx.jobs.start_background().unwrap();
    tokio::time::sleep(fast_scheduler().startup_delay + QUIET).await;
    assert_eq!(
        fx.task_count().await,
        1,
        "only the task that was in the way"
    );
    assert_eq!(fx.active_ids().await, vec![blocker.id]);
    blocker.release();
    fx.stop(scheduler).await;
}

#[tokio::test]
async fn the_scheduler_ends_with_the_shutdown_token() {
    // Still waiting for the startup delay.
    let fx = Fixture::new().await;
    fx.volume("photos").await;
    let scheduler = fx.jobs.start_background().unwrap();
    fx.stop(scheduler).await;
    assert_eq!(fx.task_count().await, 0);

    // In its loop.
    let fx = Fixture::with_options(fast_scheduler()).await;
    fx.volume("photos").await;
    let scheduler = fx.jobs.start_background().unwrap();
    fx.wait_for_tasks(1).await;
    fx.stop(scheduler).await;
}
