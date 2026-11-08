//! Restoring trashed items: one `fs_move` task per request, item by item through
//! `FsOps::restore`.

use std::collections::BTreeSet;

use apm_core::dto::{ConflictPolicy, TaskKind, TrashRestoreRequest, TrashTarget};
use apm_core::paths::{TaskId, TrashItemId, VolumeId};
use serde_json::{Value, json};
use sqlx::{FromRow, QueryBuilder, Sqlite};

use super::report::{Failure, Report};
use super::{TrashError, TrashService, to_i64, to_u64};
use crate::error::AppError;
use crate::fsops::{FsOpError, RestoreOutcome};
use crate::tasks::{TaskCtx, TaskError, TaskSpec};

/// Ids per statement when a request's ids are looked up (SQLite allows 32766 binds).
const ID_CHUNK: usize = 5_000;

/// An item a run has to restore.
#[derive(Debug, FromRow)]
struct Pending {
    id: TrashItemId,
    orig_path: String,
    size: i64,
}

impl TrashService {
    /// Queues the restore of the ticked items, or of a whole deletion, as one `fs_move` task and
    /// returns its id. `conflict_policy` of the request, else the `default_conflict_policy`
    /// setting, decides what happens when an original name is taken again.
    ///
    /// Refused up front: invalid fields (422), an unknown volume (404), a deletion that is not in
    /// the trash (404 `batch_not_found`), and a request whose only volume is offline (409
    /// `volume_offline`). Everything else is decided per item while the task runs: an id that is
    /// not in the trash (any more), a volume that is read-only or went offline, a name that cannot
    /// be restored. A failing item never stops the next one.
    pub async fn restore(&self, request: TrashRestoreRequest) -> Result<TaskId, TrashError> {
        request.validate().map_err(TrashError::Validation)?;
        let TrashRestoreRequest {
            target,
            conflict_policy,
        } = request;
        let policy = conflict_policy.unwrap_or_else(|| self.settings.get().default_conflict_policy);

        let (count, target, params) = match target {
            TrashTarget::Ids { ids } => {
                let ids: Vec<TrashItemId> = ids
                    .into_iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                self.refuse_offline(&ids).await?;
                let count = ids.len() as u64;
                let params = json!({
                    "op": "trash_restore",
                    "target": "ids",
                    "count": count,
                    "conflict_policy": policy,
                });
                (count, TrashTarget::Ids { ids }, params)
            }
            TrashTarget::Batch { volume_id, batch } => {
                self.volumes.get(volume_id)?;
                let count = self.count_batch(volume_id, &batch).await?;
                if count == 0 {
                    return Err(AppError::not_found(
                        "batch_not_found",
                        "no such deletion in the trash",
                    )
                    .into());
                }
                let params = json!({
                    "op": "trash_restore",
                    "target": "batch",
                    "volume_id": volume_id,
                    "batch": batch,
                    "count": count,
                    "conflict_policy": policy,
                });
                (count, TrashTarget::Batch { volume_id, batch }, params)
            }
        };

        let service = self.clone();
        let spec = TaskSpec::new(TaskKind::FsMove, restoring_title(count)).with_params(params);
        let id = self
            .tasks
            .submit(spec, move |ctx| async move {
                service.run_restore(ctx, target, policy).await
            })
            .await?;
        tracing::info!(task = id, items = count, "restore of trashed items queued");
        Ok(id)
    }

    /// 409 `volume_offline` when every item of the request lives on one volume and that volume is
    /// offline: nothing could be restored, so the user hears it at once instead of from a failed
    /// task. A request that spans several volumes fails the unreachable ones item by item.
    async fn refuse_offline(&self, ids: &[TrashItemId]) -> Result<(), AppError> {
        let mut volumes = BTreeSet::new();
        for chunk in ids.chunks(ID_CHUNK) {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "SELECT DISTINCT volume_id FROM trash_items WHERE id IN ",
            );
            push_id_list(&mut builder, chunk);
            let found: Vec<VolumeId> = builder
                .build_query_scalar()
                .fetch_all(self.db.read())
                .await?;
            volumes.extend(found);
        }
        if volumes.len() == 1
            && let Some(only) = volumes.first()
        {
            self.volumes.get(*only)?;
        }
        Ok(())
    }

    async fn count_batch(&self, volume_id: VolumeId, batch: &str) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trash_items WHERE volume_id = ? AND batch = ?",
        )
        .bind(volume_id)
        .bind(batch)
        .fetch_one(self.db.read())
        .await?;
        Ok(to_u64(count))
    }

    /// The items a run restores, in id order, and the requested ids that are not in the trash.
    async fn load_pending(
        &self,
        target: &TrashTarget,
    ) -> Result<(Vec<Pending>, Vec<TrashItemId>), AppError> {
        match target {
            TrashTarget::Ids { ids } => {
                let mut found: Vec<Pending> = Vec::with_capacity(ids.len());
                for chunk in ids.chunks(ID_CHUNK) {
                    let mut builder = QueryBuilder::<Sqlite>::new(
                        "SELECT id, orig_path, size FROM trash_items WHERE id IN ",
                    );
                    push_id_list(&mut builder, chunk);
                    builder.push(" ORDER BY id");
                    found.extend(
                        builder
                            .build_query_as::<Pending>()
                            .fetch_all(self.db.read())
                            .await?,
                    );
                }
                let present: BTreeSet<TrashItemId> = found.iter().map(|item| item.id).collect();
                let missing = ids
                    .iter()
                    .copied()
                    .filter(|id| !present.contains(id))
                    .collect();
                Ok((found, missing))
            }
            TrashTarget::Batch { volume_id, batch } => {
                let found = sqlx::query_as::<_, Pending>(
                    "SELECT id, orig_path, size FROM trash_items \
                     WHERE volume_id = ? AND batch = ? ORDER BY id",
                )
                .bind(volume_id)
                .bind(batch)
                .fetch_all(self.db.read())
                .await?;
                Ok((found, Vec::new()))
            }
        }
    }

    async fn run_restore(
        &self,
        ctx: TaskCtx,
        target: TrashTarget,
        policy: ConflictPolicy,
    ) -> Result<Value, TaskError> {
        let (items, missing) = self.load_pending(&target).await?;
        let total = (items.len() + missing.len()) as u64;
        let bytes: i64 = items.iter().map(|item| item.size.max(0)).sum();
        ctx.progress
            .phase(restoring_title(total), Some(to_i64(total)), Some(bytes));

        let cx = self.op_context(&ctx);
        let mut report = Report::new("left in the trash");
        for id in missing {
            report.fail(Failure {
                id,
                path: format!("item {id}"),
                code: "not_in_trash",
                message: "the item is not in the trash (any more)".to_owned(),
            });
            ctx.progress.add(1, 0);
        }
        for item in &items {
            if ctx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            match self.fsops.restore(item.id, policy, &cx).await {
                Ok(RestoreOutcome::Restored { bytes, .. }) => {
                    report.done += 1;
                    report.bytes = report.bytes.saturating_add(bytes);
                }
                Ok(RestoreOutcome::Skipped(reason)) => {
                    report.skipped += 1;
                    tracing::debug!(
                        item = item.id,
                        ?reason,
                        "restore skipped, the item stays in the trash"
                    );
                }
                Err(FsOpError::Cancelled) => return Err(TaskError::Cancelled),
                Err(err) => report.fail(Failure {
                    id: item.id,
                    path: item.orig_path.clone(),
                    code: err.code(),
                    message: err.to_string(),
                }),
            }
            ctx.progress.add(1, item.size.max(0));
        }

        tracing::info!(
            task = ctx.id,
            restored = report.done,
            skipped = report.skipped,
            failed = report.failed(),
            bytes = report.bytes,
            "restore of trashed items finished"
        );
        tracing::debug!(summary = %report.summary("restored"), "restore report");
        ctx.progress.set_title(report.title("Restored", total));
        report.outcome("restored", total, "Restored")
    }
}

fn restoring_title(count: u64) -> String {
    let items = if count == 1 { "item" } else { "items" };
    format!("Restoring {count} {items} from the trash")
}

/// Pushes `(?, ?, ...)` with one bind per id.
fn push_id_list(builder: &mut QueryBuilder<Sqlite>, ids: &[TrashItemId]) {
    builder.push("(");
    let mut list = builder.separated(", ");
    for id in ids {
        list.push_bind(*id);
    }
    list.push_unseparated(")");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_counts_the_items() {
        assert_eq!(restoring_title(1), "Restoring 1 item from the trash");
        assert_eq!(restoring_title(120), "Restoring 120 items from the trash");
    }

    #[test]
    fn an_id_list_binds_every_id() {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT 1 WHERE id IN ");
        push_id_list(&mut builder, &[3, 5, 8]);
        assert_eq!(builder.sql(), "SELECT 1 WHERE id IN (?, ?, ?)");
    }
}
