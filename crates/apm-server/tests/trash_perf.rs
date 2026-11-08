//! Timings of the trash queries on one million rows (2,500 deletions of 400 items over two
//! volumes), through the real service: what the page asks for when it opens (the summary, the first
//! page, the deletions) and what its toolbar changes (sorts, filters, a deep page).
//!
//! Ignored by default: it seeds a lot of rows, and its numbers only mean something in a release
//! build (SQLite is compiled with the profile's optimisation level), where the three targets below
//! are asserted. Run it with
//!
//! ```text
//! CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off \
//!   cargo test --release -p apm-server --test trash_perf -- --ignored --nocapture
//! ```
//!
//! and read the table it prints. The numbers of the last run are in the docs of
//! `apm_server::trash`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::time::Instant;

use apm_core::dto::{SortOrder, TrashBatchesQuery, TrashListQuery, TrashSort};
use apm_server::db::Db;
use common::TestApp;

const ROWS: i64 = 1_000_000;
const BATCH_SIZE: i64 = 400;
const RUNS: usize = 7;
/// The page opens on these three, so they carry the targets (milliseconds, median of the runs).
const FIRST_PAGE_MS: f64 = 30.0;
const BATCHES_MS: f64 = 300.0;
const SUMMARY_MS: f64 = 150.0;

/// One statement that makes `ROWS` items in `ROWS / BATCH_SIZE` deletions, alternating between
/// the volumes `?1` and `?2`, one hour apart, with the reasons and sizes of a real trash.
const SEED: &str = "
WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 999999),
b AS (SELECT i, i / 400 AS bn, 1700000000 + (i / 400) * 3600 AS bt FROM n)
INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, reason, task_id,
                         meta_json, trashed_at)
SELECT CASE bn % 2 WHEN 0 THEN ?1 ELSE ?2 END,
       strftime('%Y%m%d-%H%M%S', bt, 'unixepoch') || '-t' || (bn + 1),
       '2023/Trip ' || (i % 977) || '/IMG_' || i || '.jpg',
       '.apm-trash/' || strftime('%Y%m%d-%H%M%S', bt, 'unixepoch') || '-t' || (bn + 1)
           || '/2023/Trip ' || (i % 977) || '/IMG_' || i || '.jpg',
       0,
       1000000 + (i * 7919) % 9000000,
       CASE i % 4 WHEN 0 THEN 'user' WHEN 1 THEN 'replaced' WHEN 2 THEN NULL ELSE 'junk' END,
       bn + 1,
       CASE WHEN i % 10 = 0 THEN NULL ELSE '{\"a\":1}' END,
       bt + (i % 400)
FROM b";

async fn seed(db: &Db, first: i64, second: i64) {
    sqlx::query(SEED)
        .bind(first)
        .bind(second)
        .execute(db.write())
        .await
        .unwrap();
}

/// Runs `request` [`RUNS`] times, prints the cold run and the median, and returns the median in
/// milliseconds.
async fn measure<T, Fut>(label: &str, mut request: impl FnMut() -> Fut) -> f64
where
    Fut: Future<Output = T>,
{
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let started = Instant::now();
        let answer = request().await;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(answer);
    }
    let cold = times[0];
    times.sort_by(f64::total_cmp);
    let median = times[RUNS / 2];
    println!(
        "{label:<48} cold {cold:>8.2} ms   median {median:>8.2} ms   max {:>8.2} ms",
        times[RUNS - 1]
    );
    median
}

fn list(change: impl FnOnce(&mut TrashListQuery)) -> TrashListQuery {
    let mut query = TrashListQuery::default();
    change(&mut query);
    query
}

#[tokio::test]
#[ignore = "seeds 1,000,000 trash items and prints timings; run it by hand in a release build"]
async fn timings_on_a_trash_of_a_million_items() {
    let app = TestApp::new().await;
    let (a, _) = app.add_volume("a").await;
    let (b, _) = app.add_volume("b").await;
    let started = Instant::now();
    seed(&app.state.db, a, b).await;
    println!(
        "\ntrash: {ROWS} items in {} deletions (seeded in {:.1} s)\n",
        ROWS / BATCH_SIZE,
        started.elapsed().as_secs_f64()
    );
    let trash = &app.state.trash;

    let summary = trash.summary().await.unwrap();
    assert_eq!(summary.items, ROWS as u64);
    let page = trash.list(TrashListQuery::default()).await.unwrap();
    assert_eq!((page.total, page.items.len()), (ROWS as u64, 50));
    let deletions = trash.batches(TrashBatchesQuery::default()).await.unwrap();
    assert_eq!(deletions.total, (ROWS / BATCH_SIZE) as u64);
    let batch = deletions.batches[0].batch.clone();
    let batch_volume = deletions.batches[0].volume_id;

    println!("-- what the page asks for when it opens");
    let summary_ms = measure("summary", || trash.summary()).await;
    let first_page_ms = measure("items: first page of 50, newest first", || {
        trash.list(TrashListQuery::default())
    })
    .await;
    let batches_ms = measure("deletions: first page of 50", || {
        trash.batches(TrashBatchesQuery::default())
    })
    .await;

    println!("\n-- what the toolbar changes");
    measure("items: page of 200", || {
        trash.list(list(|query| query.limit = 200))
    })
    .await;
    measure("items: one volume", || {
        trash.list(list(|query| query.volume_id = Some(a)))
    })
    .await;
    measure("items: one deletion (400 items)", || {
        trash.list(list(|query| {
            query.volume_id = Some(batch_volume);
            query.batch = Some(batch.clone());
        }))
    })
    .await;
    measure("items: page 2,000 (offset 100,000)", || {
        trash.list(list(|query| query.offset = 100_000))
    })
    .await;
    measure("items: oldest first", || {
        trash.list(list(|query| query.order = Some(SortOrder::Asc)))
    })
    .await;
    for (label, sort) in [
        ("items: largest first", TrashSort::Size),
        ("items: by original path", TrashSort::Path),
        ("items: by name", TrashSort::Name),
    ] {
        measure(label, || trash.list(list(|query| query.sort = sort))).await;
    }
    measure("items: search (matches 1 of 977 trips)", || {
        trash.list(list(|query| query.q = Some("trip 12/".to_owned())))
    })
    .await;
    measure("items: search (matches nothing)", || {
        trash.list(list(|query| query.q = Some("no such thing".to_owned())))
    })
    .await;
    measure("deletions: one volume", || {
        trash.batches(TrashBatchesQuery {
            volume_id: Some(b),
            ..TrashBatchesQuery::default()
        })
    })
    .await;
    measure("deletions: page 25 (offset 1,250)", || {
        trash.batches(TrashBatchesQuery {
            offset: 1_250,
            ..TrashBatchesQuery::default()
        })
    })
    .await;

    if cfg!(debug_assertions) {
        println!("\ndebug build: the targets are not checked");
        return;
    }
    assert!(
        first_page_ms < FIRST_PAGE_MS,
        "first page took {first_page_ms:.2} ms, the target is {FIRST_PAGE_MS} ms"
    );
    assert!(
        batches_ms < BATCHES_MS,
        "the deletions took {batches_ms:.2} ms, the target is {BATCHES_MS} ms"
    );
    assert!(
        summary_ms < SUMMARY_MS,
        "the summary took {summary_ms:.2} ms, the target is {SUMMARY_MS} ms"
    );
}
