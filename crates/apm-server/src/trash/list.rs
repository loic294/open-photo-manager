//! Reading the trash: the summary, the listing and the deletions. Everything here answers from
//! the index (`trash_items`) inside one read transaction, so a page, its `total` and its summary
//! always describe the same moment, and nothing here changes state.

use std::collections::HashMap;

use apm_core::dto::{
    SortOrder, TrashBatchDto, TrashBatchesQuery, TrashBatchesResponse, TrashItemDto,
    TrashListQuery, TrashListResponse, TrashSort, TrashSummary, TrashVolumeSummary,
};
use apm_core::paths::{RelPath, VolumeId};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection};

use super::{TrashService, to_u64};
use crate::error::AppError;

/// Reasons a deletion lists.
const MAX_REASONS: usize = 5;

/// The name of an item, in SQL: what follows the last `/` of `orig_path`, in lower case.
/// `rtrim(p, replace(p, '/', ''))` strips every trailing character that is not a slash, which
/// leaves the folder part; removing it from `p` leaves the name.
pub(super) const NAME_SORT_KEY: &str =
    "lower(replace(t.orig_path, rtrim(t.orig_path, replace(t.orig_path, '/', '')), ''))";

#[derive(Debug, FromRow)]
struct VolumeTotals {
    volume_id: VolumeId,
    items: i64,
    bytes: i64,
    max_id: i64,
}

#[derive(Debug, FromRow)]
struct ItemRow {
    id: i64,
    volume_id: VolumeId,
    volume: String,
    orig_path: String,
    is_dir: bool,
    size: i64,
    reason: Option<String>,
    batch: String,
    task_id: Option<i64>,
    trashed_at: i64,
    has_snapshot: bool,
}

#[derive(Debug, FromRow)]
struct BatchRow {
    volume_id: VolumeId,
    volume: String,
    batch: String,
    first_at: i64,
    items: i64,
    bytes: i64,
    task_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct ReasonRow {
    volume_id: VolumeId,
    batch: String,
    reason: String,
    items: i64,
}

impl TrashService {
    /// What the trash holds now, per volume, with the settings the page quotes.
    pub async fn summary(&self) -> Result<TrashSummary, AppError> {
        let mut conn = self.db.read().acquire().await?;
        let totals = volume_totals(&mut conn).await?;
        Ok(self.summarize(&totals))
    }

    /// One page of items, the number of items that match the filters and the summary.
    pub async fn list(&self, query: TrashListQuery) -> Result<TrashListResponse, AppError> {
        let mut tx = self.db.read().begin().await?;
        let totals = volume_totals(&mut tx).await?;
        let summary = self.summarize(&totals);

        let rows: Vec<ItemRow> = list_query(&query)
            .build_query_as()
            .fetch_all(&mut *tx)
            .await?;
        let total = match (&query.batch, &query.q, query.volume_id) {
            (None, None, None) => summary.items,
            (None, None, Some(volume_id)) => summary
                .volumes
                .iter()
                .find(|volume| volume.volume_id == volume_id)
                .map_or(0, |volume| volume.items),
            _ => {
                let counted: i64 = count_query(&query)
                    .build_query_scalar()
                    .fetch_one(&mut *tx)
                    .await?;
                to_u64(counted)
            }
        };
        tx.commit().await?;

        Ok(TrashListResponse {
            items: rows.into_iter().map(item_dto).collect(),
            total,
            summary,
        })
    }

    /// One page of deletions (a batch of a volume), newest first, with the reasons of their items.
    pub async fn batches(
        &self,
        query: TrashBatchesQuery,
    ) -> Result<TrashBatchesResponse, AppError> {
        let mut tx = self.db.read().begin().await?;
        let totals = volume_totals(&mut tx).await?;
        let summary = self.summarize(&totals);

        let rows: Vec<BatchRow> = batches_query(&query)
            .build_query_as()
            .fetch_all(&mut *tx)
            .await?;
        let mut reasons = reasons_of(&mut tx, &rows).await?;
        let total: i64 = batches_count_query(&query)
            .build_query_scalar()
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;

        let batches = rows
            .into_iter()
            .map(|row| TrashBatchDto {
                reasons: reasons
                    .remove(&(row.volume_id, row.batch.clone()))
                    .unwrap_or_default(),
                volume_id: row.volume_id,
                volume: row.volume,
                batch: row.batch,
                task_id: row.task_id,
                trashed_at: row.first_at,
                items: to_u64(row.items),
                bytes: to_u64(row.bytes),
            })
            .collect();
        Ok(TrashBatchesResponse {
            batches,
            total: to_u64(total),
            summary,
        })
    }

    /// The summary of a set of per-volume totals: names and reachability come from the registry,
    /// the retention and the conflict policy from the settings.
    fn summarize(&self, totals: &[VolumeTotals]) -> TrashSummary {
        let settings = self.settings.get();
        let mut volumes: Vec<TrashVolumeSummary> = totals
            .iter()
            .map(|totals| {
                let known = self.volumes.volume(totals.volume_id);
                TrashVolumeSummary {
                    volume_id: totals.volume_id,
                    name: known.as_ref().map_or_else(
                        || format!("volume {}", totals.volume_id),
                        |v| v.name.clone(),
                    ),
                    items: to_u64(totals.items),
                    bytes: to_u64(totals.bytes),
                    online: known.as_ref().is_some_and(|volume| volume.online),
                    writable: known.as_ref().is_some_and(|volume| volume.writable),
                }
            })
            .collect();
        volumes.sort_by(|a, b| (&a.name, a.volume_id).cmp(&(&b.name, b.volume_id)));
        TrashSummary {
            items: volumes
                .iter()
                .fold(0, |sum, volume| sum.saturating_add(volume.items)),
            bytes: volumes
                .iter()
                .fold(0, |sum, volume| sum.saturating_add(volume.bytes)),
            max_id: totals.iter().map(|totals| totals.max_id).max().unwrap_or(0),
            volumes,
            retention_days: settings.trash_retention_days,
            conflict_policy: settings.default_conflict_policy,
        }
    }
}

/// Items, bytes and the highest id of every volume that holds trash: one pass over the covering
/// index `trash_items_volume_batch`.
pub(super) const TOTALS_SQL: &str = "SELECT volume_id, COUNT(*) AS items, \
     COALESCE(SUM(size), 0) AS bytes, MAX(id) AS max_id FROM trash_items GROUP BY volume_id";

async fn volume_totals(conn: &mut SqliteConnection) -> Result<Vec<VolumeTotals>, sqlx::Error> {
    sqlx::query_as(TOTALS_SQL).fetch_all(conn).await
}

/// The statement of one page of deletions: the groups come out of the covering index
/// `trash_items_volume_batch` in `(volume_id, batch)` order and only the `LIMIT` newest are kept,
/// then the volume names are joined to that page alone.
pub(super) fn batches_query(query: &TrashBatchesQuery) -> QueryBuilder<Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "SELECT g.volume_id, v.name AS volume, g.batch, g.first_at, g.items, g.bytes, g.task_id \
         FROM (SELECT volume_id, batch, MIN(trashed_at) AS first_at, COUNT(*) AS items, \
         COALESCE(SUM(size), 0) AS bytes, MAX(task_id) AS task_id FROM trash_items",
    );
    if let Some(volume_id) = query.volume_id {
        builder.push(" WHERE volume_id = ").push_bind(volume_id);
    }
    builder
        .push(
            " GROUP BY volume_id, batch ORDER BY first_at DESC, batch DESC, volume_id DESC LIMIT ",
        )
        .push_bind(i64::from(clamp_limit(query.limit)))
        .push(" OFFSET ")
        .push_bind(i64::from(query.offset))
        .push(
            ") g JOIN volumes v ON v.id = g.volume_id \
             ORDER BY g.first_at DESC, g.batch DESC, g.volume_id DESC",
        );
    builder
}

/// The statement that counts all the deletions.
pub(super) fn batches_count_query(query: &TrashBatchesQuery) -> QueryBuilder<Sqlite> {
    let mut builder =
        QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM (SELECT 1 FROM trash_items");
    if let Some(volume_id) = query.volume_id {
        builder.push(" WHERE volume_id = ").push_bind(volume_id);
    }
    builder.push(" GROUP BY volume_id, batch)");
    builder
}

/// The statement that counts the reasons of the items of the deletions `batches` (volume and
/// batch name): one lookup of the covering index per deletion.
pub(super) fn reasons_query(batches: &[(VolumeId, String)]) -> QueryBuilder<Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new("WITH page(volume_id, batch) AS (VALUES ");
    for (index, (volume_id, batch)) in batches.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder
            .push("(")
            .push_bind(*volume_id)
            .push(", ")
            .push_bind(batch.clone())
            .push(")");
    }
    builder.push(
        ") SELECT i.volume_id, i.batch, i.reason, COUNT(*) AS items FROM page p \
         JOIN trash_items i ON i.volume_id = p.volume_id AND i.batch = p.batch \
         WHERE i.reason IS NOT NULL GROUP BY i.volume_id, i.batch, i.reason",
    );
    builder
}

/// The reasons of the items of each batch of a page: the most frequent first, at most
/// [`MAX_REASONS`]. Items without a reason add none.
async fn reasons_of(
    conn: &mut SqliteConnection,
    rows: &[BatchRow],
) -> Result<HashMap<(VolumeId, String), Vec<String>>, sqlx::Error> {
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    let batches: Vec<(VolumeId, String)> = rows
        .iter()
        .map(|row| (row.volume_id, row.batch.clone()))
        .collect();
    let found: Vec<ReasonRow> = reasons_query(&batches)
        .build_query_as()
        .fetch_all(conn)
        .await?;

    let mut grouped: HashMap<(VolumeId, String), Vec<(i64, String)>> = HashMap::new();
    for row in found {
        grouped
            .entry((row.volume_id, row.batch))
            .or_default()
            .push((row.items, row.reason));
    }
    Ok(grouped
        .into_iter()
        .map(|(key, mut reasons)| {
            reasons.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let names = reasons
                .into_iter()
                .take(MAX_REASONS)
                .map(|(_, reason)| reason)
                .collect();
            (key, names)
        })
        .collect())
}

fn clamp_limit(limit: u32) -> u32 {
    limit.clamp(1, TrashListQuery::MAX_LIMIT)
}

/// The statement of one page of the listing. `CROSS JOIN` fixes the join order: the trash is the
/// outer loop, so an index of it can serve the `ORDER BY`, whatever the statistics say about the
/// handful of volumes.
pub(super) fn list_query(query: &TrashListQuery) -> QueryBuilder<Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        "SELECT t.id, t.volume_id, v.name AS volume, t.orig_path, t.is_dir, t.size, t.reason, \
         t.batch, t.task_id, t.trashed_at, t.meta_json IS NOT NULL AS has_snapshot \
         FROM trash_items t CROSS JOIN volumes v ON v.id = t.volume_id",
    );
    push_filters(&mut builder, query);
    push_order(&mut builder, query.sort, query.effective_order());
    builder
        .push(" LIMIT ")
        .push_bind(i64::from(clamp_limit(query.limit)))
        .push(" OFFSET ")
        .push_bind(i64::from(query.offset));
    builder
}

/// The statement that counts the items the filters keep.
pub(super) fn count_query(query: &TrashListQuery) -> QueryBuilder<Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM trash_items t");
    push_filters(&mut builder, query);
    builder
}

fn push_filters(builder: &mut QueryBuilder<Sqlite>, query: &TrashListQuery) {
    let mut glue = " WHERE ";
    if let Some(volume_id) = query.volume_id {
        builder
            .push(glue)
            .push("t.volume_id = ")
            .push_bind(volume_id);
        glue = " AND ";
    }
    if let Some(batch) = &query.batch {
        builder
            .push(glue)
            .push("t.batch = ")
            .push_bind(batch.clone());
        glue = " AND ";
    }
    if let Some(text) = &query.q {
        builder
            .push(glue)
            .push("t.orig_path LIKE ")
            .push_bind(format!("%{}%", escape_like(text)))
            .push(" ESCAPE '\\'");
    }
}

/// The sort keys in priority order. The id is the last one in the same direction, so two rows
/// never tie and a page boundary never repeats or skips a row.
fn push_order(builder: &mut QueryBuilder<Sqlite>, sort: TrashSort, order: SortOrder) {
    let keys: &[&str] = match sort {
        TrashSort::TrashedAt => &["t.trashed_at"],
        TrashSort::Size => &["t.size"],
        TrashSort::Path => &["t.orig_path COLLATE NOCASE"],
        TrashSort::Name => &[NAME_SORT_KEY, "t.orig_path COLLATE NOCASE"],
    };
    let direction = match order {
        SortOrder::Asc => "ASC",
        SortOrder::Desc => "DESC",
    };
    builder.push(" ORDER BY ");
    for key in keys {
        builder.push(*key).push(" ").push(direction).push(", ");
    }
    builder.push("t.id ").push(direction);
}

/// Escapes the wildcards of `LIKE` (`%`, `_`) and the escape character itself, so the text is
/// matched as it was typed.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn item_dto(row: ItemRow) -> TrashItemDto {
    let (name, dir) = match RelPath::new(&row.orig_path) {
        Ok(rel) => (
            rel.file_name().unwrap_or_default().to_owned(),
            rel.parent()
                .map(|parent| parent.as_str().to_owned())
                .unwrap_or_default(),
        ),
        Err(_) => (row.orig_path.clone(), String::new()),
    };
    TrashItemDto {
        id: row.id,
        volume_id: row.volume_id,
        volume: row.volume,
        name,
        dir,
        is_dir: row.is_dir,
        size: to_u64(row.size),
        reason: row.reason,
        batch: row.batch,
        task_id: row.task_id,
        trashed_at: row.trashed_at,
        has_snapshot: row.has_snapshot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_and_the_escape_character_are_escaped() {
        assert_eq!(escape_like("100%_done\\"), "100\\%\\_done\\\\");
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like(""), "");
    }

    #[test]
    fn an_item_is_split_into_its_name_and_its_folder() {
        let row = |orig_path: &str| ItemRow {
            id: 1,
            volume_id: 1,
            volume: "a".to_owned(),
            orig_path: orig_path.to_owned(),
            is_dir: false,
            size: 0,
            reason: None,
            batch: "b".to_owned(),
            task_id: None,
            trashed_at: 0,
            has_snapshot: false,
        };
        let nested = item_dto(row("2019/Trip/IMG_1.jpg"));
        assert_eq!(
            (nested.name.as_str(), nested.dir.as_str()),
            ("IMG_1.jpg", "2019/Trip")
        );
        let top = item_dto(row("IMG_2.jpg"));
        assert_eq!((top.name.as_str(), top.dir.as_str()), ("IMG_2.jpg", ""));
        let odd = item_dto(row("a/../b"));
        assert_eq!((odd.name.as_str(), odd.dir.as_str()), ("a/../b", ""));
    }

    #[test]
    fn the_listing_sql_binds_every_value() {
        let query = TrashListQuery {
            volume_id: Some(2),
            batch: Some("20240101-000000-t1'; DROP TABLE trash_items; --".to_owned()),
            q: Some("a%b".to_owned()),
            sort: TrashSort::Name,
            ..TrashListQuery::default()
        };
        let statement = list_query(&query).sql();
        let sql = statement.as_str();
        assert!(!sql.contains("DROP TABLE"), "{sql}");
        assert!(!sql.contains("a%b"), "{sql}");
        assert!(sql.contains("t.volume_id = ?"), "{sql}");
        assert!(sql.ends_with("t.id ASC LIMIT ? OFFSET ?"), "{sql}");
    }
}
