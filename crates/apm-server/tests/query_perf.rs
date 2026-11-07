//! Timings of the query engine on a library of about 200,000 files in about 20,000 directories.
//!
//! Ignored by default: it seeds a lot of rows, and its numbers only mean something in a release
//! build (SQLite is compiled with the profile's optimisation level). Run it with
//!
//! ```text
//! cargo test --release -p apm-server --test query_perf -- --ignored --nocapture
//! ```
//!
//! and read the table it prints. The numbers of the last run are in the docs of
//! `apm_server::query`.
//!
//! Set `APM_PERF_KEEP` to a path that does not exist yet to keep a copy of the seeded database
//! there (`VACUUM INTO`), for `EXPLAIN QUERY PLAN` and index experiments in the `sqlite3` shell.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::time::Instant;

use apm_core::dto::{Filter, SortKey, SortOrder};
use apm_core::names::{normalize_name, stem_norm};
use apm_core::paths::{FileId, RelPath, SelectionId, VolumeId};
use apm_server::db::Db;
use apm_server::query::{FileQuery, QueryScope};
use common::seed::Seed;
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
const PAGE: i64 = 200;
const RUNS: usize = 7;

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
    /// A server-side selection of 12 files spread over the whole library.
    selection: SelectionId,
    files: i64,
    dirs: usize,
}

/// `photos`: 20 years of 12 months of 62 events of 10 to 12 files, a few files per month, and a flat
/// `inbox` of 5,000 files. `cards`: 5,000 folders at the root, 6 files each.
fn plans(photos: VolumeId, cards: VolumeId) -> Vec<DirPlan> {
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
        let name = format!("{prefix}_{:04}.{ext}", (n * 37 + salt) % 10_000);
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
    let mut cameras = Vec::new();
    for name in CAMERAS {
        cameras.push(seeder.camera(name).await);
    }
    let mut tags = Vec::new();
    for n in 0..TAGS {
        tags.push(seeder.tag(&format!("tag {n:02}")).await);
    }

    let plans = plans(photos, cards);
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

    let spread: Vec<FileId> = (1..=12).map(|n| n * ids / 13).collect();
    let selection = seeder.selection(&spread).await;

    Library {
        photos,
        cards,
        selection,
        files: ids,
        dirs: plans.len() + 2,
    }
}

fn scope(volume_id: Option<VolumeId>, dir: &str, recursive: bool) -> QueryScope {
    QueryScope {
        volume_id,
        dir: RelPath::new(dir).unwrap(),
        recursive,
    }
}

fn query(volume_id: Option<VolumeId>, dir: &str, recursive: bool, filter: &Filter) -> FileQuery {
    FileQuery::new(scope(volume_id, dir, recursive), filter).unwrap()
}

/// Runs `run` once to warm up and `RUNS` times to measure, and prints one line of the table.
async fn measure<F, Fut>(label: &str, mut run: F)
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
        "{label:<64} {rows:>8} rows   min {:>7.1}   median {:>7.1}   max {:>7.1}  ms",
        millis[0],
        millis[RUNS / 2],
        millis[RUNS - 1]
    );
}

fn sorted(query: &FileQuery, sort: SortKey, order: SortOrder) -> FileQuery {
    query.clone().sorted(sort, order)
}

#[tokio::test]
#[ignore = "seeds 200,000 files and prints timings; run it by hand in a release build"]
async fn timings_on_a_large_library() {
    let db = Db::open_temp().await.unwrap();
    let started = Instant::now();
    let library = seed(&db).await;
    println!(
        "\nlibrary: {} files in {} directories (seeded and analysed in {:.1} s)\n",
        library.files,
        library.dirs,
        started.elapsed().as_secs_f64()
    );
    let db = &*db;
    if let Ok(path) = std::env::var("APM_PERF_KEEP") {
        sqlx::query("VACUUM INTO ?")
            .bind(&path)
            .execute(db.write())
            .await
            .unwrap();
        println!("copy of the seeded database: {path}\n");
    }
    let photos = Some(library.photos);
    let cards = Some(library.cards);

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

    println!("-- the folder list and the footer (what a navigation asks for)");
    for (label, volume_id, dir, filter) in [
        ("photos root", photos, "", &nothing),
        ("photos root, one camera", photos, "", &one_camera),
        ("photos root, selective filter", photos, "", &selective),
        ("cards root (5,000 folders)", cards, "", &nothing),
        ("photos/2015 (12 folders)", photos, "2015", &nothing),
        (
            "one event (no folders)",
            photos,
            "2015/06/10 Event",
            &nothing,
        ),
    ] {
        let q = query(volume_id, dir, false, filter);
        let q = &q;
        measure(&format!("folders + footer: {label}"), || async move {
            let folders = q.folders(db).await.unwrap();
            q.footer_counts(db).await.unwrap();
            folders.len()
        })
        .await;
    }

    println!("\n-- an item page of {PAGE}");
    let one_selection = Filter {
        selection_id: Some(library.selection),
        ..Filter::default()
    };
    let event = query(photos, "2015/06/10 Event", false, &nothing);
    let inbox = query(photos, "inbox", false, &nothing);
    let year = query(photos, "2015", true, &nothing);
    let month = query(photos, "2015/06", true, &nothing);
    let root = query(photos, "", true, &nothing);
    let root_camera = query(photos, "", true, &one_camera);
    let root_selective = query(photos, "", true, &selective);
    let root_selection = query(photos, "", true, &one_selection);
    let cards_root = query(cards, "", true, &nothing);
    let everything = query(None, "", true, &nothing);
    let everything_camera = query(None, "", true, &one_camera);
    let everything_selective = query(None, "", true, &selective);
    let everything_selection = query(None, "", true, &one_selection);
    for (label, q, sort, order, offset) in [
        (
            "one event, date desc",
            &event,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "inbox (5,000 files), name asc",
            &inbox,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "inbox, date desc",
            &inbox,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "inbox, size desc",
            &inbox,
            SortKey::Size,
            SortOrder::Desc,
            0,
        ),
        (
            "inbox, name asc, page 25",
            &inbox,
            SortKey::Name,
            SortOrder::Asc,
            4_800,
        ),
        (
            "photos/2015 recursive, date desc",
            &year,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "photos/2015 recursive, name asc",
            &year,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "photos/2015/06 (744 files) recursive, date desc",
            &month,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "photos/2015/06 recursive, date asc",
            &month,
            SortKey::Date,
            SortOrder::Asc,
            0,
        ),
        (
            "photos root recursive, name asc",
            &root,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "photos root recursive, name desc",
            &root,
            SortKey::Name,
            SortOrder::Desc,
            0,
        ),
        (
            "photos root recursive, date desc",
            &root,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "photos root recursive, date asc",
            &root,
            SortKey::Date,
            SortOrder::Asc,
            0,
        ),
        (
            "photos root recursive, size desc",
            &root,
            SortKey::Size,
            SortOrder::Desc,
            0,
        ),
        (
            "photos root recursive, date desc, page 100",
            &root,
            SortKey::Date,
            SortOrder::Desc,
            20_000,
        ),
        (
            "photos root recursive, one camera, date desc",
            &root_camera,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "photos root recursive, one camera, name asc",
            &root_camera,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "photos root recursive, selective, date desc",
            &root_selective,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "photos root recursive, selective, name asc",
            &root_selective,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "photos root recursive, 12 selected, date desc",
            &root_selection,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "cards root recursive, date desc",
            &cards_root,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "every volume, date desc",
            &everything,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "every volume, name asc",
            &everything,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, size asc",
            &everything,
            SortKey::Size,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, one camera, name asc",
            &everything_camera,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, one camera, date desc",
            &everything_camera,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "every volume, selective, name asc",
            &everything_selective,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, selective, size asc",
            &everything_selective,
            SortKey::Size,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, selective, date desc",
            &everything_selective,
            SortKey::Date,
            SortOrder::Desc,
            0,
        ),
        (
            "every volume, 12 selected, name asc",
            &everything_selection,
            SortKey::Name,
            SortOrder::Asc,
            0,
        ),
        (
            "every volume, 12 selected, size asc",
            &everything_selection,
            SortKey::Size,
            SortOrder::Asc,
            0,
        ),
    ] {
        let q = sorted(q, sort, order);
        let q = &q;
        measure(&format!("items: {label}"), || async move {
            q.items(db, offset, PAGE).await.unwrap().len()
        })
        .await;
    }

    println!("\n-- counts");
    for (label, q) in [
        ("photos root recursive", &root),
        ("photos root recursive, one camera", &root_camera),
        ("photos root recursive, selective", &root_selective),
        ("every volume, root recursive", &everything),
        ("inbox", &inbox),
    ] {
        measure(&format!("count: {label}"), || async move {
            usize::try_from(q.count(db).await.unwrap()).unwrap()
        })
        .await;
    }

    println!("\n-- id lists");
    for (label, q) in [
        ("inbox (5,000)", &inbox),
        (
            "photos root recursive, one camera (about 50,000)",
            &root_camera,
        ),
        ("photos root recursive (all)", &root),
    ] {
        measure(&format!("ids: {label}"), || async move {
            q.ids(db).await.unwrap().len()
        })
        .await;
    }

    println!("\n-- facets");
    for (label, q) in [
        ("one event", &event),
        ("photos/2015 recursive", &year),
        ("photos root recursive", &root),
        ("photos root recursive, one camera", &root_camera),
        ("every volume, root recursive", &everything),
    ] {
        measure(&format!("facets: {label}"), || async move {
            let facets = q.facets(db).await.unwrap();
            facets.cameras.len() + facets.tags.len() + facets.ratings.len()
        })
        .await;
    }
}
