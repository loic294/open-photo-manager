//! Reliable incremental watching; only `IndexJobs::submit` starts scans.
//!
//! One native instance serves the app. Linux registers NON-recursive directories from `dirs`,
//! shallow first, excluding junk. Its budget is min(max_user_watches / 2, 100,000), with an
//! unreadable sysctl default of 8192. Limits/errors report degraded health; periodic scans owned
//! by IndexJobs cover gaps. Development platforms watch volume roots recursively and filter events.
//! QUIET = 5s, MAX_WAIT = 60s; each directory drains independently. Intake is bounded at 8192
//! events; overflow/Rescan requests full non-forced repair. Failed submissions retain dirty marks.
//! Self-write suppression lasts QUIET + 10s: the periodic scan repairs an external write coinciding
//! with an app operation. Settings changes apply live, without restart.
//!
//! Inotify follows inodes: remove/rename purges stale registrations BEFORE any new registration.
//! Renamed paths are registered only after the scan completes. Reconcile also runs at startup,
//! every five minutes and after index tasks (including startup) finish. Shutdown drops all watches.
//!
//! Native tests: `CARGO_BUILD_JOBS=2 cargo test -p apm-server --test indexer_watcher_real -- --ignored`.
//! Linux: use a committed archive and native Linux cache volumes, not host bind-mounted test
//! trees (Docker Desktop's file-event bridge is not inotify on a real Linux filesystem). Check
//! free disk first. The integration coordinator lends these UID-1000-owned cache volumes:
//! ```sh
//! git archive HEAD | docker run --rm -i --cpus=2 --memory=3g \
//!   -v apm-linux-6c380c1d-target:/work/target \
//!   -v apm-linux-6c380c1d-registry:/usr/local/cargo/registry \
//!   -e CARGO_BUILD_JOBS=2 -e CARGO_INCREMENTAL=0 -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 \
//!   -e CARGO_HOME=/usr/local/cargo -e RUSTUP_HOME=/usr/local/rustup -e RUSTUP_TOOLCHAIN=1.94.1 \
//!   rust:1.94.1-bookworm bash -c 'mkdir -p /work && tar -xf - -C /work &&
//!   chown -R 1000:1000 /work/web && chmod 777 /work && cd /work &&
//!   setpriv --reuid=1000 --regid=1000 --clear-groups cargo test -p apm-server \
//!   --lib --test indexer_watcher --test indexer_watcher_real watch -- --include-ignored'
//! ```
//! Scratch trees live in the native Linux target volume. Tests use fake metadata extraction;
//! exiftool, ffmpeg, Immich and application services are not needed for this focused command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use apm_core::dto::{ServerEvent, TaskKind, TaskState, WatcherState, WatcherStatusDto};
use apm_core::junk::JunkRules;
use apm_core::paths::{RelPath, TaskId, VolumeId};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

use super::watch_backend::{EVENT_CAPACITY, EventSender, NativeBackend, WatchBackend, is_limit};
use super::watch_events::{self, Dirty, assemble};
pub use super::watch_events::{MAX_WAIT, QUIET, TARGET_LIMIT};
use super::{IndexJobs, IndexOrigin, ScanRequest, ScanTarget, VolumeRegistry};
use crate::db::{Db, now_secs};
use crate::fsops::SelfWrites;
use crate::settings::SettingsService;
use crate::tasks::TaskEngine;

pub type WatcherStatus = WatcherStatusDto;
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(300);
pub const SELF_WRITE_GRACE: Duration = Duration::from_secs(10);

/// Injectable timing and platform strategy. Production always uses the platform default.
#[derive(Clone, Copy)]
pub struct WatcherOptions {
    pub quiet: Duration,
    pub max_wait: Duration,
    pub reconcile_interval: Duration,
    pub tick: Duration,
    /// Some enables non-recursive registration with this budget; None uses recursive roots.
    pub inotify_budget: Option<u64>,
}

impl Default for WatcherOptions {
    fn default() -> Self {
        let inotify_budget = if cfg!(target_os = "linux") {
            let max = match std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches") {
                Ok(value) => match value.trim().parse::<u64>() {
                    Ok(max) => max,
                    Err(error) => {
                        tracing::warn!(%error, "invalid inotify watch limit; assuming 8192");
                        8192
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, "cannot read inotify watch limit; assuming 8192");
                    8192
                }
            };
            Some((max / 2).min(100_000))
        } else {
            None
        };
        Self {
            quiet: QUIET,
            max_wait: MAX_WAIT,
            reconcile_interval: RECONCILE_INTERVAL,
            tick: Duration::from_millis(100),
            inotify_budget,
        }
    }
}

#[derive(Clone)]
pub struct WatcherDeps {
    pub db: Db,
    pub jobs: IndexJobs,
    pub settings: SettingsService,
    pub tasks: TaskEngine,
    pub volumes: VolumeRegistry,
    pub self_writes: SelfWrites,
    pub shutdown: CancellationToken,
}

type BackendFactory = dyn Fn(EventSender) -> notify::Result<Box<dyn WatchBackend>> + Send + Sync;

#[derive(Default)]
struct Status {
    state: AtomicU8,
    watched: AtomicU64,
    unwatched: AtomicU64,
    limit: AtomicBool,
    pending: AtomicU64,
    last_event: AtomicI64,
}

struct Inner {
    deps: WatcherDeps,
    options: WatcherOptions,
    factory: Arc<BackendFactory>,
    started: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
    status: Status,
}

/// Cheap clone; construction starts nothing. All registrations and marks belong to one supervisor.
#[derive(Clone)]
pub struct Watcher(Arc<Inner>);

impl Watcher {
    pub fn new(deps: WatcherDeps) -> Self {
        Self::with_backend(deps, WatcherOptions::default(), |sender| {
            Ok(Box::new(NativeBackend::new(sender)?))
        })
    }

    pub fn with_backend(
        deps: WatcherDeps,
        options: WatcherOptions,
        factory: impl Fn(EventSender) -> notify::Result<Box<dyn WatchBackend>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(Inner {
            deps,
            options,
            factory: Arc::new(factory),
            started: AtomicBool::new(false),
            handle: Mutex::new(None),
            status: Status::default(),
        }))
    }

    pub fn status(&self) -> WatcherStatus {
        let status = &self.0.status;
        WatcherStatus {
            state: match status.state.load(Ordering::Acquire) {
                1 => WatcherState::Starting,
                2 => WatcherState::Watching,
                3 => WatcherState::Degraded,
                _ => WatcherState::Disabled,
            },
            watched_dirs: status.watched.load(Ordering::Relaxed),
            unwatched_dirs: status.unwatched.load(Ordering::Relaxed),
            limit_reached: status.limit.load(Ordering::Relaxed),
            inotify_budget: self.0.options.inotify_budget,
            pending_dirs: status.pending.load(Ordering::Relaxed),
            last_event_at: match status.last_event.load(Ordering::Relaxed) {
                0 => None,
                at => Some(at),
            },
        }
    }

    pub fn start(&self) {
        let mut handle = self.0.handle.lock().unwrap_or_else(PoisonError::into_inner);
        if self.0.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let watcher = self.clone();
        *handle = Some(tokio::spawn(
            async move {
                watcher.run().await;
            }
            .instrument(tracing::info_span!("filesystem_watcher"))
            .with_current_subscriber(),
        ));
    }

    /// Call after cancelling AppState.shutdown, before closing the database.
    pub async fn shutdown(&self) {
        let handle = self
            .0
            .handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle
            && let Err(error) = handle.await
        {
            tracing::error!(%error, "filesystem watcher supervisor failed");
        }
    }

    async fn run(&self) {
        let (sender, mut receiver) = mpsc::channel(EVENT_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let ingress = EventSender {
            sender,
            overflowed: overflowed.clone(),
        };
        let mut supervisor = Supervisor {
            watcher: self.clone(),
            backend: None,
            ingress,
            registered: BTreeMap::new(),
            dirty: Dirty::default(),
            tracked: BTreeSet::new(),
            warned_limit: false,
            warned_loss: false,
            errors: BTreeSet::new(),
            enabled: false,
        };
        let mut settings = self.0.deps.settings.subscribe();
        let mut tasks = self.0.deps.tasks.subscribe();
        let mut tick = tokio::time::interval(self.0.options.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reconcile = tokio::time::interval(self.0.options.reconcile_interval);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        supervisor.configure().await;
        loop {
            tokio::select! {
                _ = self.0.deps.shutdown.cancelled() => break,
                change = settings.changed() => {
                    if change.is_err() { break; }
                    if supervisor.enabled != self.0.deps.settings.get().index.watch_enabled {
                        while receiver.try_recv().is_ok() {}
                        overflowed.store(false, Ordering::Release);
                    }
                    supervisor.configure().await;
                }
                _ = tick.tick() => {
                    if supervisor.enabled {
                        if overflowed.swap(false, Ordering::AcqRel) { supervisor.repair(); }
                        supervisor.fire().await;
                    }
                }
                event = receiver.recv() => {
                    if let Some(event) = event
                        && supervisor.enabled {
                        match event {
                            Ok(event) => supervisor.event(event).await,
                            Err(error) => {
                                supervisor.report_error("event stream", error);
                                supervisor.repair();
                                supervisor.reconcile().await;
                            }
                        }
                    }
                }
                task = tasks.recv() => match task {
                    Ok(ServerEvent::Task { task }) if task.kind == TaskKind::Index
                        && !matches!(task.state, TaskState::Queued | TaskState::Running) => {
                        supervisor.tracked.remove(&task.id);
                        if supervisor.enabled { supervisor.reconcile().await; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if supervisor.enabled { supervisor.reconcile().await; }
                    }
                    _ => {}
                },
                _ = reconcile.tick() => {
                    if supervisor.enabled { supervisor.reconcile().await; }
                }
            }
            self.0
                .status
                .pending
                .store(supervisor.dirty.len() as u64, Ordering::Relaxed);
        }
        supervisor.disable();
    }
}

struct Supervisor {
    watcher: Watcher,
    backend: Option<Box<dyn WatchBackend>>,
    ingress: EventSender,
    registered: BTreeMap<(VolumeId, RelPath), PathBuf>,
    dirty: Dirty,
    tracked: BTreeSet<TaskId>,
    warned_limit: bool,
    warned_loss: bool,
    errors: BTreeSet<String>,
    enabled: bool,
}

impl Supervisor {
    fn report_error(&mut self, context: &str, error: impl std::fmt::Display) {
        if self.errors.insert(context.to_owned()) {
            tracing::warn!(context, %error, "filesystem watching degraded; retrying on reconcile");
        }
        self.watcher.0.status.state.store(3, Ordering::Release);
    }

    fn disable(&mut self) {
        if let Some(backend) = &mut self.backend {
            for path in self.registered.values() {
                if let Err(error) = backend.unwatch(path) {
                    tracing::debug!(%error, "watch teardown failed; dropping native instance");
                }
            }
        }
        self.backend = None;
        self.registered.clear();
        self.dirty.clear();
        self.tracked.clear();
        self.enabled = false;
        self.ingress.overflowed.store(false, Ordering::Release);
        let status = &self.watcher.0.status;
        status.watched.store(0, Ordering::Relaxed);
        status.unwatched.store(0, Ordering::Relaxed);
        status.pending.store(0, Ordering::Relaxed);
        status.limit.store(false, Ordering::Relaxed);
        status.state.store(0, Ordering::Release);
    }

    async fn configure(&mut self) {
        let enabled = self.watcher.0.deps.settings.get().index.watch_enabled;
        if !enabled {
            self.disable();
            return;
        }
        if !self.enabled {
            self.enabled = true;
            self.watcher.0.status.state.store(1, Ordering::Release);
        }
        let settings = self.watcher.0.deps.settings.get();
        self.dirty.filter(&JunkRules::new(
            &settings.junk_names,
            &settings.junk_prefixes,
        ));
        self.reconcile().await;
    }

    async fn reconcile(&mut self) {
        let deps = self.watcher.0.deps.clone();
        if let Err(error) = deps.volumes.refresh().await {
            self.report_error("volumes", error);
            return;
        }
        self.errors.remove("volumes");
        let settings = deps.settings.get();
        let junk = JunkRules::new(&settings.junk_names, &settings.junk_prefixes);
        let mut desired = BTreeMap::new();
        let mut failed_volumes = BTreeSet::new();
        for volume in deps.volumes.online() {
            let root = match deps.volumes.get(volume.id) {
                Ok(root) => root,
                Err(error) => {
                    self.report_error(&volume.name, error);
                    continue;
                }
            };
            let paths: Vec<String> = if self.watcher.0.options.inotify_budget.is_some() {
                match sqlx::query_scalar("SELECT path FROM dirs WHERE volume_id = ?")
                    .bind(volume.id)
                    .fetch_all(deps.db.read())
                    .await
                {
                    Ok(paths) => paths,
                    Err(error) => {
                        self.report_error(&volume.name, error);
                        failed_volumes.insert(volume.id);
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            let mut paths = paths;
            paths.push(String::new());
            for (n, path) in paths.into_iter().enumerate() {
                if deps.shutdown.is_cancelled() {
                    return;
                }
                if n % 256 == 0 {
                    tokio::task::yield_now().await;
                }
                let Ok(rel) = RelPath::new(&path) else {
                    continue;
                };
                if junk.is_junk_path(&rel) {
                    continue;
                }
                match root.resolve_dir(&rel) {
                    Ok(path) => {
                        desired.insert((volume.id, rel), path);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        self.report_error(&volume.name, error);
                        failed_volumes.insert(volume.id);
                    }
                }
            }
            if !failed_volumes.contains(&volume.id) {
                self.errors.remove(&volume.name);
            }
        }
        if self.backend.is_none() {
            match (self.watcher.0.factory)(self.ingress.clone()) {
                Ok(backend) => {
                    self.backend = Some(backend);
                    self.errors.remove("backend");
                }
                Err(error) => {
                    self.watcher
                        .0
                        .status
                        .unwatched
                        .store(desired.len() as u64, Ordering::Relaxed);
                    self.report_error("backend", error);
                    return;
                }
            }
        }
        let stale: Vec<_> = self
            .registered
            .keys()
            .filter(|key| !desired.contains_key(*key) && !failed_volumes.contains(&key.0))
            .cloned()
            .collect();
        for key in &stale {
            self.unwatch(key);
        }
        if stale.iter().any(|key| self.registered.contains_key(key)) {
            return; // Never register a renamed inode while its old descriptor remains.
        }
        let mut missing: Vec<_> = desired
            .iter()
            .filter(|(key, _)| !self.registered.contains_key(*key))
            .map(|(key, path)| (key.clone(), path.clone()))
            .collect();
        missing.sort_by(|a, b| a.0.1.depth().cmp(&b.0.1.depth()).then(a.0.cmp(&b.0)));
        self.watcher.0.status.limit.store(false, Ordering::Relaxed);
        for (n, (key, path)) in missing.into_iter().enumerate() {
            if deps.shutdown.is_cancelled() {
                return;
            }
            if n % 64 == 0 {
                tokio::task::yield_now().await;
            }
            if !self.register(key, path) && self.watcher.0.status.limit.load(Ordering::Relaxed) {
                break;
            }
        }
        let missing = desired
            .keys()
            .filter(|key| !self.registered.contains_key(*key))
            .count() as u64;
        let status = &self.watcher.0.status;
        status
            .watched
            .store(self.registered.len() as u64, Ordering::Relaxed);
        status.unwatched.store(missing, Ordering::Relaxed);
        let degraded =
            missing != 0 || !self.errors.is_empty() || status.limit.load(Ordering::Relaxed);
        let state = if degraded { 3 } else { 2 };
        if status.state.swap(state, Ordering::AcqRel) != state {
            tracing::info!(
                watched = self.registered.len(),
                unwatched = missing,
                degraded,
                "filesystem watch registrations reconciled"
            );
        }
    }

    fn unwatch(&mut self, key: &(VolumeId, RelPath)) {
        if let Some(path) = self.registered.get(key).cloned() {
            if let Some(backend) = &mut self.backend
                && let Err(error) = backend.unwatch(&path)
            {
                // notify may already have removed a deleted inode's watch.
                if !matches!(error.kind, notify::ErrorKind::WatchNotFound) {
                    self.report_error("unwatch", error);
                    return;
                }
            }
            self.registered.remove(key);
            self.errors.remove("unwatch");
            self.watcher
                .0
                .status
                .watched
                .store(self.registered.len() as u64, Ordering::Relaxed);
        }
    }

    fn register(&mut self, key: (VolumeId, RelPath), path: PathBuf) -> bool {
        if self.registered.contains_key(&key) {
            return true;
        }
        if self
            .watcher
            .0
            .options
            .inotify_budget
            .is_some_and(|budget| self.registered.len() as u64 >= budget)
        {
            self.limit();
            return false;
        }
        let Some(backend) = &mut self.backend else {
            return false;
        };
        let result = if self.watcher.0.options.inotify_budget.is_some() {
            backend.watch_dir(&path)
        } else {
            backend.watch_tree(&path)
        };
        match result {
            Ok(()) => {
                self.registered.insert(key, path);
                self.errors.remove("registration");
                self.watcher
                    .0
                    .status
                    .watched
                    .store(self.registered.len() as u64, Ordering::Relaxed);
                true
            }
            Err(error) if is_limit(&error) => {
                self.limit();
                false
            }
            Err(error) => {
                self.report_error("registration", error);
                false
            }
        }
    }

    fn limit(&mut self) {
        self.watcher.0.status.limit.store(true, Ordering::Relaxed);
        self.watcher.0.status.state.store(3, Ordering::Release);
        if !self.warned_limit {
            tracing::warn!(
                "filesystem watch budget reached: raise fs.inotify.max_user_watches on the NAS or rely on periodic rescans"
            );
            self.warned_limit = true;
        }
    }

    fn repair(&mut self) {
        if !self.warned_loss {
            tracing::warn!(
                "filesystem event loss: scheduling non-forced repair of every online volume"
            );
            self.warned_loss = true;
        }
        for volume in self.watcher.0.deps.volumes.online() {
            self.dirty.full(volume.id, Instant::now());
        }
    }

    async fn event(&mut self, event: notify::Event) {
        if event.need_rescan() {
            self.repair();
        }
        if !watch_events::relevant(&event) {
            return;
        }
        self.errors.remove("event stream");
        let deps = self.watcher.0.deps.clone();
        let settings = deps.settings.get();
        let junk = JunkRules::new(&settings.junk_names, &settings.junk_prefixes);
        let mut paths = Vec::new();
        for volume in deps.volumes.online() {
            let Ok(root) = deps.volumes.get(volume.id) else {
                continue;
            };
            for path in &event.paths {
                if let Some(rel) = watch_events::map_path(&root, path, &junk) {
                    paths.push((volume.id, root.clone(), rel));
                }
            }
        }
        // Purge all old names before considering any new name from the same rename event.
        if watch_events::invalidates_watches(&event) {
            for (id, root, rel) in &paths {
                let stale: Vec<_> = self
                    .registered
                    .keys()
                    .filter(|(v, dir)| v == id && dir.starts_with(rel))
                    .filter(|(_, dir)| !root.stat(dir).is_ok_and(|meta| meta.is_dir()))
                    .cloned()
                    .collect();
                for key in stale {
                    self.unwatch(&key);
                }
            }
        }
        for (id, root, rel) in paths {
            self.watcher
                .0
                .status
                .last_event
                .store(now_secs(), Ordering::Relaxed);
            self.dirty.mark(id, rel.clone(), Instant::now());
            if matches!(event.kind, notify::EventKind::Create(_))
                && self.watcher.0.options.inotify_budget.is_some()
                && let Ok(path) = root.resolve_dir(&rel)
            {
                self.register((id, rel), path);
            }
        }
    }

    async fn fire(&mut self) {
        let deps = self.watcher.0.deps.clone();
        let options = self.watcher.0.options;
        let mut batches: BTreeMap<VolumeId, Vec<watch_events::Mark>> = BTreeMap::new();
        for mark in self
            .dirty
            .drain(Instant::now(), options.quiet, options.max_wait)
        {
            batches.entry(mark.volume_id).or_default().push(mark);
        }
        for (id, marks) in batches {
            if deps.shutdown.is_cancelled() || !deps.settings.get().index.watch_enabled {
                self.dirty.clear();
                return;
            }
            let root = match deps.volumes.get(id) {
                Ok(root) => root,
                Err(error) => {
                    self.report_error("scan volume", error);
                    self.dirty.restore(marks);
                    continue;
                }
            };
            let mut targets = Vec::new();
            for mark in &marks {
                targets.push(if mark.full {
                    ScanTarget::volume(id)
                } else {
                    ScanTarget::dir(id, mark.dir.clone())
                });
                for rel in &mark.candidates {
                    match root.stat(rel) {
                        Ok(meta) if meta.is_dir() => {
                            targets.push(ScanTarget::subtree(id, rel.clone()))
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                            ) => {}
                        Err(error) => {
                            tracing::debug!(%error, dir = %rel, "watch candidate not usable")
                        }
                    }
                }
            }
            let targets = assemble(targets, &deps.self_writes, options.quiet + SELF_WRITE_GRACE);
            if targets.is_empty() {
                continue;
            }
            match deps
                .jobs
                .submit(
                    ScanRequest {
                        targets,
                        force: false,
                        confirm_removals: false,
                    },
                    IndexOrigin::Watcher,
                )
                .await
            {
                Ok(outcome) => {
                    self.tracked.insert(outcome.task_id);
                    self.errors.remove("submit");
                    // A coalesced task can have finished before this supervisor received its id.
                    match deps.tasks.get(outcome.task_id).await {
                        Ok(task)
                            if !matches!(task.state, TaskState::Queued | TaskState::Running) =>
                        {
                            self.tracked.remove(&outcome.task_id);
                            self.reconcile().await;
                        }
                        Ok(_) => {}
                        Err(error) => self.report_error("task", error),
                    }
                }
                Err(error) => {
                    self.report_error("submit", error);
                    self.dirty.restore(marks);
                }
            }
        }
    }
}
