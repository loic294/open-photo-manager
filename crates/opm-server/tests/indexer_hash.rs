//! The hash stage and `sha1_file` over real directory trees.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TestApp, Tree};
use opm_core::hash::{sha1_bytes, to_hex};
use opm_core::paths::{RelPath, VolumeId};
use opm_server::indexer::hash::{HashError, HashLimits, HashSummary, sha1_file};
use opm_server::indexer::{
    IndexPhase, IndexProgress, NoopChanges, ProgressSink, RecordingProgress, RunContext,
    ScanRequest, ScanTarget,
};
use opm_server::security::{FsError, VolumeRoot};
use tokio_util::sync::CancellationToken;

const EMPTY: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
const ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
const MILLION_A: &str = "34aa973cd4c4daa4f61eeb2bdbad27316534016f";
const MIB_ZEROS: &str = "3b71f43ff30f4b15b5cd85dd9e95ebc7e84eb5a3";
const ODD_SIZE: &str = "b7bb3c8af459d9c569b5a75d1ebc5a084d9074b9";

fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap()
}

fn odd_content() -> Vec<u8> {
    (0..2_500_000_u32)
        .map(|i| ((i * 7 + 3) % 256) as u8)
        .collect()
}

fn hex_of(root: &VolumeRoot, path: &str) -> String {
    let cancel = CancellationToken::new();
    to_hex(&sha1_file(root, &rel(path), &cancel, &mut |_| {}).unwrap())
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

    fn root(&self) -> VolumeRoot {
        VolumeRoot::new(self.id, self.tree.root()).unwrap()
    }

    async fn scan(&self) {
        self.app
            .state
            .indexer
            .scan(&ScanRequest::default(), &RunContext::noop())
            .await
            .unwrap();
    }

    async fn hash(&self) -> HashSummary {
        self.hash_with(&ScanRequest::default(), HashLimits::default())
            .await
    }

    async fn hash_with(&self, req: &ScanRequest, limits: HashLimits) -> HashSummary {
        self.hash_ctx(req, &RunContext::noop(), limits).await
    }

    async fn hash_ctx(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
        limits: HashLimits,
    ) -> HashSummary {
        self.app
            .state
            .indexer
            .hash_pending(req, ctx, limits)
            .await
            .unwrap()
    }

    /// `(hash_state, hex of the hash)` of the file `name` (in any directory).
    async fn row(&self, name: &str) -> (i64, Option<String>) {
        let (state, hash): (i64, Option<Vec<u8>>) =
            sqlx::query_as("SELECT hash_state, hash FROM files WHERE name = ?")
                .bind(name)
                .fetch_one(self.app.state.db.read())
                .await
                .unwrap();
        (
            state,
            hash.map(|bytes| to_hex(&opm_core::hash::from_slice(&bytes).unwrap())),
        )
    }

    async fn state(&self, name: &str) -> i64 {
        self.row(name).await.0
    }
}

fn contexts(sink: Arc<dyn ProgressSink>) -> RunContext {
    RunContext::new(CancellationToken::new(), sink, Arc::new(NoopChanges))
}

#[tokio::test]
async fn known_vectors() {
    let fx = Fixture::new().await;
    fx.tree.write("empty", b"");
    fx.tree.write("abc", b"abc");
    fx.tree.write("million", &vec![b'a'; 1_000_000]);
    fx.tree.write("zeros", &vec![0; 1 << 20]);
    fx.tree.write("odd", &odd_content());
    let root = fx.root();

    assert_eq!(hex_of(&root, "empty"), EMPTY);
    assert_eq!(hex_of(&root, "abc"), ABC);
    assert_eq!(hex_of(&root, "million"), MILLION_A);
    assert_eq!(hex_of(&root, "zeros"), MIB_ZEROS);
    assert_eq!(hex_of(&root, "odd"), ODD_SIZE);
}

#[tokio::test]
async fn streaming_equals_one_shot_and_reports_every_byte() {
    let fx = Fixture::new().await;
    let content = odd_content();
    fx.tree.write("odd", &content);
    let mut seen = 0;
    let digest = sha1_file(
        &fx.root(),
        &rel("odd"),
        &CancellationToken::new(),
        &mut |n| seen += n,
    )
    .unwrap();
    assert_eq!(digest, sha1_bytes(&content));
    assert_eq!(seen, content.len() as u64);
}

#[tokio::test]
async fn cancelling_stops_a_huge_file_quickly() {
    let fx = Fixture::new().await;
    let path = fx.tree.path("huge.bin");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(32 << 30)
        .unwrap();
    let cancel = CancellationToken::new();
    let root = fx.root();
    let worker = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = sha1_file(&root, &rel("huge.bin"), &cancel, &mut |_| {});
            (result, started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    cancel.cancel();
    let (result, elapsed) = worker.join().unwrap();
    assert_eq!(result, Err(HashError::Cancelled));
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
}

#[tokio::test]
async fn symlinks_and_missing_files_are_gone_not_failures() {
    let fx = Fixture::new().await;
    fx.tree.write("real", b"abc");
    symlink(fx.tree.path("real"), fx.tree.path("link")).unwrap();
    let root = fx.root();
    let cancel = CancellationToken::new();
    assert!(matches!(
        sha1_file(&root, &rel("link"), &cancel, &mut |_| {}),
        Err(HashError::Gone(FsError::SymlinkRejected(_)))
    ));
    assert!(matches!(
        sha1_file(&root, &rel("nope"), &cancel, &mut |_| {}),
        Err(HashError::Gone(FsError::NotFound(_)))
    ));
}

#[tokio::test]
async fn the_stage_hashes_every_pending_file_and_a_second_run_does_nothing() {
    let fx = Fixture::new().await;
    fx.tree.write("a/abc.jpg", b"abc");
    fx.tree.write("b/empty.jpg", b"");
    fx.tree.write("b/copy.jpg", b"abc");
    fx.scan().await;

    let summary = fx.hash().await;
    assert_eq!(
        summary,
        HashSummary {
            hashed: 3,
            failed: 0,
            skipped_changed: 0,
            remaining: 0
        }
    );
    assert_eq!(fx.row("abc.jpg").await, (1, Some(ABC.to_owned())));
    assert_eq!(fx.row("copy.jpg").await, (1, Some(ABC.to_owned())));
    assert_eq!(fx.row("empty.jpg").await, (1, Some(EMPTY.to_owned())));

    assert_eq!(fx.hash().await, HashSummary::default());
}

#[tokio::test]
async fn run_counts_hashed_files_in_its_summary() {
    let fx = Fixture::new().await;
    fx.tree.write("one.jpg", b"1");
    fx.tree.write("two.jpg", b"22");
    let summary = fx
        .app
        .state
        .indexer
        .run(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
    assert_eq!((summary.added, summary.hashed, summary.failed), (2, 2, 0));
}

#[tokio::test]
async fn size_twins_are_hashed_first_across_volumes_and_sidecars_do_not_count() {
    let app = TestApp::new().await;
    let (first, tree1) = app.add_volume("a").await;
    let (_second, tree2) = app.add_volume("b").await;
    // Sorted before the twins, unique sizes: only their sidecar shares a size.
    tree1.write("a_unique.jpg", b"12345");
    tree1.write("a_unique.xmp", b"12345");
    tree1.write("b_unique.jpg", b"1");
    tree1.write("twin.jpg", b"seven77");
    tree2.write("twin.jpg", b"SEVEN77");
    tree2.write("z.jpg", b"zz");
    app.state
        .indexer
        .scan(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();

    let summary = app
        .state
        .indexer
        .hash_pending(
            &ScanRequest::default(),
            &RunContext::noop(),
            HashLimits {
                max_files: Some(2),
                max_duration: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(summary.hashed, 2);
    assert_eq!(summary.remaining, 3);
    let done: Vec<(i64, String)> =
        sqlx::query_as("SELECT volume_id, name FROM files WHERE hash_state = 1 ORDER BY volume_id")
            .fetch_all(app.state.db.read())
            .await
            .unwrap();
    assert_eq!(
        done,
        vec![
            (first, "twin.jpg".to_owned()),
            (first + 1, "twin.jpg".to_owned())
        ]
    );
}

#[tokio::test]
async fn the_scope_limits_the_stage() {
    let fx = Fixture::new().await;
    fx.tree.write("in/a.jpg", b"a");
    fx.tree.write("out/b.jpg", b"bb");
    fx.scan().await;
    let req = ScanRequest {
        targets: vec![ScanTarget::subtree(fx.id, rel("in"))],
        ..ScanRequest::default()
    };
    let summary = fx.hash_with(&req, HashLimits::default()).await;
    assert_eq!((summary.hashed, summary.remaining), (1, 0));
    assert_eq!(fx.state("a.jpg").await, 1);
    assert_eq!(fx.state("b.jpg").await, 0);
}

#[tokio::test]
async fn limits_stop_early_and_the_next_call_finishes() {
    let fx = Fixture::new().await;
    for index in 0..5 {
        fx.tree
            .write(&format!("f{index}.jpg"), &vec![b'x'; index + 1]);
    }
    fx.scan().await;
    let req = ScanRequest::default();

    let none = fx
        .hash_with(
            &req,
            HashLimits {
                max_files: None,
                max_duration: Some(Duration::ZERO),
            },
        )
        .await;
    assert_eq!((none.hashed, none.remaining), (0, 5));

    let some = fx
        .hash_with(
            &req,
            HashLimits {
                max_files: Some(2),
                max_duration: None,
            },
        )
        .await;
    assert_eq!((some.hashed, some.remaining), (2, 3));

    let rest = fx.hash().await;
    assert_eq!((rest.hashed, rest.remaining), (3, 0));
}

#[tokio::test]
async fn an_unreadable_file_fails_and_the_rest_goes_on() {
    let fx = Fixture::new().await;
    fx.tree.write("locked.jpg", b"secret");
    fx.tree.write("fine.jpg", b"abc");
    fx.scan().await;
    let path = fx.tree.path("locked.jpg");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        return; // running as root
    }

    let summary = fx.hash().await;
    assert_eq!(
        (summary.hashed, summary.failed, summary.remaining),
        (1, 1, 0)
    );
    assert_eq!(fx.state("locked.jpg").await, 2);
    assert_eq!(fx.state("fine.jpg").await, 1);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[tokio::test]
async fn vanished_replaced_and_changed_files_stay_pending() {
    let fx = Fixture::new().await;
    fx.tree.write("gone.jpg", b"1");
    fx.tree.write("linked.jpg", b"22");
    fx.tree.write("grown.jpg", b"333");
    fx.tree.write("touched.jpg", b"4444");
    fx.tree.write("target.jpg", b"55555");
    fx.scan().await;
    fx.tree.remove("gone.jpg");
    fx.tree.remove("linked.jpg");
    symlink(fx.tree.path("target.jpg"), fx.tree.path("linked.jpg")).unwrap();
    fx.tree.write("grown.jpg", b"3333");
    fx.tree.set_mtime("touched.jpg", common::BASE_MTIME + 5);

    let summary = fx.hash().await;
    assert_eq!(summary.hashed, 1, "only target.jpg is intact");
    assert_eq!(summary.skipped_changed, 2);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.remaining, 4);
    for name in ["gone.jpg", "linked.jpg", "grown.jpg", "touched.jpg"] {
        assert_eq!(fx.row(name).await, (0, None), "{name}");
    }
}

/// Runs `action` on the first report that carries bytes, from the hashing thread.
struct OnFirstBytes {
    action: Box<dyn Fn() + Send + Sync>,
    fired: std::sync::Once,
}

impl ProgressSink for OnFirstBytes {
    fn report(&self, progress: IndexProgress) {
        if progress.bytes_done > 0 {
            self.fired.call_once(|| (self.action)());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_modified_during_the_read_is_discarded() {
    let fx = Fixture::new().await;
    fx.tree.write("busy.jpg", &vec![b'x'; 100_000]);
    fx.scan().await;
    let path = fx.tree.path("busy.jpg");
    let sink = Arc::new(OnFirstBytes {
        action: Box::new(move || {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"more").unwrap();
        }),
        fired: std::sync::Once::new(),
    });
    let summary = fx
        .hash_ctx(
            &ScanRequest::default(),
            &contexts(sink),
            HashLimits::default(),
        )
        .await;
    assert_eq!((summary.hashed, summary.skipped_changed), (0, 1));
    assert_eq!(fx.row("busy.jpg").await, (0, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_changed_meanwhile_never_gets_a_stale_hash() {
    let fx = Fixture::new().await;
    fx.tree.write("race.jpg", &vec![b'x'; 100_000]);
    fx.scan().await;
    let db = fx.app.state.db.clone();
    let handle = tokio::runtime::Handle::current();
    let sink = Arc::new(OnFirstBytes {
        action: Box::new(move || {
            handle.block_on(async {
                sqlx::query("UPDATE files SET mtime = mtime + 1")
                    .execute(db.write())
                    .await
                    .unwrap();
            });
        }),
        fired: std::sync::Once::new(),
    });
    let summary = fx
        .hash_ctx(
            &ScanRequest::default(),
            &contexts(sink),
            HashLimits::default(),
        )
        .await;
    assert_eq!((summary.hashed, summary.skipped_changed), (0, 1));
    assert_eq!(fx.row("race.jpg").await, (0, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_mid_run_leaves_the_row_pending() {
    let fx = Fixture::new().await;
    fx.tree.write("big.bin", &vec![b'x'; 3 << 20]);
    fx.scan().await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let sink = Arc::new(OnFirstBytes {
        action: Box::new(move || trigger.cancel()),
        fired: std::sync::Once::new(),
    });
    let ctx = RunContext::new(cancel, sink, Arc::new(NoopChanges));
    let summary = fx
        .hash_ctx(&ScanRequest::default(), &ctx, HashLimits::default())
        .await;
    assert_eq!(
        (summary.hashed, summary.failed, summary.remaining),
        (0, 0, 1)
    );
    assert_eq!(fx.row("big.bin").await, (0, None));
    assert_eq!(fx.hash().await.hashed, 1, "the next run resumes");
}

#[tokio::test]
async fn an_offline_volume_is_skipped_without_a_state_change() {
    let fx = Fixture::new().await;
    fx.tree.write("a.jpg", b"abc");
    fx.scan().await;
    std::fs::remove_dir_all(fx.tree.root()).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    let summary = fx.hash().await;
    assert_eq!(summary.hashed + summary.failed, 0);
    assert_eq!(fx.state("a.jpg").await, 0);
}

#[tokio::test]
async fn progress_totals_add_up_across_both_phases() {
    let fx = Fixture::new().await;
    fx.tree.write("twin1.jpg", b"12345");
    fx.tree.write("twin2.jpg", b"abcde");
    fx.tree.write("solo.jpg", &vec![b's'; 1000]);
    fx.tree.write("zero.jpg", b"");
    fx.scan().await;
    let progress = Arc::new(RecordingProgress::new());
    let summary = fx
        .hash_ctx(
            &ScanRequest::default(),
            &contexts(progress.clone()),
            HashLimits::default(),
        )
        .await;
    assert_eq!(summary.hashed, 4);
    let last = progress.last().unwrap();
    assert_eq!(last.phase, IndexPhase::Hash);
    assert_eq!((last.items_done, last.items_total), (4, Some(4)));
    assert_eq!((last.bytes_done, last.bytes_total), (1010, Some(1010)));
    assert!(
        progress
            .reports()
            .iter()
            .all(|r| r.phase == IndexPhase::Hash)
    );
}

#[tokio::test]
#[ignore = "throughput measurement: writes about 500 MiB"]
async fn throughput() {
    let fx = Fixture::new().await;
    let block = vec![0xA5_u8; 1 << 20];
    for index in 0..10 {
        let mut content = Vec::with_capacity(50 << 20);
        for chunk in 0..50_u8 {
            content.extend_from_slice(&block);
            content[(chunk as usize) << 20] = index;
        }
        fx.tree.write(&format!("big{index}.bin"), &content);
    }
    fx.scan().await;
    let started = Instant::now();
    let summary = fx.hash().await;
    let secs = started.elapsed().as_secs_f64();
    assert_eq!(summary.hashed, 10);
    println!(
        "hashed 500 MiB in {secs:.2}s: {:.0} MB/s",
        500.0 * 1.048576 / secs
    );
}
