//! The two small queries behind the browse API that [`FileQuery`] does not cover: the volumes
//! shown as folders at the top of the file system view, and "does this directory exist".
//!
//! Owned by todo `building-browse-api`. Everything else the browse API reads (folders, items,
//! counts, facets, details) goes through [`FileQuery`] and [`file_detail`](super::file_detail).

use opm_core::dto::Filter;
use opm_core::names::normalize_name;
use opm_core::paths::{RelPath, VolumeId};
use sqlx::{QueryBuilder, Sqlite};

use super::{FileQuery, QueryError, QueryScope};
use crate::db::Db;

/// One volume with the number of assets in it: a row of the volumes root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeCount {
    pub id: VolumeId,
    /// The volume name, which is the folder name at the volumes root.
    pub name: String,
    /// Assets in the volume that pass the filter (sidecars are not counted).
    pub count: i64,
    /// Assets in the volume, whatever the filter says.
    pub total: i64,
}

/// The order of every list of volumes: normalised name (NFC, lower case), then the name itself,
/// then the id. The same rule orders the folders of a directory, so a volume sorts where a folder
/// of that name would. Use it with `sort_by_cached_key`.
pub fn volume_sort_key(name: &str, id: VolumeId) -> (String, String, VolumeId) {
    (normalize_name(name), name.to_owned(), id)
}

/// Every volume known to the database, online or not, with its asset counts, in
/// [`volume_sort_key`] order. A volume without files is listed with 0 and 0.
///
/// One grouped scan of `files` counts every volume at once: the filter, the kind selection and
/// the scope all come from [`FileQuery::push_where`] (an every-volume, whole-subtree scope
/// constrains nothing, so only the kind and the filter remain), once with the filter for `count`
/// and once without it for `total`, the way [`FileQuery::folders`] counts both in one pass. The
/// filter is validated like any other (a bad one is [`QueryError::Filter`]).
pub async fn volume_counts(db: &Db, filter: &Filter) -> Result<Vec<VolumeCount>, QueryError> {
    let rows: Vec<(VolumeId, String, i64, i64)> = counts_statement(filter)?
        .build_query_as()
        .fetch_all(db.read())
        .await?;
    let mut volumes: Vec<VolumeCount> = rows
        .into_iter()
        .map(|(id, name, count, total)| VolumeCount {
            id,
            name,
            count,
            total,
        })
        .collect();
    volumes.sort_by_cached_key(|volume| volume_sort_key(&volume.name, volume.id));
    Ok(volumes)
}

/// The statement of [`volume_counts`]: one row per volume (`id`, `name`, `count`, `total`).
fn counts_statement(filter: &Filter) -> Result<QueryBuilder<Sqlite>, QueryError> {
    let filtered = FileQuery::new(all_volumes(), filter)?;
    let unfiltered = FileQuery::new(all_volumes(), &Filter::default())?;
    let filter_matters = !filtered.filter().is_empty();

    let mut qb = QueryBuilder::new("SELECT v.id, v.name, ");
    qb.push(if filter_matters {
        "COALESCE(c.matching, 0)"
    } else {
        "COALESCE(c.total, 0)"
    });
    qb.push(
        ", COALESCE(c.total, 0) FROM volumes v LEFT JOIN (\
         SELECT f.volume_id AS volume_id, SUM(CASE WHEN ",
    );
    unfiltered.push_where(&mut qb, "f");
    qb.push(" THEN 1 ELSE 0 END) AS total");
    if filter_matters {
        qb.push(", SUM(CASE WHEN ");
        filtered.push_where(&mut qb, "f");
        qb.push(" THEN 1 ELSE 0 END) AS matching");
    }
    qb.push(" FROM files f GROUP BY f.volume_id) c ON c.volume_id = v.id");
    Ok(qb)
}

/// The scope of "every directory of every volume": it constrains nothing, so a query over it
/// counts and lists the whole index (the volumes root, its facets and its footer).
pub fn all_volumes() -> QueryScope {
    QueryScope {
        volume_id: None,
        dir: RelPath::root(),
        recursive: true,
    }
}

/// Whether the index knows a volume called by this id.
pub async fn volume_exists(db: &Db, volume_id: VolumeId) -> Result<bool, QueryError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM volumes WHERE id = ?")
        .bind(volume_id)
        .fetch_optional(db.read())
        .await?;
    Ok(found.is_some())
}

/// Whether directory `dir` of the volume is in the index (`dirs`). The root of a volume that
/// exists always is; any other directory needs its own row, which the scanner writes for every
/// directory it finds, empty ones included. This only reads the index: the file system is never
/// consulted.
pub async fn dir_exists(db: &Db, volume_id: VolumeId, dir: &RelPath) -> Result<bool, QueryError> {
    let found: Option<i64> = if dir.is_root() {
        sqlx::query_scalar("SELECT 1 FROM volumes WHERE id = ?")
            .bind(volume_id)
            .fetch_optional(db.read())
            .await?
    } else {
        sqlx::query_scalar("SELECT 1 FROM dirs WHERE volume_id = ? AND path = ?")
            .bind(volume_id)
            .bind(dir.as_str())
            .fetch_optional(db.read())
            .await?
    };
    Ok(found.is_some())
}
