use std::collections::HashMap;

use futures_util::future::BoxFuture;
use opm_core::dto::{PlanResult, RenameParams};
use opm_core::paths::{FileId, RelPath};

use super::super::moves::{Item, Revalidate, Source, Stored, Wanted};
use super::names::{Names, Outcome};
use crate::fsops::Loc;
use crate::plans::{ExecContext, Plan, PlanError};

struct RenameHook {
    names: Names,
    uses_count: bool,
}

impl Revalidate for RenameHook {
    fn wanted<'a>(
        &'a self,
        ctx: &'a ExecContext,
        item: &'a Item,
        row: &'a Source,
        _transferred: &'a HashMap<FileId, Loc>,
    ) -> BoxFuture<'a, Result<Result<Wanted, &'static str>, PlanError>> {
        Box::pin(async move {
            let seq = if self.uses_count {
                let stored: Option<Option<i64>> = sqlx::query_scalar(
                    "SELECT seq FROM move_items WHERE plan_id = ? AND file_id = ?",
                )
                .bind(ctx.plan_id)
                .bind(item.file_id)
                .fetch_optional(ctx.db.read())
                .await?;
                let Some(Some(value)) = stored else {
                    return Err(PlanError::Internal(anyhow::anyhow!(
                        "rename item is missing its planned counter"
                    )));
                };
                Some(u64::try_from(value).map_err(anyhow::Error::from)?)
            } else {
                None
            };
            match self.names.one(row, seq) {
                Outcome::Pending => Ok(Err("metadata_pending")),
                Outcome::Failed(code) => Ok(Err(code)),
                Outcome::Unmatched => Ok(Err("stale_plan")),
                Outcome::Rename { name, .. } => Ok(Ok(Wanted {
                    volume_id: row.volume_id,
                    dir: RelPath::new(&row.dir_path).map_err(anyhow::Error::from)?,
                    name,
                })),
            }
        })
    }
}

pub(super) async fn execute(ctx: ExecContext, plan: &Plan) -> Result<PlanResult, PlanError> {
    let params: RenameParams = plan.params()?;
    let compiled = opm_core::rename::compile(&params.rules).map_err(|issues| {
        PlanError::Internal(anyhow::anyhow!(
            "stored rename rules no longer compile: {}",
            issues
                .first()
                .map(|issue| issue.message.as_str())
                .unwrap_or("unknown validation error")
        ))
    })?;
    let uses_count = compiled.uses_count();
    let names = Names::new(compiled, &params.rules.pattern, &ctx.settings)?;
    let stored = Stored::load(&ctx).await?;
    super::super::moves::run(ctx, stored, &RenameHook { names, uses_count }).await
}
