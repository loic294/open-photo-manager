//! The detail view of one file.

use apm_core::dto::{FileDetail, SidecarDto};
use apm_core::hash::{from_slice, to_hex};
use apm_core::paths::FileId;
use sqlx::{QueryBuilder, Row};

use super::QueryError;
use super::items::{ITEM_JOINS, item_columns, item_from_row};
use crate::db::Db;

/// Everything known about file `id`: the listing item, the content hash (lowercase hex, once
/// computed), the mime type, the birth time, the tags (sorted by name) and the sidecars (sorted by
/// name). Unknown ids are [`QueryError::FileNotFound`] (404). The parts are read in one
/// transaction, so they agree with each other.
pub async fn file_detail(db: &Db, id: FileId) -> Result<FileDetail, QueryError> {
    let mut tx = db.read().begin().await?;

    let mut qb = QueryBuilder::new(format!(
        "SELECT {} FROM files f {ITEM_JOINS} WHERE f.id = ",
        item_columns("f")
    ));
    qb.push_bind(id);
    let row = qb
        .build()
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(QueryError::FileNotFound(id))?;
    let item = item_from_row(&row)?;
    let hash: Option<Vec<u8>> = row.try_get("hash")?;
    let mime: Option<String> = row.try_get("mime")?;
    let btime: Option<i64> = row.try_get("btime")?;

    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM file_tags ft JOIN tags t ON t.id = ft.tag_id \
         WHERE ft.file_id = ? ORDER BY t.name",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;

    let sidecars: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.name, fs.kind FROM file_sidecars fs JOIN files s ON s.id = fs.sidecar_id \
         WHERE fs.file_id = ? ORDER BY s.name_norm, s.name",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(FileDetail {
        item,
        hash: hash
            .and_then(|bytes| from_slice(&bytes))
            .map(|digest| to_hex(&digest)),
        mime,
        btime,
        tags,
        sidecars: sidecars
            .into_iter()
            .map(|(name, kind)| SidecarDto { name, kind })
            .collect(),
    })
}
