//! The metadata stage (stage 2 of the indexer) over real directory trees with a fake extractor:
//! what it writes, what it leaves pending and what it reports.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{TestApp, Tree};
use futures_util::future::BoxFuture;
use opm_core::metadata::SourceMetadata;
use opm_core::paths::VolumeId;
use opm_server::indexer::{
    Embedded, ExtractFailure, ExtractOutcome, ExtractRequest, FakeExtractor, IndexError, Indexer,
    MetadataExtractor, RecordingChanges, RunContext, ScanRequest,
};
use sqlx::FromRow;
use tokio_util::sync::CancellationToken;

#[derive(Debug, FromRow)]
struct Row {
    meta_state: i64,
    capture_ts: Option<i64>,
    capture_offset_min: Option<i64>,
    capture_src: Option<String>,
    camera: Option<String>,
    rating: i64,
    flag: i64,
    width: Option<i64>,
    height: Option<i64>,
    duration_ms: Option<i64>,
    mime: Option<String>,
    size: i64,
    has_description: bool,
}

struct Fixture {
    app: TestApp,
    id: VolumeId,
    tree: Tree,
}

impl Fixture {
    async fn new() -> Self {
        let app = TestApp::new().await;
        let (id, tree) = app.add_volume("photos").await;
        Self { app, id, tree }
    }

    fn indexer(&self, fake: &Arc<FakeExtractor>) -> Indexer {
        self.app
            .state
            .indexer
            .with_extractor(Arc::clone(fake) as Arc<_>)
            .with_timezone(chrono_tz::UTC)
    }

    async fn row(&self, path: &str) -> Row {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        sqlx::query_as(
            "SELECT f.meta_state, f.capture_ts, f.capture_offset_min, f.capture_src, c.name AS camera, \
                    f.rating, f.flag, f.width, f.height, f.duration_ms, f.mime, f.size, f.has_description \
             FROM files f LEFT JOIN cameras c ON c.id = f.camera_id \
             WHERE f.volume_id = ? AND f.dir_path = ? AND f.name = ?",
        )
        .bind(self.id)
        .bind(dir)
        .bind(name)
        .fetch_one(self.app.state.db.read())
        .await
        .unwrap()
    }

    async fn tags_of(&self, path: &str) -> Vec<String> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        sqlx::query_scalar(
            "SELECT t.name FROM file_tags ft JOIN tags t ON t.id = ft.tag_id \
             JOIN files f ON f.id = ft.file_id \
             WHERE f.volume_id = ? AND f.dir_path = ? AND f.name = ? ORDER BY t.name",
        )
        .bind(self.id)
        .bind(dir)
        .bind(name)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap()
    }

    async fn count(&self, table: &str) -> i64 {
        let sql = match table {
            "cameras" => "SELECT COUNT(*) FROM cameras",
            _ => "SELECT COUNT(*) FROM tags",
        };
        sqlx::query_scalar(sql)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }
}

fn photo(width: i64, height: i64) -> Embedded {
    Embedded {
        meta: SourceMetadata {
            make: Some("Canon".into()),
            model: Some("EOS R5".into()),
            date_time_original: Some("2021:05:05 12:00:00".into()),
            offset_time_original: Some("+02:00".into()),
            rating: Some(4),
            keywords: Some(vec!["Paris".into(), "trip".into()]),
            description: Some("A trip to Paris".into()),
            ..SourceMetadata::default()
        },
        width: Some(width),
        height: Some(height),
        duration_ms: None,
        mime: Some("image/jpeg".into()),
    }
}

fn ctx() -> RunContext {
    RunContext::noop()
}

#[tokio::test]
async fn a_photo_gets_every_column() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"jpeg bytes");
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(6000, 4000))));
    let summary = fx
        .indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert_eq!((summary.extracted, summary.failed), (1, 0));
    assert!(!summary.extractor_unavailable);

    let row = fx.row("a.jpg").await;
    assert_eq!(row.meta_state, 1);
    assert_eq!(row.capture_src.as_deref(), Some("exif"));
    assert_eq!(row.capture_ts, Some(1_620_216_000));
    assert_eq!(row.capture_offset_min, Some(120));
    assert_eq!(row.camera.as_deref(), Some("Canon EOS R5"));
    assert_eq!((row.rating, row.flag), (4, 0));
    assert_eq!((row.width, row.height), (Some(6000), Some(4000)));
    assert_eq!(row.mime.as_deref(), Some("image/jpeg"));
    assert_eq!(row.duration_ms, None);
    assert!(row.has_description);
    assert_eq!(fx.tags_of("a.jpg").await, ["Paris", "trip"]);
}

#[tokio::test]
async fn sidecar_description_presence_overrides_embedded_and_empty_descriptions_clear_it() {
    let fx = Fixture::new().await;
    for (name, description) in [
        (
            "described",
            "<rdf:Alt><rdf:li xml:lang=\"x-default\">A view</rdf:li></rdf:Alt>",
        ),
        ("cleared", ""),
    ] {
        fx.tree.write(&format!("{name}.jpg"), b"jpeg bytes");
        fx.tree.write(&format!("{name}.jpg.xmp"), format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:description>{description}</dc:description></rdf:Description></rdf:RDF>"
        ).as_bytes());
    }
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(100, 50))));
    fx.indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert!(fx.row("described.jpg").await.has_description);
    assert!(!fx.row("cleared.jpg").await.has_description);
}
#[tokio::test]
async fn a_sidecar_overrides_the_embedded_values() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"jpeg bytes");
    fx.tree.write(
        "a.jpg.xmp",
        br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:exif="http://ns.adobe.com/exif/1.0/"
  xmp:Rating="2" exif:DateTimeOriginal="2020-01-02T03:04:05"/></rdf:RDF></x:xmpmeta>"#,
    );
    let fake = Arc::new(FakeExtractor::new(|request| {
        if request.path.extension().is_some_and(|ext| ext == "jpg") {
            Ok(photo(100, 50))
        } else {
            Err(ExtractFailure::Unreadable("not asked".into()))
        }
    }));
    fx.indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();

    let row = fx.row("a.jpg").await;
    assert_eq!(row.meta_state, 1);
    assert_eq!(row.rating, 2);
    assert_eq!(row.capture_src.as_deref(), Some("xmp"));
    assert_eq!(row.capture_ts, Some(1_577_934_245));
    assert_eq!(row.capture_offset_min, None);
    assert!(
        fake.calls()
            .iter()
            .flatten()
            .all(|path| path.extension().is_some_and(|e| e == "jpg")),
        "sidecars never reach the extractor"
    );
}

#[tokio::test]
async fn audio_and_other_files_use_the_file_system_and_skip_the_extractor() {
    let fx = Fixture::new().await;
    fx.tree.write("song.mp3", b"id3");
    fx.tree.write("notes.txt", b"hello");
    let fake = Arc::new(FakeExtractor::new(|_| Err(ExtractFailure::Timeout)));
    let summary = fx
        .indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert_eq!((summary.extracted, summary.failed), (2, 0));
    assert!(fake.calls().is_empty());
    for path in ["song.mp3", "notes.txt"] {
        let row = fx.row(path).await;
        assert_eq!(row.meta_state, 1, "{path}");
        assert_eq!(row.capture_src.as_deref(), Some("fs"), "{path}");
        assert!(row.capture_ts.is_some(), "{path}");
        assert!(row.mime.is_some(), "{path}");
    }
}

#[tokio::test]
async fn a_file_the_extractor_cannot_read_is_failed_and_the_rest_complete() {
    let fx = Fixture::new().await;
    fx.tree.write("bad.jpg", b"x");
    fx.tree.write("good.jpg", b"yy");
    let fake = Arc::new(FakeExtractor::new(|request| {
        if request.path.ends_with("bad.jpg") {
            Err(ExtractFailure::Unreadable(
                "Error: File format error".into(),
            ))
        } else {
            Ok(photo(10, 20))
        }
    }));
    let summary = fx
        .indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert_eq!((summary.extracted, summary.failed), (1, 1));
    let bad = fx.row("bad.jpg").await;
    assert_eq!(bad.meta_state, 2);
    assert_eq!(bad.size, 1);
    assert_eq!(fx.row("good.jpg").await.meta_state, 1);
}

#[tokio::test]
async fn running_again_does_nothing_and_a_changed_file_is_redone() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(1, 2))));
    let indexer = fx.indexer(&fake);
    indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    let calls = fake.calls().len();

    let again = indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    assert_eq!((again.extracted, again.failed), (0, 0));
    assert_eq!(fake.calls().len(), calls, "nothing is pending");

    fx.tree
        .write_at("a.jpg", b"changed", common::BASE_MTIME + 100);
    let redone = indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    assert_eq!(redone.extracted, 1);
    assert_eq!(fx.row("a.jpg").await.size, 7);
}

#[tokio::test]
async fn a_row_changed_after_the_read_keeps_pending() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    let racing = Racing {
        pool: fx.app.state.db.write().clone(),
    };
    let indexer = fx.app.state.indexer.with_extractor(Arc::new(racing));
    let summary = indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    assert_eq!(summary.extracted, 0);
    assert_eq!(fx.row("a.jpg").await.meta_state, 0);
}

struct Racing {
    pool: sqlx::SqlitePool,
}

impl MetadataExtractor for Racing {
    fn probe(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn extract(&self, requests: Vec<ExtractRequest>) -> BoxFuture<'_, Vec<ExtractOutcome>> {
        Box::pin(async move {
            sqlx::query("UPDATE files SET size = size + 1")
                .execute(&self.pool)
                .await
                .unwrap();
            requests.iter().map(|_| Ok(photo(1, 2))).collect()
        })
    }
}

#[tokio::test]
async fn the_stage_reports_the_directories_it_changed() {
    let fx = Fixture::new().await;
    fx.tree.write("one/a.jpg", b"1");
    fx.tree.write("one/two/b.jpg", b"22");
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(1, 2))));
    let indexer = fx.indexer(&fake);
    indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    fx.tree
        .write_at("one/a.jpg", b"333", common::BASE_MTIME + 5);

    let changes = Arc::new(RecordingChanges::new());
    let context = RunContext::new(
        CancellationToken::new(),
        Arc::new(opm_server::indexer::NoopProgress),
        Arc::clone(&changes) as Arc<_>,
    );
    indexer
        .run(&ScanRequest::default(), &context)
        .await
        .unwrap();
    assert!(changes.dirs(fx.id).contains("one"));
}

#[tokio::test]
async fn unused_cameras_and_tags_are_deleted() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(1, 2))));
    let indexer = fx.indexer(&fake);
    indexer.run(&ScanRequest::default(), &ctx()).await.unwrap();
    assert_eq!((fx.count("cameras").await, fx.count("tags").await), (1, 2));

    fx.tree.remove("a.jpg");
    indexer
        .run(
            &ScanRequest {
                confirm_removals: true,
                ..ScanRequest::default()
            },
            &ctx(),
        )
        .await
        .unwrap();
    assert_eq!((fx.count("cameras").await, fx.count("tags").await), (0, 0));
}

#[tokio::test]
async fn an_unavailable_extractor_leaves_files_pending() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    let fake = Arc::new(FakeExtractor::unavailable("exiftool not found"));
    let summary = fx
        .indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert!(summary.extractor_unavailable);
    assert_eq!((summary.extracted, summary.failed), (0, 0));
    assert_eq!(fx.row("a.jpg").await.meta_state, 0);
}

#[tokio::test]
async fn a_cancelled_run_stops_and_keeps_pending() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(1, 2))));
    let cancel = CancellationToken::new();
    let context = RunContext::new(
        cancel.clone(),
        Arc::new(opm_server::indexer::NoopProgress),
        Arc::new(opm_server::indexer::NoopChanges),
    );
    let indexer = fx.indexer(&fake);
    indexer.scan(&ScanRequest::default(), &ctx()).await.unwrap();
    cancel.cancel();
    let result = indexer.run(&ScanRequest::default(), &context).await;
    assert!(matches!(result, Err(IndexError::Cancelled)));
    assert_eq!(fx.row("a.jpg").await.meta_state, 0);
}

#[tokio::test]
#[ignore = "throughput check: cargo test -p opm-server --test indexer_extract -- --ignored"]
async fn twenty_thousand_files_are_extracted_quickly() {
    let fx = Fixture::new().await;
    fx.tree.populate(200, 20_000);
    let fake = Arc::new(FakeExtractor::new(|_| Ok(photo(1, 2))));
    let started = std::time::Instant::now();
    let summary = fx
        .indexer(&fake)
        .run(&ScanRequest::default(), &ctx())
        .await
        .unwrap();
    assert_eq!(summary.extracted, 20_000);
    eprintln!("20k files in {:?}", started.elapsed());
}
