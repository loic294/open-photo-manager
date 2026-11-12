//! `EXPLAIN QUERY PLAN` of the statements, on a library with the planner statistics of a real one
//! (sampled, as `Db::optimize` leaves them). A scoped query must search the `(volume_id, dir_path,
//! name)` index instead of scanning `files`, and the date index must serve the listings it was made
//! for and no others.
//!
//! When an assertion fails the message holds the plan: fix the SQL (or the index), do not loosen
//! the test. Plans can change with the SQLite version, which is why they are pinned here.

use std::fmt;

use opm_core::dto::{Filter, FlagFilter, SortKey, SortOrder};
use opm_core::paths::{RelPath, VolumeId};
use sqlx::{Execute, QueryBuilder, Sqlite};

use super::{FileQuery, QueryScope};
use crate::db::testing::{insert_dir, insert_volume};
use crate::db::{Db, TempDb};

const FILES_PER_VOLUME: i64 = 6000;
const DIR_INDEX: &str = "files_volume_dir_name";
const DATE_INDEX: &str = "files_volume_capture";

/// Two volumes of 6000 files in 70 directories (`yrY/mM`) each, with cameras, tags and the
/// statistics `Db::optimize` leaves after a scan.
async fn library() -> (TempDb, VolumeId, VolumeId) {
    let db = Db::open_temp().await.unwrap();
    let mut volumes = Vec::new();
    for name in ["one", "two"] {
        let volume_id = insert_volume(&db, name).await;
        insert_dir(&db, volume_id, "").await;
        for year in 0..10 {
            insert_dir(&db, volume_id, &format!("yr{year}")).await;
            for month in 0..7 {
                insert_dir(&db, volume_id, &format!("yr{year}/m{month}")).await;
            }
        }
        volumes.push(volume_id);
    }
    for n in 0..6 {
        sqlx::query("INSERT INTO cameras (name) VALUES (?)")
            .bind(format!("Camera {n}"))
            .execute(db.write())
            .await
            .unwrap();
    }
    for n in 0..20 {
        sqlx::query("INSERT INTO tags (name) VALUES (?)")
            .bind(format!("tag{n}"))
            .execute(db.write())
            .await
            .unwrap();
    }
    for &volume_id in &volumes {
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
             INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
                                mtime, capture_ts, camera_id, rating, flag) \
             SELECT ?, 'yr' || (i % 10) || '/m' || (i % 7), 'f' || i || '.jpg', 'f' || i || '.jpg', \
                    'f' || i, 'jpg', CASE WHEN i % 20 = 0 THEN 'sidecar' ELSE 'image' END, \
                    (i * 7919) % 1000, 1, 1700000000 + (i * 37) % 100000, 1 + i % 6, i % 6, \
                    i % 3 - 1 FROM n",
        )
        .bind(FILES_PER_VOLUME - 1)
        .bind(volume_id)
        .execute(db.write())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO file_tags (file_id, tag_id) SELECT id, 1 + id % 20 FROM files WHERE id % 2 = 0",
    )
    .execute(db.write())
    .await
    .unwrap();
    db.optimize().await.unwrap();
    let analysed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'files'")
        .fetch_one(db.write())
        .await
        .unwrap();
    assert!(analysed > 0, "PRAGMA optimize left no statistics for files");
    (db, volumes[0], volumes[1])
}

fn query(volume_id: Option<VolumeId>, dir: &str, recursive: bool, filter: &Filter) -> FileQuery {
    let scope = QueryScope {
        volume_id,
        dir: RelPath::new(dir).unwrap(),
        recursive,
    };
    FileQuery::new(scope, filter).unwrap()
}

/// A filter that keeps a few files: the planner must not let it change how the scope is searched.
fn selective() -> Filter {
    Filter {
        cameras: vec!["Camera 2".to_owned()],
        ratings: vec![3, 4],
        flags: vec![FlagFilter::Selected],
        tags: vec!["tag4".to_owned(), "tag6".to_owned()],
        selection_id: None,
    }
}

/// The `detail` lines of the `EXPLAIN QUERY PLAN` of a statement.
struct Plan(Vec<String>);

impl Plan {
    /// Is `files` (alias `f`) searched through `index` with exactly `constraint`?
    fn searches(&self, index: &str, constraint: &str) -> bool {
        self.0.iter().any(|line| {
            line.starts_with("SEARCH f USING")
                && line.contains(index)
                && line.ends_with(&format!("({constraint})"))
        })
    }

    /// Is `files` read from the first row to the last?
    fn scans_files(&self) -> bool {
        self.0.iter().any(|line| line.starts_with("SCAN f"))
    }

    fn mentions(&self, text: &str) -> bool {
        self.0.iter().any(|line| line.contains(text))
    }

    fn sorts(&self) -> bool {
        self.0
            .iter()
            .any(|line| line.starts_with("USE TEMP B-TREE FOR") && line.contains("ORDER BY"))
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|line| writeln!(f, "    {line}"))
    }
}

/// Plans a statement on the connection that ran `PRAGMA optimize`, the one that holds the
/// statistics (the readers load them when they open).
async fn plan(db: &Db, mut statement: QueryBuilder<Sqlite>) -> Plan {
    let mut built = statement.build();
    let arguments = built.take_arguments().unwrap().unwrap_or_default();
    let sql = format!("EXPLAIN QUERY PLAN {}", built.sql().as_str());
    let rows: Vec<(i64, i64, i64, String)> =
        sqlx::query_as_with(sqlx::AssertSqlSafe(sql), arguments)
            .fetch_all(db.write())
            .await
            .unwrap();
    Plan(rows.into_iter().map(|(.., detail)| detail).collect())
}

/// Every statement that reads the scope's files (not the footer and the folders, which always read
/// the subtree), with a label for the failure message.
fn scope_statements(q: &FileQuery) -> Vec<(String, QueryBuilder<Sqlite>)> {
    let mut statements = Vec::new();
    for sort in [SortKey::Name, SortKey::Date, SortKey::Size] {
        for order in [SortOrder::Asc, SortOrder::Desc] {
            statements.push((
                format!("items {sort:?} {order:?}"),
                q.clone().sorted(sort, order).items_statement(0, 200),
            ));
        }
    }
    statements.push(("ids".to_owned(), q.ids_statement()));
    statements.push(("count".to_owned(), q.count_statement()));
    statements.push(("camera facet".to_owned(), q.camera_facet_statement()));
    statements.push(("tag facet".to_owned(), q.tag_facet_statement()));
    statements.push(("rating facet".to_owned(), q.rating_facet_statement()));
    statements
}

fn both_filters() -> [(&'static str, Filter); 2] {
    [("no filter", Filter::default()), ("selective", selective())]
}

#[tokio::test]
async fn a_directory_is_one_search_of_the_directory_index() {
    let (db, one, two) = library().await;
    for (volume_id, dir) in [(Some(one), "yr3/m2"), (Some(two), "yr0"), (Some(one), "")] {
        for (filter_label, filter) in both_filters() {
            let q = query(volume_id, dir, false, &filter);
            for (label, statement) in scope_statements(&q) {
                let plan = plan(&db, statement).await;
                assert!(
                    plan.searches(DIR_INDEX, "volume_id=? AND dir_path=?") && !plan.scans_files(),
                    "{label}, dir {dir:?}, {filter_label}:\n{plan}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_subtree_is_a_range_of_the_directory_index() {
    let (db, one, _two) = library().await;
    for dir in ["yr3", "yr3/m2"] {
        for (filter_label, filter) in both_filters() {
            let q = query(Some(one), dir, true, &filter);
            let mut statements = scope_statements(&q);
            statements.push(("footer".to_owned(), q.footer_statement()));
            for (label, statement) in statements {
                let plan = plan(&db, statement).await;
                assert!(
                    plan.searches(DIR_INDEX, "volume_id=? AND dir_path>? AND dir_path<?")
                        && !plan.scans_files(),
                    "{label}, subtree {dir:?}, {filter_label}:\n{plan}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_directory_of_every_volume_keeps_the_directory_index() {
    let (db, ..) = library().await;
    for (dir, recursive, constraint) in [
        ("yr3/m2", false, "volume_id=? AND dir_path=?"),
        ("yr3", true, "volume_id=? AND dir_path>? AND dir_path<?"),
    ] {
        let q = query(None, dir, recursive, &Filter::default());
        for (label, statement) in scope_statements(&q) {
            let plan = plan(&db, statement).await;
            assert!(
                plan.searches(DIR_INDEX, constraint) && !plan.scans_files(),
                "{label}, every volume, {dir:?}, recursive {recursive}:\n{plan}"
            );
        }
    }
}

#[tokio::test]
async fn a_whole_volume_is_searched_never_scanned() {
    let (db, one, _two) = library().await;
    for (filter_label, filter) in both_filters() {
        let q = query(Some(one), "", true, &filter);
        let mut statements = scope_statements(&q);
        statements.push(("footer".to_owned(), q.footer_statement()));
        for (label, statement) in statements {
            let plan = plan(&db, statement).await;
            // Unfiltered it is the volume prefix of an index; a selective filter may drive the
            // search through an index of its own.
            let searched = plan.0.iter().any(|line| {
                line.starts_with("SEARCH f USING")
                    && (!q.filter_is_empty() || line.ends_with("(volume_id=?)"))
            });
            assert!(
                searched && !plan.scans_files(),
                "{label}, volume root, {filter_label}:\n{plan}"
            );
        }
    }
}

#[tokio::test]
async fn the_newest_first_timeline_of_a_volume_is_read_in_index_order() {
    let (db, one, _two) = library().await;
    let q = query(Some(one), "", true, &Filter::default()).sorted(SortKey::Date, SortOrder::Desc);
    for (label, statement) in [
        ("items", q.items_statement(0, 200)),
        ("ids", q.ids_statement()),
    ] {
        let plan = plan(&db, statement).await;
        assert!(
            plan.searches(DATE_INDEX, "volume_id=?") && !plan.sorts(),
            "{label}:\n{plan}"
        );
    }
}

#[tokio::test]
async fn only_the_unfiltered_whole_volume_timeline_uses_the_date_index() {
    let (db, one, _two) = library().await;
    let sort = (SortKey::Date, SortOrder::Desc);
    let cases = [
        // A filter would make the planner walk the whole index looking for the few matches.
        ("filtered volume", query(Some(one), "", true, &selective())),
        (
            "folder",
            query(Some(one), "yr3/m2", false, &Filter::default()),
        ),
        ("subtree", query(Some(one), "yr3", true, &Filter::default())),
        ("every volume", query(None, "", true, &Filter::default())),
    ];
    for (label, q) in cases {
        let plan = plan(&db, q.sorted(sort.0, sort.1).items_statement(0, 200)).await;
        assert!(!plan.mentions(DATE_INDEX), "{label}:\n{plan}");
    }
    let filtered = query(Some(one), "", true, &selective()).sorted(sort.0, sort.1);
    let plan = plan(&db, filtered.items_statement(0, 200)).await;
    assert!(plan.sorts(), "a filtered timeline is sorted:\n{plan}");
}

#[tokio::test]
async fn the_folder_list_searches_the_subtree_and_the_child_directories() {
    let (db, one, _two) = library().await;
    for dir in ["", "yr3", "yr3/m2"] {
        for (filter_label, filter) in both_filters() {
            let q = query(Some(one), dir, false, &filter);
            let plan = plan(&db, q.folders_statement(one)).await;
            let files = if dir.is_empty() {
                plan.0.iter().any(|line| {
                    line.starts_with("SEARCH f USING") && line.ends_with("(volume_id=?)")
                })
            } else {
                plan.searches(DIR_INDEX, "volume_id=? AND dir_path>? AND dir_path<?")
            };
            assert!(
                files
                    && !plan.scans_files()
                    && plan.mentions(
                        "SEARCH d USING INDEX dirs_parent (volume_id=? AND parent_path=?)"
                    ),
                "folders of {dir:?}, {filter_label}:\n{plan}"
            );
        }
    }
}
