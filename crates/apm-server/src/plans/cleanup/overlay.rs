use std::collections::HashMap;

use apm_core::dto::{CleanupParams, Overlay, PlanSide};
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
            let rel = apm_core::paths::RelPath::new(&path)
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
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT (CASE WHEN instr(substr(path, ");
    let offset = if view.dir.is_root() {
        1
    } else {
        view.dir.as_str().chars().count() as i64 + 2
    };
    qb.push_bind(offset).push("), '/') = 0 THEN substr(path, ").push_bind(offset)
        .push(") ELSE substr(substr(path, ").push_bind(offset)
        .push("), 1, instr(substr(path, ").push_bind(offset)
        .push("), '/') - 1) END) COLLATE BINARY AS child, count(*) FROM cleanup_dirs WHERE plan_id = ");
    qb.push_bind(plan.id).push(" AND ");
    Subtree::new(&view.dir).push_predicate(&mut qb, "path");
    qb.push(" AND path <> ")
        .push_bind(view.dir.as_str().to_owned())
        .push(" GROUP BY child COLLATE BINARY");
    for (name, count) in qb
        .build_query_as::<(String, i64)>()
        .fetch_all(db.read())
        .await?
    {
        if let Some(info) = flagged.get_mut(&name) {
            info.matches = Some(count);
        }
    }
    Ok(FolderOverlay {
        per_child: flagged,
        ..FolderOverlay::default()
    })
}
