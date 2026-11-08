use apm_core::dto::{CleanupExecuteOptions, CleanupParams, PlanResult};
use apm_core::paths::RelPath;
use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite};

use crate::fsops::{FsOpError, Loc, RmdirOutcome, is_protected_path};
use crate::plans::{ExecContext, ExecutePrep, Plan, PlanError, ResultBuilder, display_path};

pub(super) fn prepare(plan: &Plan, options: &Value) -> Result<ExecutePrep, PlanError> {
    serde_json::from_value::<CleanupExecuteOptions>(options.clone())
        .map_err(|err| PlanError::invalid_field("options", err.to_string()))?;
    let count = plan.stats.get("empty_folders").copied().unwrap_or(0);
    Ok(
        ExecutePrep::new(format!("Deleting {count} empty folders")).summary(
            "Deleted",
            "empty folder",
            "empty folders",
        ),
    )
}

pub(super) async fn execute(
    ctx: ExecContext,
    plan: &Plan,
    options: Value,
) -> Result<PlanResult, PlanError> {
    prepare(plan, &options)?;
    let params: CleanupParams = plan.params()?;
    let volume_id = params
        .scope
        .volume_id
        .ok_or_else(|| PlanError::Internal(anyhow::anyhow!("cleanup plan has no volume")))?;
    let base = RelPath::new(&params.scope.dir).map_err(|err| PlanError::Internal(err.into()))?;
    if ctx.only.is_some() {
        tracing::debug!(plan = plan.id, "cleanup ignores file selection");
    }
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM cleanup_dirs WHERE plan_id = ?")
        .bind(plan.id)
        .fetch_one(ctx.db.read())
        .await?;
    ctx.declare_totals(total as u64, 0);
    let mut result = ResultBuilder::new();
    let mut cursor: Option<(i64, String)> = None;
    loop {
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        let mut qb =
            QueryBuilder::<Sqlite>::new("SELECT depth, path FROM cleanup_dirs WHERE plan_id = ");
        qb.push_bind(plan.id);
        if let Some((depth, path)) = &cursor {
            qb.push(" AND (depth < ")
                .push_bind(*depth)
                .push(" OR (depth = ")
                .push_bind(*depth)
                .push(" AND path > ")
                .push_bind(path.clone())
                .push("))");
        }
        qb.push(" ORDER BY depth DESC, path ASC LIMIT 1000");
        let rows = qb
            .build_query_as::<(i64, String)>()
            .fetch_all(ctx.db.read())
            .await?;
        if rows.is_empty() {
            break;
        }
        for (depth, path) in rows {
            if ctx.cancelled() {
                result.mark_cancelled();
                return Ok(result.finish());
            }
            cursor = Some((depth, path.clone()));
            let rel = RelPath::new(&path).map_err(|err| PlanError::Internal(err.into()))?;
            let shown = display_path(&ctx.volumes, volume_id, &rel);
            if is_protected_path(&rel) {
                result.skip();
                ctx.item_processed();
                continue;
            }
            if rel.as_str() != path
                || rel == base
                || !rel.starts_with(&base)
                || depth < 1
                || rel.depth() as i64 != depth
            {
                result.fail(
                    None,
                    shown,
                    "invalid_plan_path",
                    "The folder is not strictly below the base",
                );
                ctx.item_processed();
                continue;
            }
            match ctx
                .fsops
                .remove_dir_if_removable(Loc { volume_id, rel }, &ctx.batch, &ctx.op)
                .await
            {
                Ok(RmdirOutcome::Removed { .. }) => result.done(0),
                Ok(RmdirOutcome::AlreadyGone) | Err(FsOpError::SourceMissing) => result.skip(),
                Err(FsOpError::NotEmpty) => result.skip(),
                Err(FsOpError::Cancelled) => {
                    result.mark_cancelled();
                    return Ok(result.finish());
                }
                Err(err) => {
                    if matches!(&err, FsOpError::Internal(_)) {
                        tracing::error!(plan = plan.id, error = ?err, "cleanup operation failed");
                    }
                    result.fail_fs(None, shown, &err);
                }
            }
            ctx.item_processed();
        }
    }
    Ok(result.finish())
}
