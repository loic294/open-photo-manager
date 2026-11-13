use std::collections::{HashMap, HashSet};

use opm_core::dto::{FolderEntry, Overlay, PlanSide, SortKey, SortOrder};
use opm_core::names::normalize_name;
use opm_core::paths::{FileId, PlanId, RelPath};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::db::{Db, subtree::Subtree};
use crate::plans::PlanError;
use crate::plans::overlay::{FolderOverlay, FolderPlanInfo, GhostPage, GhostView, Window};
use crate::query::{
    FileQuery, ITEM_JOINS, KindFilter, QueryScope, item_columns, item_from_row, push_child_segment,
};

fn filter(view: &GhostView) -> Result<FileQuery, PlanError> {
    Ok(FileQuery::new(
        QueryScope {
            volume_id: None,
            dir: RelPath::root(),
            recursive: true,
        },
        &view.filter,
    )?
    .kinds(KindFilter::All))
}

fn predicate(qb: &mut QueryBuilder<Sqlite>, plan: PlanId, view: &GhostView, recursive: bool) {
    qb.push("m.plan_id = ")
        .push_bind(plan)
        .push(" AND m.status = 'move' AND m.dst_volume_id = ")
        .push_bind(view.volume_id)
        .push(" AND ");
    if recursive {
        Subtree::new(&view.dir).push_predicate(qb, "m.dst_dir");
    } else {
        qb.push("m.dst_dir = ")
            .push_bind(view.dir.as_str().to_owned());
    }
}

fn count_statement(plan: PlanId, view: &GhostView, filter: &FileQuery) -> QueryBuilder<Sqlite> {
    let unfiltered = view.filter == opm_core::dto::Filter::default();
    if unfiltered {
        // Build completion verifies membership. Count the covering range, then subtract
        // rare dead dependencies; no live-file lookup is needed for an unfiltered total.
        let mut qb = QueryBuilder::new(
            "SELECT CASE WHEN EXISTS(SELECT 1 FROM plan_file_tracking WHERE plan_id = ",
        );
        qb.push_bind(plan)
            .push(") THEN (SELECT COUNT(*) FROM move_items m WHERE ");
        predicate(&mut qb, plan, view, false);
        qb.push(") - (SELECT COUNT(*) FROM (");
        for (index, (column, lookup)) in [
            ("file_id", "sqlite_autoindex_move_items_1"),
            ("counterpart_id", "move_items_counterpart"),
            ("replaced_id", "move_items_replaced"),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                qb.push(" UNION ");
            }
            qb.push("SELECT m.file_id FROM plan_file_identities i INDEXED BY plan_file_identities_dead CROSS JOIN move_items m INDEXED BY ")
                .push(lookup).push(" ON m.plan_id = i.plan_id AND m.").push(column)
                .push(" = i.file_id WHERE i.plan_id = ").push_bind(plan)
                .push(" AND i.live = 0 AND ");
            if column != "file_id" {
                qb.push("m.").push(column).push(" IS NOT NULL AND ");
            }
            predicate(&mut qb, plan, view, false);
        }
        qb.push(")) ELSE 0 END");
        return qb;
    }
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM move_items m ");
    if !unfiltered {
        qb.push("JOIN files f ON f.id = m.file_id ");
    }
    qb.push("JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = m.file_id AND i.live = 1 LEFT JOIN plan_file_identities ci ON ci.plan_id = m.plan_id AND ci.file_id = m.counterpart_id LEFT JOIN plan_file_identities ri ON ri.plan_id = m.plan_id AND ri.file_id = m.replaced_id WHERE (m.counterpart_id IS NULL OR ci.live = 1) AND (m.replaced_id IS NULL OR ri.live = 1) AND ");
    predicate(&mut qb, plan, view, false);
    if !unfiltered {
        qb.push(" AND ");
        filter.push_where(&mut qb, "f");
    }
    qb
}

fn page_statement(
    plan: PlanId,
    view: &GhostView,
    filter: &FileQuery,
    window: Window,
) -> QueryBuilder<Sqlite> {
    let mut qb = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {}, m.dst_volume_id, m.dst_dir, m.dst_name FROM move_items m JOIN files f ON f.id = m.file_id JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = m.file_id AND i.live = 1 LEFT JOIN plan_file_identities ci ON ci.plan_id = m.plan_id AND ci.file_id = m.counterpart_id LEFT JOIN plan_file_identities ri ON ri.plan_id = m.plan_id AND ri.file_id = m.replaced_id {ITEM_JOINS} WHERE (m.counterpart_id IS NULL OR ci.live = 1) AND (m.replaced_id IS NULL OR ri.live = 1) AND ",
        item_columns("f")
    ));
    predicate(&mut qb, plan, view, false);
    qb.push(" AND ");
    filter.push_where(&mut qb, "f");
    let direction = if view.order == SortOrder::Asc {
        " ASC"
    } else {
        " DESC"
    };
    qb.push(" ORDER BY ")
        .push(match view.sort {
            SortKey::Name => "m.dst_name_norm",
            SortKey::Date => "f.capture_ts",
            SortKey::Size => "f.size",
        })
        .push(direction);
    if view.sort == SortKey::Date {
        qb.push(" NULLS LAST");
    }
    if view.sort != SortKey::Name {
        qb.push(", m.dst_name_norm ASC");
    }
    qb.push(", m.file_id ASC LIMIT ")
        .push_bind(window.take.max(0))
        .push(" OFFSET ")
        .push_bind(window.skip.max(0));
    qb
}

pub(super) async fn items(
    db: &Db,
    plan: PlanId,
    side: PlanSide,
    view: &GhostView,
    window: Window,
) -> Result<GhostPage, PlanError> {
    if side == PlanSide::Source {
        return Ok(GhostPage::default());
    }
    let filter = filter(view)?;
    let total = count_statement(plan, view, &filter)
        .build_query_scalar()
        .fetch_one(db.read())
        .await?;
    if window.take == 0 {
        return Ok(GhostPage {
            total,
            items: Vec::new(),
        });
    }
    let mut qb = page_statement(plan, view, &filter, window);
    let mut items = Vec::new();
    for row in qb.build().fetch_all(db.read()).await? {
        let mut item = item_from_row(&row)?;
        item.volume_id = row.try_get("dst_volume_id")?;
        item.dir = row.try_get("dst_dir")?;
        item.name = row.try_get("dst_name")?;
        item.in_immich = false;
        item.overlay.planned = true;
        items.push(item);
    }
    Ok(GhostPage { total, items })
}

pub(super) async fn flags(
    db: &Db,
    plan: PlanId,
    side: PlanSide,
    ids: &[FileId],
) -> Result<HashMap<FileId, Overlay>, PlanError> {
    let mut result = HashMap::new();
    for ids in ids.chunks(30_000) {
        let mut qb = QueryBuilder::<Sqlite>::new(match side {
            PlanSide::Source => {
                "SELECT m.file_id, 1 AS matched, 0 AS will_delete FROM move_items m INDEXED BY move_items_live_source CROSS JOIN files f ON f.id = m.file_id CROSS JOIN plan_file_tracking t ON t.plan_id = m.plan_id CROSS JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = m.file_id AND i.live = 1 WHERE m.status = 'move' AND m.plan_id = "
            }
            PlanSide::Comparison => {
                "SELECT x.id, MAX(matched), MAX(will_delete) FROM (SELECT m.counterpart_id AS id, 1 AS matched, 0 AS will_delete FROM move_items m WHERE m.counterpart_id IS NOT NULL AND m.plan_id = "
            }
        });
        qb.push_bind(plan);
        if side == PlanSide::Comparison {
            qb.push(" UNION ALL SELECT m.replaced_id, 0, 1 FROM move_items m WHERE m.replaced_id IS NOT NULL AND m.status = 'move' AND m.plan_id = ").push_bind(plan).push(") x JOIN files f ON f.id = x.id JOIN plan_file_tracking t ON t.plan_id = ").push_bind(plan).push(" JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = x.id AND i.live = 1 WHERE x.id IN (");
        } else {
            qb.push(" AND m.file_id IN (");
        }
        let mut sep = qb.separated(", ");
        for id in ids {
            sep.push_bind(*id);
        }
        sep.push_unseparated(")");
        if side == PlanSide::Comparison {
            qb.push(" GROUP BY x.id");
        }
        for (id, matched, will_delete) in qb
            .build_query_as::<(FileId, bool, bool)>()
            .fetch_all(db.read())
            .await?
        {
            result.insert(
                id,
                Overlay {
                    matched,
                    will_delete,
                    ..Overlay::default()
                },
            );
        }
    }
    Ok(result)
}

pub(super) async fn ids(db: &Db, plan: PlanId, side: PlanSide) -> Result<Vec<FileId>, PlanError> {
    if side == PlanSide::Comparison {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar("SELECT m.file_id FROM move_items m JOIN files f ON f.id = m.file_id JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = m.file_id AND i.live = 1 WHERE m.plan_id = ? AND m.status = 'move' ORDER BY m.file_id")
        .bind(plan).fetch_all(db.read()).await?)
}

pub(super) async fn folders(
    db: &Db,
    plan: PlanId,
    side: PlanSide,
    view: &GhostView,
    children: &[String],
) -> Result<FolderOverlay, PlanError> {
    if side == PlanSide::Source {
        return Ok(FolderOverlay::default());
    }
    let filter = filter(view)?;
    let mut qb = QueryBuilder::<Sqlite>::new(
        "WITH per_dir AS (SELECT m.dst_dir, COUNT(*) AS total, SUM(CASE WHEN ",
    );
    filter.push_where(&mut qb, "f");
    qb.push(" THEN 1 ELSE 0 END) AS cnt FROM move_items m JOIN files f ON f.id = m.file_id JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = t.plan_id AND i.file_id = m.file_id AND i.live = 1 LEFT JOIN plan_file_identities ci ON ci.plan_id = m.plan_id AND ci.file_id = m.counterpart_id LEFT JOIN plan_file_identities ri ON ri.plan_id = m.plan_id AND ri.file_id = m.replaced_id WHERE (m.counterpart_id IS NULL OR ci.live = 1) AND (m.replaced_id IS NULL OR ri.live = 1) AND ");
    predicate(&mut qb, plan, view, true);
    qb.push(" GROUP BY m.dst_dir) SELECT ");
    push_child_segment(&mut qb, "dst_dir", view.dir.as_str());
    qb.push(" AS child, SUM(cnt), SUM(total) FROM per_dir GROUP BY child");
    let rows: Vec<(String, i64, i64)> = qb.build_query_as().fetch_all(db.read()).await?;
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT name FROM dirs WHERE volume_id = ? AND parent_path = ?")
            .bind(view.volume_id)
            .bind(view.dir.as_str())
            .fetch_all(db.read())
            .await?;
    let existing: HashSet<_> = existing
        .into_iter()
        .map(|s| normalize_name(&s))
        .chain(children.iter().map(|s| normalize_name(s)))
        .collect();
    let mut result = FolderOverlay::default();
    for (child, count, total) in rows {
        result.dir_planned |= total > 0;
        if child.is_empty() {
            result.ghosts.in_folder += count;
        } else {
            result.ghosts.in_subfolders += count;
            if let Some(real) = children
                .iter()
                .find(|c| normalize_name(c) == normalize_name(&child))
            {
                result.per_child.insert(
                    real.clone(),
                    FolderPlanInfo {
                        ghost_count: count,
                        ghost_total: total,
                        ..FolderPlanInfo::default()
                    },
                );
            } else if !existing.contains(&normalize_name(&child)) {
                result.ghost_folders.push(FolderEntry {
                    path: view
                        .dir
                        .join(&child)
                        .map_err(anyhow::Error::from)?
                        .as_str()
                        .to_owned(),
                    name: child,
                    volume_id: view.volume_id,
                    count,
                    total,
                    matches: None,
                    preview: None,
                    overlay: Overlay {
                        planned: true,
                        ..Overlay::default()
                    },
                });
            }
        }
    }
    result
        .ghost_folders
        .sort_by_key(|f| normalize_name(&f.name));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Execute;

    #[tokio::test]
    async fn name_page_and_exact_count_use_the_destination_index_without_a_temp_sort() {
        let temp = Db::open_temp().await.unwrap();
        let view = GhostView {
            volume_id: 1,
            dir: RelPath::new("out").unwrap(),
            filter: Default::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        let filter = filter(&view).unwrap();
        for mut statement in [
            count_statement(1, &view, &filter),
            page_statement(1, &view, &filter, Window { skip: 0, take: 200 }),
        ] {
            let mut built = statement.build();
            let arguments = built.take_arguments().unwrap().unwrap_or_default();
            let sql = format!("EXPLAIN QUERY PLAN {}", built.sql().as_str());
            let rows: Vec<(i64, i64, i64, String)> =
                sqlx::query_as_with(sqlx::AssertSqlSafe(sql), arguments)
                    .fetch_all(temp.read())
                    .await
                    .unwrap();
            let details: Vec<_> = rows.into_iter().map(|(_, _, _, detail)| detail).collect();
            assert!(
                details
                    .iter()
                    .any(|d| d.contains("move_items_live_destination")),
                "{details:?}"
            );
            assert!(
                !details.iter().any(|d| d.contains("TEMP B-TREE FOR")),
                "{details:?}"
            );
        }
    }
}
