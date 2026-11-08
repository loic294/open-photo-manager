//! Chunked bulk writes into the tables of a plan: ONE implementation for the three planners.
//!
//! A plan can hold millions of rows and the database has a single writer connection, so a build
//! must never write them in one transaction: the scanner, `FsOps` and the task engine would wait
//! for all of it. [`BulkWriter`] collects rows and writes them with `QueryBuilder::push_values`,
//! one statement and one transaction per chunk of at most [`CHUNK_ROWS`] rows (and at most what
//! the 32,766 bind limit of SQLite allows for the column count), so other writers get their turn
//! between two chunks. The cancellation token is checked before every chunk.
//!
//! ```ignore
//! let mut writer = BulkWriter::new(&ctx.db, &ctx.cancel, "dup_members", &["plan_id", "file_id"],
//!     |mut row, file_id: i64| { row.push_bind(plan_id).push_bind(file_id); })?;
//! for file_id in ids { writer.push(file_id).await?; }
//! let written = writer.finish().await?;
//! ```
//!
//! [`delete_plan_rows`] is the counterpart: it empties a table of the rows of one plan in chunks,
//! which the engine calls for every table a planner lists in `Planner::child_tables`.

use apm_core::paths::PlanId;
use sqlx::query_builder::Separated;
use sqlx::{QueryBuilder, Sqlite};
use tokio_util::sync::CancellationToken;

use super::planner::PlanError;
use crate::db::Db;

/// The most rows of one transaction. The database module asks for about 1,000 rows and the brief
/// allows 5,000 or about 100 ms: 2,000 rows of a plan table take a few milliseconds even on a NAS.
pub const CHUNK_ROWS: usize = 2_000;

/// SQLite's limit of bound variables in one statement.
const MAX_BIND_PARAMS: usize = 32_766;

/// How many rows one statement may carry for `columns` bound values per row.
pub fn rows_per_statement(columns: usize) -> usize {
    (MAX_BIND_PARAMS / columns.max(1)).clamp(1, CHUNK_ROWS)
}

/// Table and column names go into the statement text, so only plain identifiers are accepted.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn check_identifier(kind: &str, name: &str) -> Result<(), PlanError> {
    if is_identifier(name) {
        Ok(())
    } else {
        Err(PlanError::Internal(anyhow::anyhow!(
            "{name:?} is not a valid {kind} name for a bulk write"
        )))
    }
}

/// Writes rows of type `T` into one table. `bind` pushes exactly one bound value per column, in
/// the order of `columns`, for each row (`row.push_bind(..)`).
pub struct BulkWriter<T, F> {
    db: Db,
    cancel: CancellationToken,
    prefix: String,
    bind: F,
    rows_per_statement: usize,
    pending: Vec<T>,
    written: u64,
}

impl<T, F> BulkWriter<T, F>
where
    T: Send,
    F: FnMut(Separated<'_, Sqlite, &'static str>, T) + Send,
{
    /// A writer for `INSERT INTO table (columns..)`. Fails for a name that is not an identifier.
    pub fn new(
        db: &Db,
        cancel: &CancellationToken,
        table: &str,
        columns: &[&str],
        bind: F,
    ) -> Result<Self, PlanError> {
        check_identifier("table", table)?;
        if columns.is_empty() {
            return Err(PlanError::Internal(anyhow::anyhow!(
                "a bulk write needs at least one column"
            )));
        }
        for column in columns {
            check_identifier("column", column)?;
        }
        let rows_per_statement = rows_per_statement(columns.len());
        Ok(Self {
            db: db.clone(),
            cancel: cancel.clone(),
            prefix: format!("INSERT INTO {table} ({}) ", columns.join(", ")),
            bind,
            rows_per_statement,
            pending: Vec::with_capacity(rows_per_statement),
            written: 0,
        })
    }

    /// Adds a row; a full chunk is written before this returns.
    pub async fn push(&mut self, row: T) -> Result<(), PlanError> {
        self.pending.push(row);
        if self.pending.len() >= self.rows_per_statement {
            self.flush().await?;
        }
        Ok(())
    }

    /// Rows committed so far (rows still waiting for their chunk are not counted).
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Writes the last, partial chunk and returns how many rows were written in total.
    pub async fn finish(mut self) -> Result<u64, PlanError> {
        self.flush().await?;
        Ok(self.written)
    }

    async fn flush(&mut self) -> Result<(), PlanError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.cancel.is_cancelled() {
            return Err(PlanError::Cancelled);
        }
        let rows = std::mem::take(&mut self.pending);
        let count = rows.len() as u64;
        let mut query = QueryBuilder::<Sqlite>::new(self.prefix.as_str());
        query.push_values(rows, &mut self.bind);
        let mut tx = self.db.begin_write().await?;
        query.build().execute(&mut *tx).await?;
        tx.commit().await?;
        self.written += count;
        Ok(())
    }
}

/// Writes every row of `rows` through a [`BulkWriter`] and returns how many there were.
pub async fn insert_all<T, F, I>(
    db: &Db,
    cancel: &CancellationToken,
    table: &str,
    columns: &[&str],
    rows: I,
    bind: F,
) -> Result<u64, PlanError>
where
    T: Send,
    F: FnMut(Separated<'_, Sqlite, &'static str>, T) + Send,
    I: IntoIterator<Item = T> + Send,
    I::IntoIter: Send,
{
    let mut writer = BulkWriter::new(db, cancel, table, columns, bind)?;
    for row in rows {
        writer.push(row).await?;
    }
    writer.finish().await
}

/// Deletes the rows of plan `plan_id` from `table` (a rowid table with a `plan_id` column),
/// [`CHUNK_ROWS`] per statement, and returns how many there were. `cancel` is checked between
/// chunks: the rows left over belong to a plan that is no longer visible and are removed by the
/// next sweep.
pub async fn delete_plan_rows(
    db: &Db,
    cancel: &CancellationToken,
    table: &str,
    plan_id: PlanId,
) -> Result<u64, PlanError> {
    check_identifier("table", table)?;
    let sql = format!(
        "DELETE FROM {table} WHERE rowid IN \
         (SELECT rowid FROM {table} WHERE plan_id = ?1 LIMIT {CHUNK_ROWS})"
    );
    let mut total = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(PlanError::Cancelled);
        }
        let removed = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(plan_id)
            .execute(db.write())
            .await?
            .rows_affected();
        total += removed;
        if removed < CHUNK_ROWS as u64 {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TempDb;

    async fn scratch(columns: usize) -> TempDb {
        let temp = Db::open_temp().await.unwrap();
        let mut ddl = String::from("CREATE TABLE bulk_scratch (plan_id INTEGER NOT NULL");
        for index in 1..columns {
            ddl.push_str(&format!(", c{index} INTEGER NOT NULL"));
        }
        ddl.push(')');
        sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(temp.handle().write())
            .await
            .unwrap();
        temp
    }

    async fn count(db: &Db, plan_id: PlanId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM bulk_scratch WHERE plan_id = ?1")
            .bind(plan_id)
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    #[test]
    fn the_rows_per_statement_follow_the_bind_cap() {
        assert_eq!(rows_per_statement(1), CHUNK_ROWS);
        assert_eq!(rows_per_statement(2), CHUNK_ROWS);
        assert_eq!(rows_per_statement(16), CHUNK_ROWS);
        assert_eq!(rows_per_statement(17), 1_927);
        assert_eq!(rows_per_statement(40), 819);
        assert_eq!(rows_per_statement(40_000), 1);
        assert_eq!(rows_per_statement(0), CHUNK_ROWS);
        for columns in 1..200 {
            assert!(rows_per_statement(columns) * columns <= MAX_BIND_PARAMS);
        }
    }

    #[test]
    fn only_plain_identifiers_are_accepted() {
        for good in ["dup_members", "_x", "T1"] {
            assert!(is_identifier(good), "{good}");
        }
        for bad in ["", "1a", "a b", "a;b", "a\"b", "a-b", "é"] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn bad_names_are_refused_before_any_sql_runs() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let bind = |mut row: Separated<'_, Sqlite, &'static str>, value: i64| {
            row.push_bind(value);
        };
        assert!(BulkWriter::new(&db, &cancel, "t; DROP TABLE plans", &["a"], bind).is_err());
        assert!(BulkWriter::new(&db, &cancel, "bulk_scratch", &["a b"], bind).is_err());
        assert!(BulkWriter::new(&db, &cancel, "bulk_scratch", &[], bind).is_err());
        assert!(
            delete_plan_rows(&db, &cancel, "x y", 1).await.is_err(),
            "the delete helper checks the table name too"
        );
    }

    #[tokio::test]
    async fn every_row_is_written_in_chunks_and_counted() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let total = CHUNK_ROWS * 2 + 321;
        let mut writer = BulkWriter::new(
            &db,
            &cancel,
            "bulk_scratch",
            &["plan_id", "c1"],
            |mut row, value: i64| {
                row.push_bind(1_i64).push_bind(value);
            },
        )
        .unwrap();
        for value in 0..total as i64 {
            writer.push(value).await.unwrap();
        }
        assert_eq!(writer.written(), (CHUNK_ROWS * 2) as u64);
        assert_eq!(writer.finish().await.unwrap(), total as u64);
        assert_eq!(count(&db, 1).await, total as i64);
        let sum: i64 = sqlx::query_scalar("SELECT SUM(c1) FROM bulk_scratch")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(sum, (0..total as i64).sum::<i64>());
    }

    #[tokio::test]
    async fn a_wide_row_stays_under_the_bind_cap() {
        let columns = 30;
        let temp = scratch(columns).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let names: Vec<String> = std::iter::once("plan_id".to_owned())
            .chain((1..columns).map(|index| format!("c{index}")))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let written = insert_all(
            &db,
            &cancel,
            "bulk_scratch",
            &names,
            0..2_500_i64,
            |mut row, value| {
                row.push_bind(7_i64);
                for _ in 1..columns {
                    row.push_bind(value);
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(written, 2_500);
        assert_eq!(count(&db, 7).await, 2_500);
    }

    #[tokio::test]
    async fn an_empty_writer_writes_nothing() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let written = insert_all(
            &db,
            &cancel,
            "bulk_scratch",
            &["plan_id", "c1"],
            Vec::<i64>::new(),
            |mut row, value| {
                row.push_bind(1_i64).push_bind(value);
            },
        )
        .await
        .unwrap();
        assert_eq!(written, 0);
    }

    #[tokio::test]
    async fn cancellation_stops_at_the_next_chunk() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let mut writer = BulkWriter::new(
            &db,
            &cancel,
            "bulk_scratch",
            &["plan_id", "c1"],
            |mut row, value: i64| {
                row.push_bind(1_i64).push_bind(value);
            },
        )
        .unwrap();
        for value in 0..CHUNK_ROWS as i64 {
            writer.push(value).await.unwrap();
        }
        cancel.cancel();
        let mut outcome = Ok(());
        for value in 0..CHUNK_ROWS as i64 {
            outcome = writer.push(value).await;
            if outcome.is_err() {
                break;
            }
        }
        assert!(matches!(outcome, Err(PlanError::Cancelled)));
        assert_eq!(count(&db, 1).await, CHUNK_ROWS as i64);
        assert!(matches!(writer.finish().await, Err(PlanError::Cancelled)));
    }

    #[tokio::test]
    async fn deleting_a_plan_removes_only_its_rows_in_chunks() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        let rows = CHUNK_ROWS * 2 + 5;
        for (plan_id, rows) in [(1_i64, rows), (2_i64, 10)] {
            insert_all(
                &db,
                &cancel,
                "bulk_scratch",
                &["plan_id", "c1"],
                0..rows as i64,
                move |mut row, value| {
                    row.push_bind(plan_id).push_bind(value);
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            delete_plan_rows(&db, &cancel, "bulk_scratch", 1)
                .await
                .unwrap(),
            rows as u64
        );
        assert_eq!(count(&db, 1).await, 0);
        assert_eq!(count(&db, 2).await, 10);
        assert_eq!(
            delete_plan_rows(&db, &cancel, "bulk_scratch", 1)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_cancelled_delete_leaves_the_rest_for_the_sweep() {
        let temp = scratch(2).await;
        let db = temp.handle();
        let cancel = CancellationToken::new();
        insert_all(
            &db,
            &cancel,
            "bulk_scratch",
            &["plan_id", "c1"],
            0..10_i64,
            |mut row, value| {
                row.push_bind(1_i64).push_bind(value);
            },
        )
        .await
        .unwrap();
        cancel.cancel();
        let outcome = delete_plan_rows(&db, &cancel, "bulk_scratch", 1).await;
        assert!(matches!(outcome, Err(PlanError::Cancelled)));
        assert_eq!(count(&db, 1).await, 10);
    }
}
