use std::collections::HashMap;

use opm_core::dto::{Overlay, PlanSide};
use opm_core::paths::{FileId, PlanId};
use sqlx::{QueryBuilder, Sqlite};

use super::side_name;
use crate::db::{Db, subtree::Subtree};
use crate::plans::PlanError;
use crate::plans::overlay::{FolderOverlay, FolderPlanInfo, GhostView};
use crate::query::push_child_segment;

pub(super) async fn flags(
    db: &Db,
    plan: PlanId,
    side: PlanSide,
    ids: &[FileId],
) -> Result<HashMap<FileId, Overlay>, PlanError> {
    let mut flags = HashMap::new();
    for chunk in ids.chunks(30_000) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT m.file_id FROM duplicate_matches m JOIN files f ON f.id = m.file_id JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = m.plan_id AND i.file_id = m.file_id AND i.live = 1 WHERE m.live = 1 AND m.plan_id = ",
        );
        qb.push_bind(plan)
            .push(" AND m.side = ")
            .push_bind(side_name(side))
            .push(" AND m.file_id IN (");
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(*id);
        }
        separated.push_unseparated(")");
        for id in qb
            .build_query_scalar::<FileId>()
            .fetch_all(db.read())
            .await?
        {
            flags.insert(
                id,
                Overlay {
                    matched: true,
                    ..Overlay::default()
                },
            );
        }
    }
    Ok(flags)
}

pub(super) async fn ids(db: &Db, plan: PlanId, side: PlanSide) -> Result<Vec<FileId>, PlanError> {
    Ok(sqlx::query_scalar("SELECT m.file_id FROM duplicate_matches m JOIN files f ON f.id = m.file_id JOIN plan_file_tracking t ON t.plan_id = m.plan_id JOIN plan_file_identities i ON i.plan_id = m.plan_id AND i.file_id = m.file_id AND i.live = 1 WHERE m.live = 1 AND m.plan_id = ? AND m.side = ? ORDER BY m.file_id")
        .bind(plan).bind(side_name(side)).fetch_all(db.read()).await?)
}

pub(super) async fn folders(
    db: &Db,
    plan: PlanId,
    side: PlanSide,
    view: &GhostView,
    children: &[String],
) -> Result<FolderOverlay, PlanError> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "WITH per_dir AS (SELECT m.dir_path, COUNT(*) AS cnt FROM duplicate_matches m INDEXED BY duplicate_matches_location WHERE m.live = 1 AND m.plan_id = ",
    );
    qb.push_bind(plan)
        .push(" AND m.side = ")
        .push_bind(side_name(side))
        .push(" AND m.volume_id = ")
        .push_bind(view.volume_id)
        .push(" AND ");
    Subtree::new(&view.dir).push_predicate(&mut qb, "m.dir_path");
    qb.push(" AND m.dir_path <> ")
        .push_bind(view.dir.as_str().to_owned())
        .push(" GROUP BY m.dir_path) SELECT ");
    push_child_segment(&mut qb, "dir_path", view.dir.as_str());
    qb.push(" AS child, SUM(cnt) FROM per_dir WHERE EXISTS(SELECT 1 FROM plan_file_tracking WHERE plan_id = ")
        .push_bind(plan)
        .push(") GROUP BY child");
    let counts: HashMap<String, i64> = qb
        .build_query_as::<(String, i64)>()
        .fetch_all(db.read())
        .await?
        .into_iter()
        .collect();
    Ok(FolderOverlay {
        per_child: children
            .iter()
            .map(|child| {
                (
                    child.clone(),
                    FolderPlanInfo {
                        matches: Some(*counts.get(child).unwrap_or(&0)),
                        ..FolderPlanInfo::default()
                    },
                )
            })
            .collect(),
        ..FolderOverlay::default()
    })
}
