//! The folder list and the footer counts.

use opm_core::dto::FooterCounts;
use opm_core::names::normalize_name;
use opm_core::paths::VolumeId;
use sqlx::{QueryBuilder, Sqlite};

use super::filter::FilterPart;
use super::{FileQuery, QueryError};
use crate::db::Db;

/// First child segment below `dir`. `column` must be a trusted SQL identifier.
pub(crate) fn push_child_segment(qb: &mut QueryBuilder<Sqlite>, column: &str, dir: &str) {
    let start = if dir.is_empty() {
        1
    } else {
        i64::try_from(dir.chars().count()).unwrap_or(i64::MAX - 2) + 2
    };
    qb.push("CASE WHEN instr(substr(")
        .push(column)
        .push(", ")
        .push_bind(start)
        .push("), '/') = 0 THEN substr(")
        .push(column)
        .push(", ")
        .push_bind(start)
        .push(") ELSE substr(")
        .push(column)
        .push(", ")
        .push_bind(start)
        .push(", instr(substr(")
        .push(column)
        .push(", ")
        .push_bind(start)
        .push("), '/') - 1) END");
}

/// One subfolder of the listed directory with its counts (see [`FileQuery::folders`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRow {
    /// Last path component.
    pub name: String,
    /// Full path from the volume root.
    pub path: String,
    pub volume_id: VolumeId,
    /// Matching files in the folder and below it.
    pub count: i64,
    /// Files in the folder and below it, whatever the filter says.
    pub total: i64,
}

impl FileQuery {
    /// The direct subfolders of the scope's directory (`scope.recursive` does not matter here),
    /// sorted by normalised name and then name. Empty folders are listed with a count and a total
    /// of 0. Needs a scope that names a volume.
    ///
    /// `count` is the number of files in the folder and below it that match the filter and the
    /// kind selection; `total` is the same ignoring the filter. One grouped scan of the files
    /// below the listed directory answers both for every folder.
    pub async fn folders(&self, db: &Db) -> Result<Vec<FolderRow>, QueryError> {
        let volume_id = self.scope.volume_id.ok_or(QueryError::VolumeRequired)?;
        let dir = self.scope.dir.as_str();
        let rows: Vec<(String, i64, i64)> = self
            .folders_statement(volume_id)
            .build_query_as()
            .fetch_all(db.read())
            .await?;
        let mut folders: Vec<FolderRow> = rows
            .into_iter()
            .map(|(name, count, total)| FolderRow {
                path: if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}/{name}")
                },
                name,
                volume_id,
                count,
                total,
            })
            .collect();
        folders.sort_by_cached_key(|folder| (normalize_name(&folder.name), folder.name.clone()));
        Ok(folders)
    }

    /// The matching files directly in the scope's directory and in the folders below it, in one
    /// scan. Honours the filter and the kind selection; `scope.recursive` does not matter.
    pub async fn footer_counts(&self, db: &Db) -> Result<FooterCounts, QueryError> {
        let (in_folder, in_subfolders): (i64, i64) = self
            .footer_statement()
            .build_query_as()
            .fetch_one(db.read())
            .await?;
        Ok(FooterCounts {
            in_folder,
            in_subfolders,
        })
    }

    /// One row per child directory of the scope's directory: `name`, `count`, `total`.
    ///
    /// `per_dir` counts the files of every directory below the listed one (one pass over the
    /// `(volume_id, dir_path)` index range). Each of those counts is attributed to the first path
    /// segment below the listed directory and the child directories listed in `dirs` are added as
    /// zero-count rows (`listed = 1`), so one `GROUP BY` adds everything up and keeps the empty
    /// folders. Grouping a union instead of joining `per_dir` to `dirs` matters: with thousands of
    /// children the join rescans the grouped rows once per child.
    ///
    /// `HAVING MAX(listed) = 1` drops a first segment that has no `dirs` row, because the folder
    /// list is the content of `dirs`.
    pub(super) fn folders_statement(&self, volume_id: VolumeId) -> QueryBuilder<Sqlite> {
        let dir = self.scope.dir.as_str();
        let mut qb = QueryBuilder::new(
            "WITH per_dir AS (SELECT f.dir_path AS dir_path, COUNT(*) AS total, ",
        );
        if self.filter_is_empty() {
            qb.push("COUNT(*)");
        } else {
            qb.push("SUM(CASE WHEN ");
            self.push_filter_only(&mut qb, "f");
            qb.push(" THEN 1 ELSE 0 END)");
        }
        qb.push(" AS cnt FROM files f WHERE ");
        self.push_condition(&mut qb, "f", true, FilterPart::Nothing);
        qb.push(" GROUP BY f.dir_path) SELECT child, SUM(cnt), SUM(total) FROM (SELECT ");
        push_child_segment(&mut qb, "dir_path", dir);
        qb.push(" AS child, total, cnt, 0 AS listed FROM per_dir WHERE dir_path <> ")
            .push_bind(dir.to_owned())
            .push(" UNION ALL SELECT d.name, 0, 0, 1 FROM dirs d WHERE d.volume_id = ")
            .push_bind(volume_id)
            .push(" AND d.parent_path = ")
            .push_bind(dir.to_owned())
            .push(") GROUP BY child HAVING MAX(listed) = 1");
        qb
    }

    pub(super) fn footer_statement(&self) -> QueryBuilder<Sqlite> {
        let dir = self.scope.dir.as_str();
        let mut qb = QueryBuilder::new("SELECT COALESCE(SUM(f.dir_path = ");
        qb.push_bind(dir.to_owned())
            .push("), 0), COALESCE(SUM(f.dir_path <> ")
            .push_bind(dir.to_owned())
            .push("), 0) FROM files f WHERE ");
        self.push_condition(&mut qb, "f", true, FilterPart::All);
        qb
    }
}
