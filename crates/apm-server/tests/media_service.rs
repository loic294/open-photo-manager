//! `MediaService` with a controllable renderer: single flight, the concurrency gate, failure
//! memory, containment of panics, stale files and the cache sweep.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use apm_server::db::models::{FILE_COLUMNS, FileRow};
use apm_server::indexer::{Embedded, FakeExtractor, Indexer, RunContext, ScanRequest};
use apm_server::media::{MediaError, MediaService, RenderError, RenderRequest, Renderer, Variant};
use common::TestApp;
use common::media_fixtures::{file_id, jpeg, thumb_state};
use futures_util::future::BoxFuture;

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    running: AtomicUsize,
    peak: AtomicUsize,
    delay_ms: u64,
    mode: Mode,
}

#[derive(Default, Clone, Copy)]
enum Mode {
    #[default]
    Ok,
    Fail,
    Panic,
}

impl Renderer for Probe {
    fn render(&self, _req: RenderRequest) -> BoxFuture<'_, Result<Vec<u8>, RenderError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            match self.mode {
                Mode::Ok => Ok(b"JPEG".to_vec()),
                Mode::Fail => Err(RenderError::Failed("corrupt".into())),
                Mode::Panic => panic!("renderer bug"),
            }
        })
    }
}

/// An app whose files are indexed WITHOUT thumbnails, and a service rendering with `probe`.
async fn setup(names: &[&str], probe: Probe) -> (TestApp, Vec<FileRow>, MediaService, Arc<Probe>) {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    for name in names {
        tree.write(name, &jpeg(40, 30));
    }
    let state = &app.state;
    let plain = Indexer::new(
        state.db.clone(),
        state.settings.clone(),
        state.volumes.clone(),
    )
    .with_extractor(Arc::new(FakeExtractor::new(|_| {
        Ok(Embedded {
            meta: Default::default(),
            width: None,
            height: None,
            duration_ms: None,
            mime: None,
        })
    })));
    plain
        .run(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    let mut rows = Vec::new();
    for name in names {
        let id = file_id(&app, volume, "", name).await;
        let mut query =
            sqlx::QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE id = "));
        query.push_bind(id);
        rows.push(
            query
                .build_query_as::<FileRow>()
                .fetch_one(state.db.read())
                .await
                .unwrap(),
        );
    }
    let probe = Arc::new(probe);
    let service = state
        .media
        .with_renderer(Arc::clone(&probe) as Arc<dyn Renderer>);
    (app, rows, service, probe)
}

#[tokio::test]
async fn concurrent_requests_share_one_render_and_the_cache_serves_the_rest() {
    let (app, rows, service, probe) = setup(
        &["a.jpg"],
        Probe {
            delay_ms: 100,
            ..Probe::default()
        },
    )
    .await;
    let row = &rows[0];
    let results =
        futures_util::future::join_all((0..8).map(|_| service.rendition(row, Variant::ThumbSm)))
            .await;
    assert!(results.iter().all(|r| r.as_ref().unwrap().bytes == "JPEG"));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(thumb_state(&app, row.id).await, 1);

    let again = service.rendition(row, Variant::ThumbSm).await.unwrap();
    assert!(again.cached);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let other_size = service.rendition(row, Variant::ThumbLg).await.unwrap();
    assert!(!other_size.cached);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn renders_never_exceed_the_configured_concurrency() {
    let names: Vec<String> = (0..12).map(|i| format!("f{i}.jpg")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (app, rows, service, probe) = setup(
        &refs,
        Probe {
            delay_ms: 30,
            ..Probe::default()
        },
    )
    .await;
    let limit = usize::from(app.state.settings.get().index.thumbnail_concurrency);
    let results = futures_util::future::join_all(
        rows.iter()
            .map(|row| service.rendition(row, Variant::ThumbSm)),
    )
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 12);
    assert!(probe.peak.load(Ordering::SeqCst) <= limit.max(1));
    assert_eq!(service.renders_in_flight(), 0);
}

#[tokio::test]
async fn a_failed_render_is_remembered_and_marks_the_row() {
    let (app, rows, service, probe) = setup(
        &["a.jpg"],
        Probe {
            mode: Mode::Fail,
            ..Probe::default()
        },
    )
    .await;
    for _ in 0..3 {
        let err = service
            .rendition(&rows[0], Variant::ThumbSm)
            .await
            .unwrap_err();
        assert!(matches!(err, MediaError::Failed(_)), "{err:?}");
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(thumb_state(&app, rows[0].id).await, 2);
}

#[tokio::test]
async fn a_panicking_renderer_is_a_failed_render() {
    let (_app, rows, service, _probe) = setup(
        &["a.jpg"],
        Probe {
            mode: Mode::Panic,
            ..Probe::default()
        },
    )
    .await;
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        service.rendition(&rows[0], Variant::ThumbSm),
    )
    .await
    .expect("must not hang")
    .unwrap_err();
    assert!(matches!(err, MediaError::Failed(_)), "{err:?}");
    assert_eq!(service.renders_in_flight(), 0);
}

#[tokio::test]
async fn a_dropped_leader_does_not_strand_the_others() {
    let (_app, rows, service, _probe) = setup(
        &["a.jpg"],
        Probe {
            delay_ms: 150,
            ..Probe::default()
        },
    )
    .await;
    let row = rows[0].clone();
    let leader = tokio::spawn({
        let service = service.clone();
        let row = row.clone();
        async move { service.rendition(&row, Variant::ThumbSm).await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let follower = tokio::spawn({
        let service = service.clone();
        async move { service.rendition(&row, Variant::ThumbSm).await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    leader.abort();
    let result = tokio::time::timeout(Duration::from_secs(5), follower)
        .await
        .expect("must not hang")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn a_changed_file_is_rendered_fresh_without_caching() {
    let (_app, rows, service, probe) = setup(&["a.jpg"], Probe::default()).await;
    let mut stale = rows[0].clone();
    stale.mtime += 1;
    let rendition = service.rendition(&stale, Variant::ThumbSm).await.unwrap();
    assert!(rendition.etag.is_none());
    service.rendition(&stale, Variant::ThumbSm).await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2, "nothing was cached");
}

#[tokio::test]
async fn the_sweep_removes_entries_of_changed_files_only() {
    let (app, rows, service, _probe) = setup(&["a.jpg", "b.jpg"], Probe::default()).await;
    for row in &rows {
        service.rendition(row, Variant::ThumbSm).await.unwrap();
    }
    sqlx::query("UPDATE files SET mtime = mtime + 5 WHERE id = ?")
        .bind(rows[0].id)
        .execute(app.state.db.write())
        .await
        .unwrap();
    let mut removed = 0;
    for _ in 0..4 {
        removed += service.sweep_cache(1_000_000).await.removed;
    }
    assert_eq!(removed, 1);
    let kept = service.rendition(&rows[1], Variant::ThumbSm).await.unwrap();
    assert!(kept.cached);
}
