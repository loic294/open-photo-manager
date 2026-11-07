//! The scanner (stage 1 of the indexer) over real directory trees: what a scan writes, what it
//! reports and what it leaves alone.
//!
//! The tests never rely on the case or normalisation behaviour of the file system, so they run
//! the same on macOS and Linux.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use apm_core::dto::settings::SettingsDto;
use apm_core::paths::{RelPath, VolumeId};
use apm_server::db::Db;
use apm_server::indexer::{
    ChangeSink, HeldRemovals, IndexError, IndexPhase, IndexSummary, NoopChanges, NoopProgress,
    RecordingChanges, RecordingProgress, RunContext, ScanRequest, ScanSummary, ScanTarget,
};
use common::{BASE_MTIME, TestApp, Tree};
use sqlx::FromRow;
use tokio_util::sync::CancellationToken;

fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap()
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn set<const N: usize>(items: [&str; N]) -> BTreeSet<String> {
    items.into_iter().map(str::to_owned).collect()
}

fn only(targets: Vec<ScanTarget>) -> ScanRequest {
    ScanRequest {
        targets,
        ..ScanRequest::default()
    }
}

fn confirmed() -> ScanRequest {
    ScanRequest {
        confirm_removals: true,
        ..ScanRequest::default()
    }
}

#[derive(Debug, FromRow)]
struct FileRow {
    id: i64,
    name: String,
    name_norm: String,
    stem_norm: String,
    ext: String,
    kind: String,
    size: i64,
    mtime: i64,
    meta_state: i64,
    hash_state: i64,
    thumb_state: i64,
    seen_gen: i64,
    hash: Option<Vec<u8>>,
}

impl FileRow {
    fn states(&self) -> (i64, i64, i64) {
        (self.meta_state, self.hash_state, self.thumb_state)
    }
}

#[derive(Debug, FromRow)]
struct VolumeRow {
    scan_gen: i64,
    last_scan_at: Option<i64>,
    held_removals: i64,
    online: i64,
}

const FILE_SQL: &str = "SELECT id, name, name_norm, stem_norm, ext, kind, size, mtime, meta_state, \
     hash_state, thumb_state, seen_gen, hash FROM files \
     WHERE volume_id = ? AND dir_path = ? AND name = ?";

const PATHS_SQL: &str = "SELECT CASE WHEN dir_path = '' THEN name ELSE dir_path || '/' || name END \
     AS path FROM files WHERE volume_id = ? ORDER BY path";

/// One volume called `photos`, its directory and the application around it.
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

    async fn try_scan(
        &self,
        request: &ScanRequest,
        ctx: &RunContext,
    ) -> Result<ScanSummary, IndexError> {
        self.app.state.indexer.scan(request, ctx).await
    }

    /// Scans every online volume completely.
    async fn scan(&self) -> ScanSummary {
        self.scan_with(&ScanRequest::default()).await
    }

    async fn scan_with(&self, request: &ScanRequest) -> ScanSummary {
        self.try_scan(request, &RunContext::noop()).await.unwrap()
    }

    /// A scan of the whole volume and the directories it reported.
    async fn scan_reported(&self) -> (ScanSummary, BTreeSet<String>) {
        self.scan_reported_with(&ScanRequest::default()).await
    }

    async fn scan_reported_with(&self, request: &ScanRequest) -> (ScanSummary, BTreeSet<String>) {
        let changes = Arc::new(RecordingChanges::new());
        let ctx = RunContext::new(
            CancellationToken::new(),
            Arc::new(NoopProgress),
            changes.clone(),
        );
        let summary = self.try_scan(request, &ctx).await.unwrap();
        (summary, changes.dirs(self.id))
    }

    async fn scalar(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }

    async fn file_count(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM files").await
    }

    /// The path of every file row, sorted.
    async fn paths(&self) -> Vec<String> {
        sqlx::query_scalar(PATHS_SQL)
            .bind(self.id)
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// The path of every directory row, sorted (`""` is the root).
    async fn dir_paths(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT path FROM dirs WHERE volume_id = ? ORDER BY path")
            .bind(self.id)
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    async fn find(&self, path: &str) -> Option<FileRow> {
        let (dir, name) = split(path);
        sqlx::query_as(FILE_SQL)
            .bind(self.id)
            .bind(dir)
            .bind(name)
            .fetch_optional(self.app.state.db.read())
            .await
            .unwrap()
    }

    async fn file(&self, path: &str) -> FileRow {
        self.find(path)
            .await
            .unwrap_or_else(|| panic!("no row for {path}"))
    }

    async fn volume_row(&self) -> VolumeRow {
        sqlx::query_as(
            "SELECT scan_gen, last_scan_at, held_removals, online FROM volumes WHERE id = ?",
        )
        .bind(self.id)
        .fetch_one(self.app.state.db.read())
        .await
        .unwrap()
    }

    /// `(owner, sidecar, link kind)` of every link, sorted.
    async fn links(&self) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT CASE WHEN o.dir_path = '' THEN o.name ELSE o.dir_path || '/' || o.name END AS owner, \
                    CASE WHEN s.dir_path = '' THEN s.name ELSE s.dir_path || '/' || s.name END AS sidecar, \
                    l.kind \
             FROM file_sidecars l \
             JOIN files o ON o.id = l.file_id \
             JOIN files s ON s.id = l.sidecar_id \
             WHERE o.volume_id = ? ORDER BY owner, sidecar",
        )
        .bind(self.id)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap()
    }

    async fn sql(&self, sql: &'static str) {
        sqlx::query(sql)
            .execute(self.app.state.db.write())
            .await
            .unwrap();
    }

    /// Pretends that the later stages ran: every file that is not a sidecar is done and hashed.
    async fn mark_done(&self) {
        self.sql(
            "UPDATE files SET meta_state = 1, hash_state = 1, thumb_state = 1, hash = zeroblob(20) \
             WHERE kind <> 'sidecar'",
        )
        .await;
    }

    /// Rows written to the database by the writer connection so far.
    async fn total_changes(&self) -> i64 {
        sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(self.app.state.db.write())
            .await
            .unwrap()
    }

    async fn change_settings(&self, change: impl FnOnce(&mut SettingsDto)) {
        change_settings(&self.app, change).await;
    }

    /// Unmounts the share: its files go away and an empty mount point stays behind.
    fn unmount(&self) -> PathBuf {
        let away = self.app.dir.path().join("unmounted");
        std::fs::rename(self.tree.root(), &away).unwrap();
        std::fs::create_dir(self.tree.root()).unwrap();
        away
    }

    fn remount(&self, away: PathBuf) {
        std::fs::remove_dir(self.tree.root()).unwrap();
        std::fs::rename(away, self.tree.root()).unwrap();
    }
}

async fn change_settings(app: &TestApp, change: impl FnOnce(&mut SettingsDto)) {
    let mut settings = (*app.state.settings.get()).clone();
    change(&mut settings);
    app.state.settings.update(settings).await.unwrap();
}

/// 7 files (33 bytes) in 4 directories: a photo with a sidecar, a raw with a sidecar, a clip, a
/// picture below a sub-directory, a text file, and an empty directory.
fn build_sample(tree: &Tree) {
    tree.write("IMG_0001.jpg", b"jpeg");
    tree.write("IMG_0001.xmp", b"<x/>");
    tree.write("2019/IMG_0100.CR3", b"raw-data");
    tree.write("2019/IMG_0100.CR3.xmp", b"<x/>");
    tree.write("2019/clip.mp4", b"movie");
    tree.write("2019/summer/beach.png", b"png");
    tree.write("notes.txt", b"hello");
    tree.mkdir("empty");
}

#[tokio::test]
async fn the_first_scan_indexes_every_file_and_directory() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);

    let summary = fx.scan().await;

    assert_eq!(
        summary,
        ScanSummary {
            volumes_scanned: 1,
            dirs_visited: 4,
            dirs_added: 4,
            files_seen: 7,
            added: 7,
            ..ScanSummary::default()
        }
    );
    assert_eq!(
        fx.paths().await,
        [
            "2019/IMG_0100.CR3",
            "2019/IMG_0100.CR3.xmp",
            "2019/clip.mp4",
            "2019/summer/beach.png",
            "IMG_0001.jpg",
            "IMG_0001.xmp",
            "notes.txt",
        ]
    );
    assert_eq!(fx.dir_paths().await, ["", "2019", "2019/summer", "empty"]);

    let volume = fx.volume_row().await;
    assert_eq!(volume.scan_gen, 1);
    assert!(volume.last_scan_at.is_some());
    assert_eq!(volume.held_removals, 0);
}

#[tokio::test]
async fn rows_carry_the_derived_columns_and_the_initial_pipeline_states() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    fx.scan().await;

    let raw = fx.file("2019/IMG_0100.CR3").await;
    assert_eq!(raw.name, "IMG_0100.CR3");
    assert_eq!(raw.name_norm, "img_0100.cr3");
    assert_eq!(raw.stem_norm, "img_0100");
    assert_eq!(raw.ext, "cr3");
    assert_eq!(raw.kind, "image", "a raw is an image");
    assert_eq!(raw.size, 8);
    assert_eq!(raw.mtime, BASE_MTIME);
    assert_eq!(raw.states(), (0, 0, 0));
    assert_eq!(raw.seen_gen, 1);
    assert!(raw.hash.is_none());

    assert_eq!(fx.file("2019/clip.mp4").await.kind, "video");
    assert_eq!(fx.file("notes.txt").await.kind, "other");
    let sidecar = fx.file("IMG_0001.xmp").await;
    assert_eq!(sidecar.kind, "sidecar");
    assert_eq!(
        sidecar.states(),
        (3, 3, 3),
        "nothing to extract from a sidecar"
    );
}

#[tokio::test]
async fn progress_counts_the_files_and_bytes_it_has_listed() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);

    let progress = Arc::new(RecordingProgress::new());
    let ctx = RunContext::new(
        CancellationToken::new(),
        progress.clone(),
        Arc::new(NoopChanges),
    );
    fx.try_scan(&ScanRequest::default(), &ctx).await.unwrap();
    let last = progress.last().unwrap();
    assert_eq!(last.phase, IndexPhase::Walk);
    assert_eq!((last.items_done, last.bytes_done), (7, 33));

    // The second scan knows how much to expect.
    let progress = Arc::new(RecordingProgress::new());
    let ctx = RunContext::new(
        CancellationToken::new(),
        progress.clone(),
        Arc::new(NoopChanges),
    );
    fx.try_scan(&ScanRequest::default(), &ctx).await.unwrap();
    let reports = progress.reports();
    let last = reports.last().unwrap();
    assert_eq!((last.items_done, last.items_total), (7, Some(7)));
    assert!(
        reports
            .windows(2)
            .all(|pair| pair[0].items_done <= pair[1].items_done),
        "progress is cumulative"
    );
    assert!(
        reports
            .iter()
            .filter_map(|report| report.detail.as_deref())
            .all(|detail| detail.starts_with("photos")),
        "the detail names the volume, never an absolute path"
    );
}

#[tokio::test]
async fn a_rescan_of_an_unchanged_tree_writes_nothing_and_reports_nothing() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    fx.scan().await;
    let rows = fx.file("IMG_0001.jpg").await;

    let before = fx.total_changes().await;
    let (summary, reported) = fx.scan_reported().await;
    let after = fx.total_changes().await;

    assert_eq!(
        after - before,
        2,
        "only the two updates of the volume row: no file or directory row is written"
    );
    assert!(reported.is_empty(), "reported {reported:?}");
    assert_eq!(
        summary,
        ScanSummary {
            volumes_scanned: 1,
            dirs_visited: 4,
            files_seen: 7,
            unchanged: 7,
            ..ScanSummary::default()
        }
    );
    let again = fx.file("IMG_0001.jpg").await;
    assert_eq!((again.id, again.seen_gen), (rows.id, rows.seen_gen));
    assert_eq!(fx.volume_row().await.scan_gen, 2);
}

#[tokio::test]
async fn every_kind_of_change_reports_the_directories_whose_listing_changed() {
    let fx = Fixture::new().await;
    fx.tree.write("IMG_0001.jpg", b"x");
    fx.tree.write("a/x.jpg", b"x");
    fx.tree.write("a/b/y.jpg", b"x");

    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["", "a", "a/b"]), "first scan");

    let (_, reported) = fx.scan_reported().await;
    assert!(reported.is_empty(), "unchanged tree");

    fx.tree.write("a/new.jpg", b"x");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a"]), "file added");

    fx.tree.remove("a/new.jpg");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a"]), "file removed");

    fx.tree.rename("a/x.jpg", "a/z.jpg");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a"]), "file renamed");

    fx.tree.write("a/c/f.jpg", b"x");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a", "a/c"]), "directory with a file added");

    fx.tree.mkdir("a/e");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a", "a/e"]), "empty directory added");

    fx.tree.remove_dir("a/c");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a", "a/c"]), "directory with a file removed");

    fx.tree.remove_dir("a/e");
    let (_, reported) = fx.scan_reported().await;
    assert_eq!(reported, set(["a", "a/e"]), "empty directory removed");

    fx.tree.set_dir_mtime("a", BASE_MTIME + 99);
    let (summary, reported) = fx.scan_reported().await;
    assert!(
        reported.is_empty(),
        "a directory's mtime is not visible: {reported:?}"
    );
    assert_eq!(summary.changed, 0);
    let stored: i64 =
        sqlx::query_scalar("SELECT mtime FROM dirs WHERE volume_id = ? AND path = 'a'")
            .bind(fx.id)
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
    assert_eq!(stored, BASE_MTIME + 99, "but it is kept up to date");
}

/// Remembers, for every directory it is told about, the files the database holds in it at that
/// moment.
struct SnapshotSink {
    db: Db,
    calls: Mutex<Vec<(String, BTreeSet<String>)>>,
}

impl ChangeSink for SnapshotSink {
    fn dirs_changed(&self, volume_id: VolumeId, dirs: &[RelPath]) {
        for dir in dirs {
            let names: Vec<String> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(
                    sqlx::query_scalar(
                        "SELECT name FROM files WHERE volume_id = ? AND dir_path = ?",
                    )
                    .bind(volume_id)
                    .bind(dir.as_str())
                    .fetch_all(self.db.read()),
                )
            })
            .unwrap();
            self.calls
                .lock()
                .unwrap()
                .push((dir.as_str().to_owned(), names.into_iter().collect()));
        }
    }
}

fn disk_files(tree: &Tree, dir: &str) -> BTreeSet<String> {
    std::fs::read_dir(tree.path(dir))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .filter(|name| !name.starts_with(".apm-probe"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changes_are_reported_after_the_transaction_committed() {
    let fx = Fixture::new().await;
    fx.tree.write("a/1.jpg", b"x");
    fx.tree.write("a/2.jpg", b"x");
    fx.tree.write("b/3.jpg", b"x");

    for step in 0..3 {
        match step {
            0 => {}
            1 => fx.tree.remove("a/2.jpg"),
            _ => fx.tree.write("a/4.jpg", b"x"),
        }
        let sink = Arc::new(SnapshotSink {
            db: fx.app.state.db.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let ctx = RunContext::new(
            CancellationToken::new(),
            Arc::new(NoopProgress),
            sink.clone(),
        );
        fx.try_scan(&ScanRequest::default(), &ctx).await.unwrap();

        let calls = sink.calls.lock().unwrap().clone();
        assert!(!calls.is_empty(), "step {step} reported nothing");
        for (dir, in_database) in calls {
            assert_eq!(
                in_database,
                disk_files(&fx.tree, &dir),
                "step {step}: the database must already show {dir:?} as it is on disk"
            );
        }
    }
}

#[tokio::test]
async fn a_changed_file_keeps_its_id_and_restarts_the_pipeline() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"one");
    fx.tree.write("b.jpg", b"one");
    fx.tree.write("c.jpg", b"one");
    fx.scan().await;
    fx.mark_done().await;
    let a = fx.file("a.jpg").await;
    let b = fx.file("b.jpg").await;
    assert_eq!(a.states(), (1, 1, 1));
    assert!(a.hash.is_some());

    fx.tree.write("a.jpg", b"three"); // the size differs, the time does not
    fx.tree.set_mtime("b.jpg", BASE_MTIME + 10); // the time differs, the size does not
    let (summary, reported) = fx.scan_reported().await;

    assert_eq!(
        (summary.changed, summary.unchanged, summary.added),
        (2, 1, 0)
    );
    assert_eq!(reported, set([""]));
    let new_a = fx.file("a.jpg").await;
    assert_eq!(
        new_a.id, a.id,
        "an update keeps the id: selections and links stay valid"
    );
    assert_eq!((new_a.size, new_a.states()), (5, (0, 0, 0)));
    assert!(
        new_a.hash.is_none(),
        "the old hash no longer describes the content"
    );
    let new_b = fx.file("b.jpg").await;
    assert_eq!(new_b.id, b.id);
    assert_eq!((new_b.mtime, new_b.states()), (BASE_MTIME + 10, (0, 0, 0)));
    assert!(new_b.hash.is_none());

    let c = fx.file("c.jpg").await;
    assert_eq!(c.states(), (1, 1, 1), "an untouched file keeps its results");
    assert!(c.hash.is_some());
}

#[tokio::test]
async fn a_forced_scan_treats_every_file_as_changed() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    fx.scan().await;
    fx.mark_done().await;

    let summary = fx
        .scan_with(&ScanRequest {
            force: true,
            ..ScanRequest::default()
        })
        .await;

    assert_eq!(
        (summary.changed, summary.unchanged, summary.added),
        (7, 0, 0)
    );
    let image = fx.file("IMG_0001.jpg").await;
    assert_eq!(image.states(), (0, 0, 0));
    assert!(image.hash.is_none());
    assert_eq!(fx.file("IMG_0001.xmp").await.states(), (3, 3, 3));
}

#[tokio::test]
async fn a_removed_file_loses_its_row_and_its_links() {
    let fx = Fixture::new().await;
    fx.tree.write("p/IMG_1.CR3", b"x");
    fx.tree.write("p/IMG_1.CR3.xmp", b"x");
    fx.tree.write("p/IMG_2.jpg", b"x");
    fx.scan().await;
    assert_eq!(fx.links().await.len(), 1);

    fx.tree.remove("p/IMG_1.CR3");
    let (summary, reported) = fx.scan_reported().await;

    assert_eq!((summary.removed, summary.held), (1, 0));
    assert_eq!(fx.paths().await, ["p/IMG_1.CR3.xmp", "p/IMG_2.jpg"]);
    assert!(fx.links().await.is_empty(), "the link goes with its owner");
    assert_eq!(reported, set(["p"]));
}

#[tokio::test]
async fn a_removed_directory_takes_its_subtree_with_it() {
    let fx = Fixture::new().await;
    fx.tree.write("2019/a.jpg", b"x");
    fx.tree.write("2019/sub/b.jpg", b"x");
    fx.tree.write("2019/sub/deep/c.jpg", b"x");
    fx.tree.write("2019.old/d.jpg", b"x");
    fx.tree.write("2019x/e.jpg", b"x");
    fx.tree.write("20190/f.jpg", b"x");
    fx.scan().await;

    fx.tree.remove_dir("2019");
    let (summary, reported) = fx.scan_reported().await;

    assert_eq!((summary.removed, summary.dirs_removed), (3, 3));
    assert_eq!(
        fx.paths().await,
        ["2019.old/d.jpg", "20190/f.jpg", "2019x/e.jpg"],
        "siblings that merely share the prefix stay"
    );
    assert_eq!(
        fx.dir_paths().await,
        ["", "2019.old", "20190", "2019x"],
        "the directory rows go too"
    );
    assert_eq!(reported, set(["", "2019", "2019/sub", "2019/sub/deep"]));
}

#[tokio::test]
async fn renamed_files_and_directories_are_removed_and_added() {
    let fx = Fixture::new().await;
    fx.tree.write("a/IMG_1.jpg", b"x");
    fx.tree.write("a/IMG_2.jpg", b"x");
    fx.tree.write("old/one.jpg", b"x");
    fx.tree.write("old/two.jpg", b"x");
    fx.scan().await;
    let before = fx.file("a/IMG_1.jpg").await;

    fx.tree.rename("a/IMG_1.jpg", "a/IMG_9.jpg");
    let (summary, reported) = fx.scan_reported().await;
    assert_eq!((summary.added, summary.removed), (1, 1));
    assert_eq!(reported, set(["a"]));
    assert!(fx.find("a/IMG_1.jpg").await.is_none());
    assert_ne!(fx.file("a/IMG_9.jpg").await.id, before.id);

    fx.tree.rename("old", "new");
    let (summary, reported) = fx.scan_reported().await;
    assert_eq!(
        (
            summary.added,
            summary.removed,
            summary.dirs_added,
            summary.dirs_removed
        ),
        (2, 2, 1, 1)
    );
    assert_eq!(reported, set(["", "new", "old"]));
    assert_eq!(
        fx.paths().await,
        ["a/IMG_2.jpg", "a/IMG_9.jpg", "new/one.jpg", "new/two.jpg"]
    );
    assert_eq!(fx.dir_paths().await, ["", "a", "new"]);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_are_neither_followed_nor_indexed() {
    use std::os::unix::fs::symlink;

    let fx = Fixture::new().await;
    fx.tree.write("real/a.jpg", b"x");
    let outside = common::scratch_dir();
    std::fs::write(outside.path().join("secret.jpg"), b"x").unwrap();
    std::fs::create_dir(outside.path().join("elsewhere")).unwrap();
    std::fs::write(outside.path().join("elsewhere/b.jpg"), b"x").unwrap();

    symlink("real/a.jpg", fx.tree.path("link-file.jpg")).unwrap();
    symlink("real", fx.tree.path("link-dir")).unwrap();
    symlink("..", fx.tree.path("real/loop")).unwrap();
    symlink("missing.jpg", fx.tree.path("dangling.jpg")).unwrap();
    symlink(
        outside.path().join("secret.jpg"),
        fx.tree.path("outside.jpg"),
    )
    .unwrap();
    symlink(
        outside.path().join("elsewhere"),
        fx.tree.path("outside-dir"),
    )
    .unwrap();

    let summary = fx.scan().await;

    assert_eq!(fx.paths().await, ["real/a.jpg"]);
    assert_eq!(fx.dir_paths().await, ["", "real"]);
    assert_eq!((summary.files_seen, summary.dirs_visited), (1, 2));
    assert_eq!(summary.skipped_invalid, 0);
}

#[tokio::test]
async fn junk_is_ignored_and_empty_or_junk_only_directories_are_recorded() {
    let fx = Fixture::new().await;
    fx.tree.write("IMG_1.jpg", b"x");
    fx.tree.write(".DS_Store", b"x");
    fx.tree.write("._IMG_1.jpg", b"x");
    fx.tree.write("Thumbs.db", b"x");
    fx.tree.write("desktop.ini", b"x");
    fx.tree.write("photo.jpg.apm-part", b"x");
    fx.tree.write(".apm-probe-123", b"x");
    fx.tree.write(".apm-trash/20250101/old.jpg", b"x");
    fx.tree.write("@eaDir/IMG_1.jpg/SYNOFILE_THUMB_M.jpg", b"x");
    fx.tree.write("#recycle/deleted.jpg", b"x");
    fx.tree.write("only-junk/.DS_Store", b"x");
    fx.tree.write("only-junk/._x.jpg", b"x");
    fx.tree.mkdir("empty");

    let summary = fx.scan().await;

    assert_eq!(fx.paths().await, ["IMG_1.jpg"]);
    assert_eq!(
        fx.dir_paths().await,
        ["", "empty", "only-junk"],
        "junk directories are not descended and get no row, empty and junk-only ones do"
    );
    assert_eq!(
        (summary.files_seen, summary.added, summary.skipped_invalid),
        (1, 1, 0)
    );
}

#[tokio::test]
async fn the_junk_settings_apply_to_the_next_scan() {
    let fx = Fixture::new().await;
    fx.tree.write("IMG_1.jpg", b"x");
    fx.tree.write("draft_1.jpg", b"x");
    fx.tree.write("Thumbs.db", b"x");
    fx.scan().await;
    assert_eq!(fx.paths().await, ["IMG_1.jpg", "draft_1.jpg"]);

    fx.change_settings(|settings| {
        settings
            .junk_names
            .retain(|name| !name.eq_ignore_ascii_case("thumbs.db"));
        settings.junk_prefixes.push("draft_".to_owned());
    })
    .await;
    let summary = fx.scan().await;

    assert_eq!((summary.added, summary.removed), (1, 1));
    assert_eq!(fx.paths().await, ["IMG_1.jpg", "Thumbs.db"]);
}

#[cfg(unix)]
#[tokio::test]
async fn names_that_are_not_utf8_are_skipped_and_counted() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fx = Fixture::new().await;
    fx.tree.write("good.jpg", b"x");
    let bad = fx.tree.root().join(OsStr::from_bytes(b"bad\xff.jpg"));
    if std::fs::write(&bad, b"x").is_err() {
        eprintln!("this file system refuses names that are not UTF-8: nothing to test");
        return;
    }
    let bad_dir = fx.tree.root().join(OsStr::from_bytes(b"dir\xfe"));
    std::fs::create_dir(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("inside.jpg"), b"x").unwrap();

    let summary = fx.scan().await;

    assert_eq!(summary.skipped_invalid, 2);
    assert_eq!(fx.paths().await, ["good.jpg"]);
    assert_eq!(fx.dir_paths().await, [""]);
}

#[tokio::test]
async fn names_are_stored_as_found_with_a_normalised_search_form() {
    let fx = Fixture::new().await;
    // "e" followed by a combining acute accent (NFD), as macOS tools produce it.
    fx.tree.write("cafe\u{301}.JPG", b"x");

    fx.scan().await;

    let on_disk: Vec<String> = disk_files(&fx.tree, "").into_iter().collect();
    assert_eq!(on_disk.len(), 1);
    assert_eq!(
        fx.paths().await,
        on_disk,
        "the name is exactly what the directory listing says"
    );
    let row = fx.file(&on_disk[0]).await;
    assert_eq!(row.name, on_disk[0]);
    assert_eq!(
        row.name_norm, "caf\u{e9}.jpg",
        "composed (NFC) and lowercase"
    );
    assert_eq!(row.stem_norm, "caf\u{e9}");
    assert_eq!(row.ext, "jpg");
}

#[tokio::test]
async fn sidecars_are_linked_by_exact_name_and_by_stem() {
    let fx = Fixture::new().await;
    fx.tree.write("IMG_0001.CR3", b"x");
    fx.tree.write("IMG_0001.CR3.xmp", b"x"); // exact: the owner's whole name plus the extension
    fx.tree.write("IMG_0001.jpg", b"x");
    fx.tree.write("IMG_0001.xmp", b"x"); // stem: every media file called IMG_0001
    fx.tree.write("IMG_0004.xmp", b"x"); // IMG_00041.jpg is another stem
    fx.tree.write("IMG_00041.jpg", b"x");
    fx.tree.write("lonely.xmp", b"x");

    fx.scan().await;

    let link = |owner: &str, sidecar: &str, kind: &str| {
        (owner.to_owned(), sidecar.to_owned(), kind.to_owned())
    };
    assert_eq!(
        fx.links().await,
        [
            link("IMG_0001.CR3", "IMG_0001.CR3.xmp", "exact"),
            link("IMG_0001.CR3", "IMG_0001.xmp", "stem"),
            link("IMG_0001.jpg", "IMG_0001.xmp", "stem"),
        ],
        "a sidecar may have several owners"
    );
    for orphan in ["IMG_0004.xmp", "lonely.xmp"] {
        assert_eq!(
            fx.file(orphan).await.kind,
            "sidecar",
            "an orphan is still a sidecar"
        );
    }
}

#[tokio::test]
async fn sidecar_links_survive_scans_that_change_nothing() {
    let fx = Fixture::new().await;
    fx.tree.write("a/IMG_1.jpg", b"x");
    fx.tree.write("a/IMG_1.xmp", b"x");
    fx.scan().await;
    let links = fx.links().await;
    assert_eq!(links.len(), 1);

    fx.scan().await;
    fx.tree.write("a/other.jpg", b"x");
    fx.scan().await;

    assert_eq!(fx.links().await, links);
}

#[tokio::test]
async fn the_sidecar_extension_setting_reclassifies_the_files() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"x");
    fx.tree.write("a.json", b"x"); // a sidecar extension by default
    fx.tree.write("a.zzz", b"x"); // not one
    fx.scan().await;
    fx.mark_done().await;
    assert_eq!(fx.file("a.json").await.kind, "sidecar");
    assert_eq!(fx.file("a.zzz").await.kind, "other");
    assert_eq!(
        fx.links().await,
        [
            ("a.jpg".to_owned(), "a.json".to_owned(), "stem".to_owned()),
            ("a.zzz".to_owned(), "a.json".to_owned(), "stem".to_owned()),
        ],
        "any file that is not a sidecar can own one"
    );
    let json_id = fx.file("a.json").await.id;

    fx.change_settings(|settings| {
        settings.sidecar_extensions.retain(|ext| ext != "json");
        settings.sidecar_extensions.push("zzz".to_owned());
    })
    .await;
    let (summary, reported) = fx.scan_reported().await;

    assert_eq!((summary.changed, summary.unchanged), (2, 1));
    assert_eq!(reported, set([""]));
    let json = fx.file("a.json").await;
    assert_eq!(
        (json.id, json.kind.as_str(), json.states()),
        (json_id, "other", (0, 0, 0))
    );
    let zzz = fx.file("a.zzz").await;
    assert_eq!((zzz.kind.as_str(), zzz.states()), ("sidecar", (3, 3, 3)));
    assert_eq!(
        fx.links().await,
        [
            ("a.jpg".to_owned(), "a.zzz".to_owned(), "stem".to_owned()),
            ("a.json".to_owned(), "a.zzz".to_owned(), "stem".to_owned()),
        ]
    );
    let owner = fx.file("a.jpg").await;
    assert_eq!(
        owner.states(),
        (0, 1, 1),
        "the owner's metadata is read again, its hash and thumbnail stay"
    );
}

#[tokio::test]
async fn changing_a_sidecar_queues_its_owners_for_extraction_again() {
    let fx = Fixture::new().await;
    fx.tree.write("p/IMG_1.jpg", b"x");
    fx.tree.write("p/IMG_2.jpg", b"x");
    fx.scan().await;
    fx.mark_done().await;

    // A sidecar appears.
    fx.tree.write("p/IMG_1.jpg.xmp", b"<x/>");
    fx.scan().await;
    assert_eq!(
        fx.file("p/IMG_1.jpg").await.states(),
        (0, 1, 1),
        "owner of the new sidecar"
    );
    assert_eq!(
        fx.file("p/IMG_2.jpg").await.states(),
        (1, 1, 1),
        "unrelated file"
    );

    // It is edited.
    fx.mark_done().await;
    fx.tree
        .write("p/IMG_1.jpg.xmp", b"<x><rating>5</rating></x>");
    fx.scan().await;
    assert_eq!(
        fx.file("p/IMG_1.jpg").await.states(),
        (0, 1, 1),
        "owner of the edited sidecar"
    );
    assert_eq!(fx.file("p/IMG_2.jpg").await.states(), (1, 1, 1));

    // It is deleted.
    fx.mark_done().await;
    fx.tree.remove("p/IMG_1.jpg.xmp");
    fx.scan().await;
    assert_eq!(
        fx.file("p/IMG_1.jpg").await.states(),
        (0, 1, 1),
        "owner of the removed sidecar"
    );
    assert_eq!(fx.file("p/IMG_2.jpg").await.states(), (1, 1, 1));
    assert!(fx.links().await.is_empty());
}

#[tokio::test]
async fn an_offline_volume_is_not_scanned_and_never_purged() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    fx.scan().await;
    let files = fx.paths().await;
    let dirs = fx.dir_paths().await;

    let away = fx.app.dir.path().join("away");
    std::fs::rename(fx.tree.root(), &away).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    assert_eq!(fx.volume_row().await.online, 0);

    let summary = fx.scan().await;
    assert_eq!((summary.volumes_scanned, summary.volumes_offline), (0, 1));
    let summary = fx.scan_with(&only(vec![ScanTarget::volume(fx.id)])).await;
    assert_eq!((summary.volumes_scanned, summary.volumes_offline), (0, 1));
    assert_eq!(fx.paths().await, files);
    assert_eq!(fx.dir_paths().await, dirs);

    std::fs::rename(&away, fx.tree.root()).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    assert_eq!(fx.volume_row().await.online, 1);
    let summary = fx.scan().await;
    assert_eq!(
        (
            summary.added,
            summary.changed,
            summary.removed,
            summary.unchanged
        ),
        (0, 0, 0, 7)
    );
}

#[tokio::test]
async fn a_volume_whose_root_vanished_is_never_purged() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    fx.scan().await;
    fx.sql("UPDATE volumes SET last_scan_at = 5").await;
    let files = fx.paths().await;

    // The registry has not noticed yet: the scan finds out by itself.
    std::fs::rename(fx.tree.root(), fx.app.dir.path().join("away")).unwrap();
    let summary = fx.scan().await;

    assert_eq!((summary.volumes_scanned, summary.volumes_offline), (0, 1));
    assert_eq!(fx.paths().await, files);
    assert_eq!(fx.dir_paths().await.len(), 4);
    assert_eq!(
        fx.volume_row().await.last_scan_at,
        Some(5),
        "an unfinished scan is not recorded"
    );
}

#[tokio::test]
async fn a_mass_disappearance_is_held_until_the_user_confirms_it() {
    let fx = Fixture::new().await;
    fx.tree.populate(2, 50); // 100 files
    fx.scan().await;
    assert_eq!(fx.file_count().await, 100);

    // An unmounted share looks like this: the mount point is there, and empty.
    let away = fx.unmount();
    let (summary, reported) = fx.scan_reported().await;
    assert_eq!((summary.removed, summary.held), (0, 100));
    assert!(reported.is_empty());
    assert_eq!(fx.file_count().await, 100, "nothing was removed");
    assert_eq!(fx.dir_paths().await, ["", "d000", "d001"]);
    assert_eq!(fx.volume_row().await.held_removals, 100);
    assert_eq!(
        fx.app.state.indexer.held_removals().await.unwrap(),
        [HeldRemovals {
            volume_id: fx.id,
            volume_name: "photos".to_owned(),
            files: 100
        }]
    );

    // Forcing a re-index is not a confirmation.
    let summary = fx
        .scan_with(&ScanRequest {
            force: true,
            ..ScanRequest::default()
        })
        .await;
    assert_eq!((summary.removed, summary.held), (0, 100));
    assert_eq!(fx.file_count().await, 100);

    // The share comes back: the held removals are void.
    fx.remount(away);
    let summary = fx.scan().await;
    assert_eq!(
        (summary.held, summary.removed, summary.unchanged),
        (0, 0, 100)
    );
    assert_eq!(fx.volume_row().await.held_removals, 0);
    assert!(
        fx.app
            .state
            .indexer
            .held_removals()
            .await
            .unwrap()
            .is_empty()
    );

    // And once more, this time the user confirms.
    let away = fx.unmount();
    assert_eq!(fx.scan().await.held, 100);
    let (summary, reported) = fx.scan_reported_with(&confirmed()).await;
    assert_eq!(
        (summary.removed, summary.held, summary.dirs_removed),
        (100, 0, 2)
    );
    assert_eq!(fx.file_count().await, 0);
    assert_eq!(fx.dir_paths().await, [""]);
    assert_eq!(fx.volume_row().await.held_removals, 0);
    assert!(
        fx.app
            .state
            .indexer
            .held_removals()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(reported, set(["", "d000", "d001"]));
    drop(away);
}

#[tokio::test]
async fn files_vanishing_from_directories_that_remain_are_held_too() {
    let fx = Fixture::new().await;
    fx.tree.populate(2, 50);
    fx.scan().await;
    for dir in 0..2 {
        for file in 0..50 {
            fx.tree.remove(&format!("d{dir:03}/IMG_{file:04}.jpg"));
        }
    }

    let summary = fx.scan().await;
    assert_eq!((summary.removed, summary.held), (0, 100));
    assert_eq!(fx.file_count().await, 100);

    let summary = fx.scan_with(&confirmed()).await;
    assert_eq!((summary.removed, summary.held), (100, 0));
    assert_eq!(fx.file_count().await, 0);
    assert_eq!(
        fx.dir_paths().await,
        ["", "d000", "d001"],
        "the directories are still there"
    );
}

#[tokio::test]
async fn the_guard_trips_above_twenty_percent_of_the_rows() {
    for (removed, held) in [(60, false), (61, true)] {
        let fx = Fixture::new().await;
        fx.tree.populate(3, 100); // 300 rows
        fx.scan().await;
        for file in 0..removed {
            fx.tree.remove(&format!("d000/IMG_{file:04}.jpg"));
        }

        let summary = fx.scan().await;

        if held {
            assert_eq!(
                (summary.removed, summary.held),
                (0, removed),
                "{removed} of 300"
            );
            assert_eq!(fx.file_count().await, 300);
        } else {
            assert_eq!(
                (summary.removed, summary.held),
                (removed, 0),
                "{removed} of 300"
            );
            assert_eq!(fx.file_count().await, 300 - i64::try_from(removed).unwrap());
        }
    }
}

#[tokio::test]
async fn the_guard_leaves_small_volumes_alone() {
    for (files, held) in [(49, false), (50, true)] {
        let fx = Fixture::new().await;
        fx.tree.populate(1, files);
        fx.scan().await;
        fx.tree.remove_dir("d000");

        let summary = fx.scan().await;

        let files = u64::try_from(files).unwrap();
        if held {
            assert_eq!((summary.removed, summary.held), (0, files));
        } else {
            assert_eq!((summary.removed, summary.held), (files, 0));
            assert_eq!(fx.file_count().await, 0);
        }
    }
}

#[tokio::test]
async fn a_scoped_scan_is_guarded_against_the_rows_of_the_whole_volume() {
    let fx = Fixture::new().await;
    for file in 0..60 {
        fx.tree.write(&format!("x/{file}.jpg"), b"x");
    }
    for file in 0..40 {
        fx.tree.write(&format!("y/{file}.jpg"), b"x");
    }
    fx.scan().await;
    fx.tree.remove_dir("x");

    let summary = fx
        .scan_with(&only(vec![ScanTarget::subtree(fx.id, rel("x"))]))
        .await;

    assert_eq!((summary.removed, summary.held), (0, 60));
    assert_eq!(fx.file_count().await, 100);
    assert_eq!(fx.volume_row().await.held_removals, 60);

    let summary = fx.scan_with(&confirmed()).await;
    assert_eq!((summary.removed, summary.held), (60, 0));
    assert_eq!(fx.volume_row().await.held_removals, 0);
}

#[tokio::test]
async fn a_non_recursive_scan_handles_one_directory_and_removes_vanished_subdirectories() {
    let fx = Fixture::new().await;
    fx.tree.write("a/1.jpg", b"x");
    fx.tree.write("a/gone/g1.jpg", b"x");
    fx.tree.write("a/gone/deeper/g2.jpg", b"x");
    fx.tree.write("a/sub/s1.jpg", b"x");
    fx.tree.write("b/b1.jpg", b"x");
    fx.scan().await;

    fx.tree.remove_dir("a/gone");
    fx.tree.write("a/n.jpg", b"x");
    fx.tree.write("a/fresh/x.jpg", b"x");
    fx.tree.write("a/sub/s2.jpg", b"x");
    fx.tree.write("b/b2.jpg", b"x");

    let (summary, reported) = fx
        .scan_reported_with(&only(vec![ScanTarget::dir(fx.id, rel("a"))]))
        .await;

    assert_eq!(
        (
            summary.added,
            summary.removed,
            summary.dirs_added,
            summary.dirs_removed
        ),
        (1, 2, 1, 2)
    );
    assert_eq!(summary.dirs_visited, 1, "only `a` is listed");
    assert_eq!(
        fx.paths().await,
        ["a/1.jpg", "a/n.jpg", "a/sub/s1.jpg", "b/b1.jpg"],
        "new files below a sub-directory wait for a scan of that directory"
    );
    assert_eq!(fx.dir_paths().await, ["", "a", "a/fresh", "a/sub", "b"]);
    assert_eq!(reported, set(["a", "a/fresh", "a/gone", "a/gone/deeper"]));

    // The same scan again finds nothing to do.
    let (summary, reported) = fx
        .scan_reported_with(&only(vec![ScanTarget::dir(fx.id, rel("a"))]))
        .await;
    assert_eq!(
        (
            summary.added,
            summary.changed,
            summary.removed,
            summary.dirs_added
        ),
        (0, 0, 0, 0)
    );
    assert!(reported.is_empty());
}

#[tokio::test]
async fn a_scoped_scan_only_touches_its_scope() {
    let fx = Fixture::new().await;
    fx.tree.write("a/1.jpg", b"x");
    fx.tree.write("a/2.jpg", b"x");
    fx.tree.write("a/sub/3.jpg", b"x");
    fx.tree.write("b/4.jpg", b"x");
    fx.tree.write("b/5.jpg", b"x");
    fx.scan().await;
    fx.sql("UPDATE volumes SET last_scan_at = 5").await;

    fx.tree.write("a/new.jpg", b"x");
    fx.tree.write("a/sub/new2.jpg", b"x");
    fx.tree.write("b/new3.jpg", b"x");
    fx.tree.remove("a/2.jpg");
    fx.tree.remove("b/4.jpg");

    let (summary, reported) = fx
        .scan_reported_with(&only(vec![ScanTarget::subtree(fx.id, rel("a"))]))
        .await;

    assert_eq!(
        (
            summary.dirs_visited,
            summary.files_seen,
            summary.added,
            summary.removed
        ),
        (2, 4, 2, 1)
    );
    assert_eq!(
        fx.paths().await,
        [
            "a/1.jpg",
            "a/new.jpg",
            "a/sub/3.jpg",
            "a/sub/new2.jpg",
            "b/4.jpg",
            "b/5.jpg"
        ],
        "`b` is exactly as it was"
    );
    assert_eq!(reported, set(["a", "a/sub"]));
    assert_eq!(
        fx.volume_row().await.last_scan_at,
        Some(5),
        "a part of a volume is not a full scan"
    );

    fx.scan().await;
    assert_eq!(fx.paths().await.len(), 6);
    assert!(fx.find("b/new3.jpg").await.is_some());
    assert!(fx.find("b/4.jpg").await.is_none());
    assert!(fx.volume_row().await.last_scan_at.unwrap() > 5);
}

#[tokio::test]
async fn several_targets_of_one_volume_are_scanned_together() {
    let fx = Fixture::new().await;
    fx.tree.write("a/1.jpg", b"x");
    fx.tree.write("b/2.jpg", b"x");
    fx.tree.write("c/3.jpg", b"x");

    let summary = fx
        .scan_with(&only(vec![
            ScanTarget::subtree(fx.id, rel("a")),
            ScanTarget::subtree(fx.id, rel("b")),
            ScanTarget::subtree(fx.id, rel("a/deeper")),
        ]))
        .await;

    assert_eq!(summary.volumes_scanned, 1);
    assert_eq!(fx.paths().await, ["a/1.jpg", "b/2.jpg"]);
    assert_eq!(
        fx.volume_row().await.scan_gen,
        1,
        "one generation for the whole request"
    );
}

#[tokio::test]
async fn the_first_scan_may_be_a_scan_of_a_sub_directory() {
    let fx = Fixture::new().await;
    fx.tree.write("x/y/z/f.jpg", b"x");
    fx.tree.write("x/y/g.jpg", b"x");
    fx.tree.write("x/other.jpg", b"x");

    let (summary, reported) = fx
        .scan_reported_with(&only(vec![ScanTarget::subtree(fx.id, rel("x/y"))]))
        .await;

    assert_eq!(summary.added, 2);
    assert_eq!(fx.paths().await, ["x/y/g.jpg", "x/y/z/f.jpg"]);
    assert_eq!(
        fx.dir_paths().await,
        ["", "x", "x/y", "x/y/z"],
        "the directories above the scope get a row, so the tree can be browsed"
    );
    assert_eq!(reported, set(["", "x", "x/y", "x/y/z"]));
    assert!(fx.volume_row().await.last_scan_at.is_none());
}

#[tokio::test]
async fn a_scoped_scan_of_a_vanished_directory_removes_it() {
    let fx = Fixture::new().await;
    fx.tree.write("a/1.jpg", b"x");
    fx.tree.write("a/sub/2.jpg", b"x");
    fx.tree.write("b/3.jpg", b"x");
    fx.scan().await;

    fx.tree.remove_dir("a");
    let summary = fx
        .scan_with(&only(vec![ScanTarget::subtree(fx.id, rel("a"))]))
        .await;
    assert_eq!((summary.removed, summary.dirs_removed), (2, 2));
    assert_eq!(fx.paths().await, ["b/3.jpg"]);
    assert_eq!(fx.dir_paths().await, ["", "b"]);

    // A directory that never existed is nothing to worry about.
    let summary = fx
        .scan_with(&only(vec![
            ScanTarget::subtree(fx.id, rel("never/was")),
            ScanTarget::dir(fx.id, rel("a")),
        ]))
        .await;
    assert_eq!(
        (summary.added, summary.removed, summary.dirs_removed),
        (0, 0, 0)
    );
    assert_eq!(fx.paths().await, ["b/3.jpg"]);
}

#[tokio::test]
async fn two_volumes_are_scanned_in_one_run() {
    for concurrency in [1, 2] {
        let app = TestApp::new().await;
        change_settings(&app, |settings| settings.index.concurrency = concurrency).await;
        let (alpha, alpha_tree) = app.add_volume("alpha").await;
        let (beta, beta_tree) = app.add_volume("beta").await;
        alpha_tree.populate(3, 10);
        beta_tree.populate(2, 5);

        let changes = Arc::new(RecordingChanges::new());
        let ctx = RunContext::new(
            CancellationToken::new(),
            Arc::new(NoopProgress),
            changes.clone(),
        );
        let summary = app
            .state
            .indexer
            .scan(&ScanRequest::default(), &ctx)
            .await
            .unwrap();

        assert_eq!(
            (summary.volumes_scanned, summary.added, summary.files_seen),
            (2, 40, 40),
            "concurrency {concurrency}"
        );
        for (volume, files) in [(alpha, 30), (beta, 10)] {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE volume_id = ?")
                .bind(volume)
                .fetch_one(app.state.db.read())
                .await
                .unwrap();
            assert_eq!(count, files, "volume {volume}, concurrency {concurrency}");
            let generation: i64 = sqlx::query_scalar(
                "SELECT scan_gen FROM volumes WHERE id = ? AND last_scan_at IS NOT NULL",
            )
            .bind(volume)
            .fetch_one(app.state.db.read())
            .await
            .unwrap();
            assert_eq!(generation, 1);
        }
        assert_eq!(changes.dirs(alpha), set(["", "d000", "d001", "d002"]));
        assert_eq!(changes.dirs(beta), set(["", "d000", "d001"]));
    }
}

#[tokio::test]
async fn an_unknown_volume_fails_the_request_before_anything_is_scanned() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"x");

    let request = only(vec![ScanTarget::volume(fx.id), ScanTarget::volume(9999)]);
    let err = fx
        .try_scan(&request, &RunContext::noop())
        .await
        .unwrap_err();

    assert!(matches!(err, IndexError::UnknownVolume(9999)), "{err:?}");
    assert_eq!(fx.file_count().await, 0);
    assert_eq!(fx.volume_row().await.scan_gen, 0);
}

#[tokio::test]
async fn concurrent_scans_of_one_volume_run_one_after_the_other() {
    let fx = Fixture::new().await;
    fx.tree.populate(5, 20);

    let request = ScanRequest::default();
    let ctx = RunContext::noop();
    let (first, second) = tokio::join!(fx.try_scan(&request, &ctx), fx.try_scan(&request, &ctx));
    let (first, second) = (first.unwrap(), second.unwrap());

    assert_eq!(first.added + second.added, 100, "every file is added once");
    assert_eq!(first.unchanged + second.unchanged, 100);
    assert_eq!(fx.file_count().await, 100);
    assert_eq!(fx.volume_row().await.scan_gen, 2);
}

/// Cancels the run as soon as a directory with files has been committed.
struct CancelAfterFirstFiles {
    token: CancellationToken,
    reports: AtomicUsize,
}

impl ChangeSink for CancelAfterFirstFiles {
    fn dirs_changed(&self, _volume_id: VolumeId, dirs: &[RelPath]) {
        self.reports.fetch_add(1, Ordering::SeqCst);
        if dirs.iter().any(|dir| !dir.is_root()) {
            self.token.cancel();
        }
    }
}

#[tokio::test]
async fn a_cancelled_scan_leaves_whole_directories_and_removes_nothing() {
    let fx = Fixture::new().await;
    for file in 0..3 {
        fx.tree.write(&format!("old/{file}.jpg"), b"x");
    }
    fx.scan().await;
    fx.sql("UPDATE volumes SET last_scan_at = 5").await;
    fx.tree.remove_dir("old");
    fx.tree.populate(3, 1000); // three directories of about one write transaction each

    let token = CancellationToken::new();
    let sink = Arc::new(CancelAfterFirstFiles {
        token: token.clone(),
        reports: AtomicUsize::new(0),
    });
    let ctx = RunContext::new(token, Arc::new(NoopProgress), sink.clone());
    let err = fx
        .try_scan(&ScanRequest::default(), &ctx)
        .await
        .unwrap_err();

    assert!(matches!(err, IndexError::Cancelled), "{err:?}");
    assert!(sink.reports.load(Ordering::SeqCst) >= 1);
    let per_dir: Vec<(String, i64)> = sqlx::query_as(
        "SELECT dir_path, COUNT(*) FROM files WHERE dir_path LIKE 'd%' GROUP BY dir_path",
    )
    .fetch_all(fx.app.state.db.read())
    .await
    .unwrap();
    assert!(
        !per_dir.is_empty() && per_dir.len() < 3,
        "some but not all directories were written: {per_dir:?}"
    );
    for (dir, count) in &per_dir {
        assert_eq!(*count, 1000, "{dir} is written completely or not at all");
    }
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM files WHERE dir_path = 'old'")
            .await,
        3,
        "a scan that did not reach its end removes nothing"
    );
    assert_eq!(fx.volume_row().await.last_scan_at, Some(5));

    let indexed = i64::try_from(per_dir.len()).unwrap() * 1000;
    let summary = fx.scan().await;
    assert_eq!(i64::try_from(summary.added).unwrap(), 3000 - indexed);
    assert_eq!(summary.unchanged, u64::try_from(indexed).unwrap());
    assert_eq!((summary.removed, summary.dirs_removed), (3, 1));
    assert_eq!(fx.file_count().await, 3000);
    assert!(fx.volume_row().await.last_scan_at.unwrap() > 5);
}

#[tokio::test]
async fn a_scan_that_is_cancelled_up_front_does_nothing() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"x");
    let token = CancellationToken::new();
    token.cancel();
    let ctx = RunContext::new(token, Arc::new(NoopProgress), Arc::new(NoopChanges));

    let err = fx
        .try_scan(&ScanRequest::default(), &ctx)
        .await
        .unwrap_err();

    assert!(matches!(err, IndexError::Cancelled), "{err:?}");
    assert_eq!(fx.file_count().await, 0);
    assert_eq!(fx.volume_row().await.scan_gen, 0);
}

#[tokio::test]
async fn run_returns_the_counts_of_the_scan() {
    let fx = Fixture::new().await;
    build_sample(&fx.tree);
    let run = |request: ScanRequest| {
        let indexer = fx.app.state.indexer.clone();
        async move { indexer.run(&request, &RunContext::noop()).await.unwrap() }
    };

    let summary = run(ScanRequest::default()).await;
    assert_eq!(
        summary,
        IndexSummary {
            added: 7,
            ..IndexSummary::default()
        }
    );

    assert_eq!(run(ScanRequest::default()).await, IndexSummary::default());

    fx.tree.remove("notes.txt");
    fx.tree.write("new.jpg", b"x");
    fx.tree.write("changed.jpg", b"x");
    fx.scan().await;
    fx.tree.write("changed.jpg", b"xx");
    assert_eq!(
        run(ScanRequest::default()).await,
        IndexSummary {
            changed: 1,
            ..IndexSummary::default()
        }
    );
}

#[tokio::test]
#[ignore = "builds 50,000 files; run with --ignored --nocapture"]
async fn performance_of_50k_files() {
    let fx = Fixture::new().await;
    let started = Instant::now();
    fx.tree.populate(500, 100);
    println!("built 50k files in {:?}", started.elapsed());

    let started = Instant::now();
    let summary = fx.scan().await;
    println!("first scan: {:?} ({summary:?})", started.elapsed());
    assert_eq!(summary.added, 50_000);

    let before = fx.total_changes().await;
    let started = Instant::now();
    let summary = fx.scan().await;
    println!("unchanged rescan: {:?}", started.elapsed());
    assert_eq!(summary.unchanged, 50_000);
    assert_eq!(fx.total_changes().await - before, 2);

    for dir in 0..500 {
        for file in 0..5 {
            fx.tree
                .set_mtime(&format!("d{dir:03}/IMG_{file:04}.jpg"), BASE_MTIME + 1);
        }
    }
    let started = Instant::now();
    let summary = fx.scan().await;
    println!("rescan with 1% changed: {:?}", started.elapsed());
    assert_eq!(summary.changed, 2500);
}

/// Scans the generated fixture volumes when `make fixtures` has produced them.
#[tokio::test]
async fn the_generated_fixture_volumes_can_be_scanned() {
    let volumes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out/volumes");
    let Ok(volumes) = volumes.canonicalize() else {
        eprintln!("fixtures/out/volumes does not exist (run `make fixtures`): skipping");
        return;
    };
    let app = TestApp::with_config(|config| config.volumes_dir = volumes).await;
    app.state.volumes.refresh().await.unwrap();

    let first = app
        .state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    println!("fixtures: {first:?}");
    assert_eq!(i64::try_from(first.added).unwrap(), rows);
    assert_eq!(first.held, 0);

    let second = app
        .state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    assert_eq!(
        (second.added, second.changed, second.removed),
        (0, 0, 0),
        "an untouched tree is unchanged"
    );
}
