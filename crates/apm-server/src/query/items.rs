//! Item pages, id lists and counts, plus the row to [`FileItem`] conversion other modules reuse.

use apm_core::dto::{FileItem, FlagFilter, Overlay, SortKey, SortOrder};
use apm_core::paths::FileId;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, QueryBuilder, Row, Sqlite};

use super::{FileQuery, MAX_IDS, MAX_PAGE_SIZE, QueryError};
use crate::db::Db;
use crate::db::models::{FileRow, file_columns};

/// The camera column [`item_from_row`] reads next to the `files` columns.
const CAMERA_COLUMN: &str = "camera_name";

/// The Immich column [`item_from_row`] reads next to the `files` columns.
const IN_IMMICH_COLUMN: &str = "in_immich";

/// The select list of every item query, the ONE place that decides what a listed item is made of:
/// the `files` columns of the table aliased `alias`, the camera name (`camera_name`, needs
/// [`ITEM_JOINS`]) and the Immich flag (`in_immich`). Decode the rows with [`item_from_row`].
/// `alias` must be a literal, never user input.
///
/// The Immich flag is the constant `0` until the Immich sync tables are populated; the Immich todo
/// replaces it here by an `EXISTS` over its own table, and every listing follows.
pub fn item_columns(alias: &str) -> String {
    format!(
        "{}, cam.name AS {CAMERA_COLUMN}, 0 AS {IN_IMMICH_COLUMN}",
        file_columns(alias)
    )
}

/// The join [`item_columns`] needs, for the `files` table aliased `f`.
pub const ITEM_JOINS: &str = "LEFT JOIN cameras cam ON cam.id = f.camera_id";

/// Converts a `files` row, the name of its camera and the Immich flag into the listing item. The
/// overlay is the default (all false): the plan engine fills it afterwards.
pub fn file_item(row: FileRow, camera: Option<String>, in_immich: bool) -> FileItem {
    FileItem {
        id: row.id,
        volume_id: row.volume_id,
        dir: row.dir_path,
        name: row.name,
        kind: row.kind,
        size: row.size,
        capture_ts: row.capture_ts,
        capture_src: row.capture_src,
        camera,
        rating: row.rating,
        flag: flag_value(row.flag),
        width: row.width,
        height: row.height,
        duration_ms: row.duration_ms,
        mtime: row.mtime,
        in_immich,
        overlay: Overlay::default(),
    }
}

/// Decodes a row selected with [`item_columns`] into an item.
pub fn item_from_row(row: &SqliteRow) -> Result<FileItem, sqlx::Error> {
    let file = FileRow::from_row(row)?;
    let camera: Option<String> = row.try_get(CAMERA_COLUMN)?;
    let in_immich: bool = row.try_get(IN_IMMICH_COLUMN)?;
    Ok(file_item(file, camera, in_immich))
}

/// The wire value of a flag: -1 rejected, 0 none, 1 selected.
fn flag_value(flag: Option<FlagFilter>) -> i8 {
    match flag {
        None => 0,
        Some(FlagFilter::Selected) => 1,
        Some(FlagFilter::Rejected) => -1,
    }
}

impl FileQuery {
    /// The `ORDER BY` terms of the sort, for the table aliased `f`. Only the primary key follows
    /// the order; the tie-breakers keep the order total, so paging is stable.
    ///
    /// The date sort orders by `+capture_ts`, an expression no index can deliver, unless the
    /// `files_volume_capture` index is meant to deliver it: see [`Self::reads_date_index`].
    fn order_by(&self) -> String {
        let direction = match self.order {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        };
        match self.sort {
            SortKey::Name => format!("f.name_norm {direction}, f.id ASC"),
            SortKey::Date => {
                let plus = if self.reads_date_index() { "" } else { "+" };
                format!("{plus}f.capture_ts {direction} NULLS LAST, f.name_norm ASC, f.id ASC")
            }
            SortKey::Size => format!("f.size {direction}, f.name_norm ASC, f.id ASC"),
        }
    }

    /// Is this the one listing the `files_volume_capture` index exists for: the files of a whole
    /// volume, newest first, no filter? A page of it stops after `offset + limit` rows instead of
    /// sorting the volume.
    ///
    /// The planner must not pick that index for any other date sort. Its statistics are sampled and
    /// read the same estimate for every volume, directory and filter value, so it would walk the
    /// index through the whole volume looking for the files of one folder, or for the few that
    /// match a filter, where the other access paths cost in proportion to what they return.
    fn reads_date_index(&self) -> bool {
        matches!((self.sort, self.order), (SortKey::Date, SortOrder::Desc))
            && self.filter_is_empty()
            && self.scope.volume_id.is_some()
            && self.scope.recursive
            && self.scope.dir.is_root()
    }

    /// One page of items in the query's order. `limit` is clamped to `1..=`[`MAX_PAGE_SIZE`] and a
    /// negative `offset` counts as 0.
    pub async fn items(
        &self,
        db: &Db,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<FileItem>, QueryError> {
        let rows = self
            .items_statement(offset, limit)
            .build()
            .fetch_all(db.read())
            .await?;
        Ok(rows
            .iter()
            .map(item_from_row)
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// The ids of every matching file in the query's order (the order of [`Self::items`]), at most
    /// [`MAX_IDS`] of them. Paging does not apply.
    pub async fn ids(&self, db: &Db) -> Result<Vec<FileId>, QueryError> {
        Ok(self
            .ids_statement()
            .build_query_scalar::<FileId>()
            .fetch_all(db.read())
            .await?)
    }

    /// How many files match, whatever the paging.
    pub async fn count(&self, db: &Db) -> Result<i64, QueryError> {
        Ok(self
            .count_statement()
            .build_query_scalar::<i64>()
            .fetch_one(db.read())
            .await?)
    }

    pub(super) fn items_statement(&self, offset: i64, limit: i64) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(format!(
            "SELECT {} FROM files f {ITEM_JOINS} WHERE ",
            item_columns("f")
        ));
        self.push_where(&mut qb, "f");
        qb.push(" ORDER BY ")
            .push(self.order_by())
            .push(" LIMIT ")
            .push_bind(limit.clamp(1, MAX_PAGE_SIZE))
            .push(" OFFSET ")
            .push_bind(offset.max(0));
        qb
    }

    pub(super) fn ids_statement(&self) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new("SELECT f.id FROM files f WHERE ");
        self.push_where(&mut qb, "f");
        qb.push(" ORDER BY ")
            .push(self.order_by())
            .push(" LIMIT ")
            .push_bind(MAX_IDS);
        qb
    }

    pub(super) fn count_statement(&self) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new("SELECT COUNT(*) FROM files f WHERE ");
        self.push_where(&mut qb, "f");
        qb
    }
}
