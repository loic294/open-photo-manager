//! Fake event source, real index jobs, private volume trees.
mod common;

use std::time::Duration;

use apm_core::dto::{ConflictPolicy, WatcherState};
use apm_server::config::WatchPolicy;
use axum::http::Method;
use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind};

use common::watcher::{Harness, eventually, options, until};

#[tokio::test]
async fn polling_policy_registers_nothing_and_reports_polling_only() {
    let mut h = Harness::with_watch_policy(WatchPolicy::Poll).await;
    h.scan().await;
    let fake = h.install_fake(options(Some(100)));
    eventually(|| h.app.state.watcher.status().state == WatcherState::PollingOnly).await;

    assert!(fake.paths().is_empty());
    assert_eq!(h.app.state.watcher.status().watched_dirs, 0);

    let (_, body) = h.app.call(Method::GET, "/api/volumes", None).await;
    assert_eq!(body[0]["watch_mode"], "polling_only");
    assert!(
        body[0]["watch_reason"]
            .as_str()
            .unwrap()
            .contains("APM_WATCH_MODE=poll")
    );
    h.stop().await;
}

#[tokio::test]
async fn watch_policy_forces_registration_and_reports_why() {
    let mut h = Harness::with_watch_policy(WatchPolicy::Watch).await;
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;

    assert!(!fake.paths().is_empty());
    let (_, body) = h.app.call(Method::GET, "/api/volumes", None).await;
    assert_eq!(body[0]["watch_mode"], "watching");
    assert!(body[0]["watch_reason"].as_str().unwrap().contains("forced"));
    h.stop().await;
}

#[derive(Clone, Default)]
struct LimitWarnings(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LimitWarnings {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(bool);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message"
                    && format!("{value:?}").contains("filesystem watch budget reached")
                {
                    self.0 = true;
                }
            }
        }
        let mut message = Message(false);
        event.record(&mut message);
        if message.0 {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn event(kind: EventKind, tree: &common::Tree, paths: &[&str]) -> Event {
    paths
        .iter()
        .fold(Event::new(kind), |e, path| e.add_path(tree.path(path)))
}

#[tokio::test]
async fn watcher_file_create_modify_remove_and_rename_use_incremental_jobs() {
    let mut h = Harness::new().await;
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    h.app.state.watcher.start(); // Idempotent: no second native instance.
    h.tree.write("a.txt", b"a");
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["a.txt"],
    ));
    h.wait_files(&[("", "a.txt", 1)]).await;
    h.tree.write("a.txt", b"longer");
    fake.send(event(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        &h.tree,
        &["a.txt"],
    ));
    h.wait_files(&[("", "a.txt", 6)]).await;
    std::fs::rename(h.tree.path("a.txt"), h.tree.path("b.txt")).unwrap();
    fake.send(event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &h.tree,
        &["a.txt", "b.txt"],
    ));
    h.wait_files(&[("", "b.txt", 6)]).await;
    std::fs::remove_file(h.tree.path("b.txt")).unwrap();
    fake.send(event(
        EventKind::Remove(RemoveKind::File),
        &h.tree,
        &["b.txt"],
    ));
    h.wait_files(&[]).await;
    let requests: Vec<String> = sqlx::query_scalar(
        "SELECT params_json FROM tasks WHERE kind = 'index' AND params_json LIKE '%watcher%'",
    )
    .fetch_all(h.app.state.db.read())
    .await
    .unwrap();
    assert!(!requests.is_empty());
    for request in requests {
        let value: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(value["force"], false, "{value}");
        assert_eq!(value["confirm_removals"], false, "{value}");
    }
    h.stop().await;
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn watcher_directory_create_delete_and_rename_purge_old_watches_first() {
    let mut h = Harness::new().await;
    h.tree.write("old/nested/a.txt", b"a");
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    eventually(|| fake.paths().contains(&h.tree.path("old/nested"))).await;
    std::fs::rename(h.tree.path("old"), h.tree.path("new")).unwrap();
    fake.send(event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &h.tree,
        &["old", "new"],
    ));
    h.wait_files(&[("new/nested", "a.txt", 1)]).await;
    eventually(|| fake.paths().contains(&h.tree.path("new/nested"))).await;
    assert!(!fake.paths().contains(&h.tree.path("old")));
    let ops = fake.0.lock().unwrap().operations.clone();
    let last_unwatch = ops
        .iter()
        .rposition(|(watch, path)| !watch && path.starts_with(h.tree.path("old")))
        .unwrap();
    let first_new = ops
        .iter()
        .position(|(watch, path)| *watch && path.starts_with(h.tree.path("new")))
        .unwrap();
    assert!(last_unwatch < first_new);
    h.tree.write("copy/nested/b.txt", b"bb");
    fake.send(event(
        EventKind::Create(CreateKind::Folder),
        &h.tree,
        &["copy"],
    ));
    h.wait_files(&[("copy/nested", "b.txt", 2), ("new/nested", "a.txt", 1)])
        .await;
    std::fs::remove_dir_all(h.tree.path("copy")).unwrap();
    fake.send(event(
        EventKind::Remove(RemoveKind::Folder),
        &h.tree,
        &["copy"],
    ));
    h.wait_files(&[("new/nested", "a.txt", 1)]).await;
    until(|| async {
        let dirs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM dirs WHERE volume_id = ? AND path IN ('copy', 'copy/nested')",
        )
        .bind(h.id)
        .fetch_one(h.app.state.db.read())
        .await
        .unwrap();
        dirs == 0
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn watcher_budget_status_endpoint_shallow_order_and_limit_failure() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::prelude::*;
    let mut h = Harness::new().await;
    h.tree.mkdir("a/b/c");
    h.tree.mkdir("z");
    h.tree.mkdir("@eaDir/ignored");
    h.scan().await;
    let warnings = LimitWarnings::default();
    let subscriber = tracing_subscriber::registry().with(warnings.clone());
    let fake = h
        .start_fake(options(Some(2)))
        .with_subscriber(subscriber)
        .await;
    eventually(|| h.app.state.watcher.status().state == WatcherState::Degraded).await;
    let status = h.app.state.watcher.status();
    assert_eq!(status.watched_dirs, 2);
    assert_eq!(status.unwatched_dirs, 3);
    assert!(status.limit_reached);
    assert_eq!(
        fake.paths(),
        vec![h.tree.root().to_owned(), h.tree.path("a")]
    );
    let (_, body) = h.app.call(Method::GET, "/api/index/status", None).await;
    assert_eq!(body["watcher"]["inotify_budget"], 2);
    assert_eq!(body["watcher"]["state"], "degraded");
    h.enabled(false).await;
    eventually(|| fake.paths().is_empty()).await;
    h.enabled(true).await;
    eventually(|| fake.paths().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(warnings.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    h.stop().await;

    let mut h = Harness::new().await;
    h.tree.mkdir("a/b");
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    h.enabled(false).await;
    eventually(|| fake.paths().is_empty()).await;
    fake.0.lock().unwrap().fail_after = Some(1);
    h.enabled(true).await;
    eventually(|| h.app.state.watcher.status().limit_reached).await;
    assert_eq!(h.app.state.watcher.status().watched_dirs, 1);
    assert_eq!(h.app.state.watcher.status().unwatched_dirs, 2);
    assert!(fake.0.lock().unwrap().failures > 0);
    fake.0.lock().unwrap().fail_after = None;
    eventually(|| h.app.state.watcher.status().state == WatcherState::Watching).await;
    h.stop().await;
}

#[tokio::test]
async fn watcher_registers_empty_directories_immediately_and_settings_keep_pending_marks() {
    let mut h = Harness::new().await;
    h.scan().await;
    let mut timing = options(Some(100));
    timing.quiet = Duration::from_millis(500);
    timing.max_wait = Duration::from_secs(2);
    let fake = h.start_fake(timing).await;
    h.tree.mkdir("empty");
    fake.send(event(
        EventKind::Create(CreateKind::Folder),
        &h.tree,
        &["empty"],
    ));
    eventually(|| fake.paths().contains(&h.tree.path("empty"))).await;
    assert_eq!(h.app.state.watcher.status().pending_dirs, 1);
    let indexed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dirs WHERE volume_id = ? AND path = 'empty'")
            .bind(h.id)
            .fetch_one(h.app.state.db.read())
            .await
            .unwrap();
    assert_eq!(
        indexed, 0,
        "empty directory registered before the delayed scan"
    );
    h.tree.write("empty/a.txt", b"a");
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["empty/a.txt"],
    ));
    let mut settings = (*h.app.state.settings.get()).clone();
    settings.index.rescan_interval_minutes = 17;
    h.app.state.settings.update(settings).await.unwrap();
    h.wait_files(&[("empty", "a.txt", 1)]).await;
    h.stop().await;
}

#[tokio::test]
async fn watcher_backend_creation_failure_reports_unwatched_dirs_and_retries() {
    use apm_server::indexer::watcher::Watcher;
    let mut h = Harness::new().await;
    h.tree.mkdir("a");
    h.scan().await;
    let fake = common::watcher::Fake::default();
    let backend = fake.clone();
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let failing = fail.clone();
    h.app.state.watcher = Watcher::with_backend(h.deps(), options(Some(100)), move |ingress| {
        if failing.load(std::sync::atomic::Ordering::Relaxed) {
            Err(notify::Error::generic("unavailable"))
        } else {
            Ok(backend.factory(ingress))
        }
    });
    h.app.state.watcher.start();
    eventually(|| h.app.state.watcher.status().state == WatcherState::Degraded).await;
    assert_eq!(h.app.state.watcher.status().unwatched_dirs, 2);
    assert_eq!(h.app.state.watcher.status().watched_dirs, 0);
    fail.store(false, std::sync::atomic::Ordering::Relaxed);
    eventually(|| h.app.state.watcher.status().state == WatcherState::Watching).await;
    h.stop().await;
}

#[tokio::test]
async fn watcher_live_disable_discards_pending_events_and_shutdown_drops_watches() {
    let mut h = Harness::new().await;
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    h.tree.write("dropped.txt", b"x");
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["dropped.txt"],
    ));
    eventually(|| h.app.state.watcher.status().pending_dirs == 1).await;
    h.enabled(false).await;
    eventually(|| h.app.state.watcher.status().state == WatcherState::Disabled).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.files().await.is_empty());
    assert_eq!(h.app.state.watcher.status().pending_dirs, 0);
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["dropped.txt"],
    ));
    h.enabled(true).await;
    eventually(|| h.app.state.watcher.status().state == WatcherState::Watching).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.files().await.is_empty());
    h.tree.write("new.txt", b"x");
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["new.txt"],
    ));
    h.wait_files(&[("", "dropped.txt", 1), ("", "new.txt", 1)])
        .await;
    h.stop().await;
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn watcher_offline_volume_recovers_and_overflow_rescans_every_online_volume() {
    let mut h = Harness::new().await;
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    let absent = h.app.dir.path().join("unmounted");
    std::fs::rename(h.tree.root(), &absent).unwrap();
    eventually(|| fake.paths().is_empty()).await;
    std::fs::rename(&absent, h.tree.root()).unwrap();
    eventually(|| fake.paths().contains(&h.tree.root().to_owned())).await;
    h.tree.write("missed/a.txt", b"a");
    let (second, tree) = h.app.add_volume("archive").await;
    tree.write("b.txt", b"bb");
    h.app.state.volumes.refresh().await.unwrap();
    let rescan = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
    fake.send(rescan);
    h.wait_files(&[("missed", "a.txt", 1)]).await;
    until(|| async {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM files WHERE volume_id = ?")
            .bind(second)
            .fetch_one(h.app.state.db.read())
            .await
            .unwrap();
        n == 1
    })
    .await;
    h.tree.write("overflow.txt", b"xx");
    // >64 paths is intentionally treated as overflow at the non-blocking ingress.
    fake.send((0..65).fold(Event::new(EventKind::Other), |e, _| {
        e.add_path(h.tree.path("ignored"))
    }));
    h.wait_files(&[("", "overflow.txt", 2), ("missed", "a.txt", 1)])
        .await;
    h.tree.write("backend-error.txt", b"xxx");
    fake.0
        .lock()
        .unwrap()
        .ingress
        .as_ref()
        .unwrap()
        .send(Err(notify::Error::generic("event source error")));
    h.wait_files(&[
        ("", "backend-error.txt", 3),
        ("", "overflow.txt", 2),
        ("missed", "a.txt", 1),
    ])
    .await;
    h.stop().await;
}

#[tokio::test]
async fn watcher_ignores_fsops_self_writes_but_not_external_changes_elsewhere() {
    let mut h = Harness::new().await;
    h.tree.write("source/a.txt", b"a");
    h.tree.mkdir("destination");
    h.tree.mkdir("external");
    h.scan().await;
    let fake = h.start_fake(options(Some(100))).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks")
        .fetch_one(h.app.state.db.read())
        .await
        .unwrap();
    use common::fsops::{loc, request};
    h.app
        .state
        .fsops
        .move_file(
            request(
                loc(h.id, "source/a.txt"),
                loc(h.id, "destination/a.txt"),
                ConflictPolicy::Skip,
            ),
            &apm_server::fsops::OpContext::detached(),
        )
        .await
        .unwrap();
    fake.send(event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &h.tree,
        &["source/a.txt", "destination/a.txt"],
    ));
    tokio::time::sleep(Duration::from_millis(120)).await;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks")
        .fetch_one(h.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(before, after, "app mutation must not start another scan");
    h.tree.write("external/b.txt", b"bb");
    fake.send(event(
        EventKind::Create(CreateKind::File),
        &h.tree,
        &["external/b.txt"],
    ));
    h.wait_files(&[("destination", "a.txt", 1), ("external", "b.txt", 2)])
        .await;
    h.stop().await;
}
