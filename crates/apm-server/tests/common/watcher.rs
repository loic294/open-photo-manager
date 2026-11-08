//! Shared scratch-volume watcher harness; no production data or external services.
#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apm_core::dto::TaskState;
use apm_server::config::WatchPolicy;
use apm_server::indexer::watch_backend::{EventSender, WatchBackend};
use apm_server::indexer::watcher::{Watcher, WatcherDeps, WatcherOptions};
use apm_server::indexer::{FakeExtractor, IndexJobs, IndexOrigin, Indexer, ScanRequest};

use super::{TestApp, Tree};

pub fn options(budget: Option<u64>) -> WatcherOptions {
    WatcherOptions {
        quiet: Duration::from_millis(40),
        max_wait: Duration::from_millis(200),
        tick: Duration::from_millis(5),
        reconcile_interval: Duration::from_millis(150),
        inotify_budget: budget,
    }
}

pub async fn eventually(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("watcher condition timed out");
}

pub async fn until<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(15), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("watcher index condition timed out");
}

#[derive(Default)]
pub struct FakeState {
    pub ingress: Option<EventSender>,
    pub operations: Vec<(bool, PathBuf)>,
    pub paths: std::collections::BTreeSet<PathBuf>,
    pub fail_after: Option<usize>,
    pub failures: usize,
}

#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<FakeState>>);

impl Fake {
    pub fn send(&self, event: notify::Event) {
        self.0
            .lock()
            .unwrap()
            .ingress
            .as_ref()
            .unwrap()
            .send(Ok(event));
    }
    pub fn paths(&self) -> Vec<PathBuf> {
        self.0.lock().unwrap().paths.iter().cloned().collect()
    }
    pub fn factory(&self, ingress: EventSender) -> Box<dyn WatchBackend> {
        self.0.lock().unwrap().ingress = Some(ingress);
        Box::new(self.clone())
    }
}

impl WatchBackend for Fake {
    fn watch_dir(&mut self, path: &Path) -> notify::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.fail_after.is_some_and(|n| state.paths.len() >= n) {
            state.failures += 1;
            return Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch));
        }
        state.operations.push((true, path.to_owned()));
        state.paths.insert(path.to_owned());
        Ok(())
    }
    fn watch_tree(&mut self, path: &Path) -> notify::Result<()> {
        self.watch_dir(path)
    }
    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.operations.push((false, path.to_owned()));
        state.paths.remove(path);
        Ok(())
    }
}

pub struct Harness {
    pub app: TestApp,
    pub tree: Tree,
    pub id: i64,
}

impl Harness {
    pub async fn new() -> Self {
        Self::with_watch_policy(WatchPolicy::Auto).await
    }

    pub async fn with_watch_policy(policy: WatchPolicy) -> Self {
        let app = TestApp::with_config(|config| config.watch_mode = policy).await;
        let (id, tree) = app.add_volume("photos").await;
        let indexer = Indexer::new(
            app.state.db.clone(),
            app.state.settings.clone(),
            app.state.volumes.clone(),
        )
        .with_extractor(Arc::new(FakeExtractor::unavailable("watcher tests")));
        let jobs = IndexJobs::new(
            indexer,
            app.state.tasks.clone(),
            app.state.fsops.clone(),
            app.state.shutdown.clone(),
        );
        let mut app = app;
        app.state.index_jobs = jobs;
        Self { app, tree, id }
    }
    pub fn deps(&self) -> WatcherDeps {
        WatcherDeps {
            db: self.app.state.db.clone(),
            jobs: self.app.state.index_jobs.clone(),
            settings: self.app.state.settings.clone(),
            tasks: self.app.state.tasks.clone(),
            volumes: self.app.state.volumes.clone(),
            self_writes: self.app.state.fsops.self_writes().clone(),
            shutdown: self.app.state.shutdown.clone(),
        }
    }
    pub async fn scan(&self) {
        let outcome = self
            .app
            .state
            .index_jobs
            .submit(ScanRequest::default(), IndexOrigin::User)
            .await
            .unwrap();
        let done = self
            .app
            .state
            .tasks
            .wait_finished(outcome.task_id)
            .await
            .unwrap();
        assert_eq!(done.state, TaskState::Done, "{done:?}");
    }
    pub async fn start_fake(&mut self, options: WatcherOptions) -> Fake {
        let fake = self.install_fake(options);
        eventually(|| self.app.state.watcher.status().watched_dirs > 0).await;
        fake
    }

    pub fn install_fake(&mut self, options: WatcherOptions) -> Fake {
        let fake = Fake::default();
        let backend = fake.clone();
        self.app.state.watcher = Watcher::with_backend(self.deps(), options, move |ingress| {
            Ok(backend.factory(ingress))
        });
        self.app.state.watcher.start();
        self.app.router = apm_server::app::router(self.app.state.clone());
        fake
    }
    pub async fn start_native(&mut self) -> Fake {
        use apm_server::indexer::watch_backend::NativeBackend;
        struct ObservedNative(NativeBackend, Fake);
        impl WatchBackend for ObservedNative {
            fn watch_dir(&mut self, path: &Path) -> notify::Result<()> {
                self.0.watch_dir(path)?;
                self.1.watch_dir(path)
            }
            fn watch_tree(&mut self, path: &Path) -> notify::Result<()> {
                self.0.watch_tree(path)?;
                self.1.watch_tree(path)
            }
            fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
                let result = self.0.unwatch(path);
                if result.is_ok()
                    || matches!(&result, Err(e) if matches!(e.kind, notify::ErrorKind::WatchNotFound))
                {
                    self.1.unwatch(path)?;
                }
                result
            }
        }
        let observed = Fake::default();
        let record = observed.clone();
        let mut options = options(WatcherOptions::default().inotify_budget);
        options.quiet = Duration::from_millis(150);
        self.app.state.watcher = Watcher::with_backend(self.deps(), options, move |ingress| {
            Ok(Box::new(ObservedNative(
                NativeBackend::new(ingress)?,
                record.clone(),
            )))
        });
        self.app.state.watcher.start();
        self.app.router = apm_server::app::router(self.app.state.clone());
        eventually(|| self.app.state.watcher.status().watched_dirs > 0).await;
        observed
    }
    pub async fn files(&self) -> Vec<(String, String, i64)> {
        sqlx::query_as(
            "SELECT dir_path, name, size FROM files WHERE volume_id = ? ORDER BY dir_path, name",
        )
        .bind(self.id)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap()
    }
    pub async fn wait_files(&self, files: &[(&str, &str, i64)]) {
        let expected: Vec<_> = files
            .iter()
            .map(|(d, n, s)| (d.to_string(), n.to_string(), *s))
            .collect();
        until(|| async { self.files().await == expected }).await;
    }
    pub async fn enabled(&self, enabled: bool) {
        let mut settings = (*self.app.state.settings.get()).clone();
        settings.index.watch_enabled = enabled;
        self.app.state.settings.update(settings).await.unwrap();
    }
    pub async fn stop(&self) {
        self.app.state.begin_shutdown();
        tokio::time::timeout(Duration::from_secs(3), self.app.state.watcher.shutdown())
            .await
            .unwrap();
        self.app.state.tasks.shutdown().await;
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app.state.begin_shutdown();
    }
}
