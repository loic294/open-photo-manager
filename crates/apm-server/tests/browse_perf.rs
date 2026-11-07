//! Timings of the browse API over HTTP on a library of about 250,000 files, 7 runs each: the volumes
//! root, the folder lists, item pages, id lists and facets, each as one request through the real
//! router (routing, validation, the query engine, JSON in both directions).
//!
//! Ignored by default: it seeds a lot of rows, and its numbers only mean something in a release
//! build (SQLite is compiled with the profile's optimisation level). Run it with
//!
//! ```text
//! CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off \
//!   cargo test --release -p apm-server --test browse_perf -- --ignored --nocapture
//! ```
//!
//! and read the table it prints (`!` marks a median over the 200 ms target). The numbers of the last
//! run are in the docs of `apm_server::api::browse`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::time::Instant;

use apm_core::dto::Filter;
use apm_core::names::{normalize_name, stem_norm};
use apm_core::paths::{FileId, VolumeId};
use apm_server::db::Db;
use axum::http::StatusCode;
use common::TestApp;
use common::browse;
use common::seed::Seed;
use serde_json::{Value, json};
use sqlx::QueryBuilder;

const CAMERAS: [&str; 7] = [
    "Canon EOS R5",
    "Canon EOS 5D Mark IV",
    "Sony ILCE-7M3",
    "Nikon Z6",
    "Apple iPhone 14 Pro",
    "Fujifilm X-T4",
    "Google Pixel 7",
];
/// Cumulative share (percent) of the files taken by each camera; the rest has no camera.
const CAMERA_SHARE: [u64; 7] = [30, 55, 70, 80, 88, 93, 97];
const TAGS: u64 = 30;
const RUNS: usize = 7;
const TARGET_MS: f64 = 200.0;

/// splitmix64: deterministic, so every run measures the same library.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A directory to create and how many files to put in it. `year` and `month` are 0 when the
/// directory says nothing about the capture time of its files.
struct DirPlan {
    volume_id: VolumeId,
    path: String,
    files: u64,
    year: i64,
    month: i64,
}

struct FileRow {
    id: FileId,
    volume_id: VolumeId,
    dir: String,
    name: String,
    kind: &'static str,
    ext: &'static str,
    size: i64,
    mtime: i64,
    capture_ts: Option<i64>,
    camera_id: Option<i64>,
    rating: i64,
    flag: i64,
}

struct Library {
    photos: VolumeId,
    cards: VolumeId,
    dump: VolumeId,
    files: i64,
    dirs: usize,
}

/// `photos`: 20 years of 12 months of 62 events of 10 to 12 files, a few files per month, and a flat
/// `inbox` of 5,000 files. `cards`: 5,000 folders at the root, 6 files each. `dump`: one flat folder
/// of 50,000 files.
fn plans(photos: VolumeId, cards: VolumeId, dump: VolumeId) -> Vec<DirPlan> {
    let plan = |volume_id, path: String, files, year, month| DirPlan {
        volume_id,
        path,
        files,
        year,
        month,
    };
    let mut plans = Vec::new();
    for year in 2005..=2024 {
        plans.push(plan(photos, format!("{year}"), 0, year, 6));
        for month in 1..=12 {
            plans.push(plan(photos, format!("{year}/{month:02}"), 2, year, month));
            for event in 1..=62u64 {
                plans.push(plan(
                    photos,
                    format!("{year}/{month:02}/{event:02} Event"),
                    10 + event % 3,
                    year,
                    month,
                ));
            }
        }
    }
    plans.push(plan(photos, "inbox".to_owned(), 5000, 0, 0));
    for card in 0..5000 {
        plans.push(plan(cards, format!("card{card:04}"), 6, 0, 0));
    }
    plans.push(plan(dump, "all".to_owned(), 50_000, 0, 0));
    plans
}

fn generate(rng: &mut Rng, ids: &mut i64, plan: &DirPlan, cameras: &[i64], out: &mut Vec<FileRow>) {
    let salt = rng.below(10_000);
    for n in 0..plan.files {
        *ids += 1;
        let roll = rng.below(100);
        let (kind, ext, prefix) = match roll {
            0..=7 => ("sidecar", "xmp", "IMG"),
            8..=12 => ("video", "mp4", "MVI"),
            _ => ("image", "jpg", "IMG"),
        };
        let (year, month) = if plan.year == 0 {
            (2005 + rng.below(20) as i64, 1 + rng.below(12) as i64)
        } else {
            (plan.year, plan.month)
        };
        let sidecar = kind == "sidecar";
        let capture_ts = (!sidecar && rng.below(100) < 95).then(|| {
            ((year - 1970) * 365 + (month - 1) * 30) * 86_400 + rng.below(28 * 86_400) as i64
        });
        let camera_roll = rng.below(100);
        let camera_id = (!sidecar)
            .then(|| CAMERA_SHARE.iter().position(|&share| camera_roll < share))
            .flatten()
            .map(|index| cameras[index]);
        let rating = match rng.below(100) {
            _ if sidecar => 0,
            0..=79 => 0,
            80..=85 => 1,
            86..=90 => 2,
            91..=94 => 3,
            95..=97 => 4,
            _ => 5,
        };
        let flag = match rng.below(100) {
            _ if sidecar => 0,
            0..=2 => 1,
            3..=4 => -1,
            _ => 0,
        };
        // The name is unique in its directory: the counter, not the salted number, tells files apart.
        let name = format!("{prefix}_{n:05}_{}.{ext}", (n * 37 + salt) % 10_000);
        out.push(FileRow {
            id: *ids,
            volume_id: plan.volume_id,
            dir: plan.path.clone(),
            name,
            kind,
            ext,
            size: if sidecar {
                4_000
            } else {
                1_000_000 + rng.below(30_000_000) as i64
            },
            mtime: 1_600_000_000 + rng.below(100_000_000) as i64,
            capture_ts,
            camera_id,
            rating,
            flag,
        });
    }
}

/// Bulk `INSERT`s, at most 2,000 rows (30,000 bind values) each: SQLite allows 32,766 per statement.
async fn insert_files(tx: &mut sqlx::SqliteConnection, files: &[FileRow]) {
    for chunk in files.chunks(2_000) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO files (id, volume_id, dir_path, name, name_norm, stem_norm, ext, kind, \
             size, mtime, capture_ts, capture_src, camera_id, rating, flag) ",
        );
        qb.push_values(chunk, |mut row, file| {
            row.push_bind(file.id)
                .push_bind(file.volume_id)
                .push_bind(file.dir.clone())
                .push_bind(file.name.clone())
                .push_bind(normalize_name(&file.name))
                .push_bind(stem_norm(&file.name))
                .push_bind(file.ext)
                .push_bind(file.kind)
                .push_bind(file.size)
                .push_bind(file.mtime)
                .push_bind(file.capture_ts)
                .push_bind(file.capture_ts.map(|_| "exif"))
                .push_bind(file.camera_id)
                .push_bind(file.rating)
                .push_bind(file.flag);
        });
        qb.build().execute(&mut *tx).await.unwrap();
    }
}

/// Fills the database the way a scan of the whole library would, and refreshes the statistics.
async fn seed(db: &Db) -> Library {
    let seeder = Seed::new(db);
    let photos = seeder.volume("photos").await;
    let cards = seeder.volume("cards").await;
    let dump = seeder.volume("dump").await;
    let mut cameras = Vec::new();
    for name in CAMERAS {
        cameras.push(seeder.camera(name).await);
    }
    let mut tags = Vec::new();
    for n in 0..TAGS {
        tags.push(seeder.tag(&format!("tag {n:02}")).await);
    }

    let plans = plans(photos, cards, dump);
    let mut rng = Rng(0x5EED);
    let mut tx = db.begin_write().await.unwrap();

    for chunk in plans.chunks(4_000) {
        let mut qb = QueryBuilder::new("INSERT INTO dirs (volume_id, path, parent_path, name) ");
        qb.push_values(chunk, |mut row, plan| {
            let (parent, name) = plan
                .path
                .rsplit_once('/')
                .unwrap_or(("", plan.path.as_str()));
            row.push_bind(plan.volume_id)
                .push_bind(plan.path.clone())
                .push_bind(parent.to_owned())
                .push_bind(name.to_owned());
        });
        qb.build().execute(&mut *tx).await.unwrap();
    }

    let mut ids = 0;
    let mut buffer = Vec::new();
    let mut tagged: Vec<(FileId, i64)> = Vec::new();
    for plan in &plans {
        let first = buffer.len();
        generate(&mut rng, &mut ids, plan, &cameras, &mut buffer);
        for file in &buffer[first..] {
            if file.kind != "sidecar" && rng.below(100) < 20 {
                let tag = tags[rng.below(TAGS) as usize];
                tagged.push((file.id, tag));
                if rng.below(100) < 30 {
                    let other = tags[rng.below(TAGS) as usize];
                    if other != tag {
                        tagged.push((file.id, other));
                    }
                }
            }
        }
        if buffer.len() >= 2_000 {
            insert_files(&mut tx, &buffer).await;
            buffer.clear();
        }
    }
    insert_files(&mut tx, &buffer).await;
    for chunk in tagged.chunks(10_000) {
        let mut qb = QueryBuilder::new("INSERT INTO file_tags (file_id, tag_id) ");
        qb.push_values(chunk.to_vec(), |mut row, (file_id, tag_id)| {
            row.push_bind(file_id).push_bind(tag_id);
        });
        qb.build().execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();

    // What the scanner does at the end of a scan; creates sqlite_stat1, which the readers then load.
    db.optimize().await.unwrap();
    let analysed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'files'")
        .fetch_one(db.read())
        .await
        .unwrap();
    assert!(analysed > 0, "no statistics for files");

    Library {
        photos,
        cards,
        dump,
        files: ids,
        dirs: plans.len() + 3,
    }
}

#[derive(Clone, Copy)]
enum Endpoint {
    Folders,
    Items,
    Ids,
    Facets,
    Volumes,
}

/// The size of a response, in the unit of its endpoint: entries, ids or facet values.
fn rows(endpoint: Endpoint, response: &Value) -> usize {
    let len = |key: &str| response[key].as_array().map_or(0, Vec::len);
    match endpoint {
        Endpoint::Folders => len("folders"),
        Endpoint::Items => len("items"),
        Endpoint::Ids => len("ids"),
        Endpoint::Facets => len("cameras") + len("tags") + len("ratings"),
        Endpoint::Volumes => response.as_array().map_or(0, Vec::len),
    }
}

/// Runs the request once to warm up and `RUNS` times to measure, and prints one line of the table.
async fn measure(app: &TestApp, label: &str, endpoint: Endpoint, body: Value) {
    let body = &body;
    let run = || async move {
        let (status, response) = match endpoint {
            Endpoint::Folders => browse::folders(app, body.clone()).await,
            Endpoint::Items => browse::items(app, body.clone()).await,
            Endpoint::Ids => browse::ids(app, body.clone()).await,
            Endpoint::Facets => browse::facets(app, body.clone()).await,
            Endpoint::Volumes => browse::get(app, "/api/volumes").await,
        };
        assert_eq!(status, StatusCode::OK, "{label}: {response}");
        rows(endpoint, &response)
    };

    let count = run().await;
    let mut millis = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        assert_eq!(run().await, count, "{label}");
        millis.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    millis.sort_by(f64::total_cmp);
    let median = millis[RUNS / 2];
    let flag = if median > TARGET_MS { '!' } else { ' ' };
    println!(
        "{label:<58} {count:>8} rows   min {:>7.1}   median {:>7.1}{flag}  max {:>7.1}  ms",
        millis[0],
        median,
        millis[RUNS - 1]
    );
}

fn request(volume: Option<VolumeId>, dir: &str, filter: &Filter) -> Value {
    json!({"volume_id": volume, "dir": dir, "filter": filter})
}

fn paged(mut body: Value, sort: &str, order: &str, offset: u64) -> Value {
    body["sort"] = json!(sort);
    body["order"] = json!(order);
    body["offset"] = json!(offset);
    body
}

async fn time<F, Fut>(label: &str, run: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    println!("\n-- {label}");
    run().await;
}

#[tokio::test]
#[ignore = "seeds 250,000 files and prints timings; run it by hand in a release build"]
async fn timings_over_http_on_a_large_library() {
    let app = TestApp::new().await;
    let started = Instant::now();
    let library = seed(&app.state.db).await;
    println!(
        "\nlibrary: {} files in {} directories (seeded and analysed in {:.1} s)",
        library.files,
        library.dirs,
        started.elapsed().as_secs_f64()
    );
    let app = &app;
    let photos = Some(library.photos);
    let cards = Some(library.cards);
    let dump = Some(library.dump);

    let nothing = Filter::default();
    let one_camera = Filter {
        cameras: vec![CAMERAS[0].to_owned()],
        ..Filter::default()
    };
    let selective = Filter {
        cameras: vec![CAMERAS[0].to_owned(), CAMERAS[1].to_owned()],
        ratings: vec![3, 4, 5],
        tags: vec![
            "tag 00".to_owned(),
            "tag 01".to_owned(),
            "tag 02".to_owned(),
        ],
        ..Filter::default()
    };

    time("the volumes", || async {
        measure(app, "GET /api/volumes", Endpoint::Volumes, Value::Null).await;
        for (label, filter) in [
            ("volumes root", &nothing),
            ("volumes root, one camera", &one_camera),
            ("volumes root, selective filter", &selective),
        ] {
            let body = request(None, "", filter);
            measure(app, &format!("folders: {label}"), Endpoint::Folders, body).await;
        }
        let body = request(None, "", &nothing);
        measure(app, "items: volumes root (empty)", Endpoint::Items, body).await;
    })
    .await;

    time(
        "the folder list and the footer (what a navigation asks for)",
        || async {
            for (label, volume, dir, filter) in [
                ("photos root (20 folders)", photos, "", &nothing),
                ("photos root, one camera", photos, "", &one_camera),
                ("photos root, selective filter", photos, "", &selective),
                ("cards root (5,000 folders)", cards, "", &nothing),
                ("cards root, selective filter", cards, "", &selective),
                ("dump root (1 folder)", dump, "", &nothing),
                ("photos/2015 (12 folders)", photos, "2015", &nothing),
                (
                    "one event (no folders)",
                    photos,
                    "2015/06/10 Event",
                    &nothing,
                ),
            ] {
                let body = request(volume, dir, filter);
                measure(app, &format!("folders: {label}"), Endpoint::Folders, body).await;
            }
        },
    )
    .await;

    time("an item page of 200", || async {
        for (label, volume, dir, filter, sort, order, offset) in [
            (
                "one event, date desc",
                photos,
                "2015/06/10 Event",
                &nothing,
                "date",
                "desc",
                0,
            ),
            (
                "inbox (5,000 files), name asc",
                photos,
                "inbox",
                &nothing,
                "name",
                "asc",
                0,
            ),
            (
                "inbox, date desc",
                photos,
                "inbox",
                &nothing,
                "date",
                "desc",
                0,
            ),
            (
                "inbox, size desc",
                photos,
                "inbox",
                &nothing,
                "size",
                "desc",
                0,
            ),
            (
                "inbox, name asc, page 25",
                photos,
                "inbox",
                &nothing,
                "name",
                "asc",
                4_800,
            ),
            (
                "dump/all (50,000 files), name asc",
                dump,
                "all",
                &nothing,
                "name",
                "asc",
                0,
            ),
            (
                "dump/all, date desc",
                dump,
                "all",
                &nothing,
                "date",
                "desc",
                0,
            ),
            (
                "dump/all, size desc",
                dump,
                "all",
                &nothing,
                "size",
                "desc",
                0,
            ),
            (
                "dump/all, name asc, page 250",
                dump,
                "all",
                &nothing,
                "name",
                "asc",
                49_800,
            ),
            (
                "dump/all, date desc, page 250",
                dump,
                "all",
                &nothing,
                "date",
                "desc",
                49_800,
            ),
            (
                "dump/all, one camera, name asc",
                dump,
                "all",
                &one_camera,
                "name",
                "asc",
                0,
            ),
            (
                "dump/all, selective, date desc",
                dump,
                "all",
                &selective,
                "date",
                "desc",
                0,
            ),
        ] {
            let body = paged(request(volume, dir, filter), sort, order, offset);
            measure(app, &format!("items: {label}"), Endpoint::Items, body).await;
        }
    })
    .await;

    time("id lists", || async {
        for (label, volume, dir, filter, sort, order) in [
            (
                "inbox (about 4,600)",
                photos,
                "inbox",
                &nothing,
                "name",
                "asc",
            ),
            (
                "dump/all (about 46,000), name asc",
                dump,
                "all",
                &nothing,
                "name",
                "asc",
            ),
            ("dump/all, date desc", dump, "all", &nothing, "date", "desc"),
            ("dump/all, size asc", dump, "all", &nothing, "size", "asc"),
            (
                "dump/all, one camera (about 14,000)",
                dump,
                "all",
                &one_camera,
                "name",
                "asc",
            ),
        ] {
            let body = paged(request(volume, dir, filter), sort, order, 0);
            measure(app, &format!("ids: {label}"), Endpoint::Ids, body).await;
        }
    })
    .await;

    time("facets", || async {
        for (label, volume, dir, filter) in [
            ("one event", photos, "2015/06/10 Event", &nothing),
            ("photos/2015", photos, "2015", &nothing),
            ("dump/all", dump, "all", &nothing),
            ("photos root", photos, "", &nothing),
            ("photos root, one camera", photos, "", &one_camera),
            ("volumes root (every volume)", None, "", &nothing),
        ] {
            let body = request(volume, dir, filter);
            measure(app, &format!("facets: {label}"), Endpoint::Facets, body).await;
        }
    })
    .await;
}
