use std::collections::{HashMap, HashSet};

use opm_core::dto::{CleanupParams, Overlay, PlanSide};
use sqlx::{QueryBuilder, Sqlite};

use crate::db::{Db, subtree::Subtree};
use crate::plans::overlay::{FolderOverlay, FolderPlanInfo, GhostView};
use crate::plans::{Plan, PlanError};

pub(super) async fn folders(
    db: &Db,
    plan: &Plan,
    side: PlanSide,
    view: &GhostView,
    children: &[String],
) -> Result<FolderOverlay, PlanError> {
    let params: CleanupParams = plan.params()?;
    if side != PlanSide::Comparison || params.scope.volume_id != Some(view.volume_id) {
        return Ok(FolderOverlay::default());
    }
    let mut flagged = HashMap::new();
    for chunk in children.chunks(500) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT path FROM cleanup_dirs WHERE plan_id = ");
        qb.push_bind(plan.id).push(" AND path IN (");
        let mut separated = qb.separated(", ");
        for name in chunk {
            let path = view
                .dir
                .join(name)
                .map_err(|err| PlanError::Internal(err.into()))?;
            separated.push_bind(path.as_str().to_owned());
        }
        separated.push_unseparated(")");
        for path in qb
            .build_query_scalar::<String>()
            .fetch_all(db.read())
            .await?
        {
            let rel = opm_core::paths::RelPath::new(&path)
                .map_err(|err| PlanError::Internal(err.into()))?;
            if let Some(name) = rel.file_name() {
                flagged.insert(
                    name.to_owned(),
                    FolderPlanInfo {
                        matches: Some(0),
                        overlay: Overlay {
                            will_delete: true,
                            ..Overlay::default()
                        },
                        ..FolderPlanInfo::default()
                    },
                );
            }
        }
    }
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT (");
    crate::query::push_child_segment(&mut qb, "path", view.dir.as_str());
    qb.push(") COLLATE BINARY AS child, count(*) FROM cleanup_dirs WHERE plan_id = ");
    qb.push_bind(plan.id).push(" AND ");
    Subtree::new(&view.dir).push_predicate(&mut qb, "path");
    qb.push(" AND path <> ")
        .push_bind(view.dir.as_str().to_owned())
        .push(" GROUP BY child COLLATE BINARY");
    let visible_children: HashSet<_> = children.iter().map(String::as_str).collect();
    for (name, count) in qb
        .build_query_as::<(String, i64)>()
        .fetch_all(db.read())
        .await?
    {
        if visible_children.contains(name.as_str()) {
            flagged
                .entry(name)
                .or_insert_with(FolderPlanInfo::default)
                .matches = Some(count);
        }
    }
    Ok(FolderOverlay {
        per_child: flagged,
        ..FolderOverlay::default()
    })
}
