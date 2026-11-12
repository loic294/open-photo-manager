//! Timings of the Immich asset sync on a library of 200,000 assets, and of what the `in_immich`
//! badge adds to an item page of 200 files: the real router, the real task engine and database,
//! and the fake Immich (wiremock, over a loopback socket).
//!
//! Ignored by default: it seeds hundreds of thousands of rows, and its numbers only mean something
//! in a release build (SQLite is compiled with the profile's optimisation level). Run it with
//!
//! ```text
//! CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off \
//!   cargo test --release -p opm-server --test immich_sync_perf -- --ignored --nocapture
//! ```
//!
//! and read what it prints (`!` marks a number over its target: 60 s for the first sync, 20 ms of
//! extra time for a page). The numbers of the last run are in the docs of `opm_server::immich`.
//!
//! The library: 1,100 folders of 200 files in the volume `photos`. Immich holds the files of the
//! first 1,000 folders (1% of its assets are trashed and 1% offline, and a folder changed an hour
//! after the one before it); the last 100 folders are not in Immich. The fake answers each page by
//! scanning its 200,000 assets, so part of the time of a sync is the fake's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::browse;
use immich_support::{Fixture, START_MS};
use opm_core::dto::{TaskDto, TaskState};
use opm_core::names::{normalize_name, stem_norm};
use opm_server::db::Db;
use serde_json::{Value, json};
use sqlx::QueryBuilder;

const PREFIX: &str = "/mnt/photos";
const SEARCH: &str = "/api/search/metadata";
/// Folders whose files Immich has, folders whose files it does not, and files per folder.
const KNOWN_FOLDERS: usize = 1_000;
const OTHER_FOLDERS: usize = 100;
const PER_FOLDER: usize = 200;
const ASSETS: usize = KNOWN_FOLDERS * PER_FOLDER;
const RUNS: usize = 7;
const SYNC_TARGET_S: f64 = 60.0;
const BADGE_TARGET_MS: f64 = 20.0;
const HOUR_MS: i64 = 60 * 60 * 1000;

/// `2005/event00` to `2026/event49`: 50 folders a year.
fn folder(index: usize) -> String {
    format!("{}/event{:02}", 2005 + index / 50, index % 50)
}

fn file_name(n: usize) -> String {
    format!("IMG_{n:05}.jpg")
}

async fn insert_files(tx: &mut sqlx::SqliteConnection, volume: i64, files: &[(String, String)]) {
    if files.is_empty() {
        return;
    }
    let mut qb = QueryBuilder::new(
        "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
         mtime) ",
    );
    qb.push_values(files, |mut row, (dir, name)| {
        row.push_bind(volume)
            .push_bind(dir.as_str())
            .push_bind(name.as_str())
            .push_bind(normalize_name(name))
            .push_bind(stem_norm(name))
            .push_bind("jpg")
            .push_bind("image")
            .push_bind(1_000_i64)
            .push_bind(1_700_000_000_i64);
    });
    qb.build().execute(tx).await.unwrap();
}

/// Indexes every file the way a scan would, and refreshes the statistics the way it ends.
async fn seed_files(db: &Db, volume: i64) -> usize {
    let folders = KNOWN_FOLDERS + OTHER_FOLDERS;
    let mut dirs: Vec<(String, Option<String>, String)> =
        vec![(String::new(), None, String::new())];
    for year in 0..folders.div_ceil(50) {
        let name = (2005 + year).to_string();
        dirs.push((name.clone(), Some(String::new()), name));
    }
    for index in 0..folders {
        let path = folder(index);
        let (parent, name) = path.rsplit_once('/').unwrap();
        dirs.push((path.clone(), Some(parent.to_owned()), name.to_owned()));
    }

    let mut tx = db.begin_write().await.unwrap();
    for chunk in dirs.chunks(4_000) {
        let mut qb =
            QueryBuilder::new("INSERT OR IGNORE INTO dirs (volume_id, path, parent_path, name) ");
        qb.push_values(chunk, |mut row, (path, parent, name)| {
            row.push_bind(volume)
                .push_bind(path.as_str())
                .push_bind(parent.as_deref())
                .push_bind(name.as_str());
        });
        qb.build().execute(&mut *tx).await.unwrap();
    }
    let mut buffer = Vec::with_capacity(2_000);
    for index in 0..folders {
        let dir = folder(index);
        for n in 0..PER_FOLDER {
            buffer.push((dir.clone(), file_name(n)));
            if buffer.len() == 2_000 {
                insert_files(&mut tx, volume, &buffer).await;
                buffer.clear();
            }
        }
    }
    insert_files(&mut tx, volume, &buffer).await;
    tx.commit().await.unwrap();
    db.optimize().await.unwrap();
    folders * PER_FOLDER
}

/// Puts the assets of the first [`KNOWN_FOLDERS`] folders into the fake and returns their ids, in
/// the order of the folders.
fn fill_fake(fx: &Fixture, library: &str) -> Vec<String> {
    let mut ids = Vec::with_capacity(ASSETS);
    for index in 0..KNOWN_FOLDERS {
        let dir = folder(index);
        let updated = START_MS - i64::try_from(KNOWN_FOLDERS - index).unwrap() * HOUR_MS;
        for n in 0..PER_FOLDER {
            let path = format!("{PREFIX}/{dir}/{}", file_name(n));
            ids.push(fx.fake.add_asset_with(library, &path, |asset| {
                asset.updated_at_ms = updated;
                asset.is_trashed = n % 100 == 0;
                asset.is_offline = n % 100 == 1;
            }));
        }
    }
    ids
}

/// What a request for an item page took: the median of [`RUNS`] runs, and what the page said.
struct Page {
    median_ms: f64,
    items: usize,
    in_immich: usize,
}

fn page_request(volume: i64, dir: &str) -> Value {
    json!({ "volume_id": volume, "dir": dir, "limit": PER_FOLDER })
}

/// One request for the page, through the router: the number of items and of them in Immich.
async fn request_page(fx: &Fixture, body: &Value) -> (usize, usize) {
    let (status, response) = browse::items(&fx.app, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let items = response["items"].as_array().unwrap();
    let in_immich = items
        .iter()
        .filter(|item| item["in_immich"] == Value::Bool(true))
        .count();
    (items.len(), in_immich)
}

async fn measure_page(fx: &Fixture, volume: i64, dir: &str) -> Page {
    let body = page_request(volume, dir);
    let (items, in_immich) = request_page(fx, &body).await;
    let mut millis = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        assert_eq!(request_page(fx, &body).await, (items, in_immich));
        millis.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    millis.sort_by(f64::total_cmp);
    Page {
        median_ms: millis[RUNS / 2],
        items,
        in_immich,
    }
}

fn report_page(label: &str, before: &Page, after: &Page) {
    let extra = after.median_ms - before.median_ms;
    let flag = if extra > BADGE_TARGET_MS { '!' } else { ' ' };
    println!(
        "{label:<34} {:>3} items, {:>3} in Immich   before {:>6.1}   after {:>6.1}   \
         extra {extra:>6.1}{flag} ms",
        after.items, after.in_immich, before.median_ms, after.median_ms
    );
}

/// How a sync went: its time, its task and the item pages asked while it ran.
struct Run {
    elapsed: Duration,
    task: TaskDto,
    searches: usize,
    pages_ms: Vec<f64>,
}

/// Asks for a sync and waits for it. With a `probe`, an item page is asked for every 100 ms until
/// the task ends (and the time has that resolution); without one the task is awaited.
async fn timed_sync(fx: &Fixture, full: bool, probe: Option<&Value>) -> Run {
    fx.fake.clear_calls();
    let started = Instant::now();
    let id = fx.submit_sync(full).await;
    let mut pages_ms = Vec::new();
    let task = match probe {
        None => fx.app.state.tasks.wait_finished(id).await.unwrap(),
        Some(body) => loop {
            let task = fx.task(id).await;
            if !matches!(task.state, TaskState::Queued | TaskState::Running) {
                break task;
            }
            let asked = Instant::now();
            request_page(fx, body).await;
            pages_ms.push(asked.elapsed().as_secs_f64() * 1000.0);
            tokio::time::sleep(Duration::from_millis(100)).await;
        },
    };
    let elapsed = started.elapsed();
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    Run {
        elapsed,
        task,
        searches: fx.fake.count("POST", SEARCH),
        pages_ms,
    }
}

fn report_sync(label: &str, run: &Run, target_s: Option<f64>) {
    let seconds = run.elapsed.as_secs_f64();
    let flag = if target_s.is_some_and(|target| seconds > target) {
        '!'
    } else {
        ' '
    };
    let rate = run.task.items_done as f64 / seconds.max(0.001);
    let mut pages = run.pages_ms.clone();
    pages.sort_by(f64::total_cmp);
    let meanwhile = match pages.last() {
        Some(max) => format!(
            "   {} item pages meanwhile: median {:.1}, max {max:.1} ms",
            pages.len(),
            pages[pages.len() / 2]
        ),
        None => String::new(),
    };
    println!(
        "{label:<44} {seconds:>7.2}{flag}s {:>8} read {:>4} searches {rate:>8.0}/s{meanwhile}",
        run.task.items_done, run.searches
    );
}

async fn database_mib(fx: &Fixture) -> f64 {
    let pool = fx.app.state.db.read();
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(pool)
        .await
        .unwrap();
    let size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(pool)
        .await
        .unwrap();
    (pages * size) as f64 / (1024.0 * 1024.0)
}

/// The counters the status says about the sync: total, live, matched, unmapped.
async fn counters(fx: &Fixture) -> [i64; 4] {
    let status = fx.status(false).await;
    let sync = &status["sync"];
    [
        "assets_total",
        "assets_live",
        "assets_matched",
        "assets_unmapped",
    ]
    .map(|field| sync[field].as_i64().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "seeds 220,000 files and 200,000 assets and prints timings; run it by hand in a release build"]
async fn the_sync_and_the_badge_at_200k_assets() {
    let fx = Fixture::connected().await;
    let (library, volume) = fx.map_library("Photos", "photos", PREFIX).await;

    let started = Instant::now();
    let files = seed_files(&fx.app.state.db, volume).await;
    println!(
        "\nlibrary: {files} files in {} folders (indexed in {:.1} s)",
        KNOWN_FOLDERS + OTHER_FOLDERS,
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let ids = fill_fake(&fx, &library);
    println!(
        "Immich: {} assets (the fake was filled in {:.1} s)",
        ids.len(),
        started.elapsed().as_secs_f64()
    );

    let known = folder(KNOWN_FOLDERS / 2);
    let other = folder(KNOWN_FOLDERS + OTHER_FOLDERS / 2);
    let known_before = measure_page(&fx, volume, &known).await;
    let other_before = measure_page(&fx, volume, &other).await;
    assert_eq!(known_before.items, PER_FOLDER);
    assert_eq!(known_before.in_immich, 0);
    let size_before = database_mib(&fx).await;

    println!("\n-- the sync");
    let probe = page_request(volume, &known);
    let first = timed_sync(&fx, false, Some(&probe)).await;
    report_sync(
        "first sync (everything is new)",
        &first,
        Some(SYNC_TARGET_S),
    );
    let live = ASSETS - ASSETS / 50;
    let expected = [ASSETS, live, ASSETS, 0].map(|count| i64::try_from(count).unwrap());
    assert_eq!(counters(&fx).await, expected);
    assert_eq!(fx.asset_count().await, i64::try_from(ASSETS).unwrap());
    let size_after = database_mib(&fx).await;
    let known_after = measure_page(&fx, volume, &known).await;
    let other_after = measure_page(&fx, volume, &other).await;
    assert_eq!(known_after.in_immich, PER_FOLDER - PER_FOLDER / 50);
    assert_eq!(other_after.in_immich, 0);

    let nothing = timed_sync(&fx, false, None).await;
    report_sync("incremental, nothing changed", &nothing, None);

    // 10 assets in each of 100 folders, none of them trashed or offline yet.
    fx.fake.advance(Duration::from_secs(24 * 60 * 60));
    for event in (0..KNOWN_FOLDERS).step_by(KNOWN_FOLDERS / 100) {
        for n in (5..PER_FOLDER).step_by(PER_FOLDER / 10) {
            fx.fake.update_asset(&ids[event * PER_FOLDER + n], |asset| {
                asset.is_trashed = true
            });
        }
    }
    let incremental = timed_sync(&fx, false, None).await;
    report_sync("incremental, 1,000 assets changed", &incremental, None);
    assert_eq!(counters(&fx).await[1], expected[1] - 1_000);

    for event in (0..KNOWN_FOLDERS).step_by(KNOWN_FOLDERS / 100) {
        for n in (7..PER_FOLDER).step_by(PER_FOLDER / 10) {
            fx.fake.remove_asset(&ids[event * PER_FOLDER + n]);
        }
    }
    let full = timed_sync(&fx, true, Some(&probe)).await;
    report_sync("full pass, 1,000 assets gone", &full, None);
    assert_eq!(counters(&fx).await[0], expected[0] - 1_000);

    println!(
        "\ndatabase: {size_before:.0} MiB before the first sync, {size_after:.0} MiB after it \
         ({:.0} bytes an asset)",
        (size_after - size_before) * 1024.0 * 1024.0 / ASSETS as f64
    );

    println!(
        "\n-- the badge: a page of {PER_FOLDER} files, the median of {RUNS} requests, with no asset \
         stored and with {ASSETS}"
    );
    report_page("a folder Immich has", &known_before, &known_after);
    report_page("a folder Immich does not have", &other_before, &other_after);
}
