//! The file query engine: ONE implementation that turns a scope, a filter and a sort order into
//! SQL (via `sqlx::QueryBuilder`) for the folder list, item pages, id lists, counts, facets and the
//! file detail.
//!
//! Owned by todo `building-filter-query-engine`. Every other module that needs to select files
//! (plans, selections, Immich sync, re-index scopes, ...) reuses it instead of writing its own SQL
//! for scopes and filters, so the semantics are identical everywhere.
//!
//! ```ignore
//! let query = FileQuery::new(QueryScope::try_from(&request.scope)?, &request.filter)?
//!     .sorted(SortKey::Date, SortOrder::Desc);
//! let page = query.items(&db, offset, limit).await?;
//! let all = query.ids(&db).await?;
//! ```
//!
//! # Semantics
//!
//! * **Scope** ([`QueryScope`]): `dir` on `volume_id` (`None` = every volume, with the same
//!   directory in each). `recursive` includes the subdirectories through
//!   [`crate::db::subtree::Subtree`] (an index range, correct for siblings that share a prefix
//!   such as `2024` and `2024-05`); otherwise the directory must be equal.
//! * **Kinds** ([`KindFilter`]): `Assets` (the default) hides sidecar rows, because the UI shows
//!   them attached to their media; `All` counts them. Cleanup, file operations and Immich use `All`.
//! * **Filter** (`opm_core::dto::Filter`, validated and put in canonical form by
//!   `Filter::normalise` when the query is built): AND across dimensions, OR within one. Cameras
//!   and tags match by name, ignoring ASCII case (the `COLLATE NOCASE` columns of the schema; a
//!   difference of Unicode case only, `É` against `é`, is not folded, unlike the reference
//!   predicate `Filter::matches`). Ratings are `IN (...)` with 0 meaning unrated. Flags `selected`
//!   and `rejected` are `1` and `-1` (an unflagged file matches no non-empty flag list). Tags match
//!   when the file carries ANY of them. `selection_id` restricts to the members of that selection.
//! * **Order**: `name` sorts by the normalised name, `date` by the capture time (files without one
//!   come LAST in both directions), `size` by size; ties fall back to the normalised name and then
//!   the id, and `desc` reverses only the primary key, so the order is total and offset paging is
//!   stable.
//! * **Folders** ([`FileQuery::folders`], needs a volume): the child directories of `dir`, each with
//!   `count` (the files in it and below it that match the filter and the kinds) and `total` (the
//!   same ignoring the filter). [`FileQuery::footer_counts`] counts the matching files directly in
//!   `dir` and in the folders below it, whatever `recursive` says.
//!
//! # Embedding the filter in other statements
//!
//! [`FileQuery::push_where`] appends a parenthesised boolean expression over the `files` table under
//! the alias you give it. Other modules use the alias `f`:
//!
//! ```sql
//! INSERT INTO selection_items (selection_id, file_id) SELECT ?, f.id FROM files f WHERE <push_where>
//! UPDATE files AS f SET rating = 3 WHERE <push_where>
//! SELECT SUM(CASE WHEN <push_where> THEN 1 ELSE 0 END) FROM files f GROUP BY f.volume_id
//! ```
//!
//! The expression never references another table without a subquery of its own, binds every value,
//! and is `(1 = 1)` when nothing constrains. [`item_columns`], [`ITEM_JOINS`] and
//! [`item_from_row`] are the one definition of "a listed item": select the columns, add the join and
//! decode with the function.
//!
//! # Limits
//!
//! * [`FileQuery::items`] clamps `limit` to `1..=`[`MAX_PAGE_SIZE`] and treats a negative `offset`
//!   as 0.
//! * [`FileQuery::ids`] returns at most [`MAX_IDS`] ids; a bigger match is silently truncated.
//! * Each facet list holds at most [`MAX_FACET_VALUES`] values (the most frequent first).
//!
//! # Performance
//!
//! `tests/query_perf.rs` (`#[ignore]`) seeds a library of 199,400 files in 20,143 directories and
//! times the query shapes of the UI, 7 runs each. The `photos` volume has 20 years of 12 months of
//! 62 events of 10 to 12 files (a few files per month directory too) and a flat `inbox` of 5,000
//! files; `cards` has 5,000 folders of 6 files at its root. Files carry one of 7 cameras (or none),
//! 30 tags, ratings and flags, and 8% of them are sidecars. Statistics are sampled the way
//! production samples them (`Db::optimize`).
//!
//! Run it in release mode. The workspace release profile strips the proc-macro dylib sqlx needs, so
//! override it:
//!
//! ```text
//! CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_LTO=off \
//!   cargo test --release -p opm-server --test query_perf -- --ignored --nocapture
//! ```
//!
//! Medians in milliseconds on an Apple M1 Max (32 GB, SSD). "scope" is the 156,000-file `photos`
//! volume unless noted; a page is 200 items. "Before" is the first implementation, before the folder
//! statement was rewritten and the `files_volume_capture` index was added.
//!
//! ```text
//! folders + footer   scope root (21 folders)           50     one camera 58     selective 80
//!                    cards root (5,000 folders)        17     (before: 841)
//!                    photos/2015 (12 folders)           5     one event           0
//! page, one folder   one event 0.1;   inbox (5,000 files) name 2.4, date 2.9, size 2.9, offset 4,800 3.7
//! page, a subtree    a year (photos/2015) date 7.0, name 3.1;   a month (photos/2015/06) date 1.2
//! page, whole scope  date desc 0.9 (before: 114), at offset 20,000 7.1 (before: 170)
//!                    name asc 29, name desc 25, date asc 25, size desc 27 (no index serves them)
//!                    one camera: date 57, name 21;   selective (3 dimensions, 193 matches): 28
//!                    12-file selection 0.1;   cards root date desc 1.0 (before: 6.1)
//! page, every volume name asc 1.0;   date desc 120, size asc 29;   one camera 20 (name), 55 (date)
//!                    selective 14;   12-file selection 0.1
//! count              scope 13, one camera 17, selective 26;   every volume 13;   inbox 1.1
//! ids                inbox 3.7;   scope with one camera (47,001 ids) 62;   whole scope (155,970) 160
//! facets             one event 0.2;   photos/2015 5.7;   scope 176 (best 83), one camera 95;
//!                    every volume 298 (best 147)
//! ```
//!
//! Against the targets (folders and footer at a volume root under 150 ms, an item page under 20 ms,
//! 50,000 ids under 100 ms) the engine meets all of them for folders, subtrees, the inbox and the
//! newest-first page of a whole volume, and for ids. It misses them for a page of a whole large
//! volume in another order (about 25 ms: SQLite sorts the 156,000 rows of the scope, 160 ns each),
//! for filtered pages of a whole volume (20 to 57 ms, in proportion to the rows it has to read),
//! and for the newest-first page across every volume (120 ms, as no index spans volumes). Facets
//! have no target; they run three queries over the whole scope, concurrently.
//!
//! What the numbers rest on:
//!
//! * **Folders.** One grouped pass over the `(volume_id, dir_path)` index range counts every
//!   directory below the listed one, and a `UNION ALL` with the child rows of `dirs` adds them up
//!   per child (see `folders.rs`). A join of the two was quadratic in the number of children.
//! * **One index**, `files_volume_capture (volume_id, capture_ts DESC, name_norm)` (migration
//!   `0030`): the newest-first page of a whole volume stops after `offset + limit` rows instead of
//!   sorting the volume. [`FileQuery::items`] only lets the planner use it for that listing
//!   (`reads_date_index` in `items.rs`); any other date sort orders by `+capture_ts`, an expression
//!   no index delivers. The planner reads sampled statistics that give every volume, directory and
//!   filter value the same estimate, so left alone it would walk the index through the whole volume
//!   looking for the files of one folder or the few that match a filter.
//! * **Plans are asserted**, not just timed: `plan_tests.rs` checks with `EXPLAIN QUERY PLAN` that a
//!   directory or a subtree is a search of `files_volume_dir_name`, that a whole volume is never
//!   scanned, and that only the unfiltered whole-volume timeline reads the date index.
//!
//! Tried and not taken: a covering index `(volume_id, dir_path, kind, camera_id, rating, flag)`
//! (facets and footers about twice as fast, but 7 MB larger and rewritten by every rating or flag
//! change); a global `(capture_ts DESC, name_norm)` index for the every-volume timeline (one more
//! index to maintain on every insert, for a view nothing needs yet); a late-materialised page query
//! (1.4 to 2.7 times faster, still proportional to the scope); keyset paging instead of offsets
//! (the contract of [`FileQuery::items`] is an offset and a limit).

use opm_core::dto::{Filter, FolderEntry, Overlay, Scope, SortKey, SortOrder};
use opm_core::filter::FilterError;
use opm_core::paths::{FileId, PathError, RelPath, VolumeId};

use crate::db::DbError;
use crate::error::AppError;

mod detail;
mod facets;
mod filter;
mod folders;
mod items;
#[cfg(test)]
mod plan_tests;
pub mod volumes;

pub use detail::file_detail;
pub use folders::FolderRow;
pub(crate) use folders::push_child_segment;
pub use items::{ITEM_JOINS, file_item, item_columns, item_from_row};

/// The largest page [`FileQuery::items`] returns.
pub const MAX_PAGE_SIZE: i64 = 500;

/// The most ids [`FileQuery::ids`] returns.
pub const MAX_IDS: i64 = 1_000_000;

/// The most values in one facet list.
pub const MAX_FACET_VALUES: i64 = 200;

/// Why a query could not be built or run. Invalid input is the caller's fault ([`AppError`] 400),
/// an unknown file is a 404, database failures follow [`DbError`].
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("invalid scope: {0}")]
    Scope(#[from] PathError),
    #[error("invalid filter: {0}")]
    Filter(#[from] FilterError),
    /// The folder list needs to know which volume to list.
    #[error("the folder list needs a volume")]
    VolumeRequired,
    #[error("file {0} does not exist")]
    FileNotFound(FileId),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for QueryError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(err))
    }
}

impl From<QueryError> for AppError {
    fn from(err: QueryError) -> Self {
        match err {
            QueryError::Scope(_) | QueryError::Filter(_) | QueryError::VolumeRequired => {
                Self::bad_request("bad_request", err.to_string())
            }
            QueryError::FileNotFound(_) => Self::not_found("not_found", err.to_string()),
            QueryError::Db(db) => db.into(),
        }
    }
}

/// Where to look: a directory of one volume (or of every volume), with or without its
/// subdirectories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryScope {
    /// `None` means every volume.
    pub volume_id: Option<VolumeId>,
    /// Volume-relative directory, `""` is the volume root.
    pub dir: RelPath,
    pub recursive: bool,
}

impl TryFrom<&Scope> for QueryScope {
    type Error = QueryError;

    /// Fails when `dir` is not a valid volume-relative path (absolute, `..`, NUL).
    fn try_from(scope: &Scope) -> Result<Self, QueryError> {
        Ok(Self {
            volume_id: scope.volume_id,
            dir: scope.dir_rel()?,
            recursive: scope.recursive,
        })
    }
}

/// Which kinds of files a query covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindFilter {
    /// Every kind except sidecars: what the UI lists.
    #[default]
    Assets,
    /// Every row of `files`, sidecars included.
    All,
}

/// A scope, a filter, a kind selection and a sort order, ready to run. See the module docs.
#[derive(Clone, Debug)]
pub struct FileQuery {
    scope: QueryScope,
    filter: Filter,
    kinds: KindFilter,
    sort: SortKey,
    order: SortOrder,
}

impl FileQuery {
    /// A query over `scope` for the files that pass `filter`, listing assets sorted by name.
    /// Rejects a filter that does not validate (ratings above 5, too many or too long entries);
    /// the filter is kept in its canonical form.
    pub fn new(scope: QueryScope, filter: &Filter) -> Result<Self, QueryError> {
        Ok(Self {
            scope,
            filter: filter.normalise()?,
            kinds: KindFilter::default(),
            sort: SortKey::default(),
            order: SortOrder::default(),
        })
    }

    /// Which kinds of files the query covers (default [`KindFilter::Assets`]).
    #[must_use]
    pub fn kinds(mut self, kinds: KindFilter) -> Self {
        self.kinds = kinds;
        self
    }

    /// The order of [`Self::items`] and [`Self::ids`] (default: by name, ascending).
    #[must_use]
    pub fn sorted(mut self, sort: SortKey, order: SortOrder) -> Self {
        self.sort = sort;
        self.order = order;
        self
    }

    pub fn scope(&self) -> &QueryScope {
        &self.scope
    }

    /// The filter in canonical form.
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub fn kind_filter(&self) -> KindFilter {
        self.kinds
    }

    pub fn sort(&self) -> (SortKey, SortOrder) {
        (self.sort, self.order)
    }
}

impl From<FolderRow> for FolderEntry {
    /// The default overlay and no plan matches: the browse API fills those in.
    fn from(row: FolderRow) -> Self {
        Self {
            name: row.name,
            path: row.path,
            volume_id: row.volume_id,
            count: row.count,
            total: row.total,
            matches: None,
            preview: row.preview,
            overlay: Overlay::default(),
        }
    }
}
