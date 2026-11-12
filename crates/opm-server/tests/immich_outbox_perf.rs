//! Timings of the outbox of file operations (`immich/outbox/`) at the sizes it is built for: the
//! real file operations, scheduler, task engine and database, and the fake Immich (wiremock, over
//! a loopback socket).
//!
//! Ignored by default: it moves tens of thousands of files, and its numbers only mean something
//! in a release build (SQLite is compiled with the profile's optimisation level). Run it with
//!
//! ```text
//! CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off \
//!   cargo test --release -p opm-server --test immich_outbox_perf -- --ignored --nocapture \
//!   --test-threads=1
//! ```
//!
//! and read what it prints (`!` marks a number over its budget). Three runs:
//!
//! - `the_hook_adds_little_to_a_move`: 10,000 moves with the hook and 10,000 without it, in
//!   rounds that alternate so that the drift of the machine lands on both; the hook may add at
//!   most 25% to the finalising transaction. Then bursts of 100,000 operations straight at the
//!   hook (queued, then cancelled out again): the cost of one must not grow with the queue, and
//!   nothing is logged per operation.
//! - `a_burst_of_ten_thousand_moves_is_worked_off_by_one_task`: the loop of the application
//!   starts no pass while files are moving, then ONE task (and one sync after it) takes the lot.
//! - `a_pass_over_fifty_thousand_rows_carries_five_thousand_assets_in_time`: a queue of 50,000
//!   rows, 5,000 of which Immich holds assets for, against a fake that takes 5 ms to answer: both
//!   passes (the scan, then the check and carry-over) must end within 5 minutes, and the
//!   process's memory is sampled while they run (the fake's share, about 9 KiB per request it
//!   serves, is subtracted: wiremock keeps them all). The plans of the two queries the queue
//!   lives on are checked at that size by a unit test of `outbox::store`.
//!
//! The numbers of the last run are in the docs of `opm_server::immich::outbox`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::future::Future;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::TestApp;
use common::fsops::{loc, ops_of, request};
use immich_support::{Fixture, photos_in};
use opm_core::dto::common::ConflictPolicy;
use opm_core::dto::{TaskDto, TaskKind, TaskState};
use opm_server::fsops::{
    AppliedOp, FailAction, Failpoints, FsOps, MoveOutcome, OpContext, OpHook, Step,
};
use opm_server::immich::outbox::{OutboxHook, OutboxSignal};
use serde_json::Value;
use tokio_util::sync::{CancellationToken, DropGuard};

/// The share of the finalising transaction of a move the hook may take.
const BUDGET_PCT: f64 = 25.0;
/// Moves per round and per variant; the rounds alternate in the order below (ABBA ABBA), so each
/// variant moves 4 x 2,500 = 10,000 files.
const PER_ROUND: usize = 2_500;
const ORDER: [bool; 8] = [false, true, true, false, false, true, true, false];
const WARMUP: usize = 300;
/// Operations straight at the hook: blocks of 10,000, 100,000 in all.
const BLOCK_OPS: usize = 10_000;
const BLOCKS: usize = 10;
/// How much dearer the last block may be than the first (a lookup that scanned would be far worse).
const GROWTH_LIMIT: f64 = 3.0;
const FILE_ID_BASE: i64 = 5_000_000;

/// The quiet period of the burst test and how often its loop looks.
const QUIET_SECS: u64 = 5;
const TICK: Duration = Duration::from_millis(100);
const BURST: usize = 10_000;

/// The scale test: assets Immich holds, rows it does not know, and the time allowed.
const CARRIED: usize = 5_000;
const UNKNOWN: usize = 45_000;
const LATENCY: Duration = Duration::from_millis(5);
const SCALE_BUDGET: Duration = Duration::from_secs(300);
const SCALE_PATIENCE: Duration = Duration::from_secs(900);
/// What the in-process fake keeps of every request it serves, in KiB. wiremock holds each request
/// for the whole life of the server, whether or not it records them (measured with 30,000 plain
/// requests: +271 MiB). That is where most of the growth of this run comes from.
const FAKE_KIB_PER_REQUEST: f64 = 9.0;
/// A gross bound on what the passes themselves may add to the process, the fake's share left out.
const MEMORY_LIMIT_MIB: f64 = 512.0;

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

fn percentile(sorted: &[f64], share: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * share).round() as usize]
}

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    percentile(&sorted, 0.5)
}

fn mean(samples: &[f64]) -> f64 {
    samples.iter().sum::<f64>() / samples.len() as f64
}

fn p95(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    percentile(&sorted, 0.95)
}

/// `+12.3%`: how much dearer `with` is than `without`.
fn overhead(with: f64, without: f64) -> f64 {
    (with / without - 1.0) * 100.0
}

fn mark(over: bool) -> &'static str {
    if over { " !" } else { "" }
}

/// Polls `check` until it holds, for at most `patience` (the minute of `immich_support::eventually`
/// is too short for these runs).
async fn wait_for<F, Fut>(what: &str, patience: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let started = Instant::now();
    while !check().await {
        assert!(started.elapsed() < patience, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ----- the hook on the moves of the file operations ---------------------------------------------

/// One way of moving files, with what each move took.
struct Variant {
    name: &'static str,
    ops: FsOps,
    /// When the last move reached the end of its disk work (`Step::BeforeIndexTx`): what follows
    /// is the finalising transaction, hooks included.
    stamp: Arc<Mutex<Option<Instant>>>,
    /// The whole call, in microseconds.
    call: Vec<f64>,
    /// From the end of the disk work to the end of the call, in microseconds.
    finalising: Vec<f64>,
}

impl Variant {
    fn new(name: &'static str, app: &TestApp, hook: Option<OutboxHook>) -> Self {
        let stamp = Arc::new(Mutex::new(None::<Instant>));
        let seen = Arc::clone(&stamp);
        let mut ops = ops_of(app).with_failpoints(Failpoints::new(move |step| {
            if step == Step::BeforeIndexTx {
                *seen.lock().unwrap() = Some(Instant::now());
            }
            FailAction::Continue
        }));
        if let Some(hook) = hook {
            ops = ops.with_hook(Arc::new(hook));
        }
        Self {
            name,
            ops,
            stamp,
            call: Vec::new(),
            finalising: Vec::new(),
        }
    }

    /// Moves the `count` photos `p0000.jpg` and on of `from` to `to`, one after the other.
    async fn run(&mut self, volume: i64, from: &str, to: &str, count: usize, record: bool) {
        let cx = OpContext::detached();
        for n in 0..count {
            let name = format!("p{n:04}.jpg");
            let moving = request(
                loc(volume, &format!("{from}/{name}")),
                loc(volume, &format!("{to}/{name}")),
                ConflictPolicy::Skip,
            );
            let started = Instant::now();
            let moved = self.ops.move_file(moving, &cx).await.unwrap();
            let ended = Instant::now();
            assert!(matches!(moved, MoveOutcome::Moved { .. }), "{moved:?}");
            if record {
                let stamped = self
                    .stamp
                    .lock()
                    .unwrap()
                    .take()
                    .expect("the move was timed");
                self.call.push(micros(ended.duration_since(started)));
                self.finalising.push(micros(ended.duration_since(stamped)));
            }
        }
    }

    fn print(&self) {
        let line = |label: &str, samples: &[f64]| {
            println!(
                "  {:<7} {label}: median {:>7.1} us  mean {:>7.1} us  p95 {:>7.1} us",
                self.name,
                median(samples),
                mean(samples),
                p95(samples)
            );
        };
        line("whole move      ", &self.call);
        line("finalising tx   ", &self.finalising);
    }
}

/// Operations straight at the hook, each in a transaction of its own as the file operations have
/// them: the microseconds per operation of each block of `BLOCK_OPS`. `forward` moves the files
/// from `burst/a` to `burst/b`; backwards moves them home again, which cancels the waiting rows.
async fn hook_blocks(fx: &Fixture, hook: &OutboxHook, volume: i64, forward: bool) -> Vec<f64> {
    let db = &fx.app.state.db;
    let (from, to) = if forward { ("a", "b") } else { ("b", "a") };
    let mut blocks = Vec::new();
    for block in 0..BLOCKS {
        let started = Instant::now();
        for i in 0..BLOCK_OPS {
            let n = block * BLOCK_OPS + i;
            let op = AppliedOp::Moved {
                file_id: FILE_ID_BASE + n as i64,
                from: loc(volume, &format!("burst/{from}/x{n}.jpg")),
                to: loc(volume, &format!("burst/{to}/x{n}.jpg")),
            };
            let mut tx = db.begin_write().await.unwrap();
            hook.on_applied(&mut tx, &op).await.unwrap();
            tx.commit().await.unwrap();
        }
        blocks.push(micros(started.elapsed()) / BLOCK_OPS as f64);
    }
    blocks
}

fn print_blocks(label: &str, blocks: &[f64], baseline: f64) {
    let shown: Vec<String> = blocks.iter().map(|block| format!("{block:.0}")).collect();
    println!(
        "  {label}: {} us per operation by block of 10,000",
        shown.join(" ")
    );
    let first = blocks[0];
    let last = blocks[blocks.len() - 1];
    println!(
        "  {label}: first block {first:.1} us, last {last:.1} us ({:.2}x), the empty transaction \
         alone {baseline:.1} us{}",
        last / first,
        mark(last / first > GROWTH_LIMIT)
    );
}

#[tokio::test]
#[ignore = "timing run: see the module docs"]
async fn the_hook_adds_little_to_a_move() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;

    // `s00` to `s07` hold what the rounds move; `w0` and `w1` what the warm-up of each variant does.
    let started = Instant::now();
    for dir in ["w0", "w1"] {
        for n in 0..WARMUP {
            photos.tree.write(
                &format!("{dir}/p{n:04}.jpg"),
                format!("{dir} {n}").as_bytes(),
            );
        }
    }
    for round in 0..ORDER.len() {
        for n in 0..PER_ROUND {
            photos.tree.write(
                &format!("s{round:02}/p{n:04}.jpg"),
                format!("{round} {n}").as_bytes(),
            );
        }
    }
    fx.index().await;
    println!(
        "\n== hook overhead: {} files written and indexed in {:.1} s",
        ORDER.len() * PER_ROUND + 2 * WARMUP,
        started.elapsed().as_secs_f64()
    );

    let hook = OutboxHook::new(OutboxSignal::new());
    let mut plain = Variant::new("plain", &fx.app, None);
    let mut hooked = Variant::new("hooked", &fx.app, Some(hook.clone()));
    plain.run(photos.volume_id, "w0", "w2", WARMUP, false).await;
    hooked
        .run(photos.volume_id, "w1", "w3", WARMUP, false)
        .await;
    // The capture of the log of this test must be live, or "nothing was logged" proves nothing.
    tracing::debug!(target: "opm_server::immich::outbox", "control");

    let started = Instant::now();
    for (round, with_hook) in ORDER.into_iter().enumerate() {
        let variant = if with_hook { &mut hooked } else { &mut plain };
        let (from, to) = (format!("s{round:02}"), format!("d{round:02}"));
        variant
            .run(photos.volume_id, &from, &to, PER_ROUND, true)
            .await;
    }
    println!(
        "  {} moves in {:.1} s",
        ORDER.len() * PER_ROUND,
        started.elapsed().as_secs_f64()
    );
    plain.print();
    hooked.print();
    let whole = overhead(median(&hooked.call), median(&plain.call));
    let finalising = overhead(median(&hooked.finalising), median(&plain.finalising));
    println!(
        "  the hook adds {whole:+.1}% to the whole move and {finalising:+.1}% to the finalising \
         transaction by the median ({:+.1}% by the mean; budget {BUDGET_PCT}%){}",
        overhead(mean(&hooked.finalising), mean(&plain.finalising)),
        mark(finalising > BUDGET_PCT)
    );
    assert_eq!(
        fx.row_count().await,
        (WARMUP + ORDER.len() / 2 * PER_ROUND) as i64,
        "one row per move that had the hook, none for the others"
    );
    assert!(
        finalising <= BUDGET_PCT,
        "the hook adds {finalising:.1}% to the finalising transaction, the budget is {BUDGET_PCT}%"
    );

    // Straight at the hook: the cost of one operation must not grow with the queue.
    let started = Instant::now();
    for _ in 0..BLOCK_OPS {
        let tx = fx.app.state.db.begin_write().await.unwrap();
        tx.commit().await.unwrap();
    }
    let baseline = micros(started.elapsed()) / BLOCK_OPS as f64;
    let queued = hook_blocks(&fx, &hook, photos.volume_id, true).await;
    print_blocks("queued ", &queued, baseline);
    let rows = fx.row_count().await;
    let cancelled = hook_blocks(&fx, &hook, photos.volume_id, false).await;
    print_blocks("merged ", &cancelled, baseline);
    assert_eq!(
        fx.row_count().await,
        rows - (BLOCKS * BLOCK_OPS) as i64,
        "a move back cancels the waiting row"
    );
    for (what, blocks) in [("queued", &queued), ("merged", &cancelled)] {
        assert!(
            blocks[blocks.len() - 1] <= blocks[0] * GROWTH_LIMIT,
            "the cost of a {what} operation grew with the queue: {blocks:?}"
        );
    }

    assert_eq!(
        fx.logs
            .contents()
            .matches("opm_server::immich::outbox")
            .count(),
        1,
        "the hook logged something (the one line is the test's own control)"
    );
}

// ----- a burst of moves worked off by the loop of the application -------------------------------

/// Starts the loop as the application does, with a short tick. It ends when the guard is dropped.
fn start_loop(fx: &Fixture) -> DropGuard {
    let stop = CancellationToken::new();
    fx.app
        .state
        .immich_outbox
        .start_background_every(stop.clone(), TICK);
    stop.drop_guard()
}

fn is_over(task: &TaskDto) -> bool {
    !matches!(task.state, TaskState::Queued | TaskState::Running)
}

/// Every pass the application runs or ran.
async fn passes(fx: &Fixture) -> Vec<TaskDto> {
    fx.app
        .state
        .tasks
        .list(500)
        .await
        .unwrap()
        .into_iter()
        .filter(|task| task.kind == TaskKind::ImmichCleanup)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing run: see the module docs"]
async fn a_burst_of_ten_thousand_moves_is_worked_off_by_one_task() {
    let fx = Fixture::connected_with(|config| config.immich_quiet_secs = QUIET_SECS).await;
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;
    for n in 0..BURST {
        photos
            .tree
            .write(&format!("a/p{n:05}.jpg"), format!("burst {n}").as_bytes());
    }
    fx.index().await;
    fx.sync_ok(false).await;
    let syncs = fx.tasks_of(TaskKind::ImmichSync).await;
    let _loop = start_loop(&fx);

    let started = Instant::now();
    for n in 0..BURST {
        let (from, to) = (format!("a/p{n:05}.jpg"), format!("b/p{n:05}.jpg"));
        fx.move_file((&photos, &from), (&photos, &to)).await;
    }
    let moving = started.elapsed();
    println!(
        "\n== burst: {BURST} moves in {:.1} s ({:.0} moves/s)",
        moving.as_secs_f64(),
        BURST as f64 / moving.as_secs_f64()
    );
    assert_eq!(fx.row_count().await, BURST as i64);
    assert!(
        passes(&fx).await.is_empty(),
        "a pass started while files were still being moved"
    );

    let started = Instant::now();
    wait_for("the pass to end", SCALE_PATIENCE, || async {
        let passes = passes(&fx).await;
        !passes.is_empty() && passes.iter().all(is_over)
    })
    .await;
    let ended = passes(&fx).await;
    assert_eq!(ended.len(), 1, "{ended:?}");
    let pass = &ended[0];
    assert_eq!(pass.state, TaskState::Done, "{pass:?}");
    assert_eq!(pass.error, None);
    let report = fx.report(pass).await;
    println!(
        "  one task, \"{}\", {:.1} s after the last move (the quiet period is {QUIET_SECS} s): {report}",
        pass.title,
        started.elapsed().as_secs_f64()
    );
    assert_eq!(report["rows"], BURST);
    assert_eq!(report["scans"], 1);
    assert_eq!(report["scanned"], BURST);
    assert_eq!(fx.row_count().await, 0, "the queue is drained");

    fx.quiesce().await;
    assert_eq!(
        fx.tasks_of(TaskKind::ImmichSync).await,
        syncs + 1,
        "a drained queue starts one sync"
    );
    assert_eq!(passes(&fx).await.len(), 1, "no second pass");
    fx.fake.assert_no_violations();
}

// ----- a queue of 50,000 rows ---------------------------------------------------------------

/// The resident memory of this process in KiB, 0 when `ps` cannot tell.
fn rss_kib() -> u64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// Samples the resident memory of the process until it is finished.
struct RssSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: JoinHandle<()>,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(rss_kib()));
        let thread = {
            let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    peak.fetch_max(rss_kib(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        };
        Self { stop, peak, thread }
    }

    /// The highest value seen, in KiB.
    fn finish(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap();
        self.peak.load(Ordering::Relaxed).max(rss_kib())
    }
}

/// Starts a pass and waits for it for as long as the scale test allows; what it reported and how
/// long it took.
async fn timed_pass(fx: &Fixture) -> (Value, Duration) {
    let started = Instant::now();
    let task = fx
        .app
        .state
        .immich_outbox
        .submit_now()
        .await
        .unwrap()
        .expect("a pass is due");
    let task = tokio::time::timeout(SCALE_PATIENCE, fx.app.state.tasks.wait_finished(task))
        .await
        .expect("the pass did not end in time")
        .expect("the task exists");
    let took = started.elapsed();
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(task.error, None);
    (fx.report(&task).await, took)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing run: see the module docs"]
async fn a_pass_over_fifty_thousand_rows_carries_five_thousand_assets_in_time() {
    let fx = Fixture::connected().await;
    let started = Instant::now();
    let (photos, olds) = photos_in(&fx, CARRIED).await;

    // What the old assets carry, so that every carry-over has something to do.
    let ids: Vec<&str> = olds.iter().map(String::as_str).collect();
    fx.fake.add_album("Holidays", &ids);
    let trip = fx.fake.add_tag("trip");
    for (n, id) in olds.iter().enumerate() {
        if n % 2 == 0 {
            fx.fake.update_asset(id, |asset| asset.is_favorite = true);
        }
        if n % 3 == 0 {
            fx.fake.tag_asset(&trip, id);
        }
    }
    for n in 0..CARRIED {
        let (from, to) = (format!("2020/p{n:03}.jpg"), format!("2021/p{n:03}.jpg"));
        fx.move_file((&photos, &from), (&photos, &to)).await;
    }
    assert_eq!(fx.row_count().await, CARRIED as i64);
    // The rest of the queue: files Immich holds nothing for, as after a big import that moved.
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, new_volume_id, \
         new_dir, new_name, next_attempt_at, created_at) \
         SELECT 1000000 + i, ?2, 'bulk/2020', 'x' || i || '.jpg', ?2, 'bulk/2021', \
         'x' || i || '.jpg', 0, CAST(strftime('%s', 'now') AS INTEGER) FROM n",
    )
    .bind(UNKNOWN as i64)
    .bind(photos.volume_id)
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
    assert_eq!(fx.row_count().await, (CARRIED + UNKNOWN) as i64);
    println!(
        "\n== scale: {CARRIED} assets moved and {UNKNOWN} changes Immich knows nothing about \
         ({:.1} s to set up), answers take {} ms",
        started.elapsed().as_secs_f64(),
        LATENCY.as_millis()
    );

    fx.fake.set_latency(LATENCY);
    fx.fake.clear_calls();
    let before = rss_kib();
    let sampler = RssSampler::start();

    let (first, first_took) = timed_pass(&fx).await;
    let first_calls = fx.fake.call_count();
    println!(
        "  pass 1 (the scan): {:.1} s for {} rows ({:.0} rows/s), {first_calls} requests: {first}",
        first_took.as_secs_f64(),
        CARRIED + UNKNOWN,
        (CARRIED + UNKNOWN) as f64 / first_took.as_secs_f64()
    );
    assert_eq!(first["rows"], CARRIED + UNKNOWN);
    assert_eq!(first["scans"], 1);
    assert_eq!(first["scanned"], UNKNOWN);
    assert_eq!(
        fx.row_count().await,
        CARRIED as i64,
        "the changes Immich knows nothing about are done once the scan is asked"
    );
    assert!(
        fx.rows().await.iter().all(|row| row.stage == 1),
        "the others wait for the scan"
    );

    fx.make_due().await;
    fx.fake.clear_calls();
    let (second, second_took) = timed_pass(&fx).await;
    let second_calls = fx.fake.call_count();
    println!(
        "  pass 2 (check and carry over): {:.1} s for {CARRIED} rows ({:.0} rows/s), \
         {second_calls} requests ({:.1} per row): {second}",
        second_took.as_secs_f64(),
        CARRIED as f64 / second_took.as_secs_f64(),
        second_calls as f64 / CARRIED as f64
    );
    assert_eq!(second["rows"], CARRIED);
    assert_eq!(second["deleted"], CARRIED);
    assert_eq!(second["carried"], CARRIED);
    assert_eq!(fx.row_count().await, 0, "the queue is drained");

    let peak = sampler.finish();
    let total = first_took + second_took;
    println!(
        "  both passes: {:.1} s for {CARRIED} carried assets (budget {} s){}",
        total.as_secs_f64(),
        SCALE_BUDGET.as_secs(),
        mark(total > SCALE_BUDGET)
    );
    let grew = mib(peak.saturating_sub(before));
    let fake = (first_calls + second_calls) as f64 * FAKE_KIB_PER_REQUEST / 1024.0;
    let ours = (grew - fake).max(0.0);
    println!(
        "  memory: {:.0} MiB before, {:.0} MiB at the peak, +{grew:.0} MiB, of which about \
         {fake:.0} MiB is the fake's (it keeps every request it serves): about {ours:.0} MiB is \
         the passes' (limit {MEMORY_LIMIT_MIB:.0})",
        mib(before),
        mib(peak),
    );
    assert!(
        total <= SCALE_BUDGET,
        "the two passes took {total:?}, the budget is {SCALE_BUDGET:?}"
    );
    assert!(
        ours < MEMORY_LIMIT_MIB,
        "the process grew by {grew:.0} MiB, about {ours:.0} MiB without the fake's share"
    );

    // Everything arrived: the favorite, the album and the tag, and the old assets are gone.
    fx.quiesce().await;
    for (n, old) in olds.iter().enumerate() {
        let new = fx
            .fake
            .asset_at(&photos.immich_path(&format!("2021/p{n:03}.jpg")));
        assert_eq!(new.is_favorite, n % 2 == 0, "favorite of p{n:03}");
        assert_eq!(
            fx.fake.albums_of(&new.id),
            ["Holidays"],
            "albums of p{n:03}"
        );
        assert_eq!(
            fx.fake.tags_of(&new.id).len(),
            usize::from(n % 3 == 0),
            "tags of p{n:03}"
        );
        assert!(
            !fx.fake.has_asset(old),
            "the old asset of p{n:03} is still there"
        );
    }
    fx.fake.assert_no_violations();
}
