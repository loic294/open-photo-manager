//! Timings of the stage queue (`indexer::pending`) on a library of 200,000 files in about 17,000
//! directories: the first page, a full pass of pages of 500, a page from the middle of the queue and
//! the totals, for a queue that holds every file (the first scan of a big library) and for one that
//! holds 1 % or 20 of them (a rescan), with the partial indexes of migration `0011` and without them.
//!
//! Ignored by default: it seeds a lot of rows, and its numbers only mean something in a release
//! build (SQLite is compiled with the profile's optimisation level). Run it with
//!
//! ```text
//! cargo test --release -p apm-server --test pending_perf -- --ignored --nocapture
//! ```
//!
//! and read the table it prints. The numbers of the last run are in the docs of
//! `apm_server::indexer::pending`. A release build also checks the targets of the stage queue (the
//! constants below) against the median of the runs; a debug build only prints.
//!
//! With some macOS toolchains the release profile's `strip = true` leaves a proc-macro dylib that
//! does not load (`mis-aligned LINKEDIT string pool`); prefix the command with
//! `CARGO_PROFILE_RELEASE_STRIP=none` then.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::time::Instant;

use apm_core::paths::{RelPath, VolumeId};
use apm_server::db::Db;
use apm_server::indexer::pending::{PendingKey, PendingQueue, PipelineColumn, StageScope};
use apm_server::indexer::{ScanRequest, ScanTarget};
use common::seed::Seed;

const PHOTOS_FILES: i64 = 160_000;
const CARDS_FILES: i64 = 40_000;
const PAGE: usize = 500;
const RUNS: usize = 7;

/// Milliseconds the first page may take, whatever is pending.
const FIRST_PAGE_MS: f64 = 20.0;
/// Milliseconds a full pass may take when 1 % of the files are pending.
const SPARSE_PASS_MS: f64 = 1_000.0;
/// Milliseconds the totals may take.
const TOTALS_MS: f64 = 200.0;

const SIZE_TWIN: &str = "EXISTS (SELECT 1 FROM files o WHERE o.size = f.size AND o.id <> f.id)";

/// Inserts `count` files into a volume, each of them pending in the three stages. File `i` lives in
/// `YYYY/MM/DD` (12 files a directory, the way a camera roll piles up) and is called
/// `IMG_<i>.jpg`; the sizes repeat now and then, like copies do.
async fn insert_files(db: &Db, volume_id: VolumeId, count: i64) {
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
         INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, \
                            meta_state, hash_state, thumb_state) \
         SELECT ?, \
                printf('%04d/%02d/%02d', 2000 + (i / 12) / 372, (i / 12 / 31) % 12 + 1, \
                       (i / 12) % 31 + 1), \
                printf('IMG_%06d.jpg', i), printf('img_%06d.jpg', i), printf('img_%06d', i), \
                'jpg', 'image', ((i * 2654435761) >> 7) % 2000000, 1700000000, 0, 0, 0 \
         FROM n",
    )
    .bind(count - 1)
    .bind(volume_id)
    .execute(db.write())
    .await
    .unwrap();
}

struct Library {
    photos: VolumeId,
    cards: VolumeId,
}

impl Library {
    fn volumes(&self) -> [VolumeId; 2] {
        [self.photos, self.cards]
    }
}

/// Fills a database the way a first scan of the whole library leaves it, with the statistics the
/// scanner refreshes at the end of such a scan. Returns the seconds the inserts took.
async fn seed(db: &Db) -> (Library, f64) {
    let seeder = Seed::new(db);
    let photos = seeder.volume("photos").await;
    let cards = seeder.volume("cards").await;
    let started = Instant::now();
    insert_files(db, photos, PHOTOS_FILES).await;
    insert_files(db, cards, CARDS_FILES).await;
    let seconds = started.elapsed().as_secs_f64();
    db.optimize().await.unwrap();
    (Library { photos, cards }, seconds)
}

/// Finishes every pending row but one in `every` (by id), in the three stages at once, the way the
/// stages drain a queue. Returns the seconds it took.
async fn leave_pending(db: &Db, every: i64) -> f64 {
    let started = Instant::now();
    sqlx::query(
        "UPDATE files SET meta_state = 1, hash_state = 1, thumb_state = 1 \
         WHERE meta_state = 0 AND id % ? <> 0",
    )
    .bind(every)
    .execute(db.write())
    .await
    .unwrap();
    started.elapsed().as_secs_f64()
}

/// The size of an index or table in the file, if this SQLite has the `dbstat` table.
async fn bytes_of(db: &Db, name: &str) -> String {
    let bytes: Result<Option<i64>, _> =
        sqlx::query_scalar("SELECT SUM(pgsize) FROM dbstat WHERE name = ?")
            .bind(name)
            .fetch_one(db.write())
            .await;
    match bytes {
        Ok(Some(bytes)) => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => "n/a".to_owned(),
    }
}

/// The medians that are compared with the targets, collected so the table is complete when one
/// of them misses.
#[derive(Default)]
struct Targets(Vec<String>);

impl Targets {
    fn check(&mut self, label: &str, median_ms: f64, limit_ms: f64) {
        if median_ms >= limit_ms {
            self.0.push(format!(
                "{label}: {median_ms:.1} ms, the target is {limit_ms:.0} ms"
            ));
        }
    }
}

/// Runs `run` once to warm up and `RUNS` times to measure, prints one line of the table and returns
/// the median in milliseconds.
async fn measure<F, Fut>(label: &str, mut run: F) -> f64
where
    F: FnMut() -> Fut,
    Fut: Future<Output = usize>,
{
    let rows = run().await;
    let mut millis = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        assert_eq!(run().await, rows, "{label}");
        millis.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    millis.sort_by(f64::total_cmp);
    println!(
        "{label:<58} {rows:>8} rows   min {:>8.2}   median {:>8.2}   max {:>8.2}  ms",
        millis[0],
        millis[RUNS / 2],
        millis[RUNS - 1]
    );
    millis[RUNS / 2]
}

fn pending_queue(
    column: PipelineColumn,
    targets: Vec<ScanTarget>,
    volumes: &[VolumeId],
) -> PendingQueue {
    let request = ScanRequest {
        targets,
        ..ScanRequest::default()
    };
    PendingQueue::new(column, StageScope::new(&request, volumes))
}

/// Reads the whole queue in pages of `PAGE`, as a stage that does not change the states would.
async fn full_pass(queue: &PendingQueue, db: &Db) -> usize {
    let mut seen = 0;
    let mut after = None;
    loop {
        let page = queue.page(db, after.as_ref(), PAGE).await.unwrap();
        seen += page.len();
        match page.last() {
            Some(last) if page.len() == PAGE => after = Some(PendingKey::from(last)),
            _ => return seen,
        }
    }
}

/// The measurements the targets are about, on the whole library.
async fn whole_library(
    db: &Db,
    library: &Library,
    label: &str,
    sparse: bool,
    targets: &mut Targets,
) {
    let everything = pending_queue(PipelineColumn::Meta, Vec::new(), &library.volumes());
    let (queue, db) = (&everything, db);
    // A key in the middle of the library, not necessarily a pending row.
    let middle = PendingKey {
        volume_id: library.photos,
        dir_path: "2022/05/26".to_owned(),
        name: "IMG_100000.jpg".to_owned(),
    };
    let middle = &middle;

    let first = measure("first page of 500", || async move {
        queue.page(db, None, PAGE).await.unwrap().len()
    })
    .await;
    let pass = measure("full pass of pages of 500", || full_pass(queue, db)).await;
    measure("page of 500 after a key in the middle", || async move {
        queue.page(db, Some(middle), PAGE).await.unwrap().len()
    })
    .await;
    let totals = measure("totals", || async move {
        usize::try_from(queue.totals(db).await.unwrap().files).unwrap()
    })
    .await;

    targets.check(&format!("{label}: first page"), first, FIRST_PAGE_MS);
    if sparse {
        targets.check(&format!("{label}: full pass"), pass, SPARSE_PASS_MS);
    }
    targets.check(&format!("{label}: totals"), totals, TOTALS_MS);
}

/// What a stage meets on a run that does not cover the whole library, and the hash stage's
/// condition.
async fn narrower_queues(db: &Db, library: &Library) {
    let photos = library.photos;
    let cards = library.cards;
    let year = vec![ScanTarget::subtree(photos, RelPath::new("2010").unwrap())];
    let several = vec![
        ScanTarget::subtree(photos, RelPath::new("2003").unwrap()),
        ScanTarget::subtree(photos, RelPath::new("2011/02").unwrap()),
        ScanTarget::dir(photos, RelPath::new("2015/06/07").unwrap()),
        ScanTarget::subtree(cards, RelPath::new("2001").unwrap()),
        ScanTarget::dir(cards, RelPath::new("2005/03/02").unwrap()),
    ];
    for (label, targets) in [("one year (4,464 files)", year), ("five scopes", several)] {
        let scoped = pending_queue(PipelineColumn::Meta, targets, &library.volumes());
        let scoped = &scoped;
        measure(&format!("first page, {label}"), || async move {
            scoped.page(db, None, PAGE).await.unwrap().len()
        })
        .await;
        measure(&format!("totals, {label}"), || async move {
            usize::try_from(scoped.totals(db).await.unwrap().files).unwrap()
        })
        .await;
    }
    let twins = pending_queue(PipelineColumn::Hash, Vec::new(), &library.volumes())
        .with_condition(SIZE_TWIN);
    let twins = &twins;
    measure("first page, hash stage, size twins", || async move {
        twins.page(db, None, PAGE).await.unwrap().len()
    })
    .await;
    measure("totals, hash stage, size twins", || async move {
        usize::try_from(twins.totals(db).await.unwrap().files).unwrap()
    })
    .await;
}

#[tokio::test]
#[ignore = "seeds 200,000 files and prints timings; run it by hand in a release build"]
async fn timings_on_a_large_library() {
    let mut targets = Targets::default();
    println!(
        "\nbuild: {}",
        if cfg!(debug_assertions) {
            "debug (the numbers mean little)"
        } else {
            "release"
        }
    );

    let indexed = Db::open_temp().await.unwrap();
    let (library, seconds_indexed) = seed(&indexed).await;
    let files_bytes = bytes_of(&indexed, "files").await;
    let dir_name_bytes = bytes_of(&indexed, "files_volume_dir_name").await;
    let pending_bytes = bytes_of(&indexed, "files_meta_pending").await;

    let plain = Db::open_temp().await.unwrap();
    for drop in [
        "DROP INDEX files_meta_pending",
        "DROP INDEX files_hash_pending",
        "DROP INDEX files_thumb_pending",
    ] {
        sqlx::query(drop).execute(plain.write()).await.unwrap();
    }
    let (plain_library, seconds_plain) = seed(&plain).await;
    println!(
        "library: {} files in 2 volumes; inserted in {seconds_indexed:.2} s with the partial \
         indexes, {seconds_plain:.2} s without",
        PHOTOS_FILES + CARDS_FILES
    );
    println!(
        "sizes in the file: files {files_bytes}, files_volume_dir_name {dir_name_bytes}, each \
         partial index (all pending) {pending_bytes}"
    );

    println!("\n######## with the partial indexes");
    println!("\n== all pending");
    whole_library(&indexed, &library, "all pending", false, &mut targets).await;
    narrower_queues(&indexed, &library).await;

    let seconds = leave_pending(&indexed, 100).await;
    println!(
        "\n== 1 % pending, the statistics of the first scan (finishing 198,000 files in the three \
         stages took {seconds:.2} s; each partial index is now {})",
        bytes_of(&indexed, "files_meta_pending").await
    );
    whole_library(&indexed, &library, "1 % pending", true, &mut targets).await;
    narrower_queues(&indexed, &library).await;

    leave_pending(&indexed, 10_000).await;
    println!("\n== 20 pending, the statistics of the first scan");
    whole_library(&indexed, &library, "20 pending", true, &mut targets).await;
    narrower_queues(&indexed, &library).await;

    sqlx::query("ANALYZE")
        .execute(indexed.write())
        .await
        .unwrap();
    let analysed = Db::open(&indexed.path()).await.unwrap();
    println!("\n== 20 pending, fresh statistics");
    whole_library(&analysed, &library, "20 pending, fresh", true, &mut targets).await;
    narrower_queues(&analysed, &library).await;
    analysed.close().await;

    println!("\n######## without the partial indexes");
    println!("\n== all pending");
    whole_library(
        &plain,
        &plain_library,
        "plain, all pending",
        false,
        &mut Targets::default(),
    )
    .await;
    let seconds = leave_pending(&plain, 100).await;
    println!("\n== 1 % pending (finishing 198,000 files in the three stages took {seconds:.2} s)");
    whole_library(
        &plain,
        &plain_library,
        "plain, 1 % pending",
        true,
        &mut Targets::default(),
    )
    .await;
    leave_pending(&plain, 10_000).await;
    println!("\n== 20 pending");
    whole_library(
        &plain,
        &plain_library,
        "plain, 20 pending",
        true,
        &mut Targets::default(),
    )
    .await;

    if !cfg!(debug_assertions) {
        assert!(
            targets.0.is_empty(),
            "the stage queue missed its targets:\n{}",
            targets.0.join("\n")
        );
    }
}
