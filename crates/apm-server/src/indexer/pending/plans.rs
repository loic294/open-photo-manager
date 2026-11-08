//! `EXPLAIN QUERY PLAN` of the statements of the stage queue, on a library with each kind of
//! planner statistics a real one goes through: none yet, current ones, and ones the stages have
//! made out of date. A page must be one search of the partial index of its state column, in the
//! order of the page, whatever the scope is, and the totals must never read the whole table.
//!
//! When an assertion fails the message holds the plan: fix the SQL (or the index), do not loosen
//! the test. Plans can change with the SQLite version, which is why they are pinned here.

use std::fmt;

use apm_core::paths::{RelPath, VolumeId};
use sqlx::{Execute, QueryBuilder, Sqlite};

use super::{PendingKey, PendingQueue, PipelineColumn, Scope, ScopeFilter, StageScope, VolumePlan};
use crate::db::testing::insert_volume;
use crate::db::{Db, TempDb};

const FILES_PER_VOLUME: i64 = 6000;
const COLUMNS: [PipelineColumn; 3] = [
    PipelineColumn::Meta,
    PipelineColumn::Hash,
    PipelineColumn::Thumb,
];

/// Files in the root, in `yrY` and in `yrY/mM`; one in twenty is a sidecar and the sizes repeat.
const SEED: &str = "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
     INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime, \
                        meta_state, hash_state, thumb_state) \
     SELECT ?, \
            CASE WHEN i % 50 = 0 THEN '' \
                 WHEN i % 41 = 0 THEN 'yr' || (i % 10) \
                 ELSE 'yr' || (i % 10) || '/m' || (i % 7) END, \
            'f' || i || '.jpg', 'f' || i || '.jpg', 'f' || i, 'jpg', \
            CASE WHEN i % 20 = 0 THEN 'sidecar' ELSE 'image' END, \
            (i * 7919) % 1000, 1, 0, 0, 0 \
     FROM n";

/// The stages finish 19 rows in 20, not the same ones in each state column.
const SETTLE: &str = "UPDATE files SET \
     meta_state = CASE WHEN ((id * 2654435761) >> 7) % 20 = 0 THEN 0 ELSE 1 END, \
     hash_state = CASE WHEN ((id * 2654435761) >> 9) % 20 = 0 THEN 0 ELSE 1 END, \
     thumb_state = CASE WHEN ((id * 2654435761) >> 11) % 20 = 0 THEN 0 ELSE 1 END";

/// What the planner knows about the partial indexes.
#[derive(Debug, Clone, Copy)]
enum Statistics {
    /// No `sqlite_stat1` yet, everything but one row in twenty has been done.
    Unanalysed,
    /// Everything is pending and `Db::optimize` has seen it: the partial indexes are as big as the
    /// table, which is how a first scan leaves them.
    Dense,
    /// One row in twenty is pending, and the statistics say so.
    Sparse,
    /// The statistics are those of `Dense`, then the stages finished 19 rows in 20.
    Stale,
}

const STATISTICS: [Statistics; 4] = [
    Statistics::Unanalysed,
    Statistics::Dense,
    Statistics::Sparse,
    Statistics::Stale,
];

/// Two volumes of 6000 files in 71 directories each; returns the first volume.
async fn library(statistics: Statistics) -> (TempDb, VolumeId) {
    let db = Db::open_temp().await.unwrap();
    let one = insert_volume(&db, "one").await;
    let two = insert_volume(&db, "two").await;
    for volume_id in [one, two] {
        sqlx::query(SEED)
            .bind(FILES_PER_VOLUME - 1)
            .bind(volume_id)
            .execute(db.write())
            .await
            .unwrap();
    }
    if matches!(statistics, Statistics::Dense | Statistics::Stale) {
        db.optimize().await.unwrap();
    }
    if !matches!(statistics, Statistics::Dense) {
        sqlx::query(SETTLE).execute(db.write()).await.unwrap();
    }
    if matches!(statistics, Statistics::Sparse) {
        db.optimize().await.unwrap();
    }
    (db, one)
}

/// `meta`, `hash` or `thumb`: what the column and index names of a stage are made of. Spelled out
/// here, not taken from the code under test.
fn stage(column: PipelineColumn) -> &'static str {
    match column {
        PipelineColumn::Meta => "meta",
        PipelineColumn::Hash => "hash",
        PipelineColumn::Thumb => "thumb",
    }
}

fn scope(dir: &str, recursive: bool) -> Scope {
    Scope {
        dir: RelPath::new(dir).unwrap(),
        recursive,
    }
}

/// Which bounds the scopes put on `dir_path`, which is what the constraint of the search shows.
#[derive(Debug, Clone, Copy)]
enum Bounds {
    /// None: the whole volume.
    Unbounded,
    /// An upper bound only: the root directory is one of the scopes.
    Upper,
    /// Both.
    Both,
}

struct Shape {
    label: &'static str,
    bounds: Bounds,
    scopes: Vec<Scope>,
}

impl Shape {
    fn volume_plan(&self, volume_id: VolumeId) -> VolumePlan {
        VolumePlan {
            volume_id,
            scopes: self.scopes.clone(),
        }
    }
}

fn shape(label: &'static str, bounds: Bounds, scopes: &[(&str, bool)]) -> Shape {
    Shape {
        label,
        bounds,
        scopes: scopes
            .iter()
            .map(|&(dir, recursive)| scope(dir, recursive))
            .collect(),
    }
}

fn shapes() -> Vec<Shape> {
    vec![
        shape("whole volume", Bounds::Unbounded, &[("", true)]),
        shape("subtree", Bounds::Both, &[("yr3", true)]),
        shape("nested subtree", Bounds::Both, &[("yr3/m2", true)]),
        shape("directory", Bounds::Both, &[("yr3/m2", false)]),
        shape("root directory", Bounds::Upper, &[("", false)]),
        shape(
            "root directory and a subtree",
            Bounds::Upper,
            &[("", false), ("yr3", true)],
        ),
        shape(
            "scattered scopes",
            Bounds::Both,
            &[("yr1", true), ("yr5/m3", false), ("yr8", true)],
        ),
    ]
}

/// Where a page starts.
#[derive(Debug, Clone, Copy)]
enum Cursor {
    /// The start of the queue.
    Start,
    /// A row inside every scope shape that has a lower bound.
    Resumed,
    /// A row before the first scope of every shape that has a lower bound.
    BeforeScopes,
}

const CURSORS: [Cursor; 3] = [Cursor::Start, Cursor::Resumed, Cursor::BeforeScopes];

impl Cursor {
    fn key(self, volume_id: VolumeId) -> Option<PendingKey> {
        let (dir_path, name) = match self {
            Self::Start => return None,
            Self::Resumed => ("yr3/m2", "f1000.jpg"),
            Self::BeforeScopes => ("yr0", "f0.jpg"),
        };
        Some(PendingKey {
            volume_id,
            dir_path: dir_path.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// The only line a page is allowed to have: a search of the partial index with the cursor (when it
/// is inside the hull of the scopes) as the lower bound, and the hull as the others.
fn page_line(column: PipelineColumn, bounds: Bounds, cursor: Cursor) -> String {
    let constraint = match (bounds, cursor) {
        (Bounds::Unbounded, Cursor::Start) => "volume_id=?",
        (Bounds::Unbounded, _) => "volume_id=? AND (dir_path,name)>(?,?)",
        (Bounds::Upper, Cursor::Start) => "volume_id=? AND dir_path<?",
        (Bounds::Upper, _) => "volume_id=? AND (dir_path,name)>(?,?) AND dir_path<?",
        (Bounds::Both, Cursor::Start | Cursor::BeforeScopes) => {
            "volume_id=? AND dir_path>? AND dir_path<?"
        }
        (Bounds::Both, Cursor::Resumed) => "volume_id=? AND (dir_path,name)>(?,?) AND dir_path<?",
    };
    format!(
        "SEARCH f USING INDEX files_{}_pending ({constraint})",
        stage(column)
    )
}

fn queue(column: PipelineColumn, plan: &VolumePlan) -> PendingQueue {
    PendingQueue::new(
        column,
        StageScope {
            plans: vec![plan.clone()],
        },
    )
}

/// The conditions a stage adds with `with_condition`, and the shapes of subquery they may hold.
const CONDITIONS: [(&str, &str); 5] = [
    (
        "sidecar",
        "EXISTS (SELECT 1 FROM files o WHERE o.volume_id = f.volume_id \
         AND o.dir_path = f.dir_path AND o.name = f.name || '.xmp')",
    ),
    (
        "no sidecar",
        "NOT EXISTS (SELECT 1 FROM files o WHERE o.volume_id = f.volume_id \
         AND o.dir_path = f.dir_path AND o.name = f.name || '.xmp')",
    ),
    (
        "size twin",
        "EXISTS (SELECT 1 FROM files o WHERE o.size = f.size AND o.id <> f.id)",
    ),
    (
        "size shared",
        "f.size IN (SELECT size FROM files GROUP BY size HAVING COUNT(*) > 1)",
    ),
    ("kind", "f.kind IN ('image', 'video')"),
];

/// The `detail` lines of the `EXPLAIN QUERY PLAN` of a statement.
struct Plan(Vec<String>);

impl Plan {
    fn is_scan_of_files(line: &str) -> bool {
        line == "SCAN f" || line.starts_with("SCAN f ")
    }

    /// Is `files` (alias `f`) read from the first row to the last? A subquery over the table has no
    /// alias, so `SCAN files ...` is not one.
    fn scans_files(&self) -> bool {
        self.0.iter().any(|line| Self::is_scan_of_files(line))
    }

    /// The lines about the rows of the queue, without the ones of a subquery.
    fn queue_lines(&self) -> Vec<&str> {
        self.0
            .iter()
            .map(String::as_str)
            .filter(|line| line.starts_with("SEARCH f ") || Self::is_scan_of_files(line))
            .collect()
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

/// The rows the planner believes an index holds: the first number of its `sqlite_stat1` row.
async fn estimated_rows(db: &Db, index: &str) -> Option<i64> {
    let analysed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'sqlite_stat1'")
            .fetch_one(db.write())
            .await
            .unwrap();
    if analysed == 0 {
        return None;
    }
    let stat: Option<String> = sqlx::query_scalar("SELECT stat FROM sqlite_stat1 WHERE idx = ?")
        .bind(index)
        .fetch_optional(db.write())
        .await
        .unwrap();
    stat?.split(' ').next()?.parse().ok()
}

#[tokio::test]
async fn the_fixture_has_the_statistics_it_names() {
    for statistics in STATISTICS {
        let (db, _) = library(statistics).await;
        for column in COLUMNS {
            let stage = stage(column);
            let believed = estimated_rows(&db, &format!("files_{stage}_pending")).await;
            let pending: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM files WHERE {stage}_state = 0"
            )))
            .fetch_one(db.write())
            .await
            .unwrap();
            let few = (500..=700).contains(&pending);
            let ok = match statistics {
                Statistics::Unanalysed => believed.is_none() && few,
                Statistics::Dense => believed == Some(12_000) && pending == 12_000,
                Statistics::Sparse => {
                    believed.is_some_and(|rows| (500..=700).contains(&rows)) && few
                }
                Statistics::Stale => believed == Some(12_000) && few,
            };
            assert!(
                ok,
                "{statistics:?}, {stage}: the planner believes {believed:?} rows are pending, {pending} are"
            );
        }
    }
}

#[tokio::test]
async fn a_page_is_one_search_of_the_partial_index() {
    for statistics in STATISTICS {
        let (db, one) = library(statistics).await;
        for shape in shapes() {
            let plan_of = shape.volume_plan(one);
            for column in COLUMNS {
                let queue = queue(column, &plan_of);
                for cursor in CURSORS {
                    let key = cursor.key(one);
                    for filter in [ScopeFilter::Sql, ScopeFilter::HullOnly] {
                        let statement = queue.page_statement(&plan_of, key.as_ref(), 500, filter);
                        let plan = plan(&db, statement).await;
                        assert_eq!(
                            plan.0,
                            [page_line(column, shape.bounds, cursor)],
                            "{statistics:?}, {}, {column:?}, {cursor:?}, {filter:?}:\n{plan}",
                            shape.label
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn a_condition_leaves_the_search_of_the_queue_alone() {
    for statistics in STATISTICS {
        let (db, one) = library(statistics).await;
        for shape in shapes() {
            let plan_of = shape.volume_plan(one);
            for (label, condition) in CONDITIONS {
                let queue = queue(PipelineColumn::Hash, &plan_of).with_condition(condition);
                for cursor in [Cursor::Start, Cursor::Resumed] {
                    let key = cursor.key(one);
                    let statement =
                        queue.page_statement(&plan_of, key.as_ref(), 500, ScopeFilter::Sql);
                    let plan = plan(&db, statement).await;
                    let expected = page_line(PipelineColumn::Hash, shape.bounds, cursor);
                    assert!(
                        plan.queue_lines() == [expected.as_str()] && !plan.sorts(),
                        "{statistics:?}, {}, {label}, {cursor:?}:\n{plan}",
                        shape.label
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn the_totals_never_read_the_whole_table() {
    for statistics in STATISTICS {
        let (db, one) = library(statistics).await;
        for shape in shapes() {
            let plan_of = shape.volume_plan(one);
            for column in COLUMNS {
                let mut queues = vec![("no condition", queue(column, &plan_of))];
                queues.extend(
                    CONDITIONS
                        .iter()
                        .map(|&(label, sql)| (label, queue(column, &plan_of).with_condition(sql))),
                );
                for (label, queue) in queues {
                    let plan = plan(&db, queue.totals_statement(&plan_of)).await;
                    assert!(
                        !plan.scans_files()
                            && plan.0.iter().any(|line| line.starts_with("SEARCH f USING")),
                        "{statistics:?}, {}, {column:?}, {label}:\n{plan}",
                        shape.label
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn the_totals_of_a_subtree_are_a_range_of_the_partial_index() {
    for statistics in STATISTICS {
        let (db, one) = library(statistics).await;
        for shape in shapes()
            .into_iter()
            .filter(|shape| matches!(shape.label, "subtree" | "nested subtree"))
        {
            let plan_of = shape.volume_plan(one);
            for column in COLUMNS {
                let plan = plan(&db, queue(column, &plan_of).totals_statement(&plan_of)).await;
                let expected = format!(
                    "SEARCH f USING INDEX files_{}_pending (volume_id=? AND dir_path>? AND dir_path<?)",
                    stage(column)
                );
                assert_eq!(
                    plan.0,
                    [expected],
                    "{statistics:?}, {}, {column:?}:\n{plan}",
                    shape.label
                );
            }
        }
    }
}

#[tokio::test]
async fn the_totals_of_a_whole_volume_are_one_search_of_an_index_of_the_queue() {
    for statistics in STATISTICS {
        let (db, one) = library(statistics).await;
        let whole = VolumePlan {
            volume_id: one,
            scopes: vec![scope("", true)],
        };
        for column in COLUMNS {
            let plan = plan(&db, queue(column, &whole).totals_statement(&whole)).await;
            let stage = stage(column);
            let allowed = [
                format!(
                    "SEARCH f USING INDEX files_{stage}_state (volume_id=? AND {stage}_state=?)"
                ),
                format!("SEARCH f USING INDEX files_{stage}_pending (volume_id=?)"),
            ];
            assert!(
                plan.0.len() == 1 && allowed.contains(&plan.0[0]),
                "{statistics:?}, {column:?}:\n{plan}"
            );
        }
    }
}
