//! The stage queue: the files of a run's scope whose pipeline state is still `pending`, in keyset pages.
//!
//! Owned by todo `building-stage-queue`. The metadata, hash and thumbnail stages (an `impl Indexer`
//! block each, in their own files) read their work through [`PendingQueue`], so none of them writes
//! scope SQL, paging SQL or a progress total of its own. The scope planning the scanner uses
//! (`plan_targets`) lives here too, so a stage sees exactly the directories the scan walked.
//!
//! ```text
//! let queue = PendingQueue::new(PipelineColumn::Meta, indexer.stage_scope(req));
//! let totals = queue.totals(db).await?;               // files and bytes still to do (progress)
//! let mut after = None;
//! loop {
//!     let page = queue.page(db, after.as_ref(), 500).await?;
//!     let Some(last) = page.last() else { break };
//!     after = Some(PendingKey::from(last));
//!     // handle the page, then set the state of every row, guarded by `size = ? AND mtime = ?`
//! }
//! ```
//!
//! # Contract
//! * **Scope.** [`StageScope::new`] plans the [`ScanRequest`] with the scanner's own
//!   `plan_targets`: no targets is every online volume completely, a recursive target covers its
//!   sub-directories, a non-recursive one only its own files. A volume that is not online is
//!   dropped, because a stage cannot read its files. [`Indexer::stage_scope`] passes the registry's
//!   online volumes.
//! * **Order.** Rows come ordered by `(volume_id, dir_path, name)`, compared bytewise (SQLite's
//!   `BINARY` collation, which is also `str` order in Rust). A page may span volumes.
//! * **Cursor.** `after` is a key, not an offset, and it is exclusive. A row the stage finished
//!   (its state is no longer `pending`) leaves the queue without shifting the later pages, a row
//!   that arrives before the cursor is not seen by this pass, and one after it is. Nothing is
//!   skipped or repeated, however the table changes between two calls.
//! * **Page length.** `limit` is clamped to `1..=2000`. A page is shorter than `limit` only when
//!   the queue is exhausted, so an empty page (or a short one) ends the pass.
//! * **Conditions.** [`PendingQueue::with_condition`] narrows both [`PendingQueue::page`] and
//!   [`PendingQueue::totals`] with a SQL fragment on the alias `f` (the hash stage looks for files
//!   that share their size with another one). Conditions are wrapped in parentheses and ANDed.
//! * **Kinds.** Every kind is returned, sidecars included when they are pending. The scanner
//!   writes `not applicable` for sidecars; a stage that does not handle a kind marks it so itself.
//!
//! # How a page is read
//! Migration `0011_pending_partial_indexes.sql` gives each state column a partial index
//! `(volume_id, dir_path, name) WHERE <state> = 0`. It holds only the rows of the queue, in the
//! order of the page, so a page is one index range of at most `limit` entries, however many rows
//! the volume has and however few of them are pending. A statement can use it when its own
//! condition is `= 0`. The code is spliced as a literal (`PENDING`) so the plan never depends on a
//! bound value: SQLite uses the index for a bound `0` only by preparing the statement again once
//! the value is known.
//!
//! Three details keep the plan from degrading, and the plan tests pin them:
//! * The exact scope rule is [`Subtree::push_predicate`] (so `2019.old` is not inside `2019`), but
//!   written on `+f.dir_path`: SQLite never uses an index for a term whose column has a unary `+`.
//!   Without it the planner chooses the subtree's range over the cursor and re-reads the subtree
//!   from its start on every page, or sorts the union of several scopes.
//! * The range hint instead is explicit: the smallest and largest directory of the scopes bound
//!   `f.dir_path`, and the cursor replaces the lower bound as a row value
//!   `(f.dir_path, f.name) > (?, ?)`, which the planner turns into the index range.
//! * A plan with more than 64 scopes in a volume (the watcher can submit thousands of
//!   directories) would hit SQLite's expression depth limit as one `OR` chain. It reads the hull in
//!   chunks of the same ordered statement and keeps the rows of its scopes in Rust, so the order
//!   and the cursor are the same.
//!
//! # Measured
//! `cargo test --release -p apm-server --test pending_perf -- --ignored --nocapture` on 200,000
//! files in two volumes, pages of 500, median of 7 runs on the development machine:
//!
//! | pending rows  | first page | whole pass | `totals` |
//! |---------------|-----------:|-----------:|---------:|
//! | 200,000 (all) |     0.7 ms |     258 ms |    19 ms |
//! | 2,000 (1 %)   |     0.9 ms |     3.5 ms |   0.9 ms |
//! | 20            |    0.08 ms |    0.08 ms |  0.02 ms |
//!
//! A page after a key in the middle of the queue costs the same as the first one. Without the
//! partial indexes a page walks `files_volume_dir_name` and filters on the state: 4 ms with 1 %
//! pending and 20 ms with 20 pending (the whole table, which grows with the library). The indexes
//! cost 0.26 s when 200,000 files are inserted and 0.25 s when the three stages finish them, and
//! each one takes 7.7 MB while every row is pending and 0.1 MB once 99 % are done. With the size
//! twin condition of the hash stage (an `EXISTS` on `files`) a first page takes 1 to 2 ms, and
//! `totals` 82 ms when 80,000 rows qualify (1.5 ms when 800 do).

use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;

use apm_core::dto::FileKind;
use apm_core::paths::{FileId, RelPath, VolumeId};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row as _, Sqlite};

use super::{Indexer, ScanRequest, ScanTarget};
use crate::db::models::{PipelineState, file_kind_from_db};
use crate::db::subtree::Subtree;
use crate::db::{Db, DbError};

#[cfg(test)]
mod plans;
#[cfg(test)]
mod tests;

/// The code `files.*_state` holds for a pending row, spliced as a literal: the partial indexes of
/// migration `0011` have `= 0` in their `WHERE`, and a statement that states the same literal can
/// use one whatever it binds.
const PENDING: i64 = PipelineState::Pending.code();
const _: () = assert!(PENDING == 0, "migration 0011 spells the pending code as 0");

/// Most rows one [`PendingQueue::page`] returns, whatever the caller asks for.
const MAX_PAGE: usize = 2000;
/// Most scopes of one volume that go into the SQL of a statement. Every scope is one more term of an
/// `OR` chain, and SQLite refuses an expression deeper than 1000.
const MAX_SQL_SCOPES: usize = 64;
/// Rows read per statement when a plan has more scopes than [`MAX_SQL_SCOPES`].
const FILTER_CHUNK: usize = 2000;

/// One directory of a volume to walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) dir: RelPath,
    pub(crate) recursive: bool,
}

/// What to do on one volume: its scopes, parents before children, none inside a recursive one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumePlan {
    pub(crate) volume_id: VolumeId,
    pub(crate) scopes: Vec<Scope>,
}

impl VolumePlan {
    /// Does a scope cover the whole volume?
    pub(crate) fn whole_volume(&self) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.dir.is_root() && scope.recursive)
    }
}

/// Groups the targets by volume and drops the ones that are already covered. No targets means
/// every volume in `online`, completely.
pub(crate) fn plan_targets(targets: &[ScanTarget], online: &[VolumeId]) -> Vec<VolumePlan> {
    if targets.is_empty() {
        return online
            .iter()
            .map(|&volume_id| VolumePlan {
                volume_id,
                scopes: vec![Scope {
                    dir: RelPath::root(),
                    recursive: true,
                }],
            })
            .collect();
    }
    let mut by_volume: BTreeMap<VolumeId, Vec<Scope>> = BTreeMap::new();
    for target in targets {
        by_volume.entry(target.volume_id).or_default().push(Scope {
            dir: target.dir.clone(),
            recursive: target.recursive,
        });
    }
    by_volume
        .into_iter()
        .map(|(volume_id, scopes)| VolumePlan {
            volume_id,
            scopes: merge_scopes(scopes),
        })
        .collect()
}

/// Sorts scopes (a directory before what lies below it, a recursive scope before a non-recursive
/// one of the same directory) and drops every scope that a kept one already covers.
fn merge_scopes(mut scopes: Vec<Scope>) -> Vec<Scope> {
    scopes.sort_by(|a, b| {
        a.dir
            .cmp(&b.dir)
            .then_with(|| b.recursive.cmp(&a.recursive))
    });
    let mut kept: Vec<Scope> = Vec::new();
    for scope in scopes {
        let covered = kept.iter().any(|other| {
            other.dir == scope.dir || (other.recursive && scope.dir.starts_with(&other.dir))
        });
        if !covered {
            kept.push(scope);
        }
    }
    kept
}

/// The pipeline state column a queue reads. The column name comes from here, never from a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineColumn {
    /// `meta_state`, the metadata stage.
    Meta,
    /// `hash_state`, the hash stage.
    Hash,
    /// `thumb_state`, the thumbnail stage.
    Thumb,
}

impl PipelineColumn {
    const fn column(self) -> &'static str {
        match self {
            Self::Meta => "meta_state",
            Self::Hash => "hash_state",
            Self::Thumb => "thumb_state",
        }
    }
}

/// The scope of a run as the stages see it: per online volume, the directories to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageScope {
    plans: Vec<VolumePlan>,
}

impl StageScope {
    /// Plans `req` like the scan did. No targets means every volume in `online`, completely.
    /// Volumes that are not in `online` are dropped; the plans are ordered by volume id.
    pub fn new(req: &ScanRequest, online: &[VolumeId]) -> Self {
        let mut plans: Vec<VolumePlan> = plan_targets(&req.targets, online)
            .into_iter()
            .filter(|plan| online.contains(&plan.volume_id) && !plan.scopes.is_empty())
            .collect();
        plans.sort_by_key(|plan| plan.volume_id);
        plans.dedup_by_key(|plan| plan.volume_id);
        Self { plans }
    }

    /// Is there nothing to read: no volume of the request is online?
    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }

    /// The volumes the scope reaches, in id order.
    pub fn volume_ids(&self) -> Vec<VolumeId> {
        self.plans.iter().map(|plan| plan.volume_id).collect()
    }
}

/// A pending file with what a stage needs to open it and to guard its update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    pub id: FileId,
    pub volume_id: VolumeId,
    /// The directory, as stored (`""` for the volume root).
    pub dir_path: String,
    pub name: String,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: i64,
    pub btime: Option<i64>,
}

/// The position of a row in a queue: the cursor [`PendingQueue::page`] continues after. It orders
/// like the queue does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingKey {
    pub volume_id: VolumeId,
    pub dir_path: String,
    pub name: String,
}

impl From<&PendingFile> for PendingKey {
    fn from(file: &PendingFile) -> Self {
        Self {
            volume_id: file.volume_id,
            dir_path: file.dir_path.clone(),
            name: file.name.clone(),
        }
    }
}

/// What is left in a queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueTotals {
    pub files: u64,
    /// The sum of the files' sizes.
    pub bytes: u64,
}

/// The pending rows of one pipeline state in a [`StageScope`], read in keyset pages. Cheap to clone;
/// it holds no connection and no cursor.
#[derive(Debug, Clone)]
pub struct PendingQueue {
    column: PipelineColumn,
    scope: StageScope,
    conditions: Vec<&'static str>,
}

impl PendingQueue {
    pub fn new(column: PipelineColumn, scope: StageScope) -> Self {
        Self {
            column,
            scope,
            conditions: Vec::new(),
        }
    }

    /// Narrows the queue to the rows for which `sql` holds. It is a condition on the row of `files`
    /// aliased `f` (`f.size`, `EXISTS (SELECT 1 FROM files o WHERE o.size = f.size ...)`), without
    /// placeholders, applied by [`Self::page`] and [`Self::totals`] alike. It is wrapped in
    /// parentheses and ANDed with the rest, so it may contain `OR`; every call adds one more
    /// condition. The text is spliced into the statement: pass a literal, never user input.
    #[must_use]
    pub fn with_condition(mut self, sql: &'static str) -> Self {
        self.conditions.push(sql);
        self
    }

    /// How many files, and how many bytes, are in the queue now. It counts exactly the rows
    /// [`Self::page`] returns.
    pub async fn totals(&self, db: &Db) -> Result<QueueTotals, DbError> {
        let mut totals = QueueTotals::default();
        for plan in &self.scope.plans {
            if plan.scopes.len() <= MAX_SQL_SCOPES {
                let (files, bytes): (i64, i64) = self
                    .totals_statement(plan)
                    .build_query_as()
                    .fetch_one(db.read())
                    .await?;
                totals.files += u64::try_from(files).unwrap_or_default();
                totals.bytes += u64::try_from(bytes).unwrap_or_default();
                continue;
            }
            let matcher = ScopeMatcher::new(&plan.scopes);
            self.walk_hull(db, plan, None, |row| {
                if matcher.contains(row.try_get::<&str, _>("dir_path")?) {
                    totals.files += 1;
                    totals.bytes +=
                        u64::try_from(row.try_get::<i64, _>("size")?).unwrap_or_default();
                }
                Ok(ControlFlow::Continue(()))
            })
            .await?;
        }
        Ok(totals)
    }

    /// Up to `limit` pending rows after `after` (the start of the queue for `None`), in queue
    /// order; see the module docs for the cursor. `limit` is clamped to `1..=2000`.
    pub async fn page(
        &self,
        db: &Db,
        after: Option<&PendingKey>,
        limit: usize,
    ) -> Result<Vec<PendingFile>, DbError> {
        let limit = limit.clamp(1, MAX_PAGE);
        let mut files = Vec::new();
        for plan in &self.scope.plans {
            if files.len() >= limit {
                break;
            }
            let cursor = match after {
                Some(key) if key.volume_id > plan.volume_id => continue,
                Some(key) if key.volume_id == plan.volume_id => Some(key),
                _ => None,
            };
            self.read_volume(db, plan, cursor, limit, &mut files)
                .await?;
        }
        Ok(files)
    }

    /// Appends rows of `plan` after `cursor` to `files` until it holds `limit` or the volume has no
    /// more.
    async fn read_volume(
        &self,
        db: &Db,
        plan: &VolumePlan,
        cursor: Option<&PendingKey>,
        limit: usize,
        files: &mut Vec<PendingFile>,
    ) -> Result<(), DbError> {
        if plan.scopes.len() <= MAX_SQL_SCOPES {
            let rows = self
                .page_statement(plan, cursor, limit - files.len(), ScopeFilter::Sql)
                .build()
                .fetch_all(db.read())
                .await?;
            for row in &rows {
                files.push(pending_from_row(row)?);
            }
            return Ok(());
        }
        let matcher = ScopeMatcher::new(&plan.scopes);
        self.walk_hull(db, plan, cursor, |row| {
            if matcher.contains(row.try_get::<&str, _>("dir_path")?) {
                files.push(pending_from_row(row)?);
                if files.len() == limit {
                    return Ok(ControlFlow::Break(()));
                }
            }
            Ok(ControlFlow::Continue(()))
        })
        .await
    }

    /// Shows `visit` the pending rows of the hull of `plan`'s scopes after `cursor`, in queue order,
    /// until it breaks or the rows run out. The hull holds the rows of the scopes and more: `visit`
    /// decides which are in scope. Reads in chunks, so no connection is held between two of them.
    async fn walk_hull(
        &self,
        db: &Db,
        plan: &VolumePlan,
        cursor: Option<&PendingKey>,
        mut visit: impl FnMut(&SqliteRow) -> Result<ControlFlow<()>, sqlx::Error>,
    ) -> Result<(), DbError> {
        let mut after = cursor.cloned();
        loop {
            let rows = self
                .page_statement(plan, after.as_ref(), FILTER_CHUNK, ScopeFilter::HullOnly)
                .build()
                .fetch_all(db.read())
                .await?;
            for row in &rows {
                if visit(row)?.is_break() {
                    return Ok(());
                }
            }
            if rows.len() < FILTER_CHUNK {
                return Ok(());
            }
            after = rows
                .last()
                .map(|row| {
                    Ok::<_, sqlx::Error>(PendingKey {
                        volume_id: plan.volume_id,
                        dir_path: row.try_get("dir_path")?,
                        name: row.try_get("name")?,
                    })
                })
                .transpose()?;
        }
    }

    /// `FROM files f WHERE` the volume, the pending state and the extra conditions.
    fn push_queue_rows(&self, qb: &mut QueryBuilder<Sqlite>, volume_id: VolumeId) {
        qb.push(" FROM files f WHERE f.volume_id = ")
            .push_bind(volume_id)
            .push(" AND f.")
            .push(self.column.column())
            .push(" = ")
            .push(PENDING);
        for condition in &self.conditions {
            qb.push(" AND (").push(*condition).push(")");
        }
    }

    /// One page of one volume, ordered like the queue. See the module docs for why the scope is
    /// written the way it is.
    fn page_statement(
        &self,
        plan: &VolumePlan,
        cursor: Option<&PendingKey>,
        limit: usize,
        filter: ScopeFilter,
    ) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(
            "SELECT f.id, f.volume_id, f.dir_path, f.name, f.kind, f.size, f.mtime, f.btime",
        );
        self.push_queue_rows(&mut qb, plan.volume_id);
        if filter == ScopeFilter::Sql && !plan.whole_volume() {
            push_scope_terms(&mut qb, &plan.scopes, "+f.dir_path");
        }
        Hull::of(plan).push_bounds(&mut qb, cursor);
        qb.push(" ORDER BY f.dir_path, f.name LIMIT ")
            .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
        qb
    }

    /// The counts of one volume. Without an `ORDER BY` the planner is free to use the scope as an
    /// index search, so the terms are written on the plain column.
    fn totals_statement(&self, plan: &VolumePlan) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new("SELECT COUNT(*), COALESCE(SUM(f.size), 0)");
        self.push_queue_rows(&mut qb, plan.volume_id);
        if !plan.whole_volume() {
            push_scope_terms(&mut qb, &plan.scopes, "f.dir_path");
        }
        qb
    }
}

impl Indexer {
    /// The scope of `req` for the stage queues: the plans of the scan, for the volumes that are
    /// online now.
    pub fn stage_scope(&self, req: &ScanRequest) -> StageScope {
        let online: Vec<VolumeId> = self
            .volumes()
            .online()
            .iter()
            .map(|volume| volume.id)
            .collect();
        StageScope::new(req, &online)
    }
}

/// How much of the scope a page statement checks itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeFilter {
    /// Every scope, exactly.
    Sql,
    /// Only the range the scopes lie in; the caller keeps the rows that are in scope.
    HullOnly,
}

/// Appends `AND (<scope> OR <scope> ...)`, the exact rule of every scope of a volume. `column` is
/// `f.dir_path`, or `+f.dir_path` for a term the planner must not turn into an index search.
fn push_scope_terms(qb: &mut QueryBuilder<Sqlite>, scopes: &[Scope], column: &str) {
    qb.push(" AND (");
    for (n, scope) in scopes.iter().enumerate() {
        if n > 0 {
            qb.push(" OR ");
        }
        if scope.recursive {
            Subtree::new(&scope.dir).push_predicate(qb, column);
        } else {
            qb.push(column)
                .push(" = ")
                .push_bind(scope.dir.as_str().to_owned());
        }
    }
    qb.push(")");
}

/// The range of `dir_path` that holds every scope of a volume: a hint the planner can seek with.
/// It may hold more rows than the scopes do (the directories between two scopes), never fewer.
#[derive(Debug, Default, PartialEq, Eq)]
struct Hull {
    /// Every scope is at or after this directory; `None` when that constrains nothing.
    lower: Option<String>,
    /// Every scope is before this bound, or at it when the flag is set.
    upper: Option<(String, bool)>,
}

impl Hull {
    fn of(plan: &VolumePlan) -> Self {
        if plan.whole_volume() {
            return Self::default();
        }
        let lower = plan
            .scopes
            .iter()
            .map(|scope| scope.dir.as_str())
            .min()
            .filter(|dir| !dir.is_empty())
            .map(str::to_owned);
        // `D0` is the first path after everything below `D`; a directory alone ends at `D`. A larger
        // bound reaches further, and an inclusive one reaches further than the exclusive one.
        let upper = plan
            .scopes
            .iter()
            .map(|scope| {
                if scope.recursive {
                    (format!("{}0", scope.dir.as_str()), false)
                } else {
                    (scope.dir.as_str().to_owned(), true)
                }
            })
            .max();
        Self { lower, upper }
    }

    /// Appends the bounds, with the cursor as the lower bound when it is inside the hull. A second
    /// lower bound on the same column would let the planner pick either, so the cursor, which is
    /// tighter, is the only one then.
    fn push_bounds(&self, qb: &mut QueryBuilder<Sqlite>, cursor: Option<&PendingKey>) {
        let cursor = cursor.filter(|key| {
            self.lower
                .as_deref()
                .is_none_or(|lower| key.dir_path.as_str() >= lower)
        });
        if let Some(key) = cursor {
            qb.push(" AND (f.dir_path, f.name) > (")
                .push_bind(key.dir_path.clone())
                .push(", ")
                .push_bind(key.name.clone())
                .push(")");
        } else if let Some(lower) = &self.lower {
            qb.push(" AND f.dir_path >= ").push_bind(lower.clone());
        }
        if let Some((upper, inclusive)) = &self.upper {
            qb.push(if *inclusive {
                " AND f.dir_path <= "
            } else {
                " AND f.dir_path < "
            })
            .push_bind(upper.clone());
        }
    }
}

/// [`Subtree::contains`] and the exact-directory rule for many scopes at once, in memory.
struct ScopeMatcher {
    everything: bool,
    directories: HashSet<String>,
    subtrees: HashSet<String>,
}

impl ScopeMatcher {
    fn new(scopes: &[Scope]) -> Self {
        let mut matcher = Self {
            everything: false,
            directories: HashSet::new(),
            subtrees: HashSet::new(),
        };
        for scope in scopes {
            let dir = scope.dir.as_str().to_owned();
            if !scope.recursive {
                matcher.directories.insert(dir);
            } else if scope.dir.is_root() {
                matcher.everything = true;
            } else {
                matcher.subtrees.insert(dir);
            }
        }
        matcher
    }

    /// Is a file of directory `dir` in one of the scopes? A subtree holds its directory and every
    /// directory whose path continues it after a `/`.
    fn contains(&self, dir: &str) -> bool {
        self.everything
            || self.directories.contains(dir)
            || self.subtrees.contains(dir)
            || dir
                .match_indices('/')
                .any(|(at, _)| self.subtrees.contains(&dir[..at]))
    }
}

fn pending_from_row(row: &SqliteRow) -> Result<PendingFile, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    Ok(PendingFile {
        id: row.try_get("id")?,
        volume_id: row.try_get("volume_id")?,
        dir_path: row.try_get("dir_path")?,
        name: row.try_get("name")?,
        kind: file_kind_from_db(&kind).map_err(|err| sqlx::Error::ColumnDecode {
            index: "kind".to_owned(),
            source: Box::new(err),
        })?,
        size: row.try_get("size")?,
        mtime: row.try_get("mtime")?,
        btime: row.try_get("btime")?,
    })
}
