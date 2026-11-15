use std::collections::{BTreeMap, HashMap, HashSet};

use opm_core::dto::{DuplicateFile, DuplicateFileGroup, DuplicateFileGroups, PlanKind, PlanState};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::db::Db;
use crate::plans::{Plan, PlanError};
use crate::query::{ITEM_JOINS, item_columns, item_from_row};

pub async fn groups(db: &Db, plan: &Plan) -> Result<DuplicateFileGroups, PlanError> {
    if plan.kind != PlanKind::Duplicates || super::parameters(plan.params()?)?.1.is_none() {
        return Err(PlanError::invalid("this is not a duplicate file-view plan"));
    }
    if plan.state != PlanState::Ready {
        return Err(PlanError::invalid(
            "refresh the duplicate search before reviewing files",
        ));
    }
    let mut read = db.read().begin().await?;
    let pairs: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT source_id, comparison_id FROM duplicate_pairs WHERE plan_id = ? ORDER BY source_id, comparison_id",
    ).bind(plan.id).fetch_all(&mut *read).await?;
    let mut neighbours: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for &(a, b) in &pairs {
        neighbours.entry(a).or_default().push(b);
        neighbours.entry(b).or_default().push(a);
    }
    let mut qb = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {}, f.btime, f.has_description FROM files f {ITEM_JOINS} \
         JOIN plan_file_identities i ON i.file_id = f.id WHERE i.plan_id = ",
        item_columns("f")
    ));
    qb.push_bind(plan.id).push(" AND i.live = 1 ORDER BY f.id");
    let mut files = HashMap::new();
    for row in qb.build().fetch_all(&mut *read).await? {
        let item = item_from_row(&row)?;
        files.insert(
            item.id,
            DuplicateFile {
                item,
                btime: row.try_get("btime")?,
                has_description: row.try_get("has_description")?,
                tags: Vec::new(),
            },
        );
    }
    if files.len() != neighbours.len() {
        return Err(PlanError::invalid(
            "files changed since this search; refresh before reviewing",
        ));
    }
    let tags: Vec<(i64, String)> = sqlx::query_as(
        "SELECT ft.file_id, t.name FROM plan_file_identities i \
         JOIN file_tags ft ON ft.file_id = i.file_id JOIN tags t ON t.id = ft.tag_id \
         WHERE i.plan_id = ? AND i.live = 1 ORDER BY t.name",
    )
    .bind(plan.id)
    .fetch_all(&mut *read)
    .await?;
    for (id, tag) in tags {
        if let Some(file) = files.get_mut(&id) {
            file.tags.push(tag);
        }
    }
    read.commit().await?;
    let mut visited = HashSet::new();
    let mut membership = HashMap::new();
    let mut groups = Vec::new();
    for &id in neighbours.keys() {
        if !visited.insert(id) {
            continue;
        }
        let index = groups.len();
        let mut group = DuplicateFileGroup {
            files: Vec::new(),
            pairs: Vec::new(),
        };
        let mut pending = vec![id];
        while let Some(next) = pending.pop() {
            membership.insert(next, index);
            if let Some(file) = files.remove(&next) {
                group.files.push(file);
            }
            if let Some(adjacent) = neighbours.get(&next) {
                for &other in adjacent {
                    if visited.insert(other) {
                        pending.push(other);
                    }
                }
            }
        }
        group.files.sort_by_key(|file| file.item.id);
        groups.push(group);
    }
    let mut unique = HashSet::new();
    for (a, b) in pairs {
        if unique.insert((a.min(b), a.max(b)))
            && let Some(&index) = membership.get(&a)
        {
            groups[index].pairs.push([a, b]);
        }
    }
    Ok(DuplicateFileGroups { groups })
}
