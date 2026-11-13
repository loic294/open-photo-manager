//! Emptying the trash for good: the one place the application deletes data (A15), as a
//! `trash_empty` task on the `fs` lane.
//!
//! The user's "Empty trash" and the retention sweeper share one runner. Only the selection of the
//! items differs ([`PurgeSelector`]), and it is read when the task STARTS: a task that waited in
//! the queue deletes what is due then, never a stale list.

use opm_core::dto::{TaskKind, TrashEmptyRequest};
use opm_core::paths::{TaskId, TrashItemId, VolumeId};
use serde_json::{Value, json};
use sqlx::{FromRow, QueryBuilder, Sqlite};

use super::report::{Failure, Report};
use super::{TrashError, TrashService, to_i64, to_u64};
use crate::error::AppError;
use crate::fsops::FsOpError;
use crate::tasks::{TaskCtx, TaskError, TaskSpec};

/// Which items a purge run deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PurgeSelector {
    /// What the user saw when they confirmed: the items up to `max_id`, on one volume or on all.
    /// Ids are never reused, so an item trashed afterwards is never covered.
    UpToId {
        max_id: TrashItemId,
        volume_id: Option<VolumeId>,
    },
    /// What the retention policy ages out: everything trashed before `cutoff` (epoch seconds).
    OlderThan { cutoff: i64 },
}

impl PurgeSelector {
    fn push_where(self, builder: &mut QueryBuilder<Sqlite>) {
        match self {
            Self::UpToId { max_id, volume_id } => {
                builder.push(" WHERE id <= ").push_bind(max_id);
                if let Some(volume_id) = volume_id {
                    builder.push(" AND volume_id = ").push_bind(volume_id);
                }
            }
            Self::OlderThan { cutoff } => {
                builder.push(" WHERE trashed_at < ").push_bind(cutoff);
            }
        }
    }

    /// The order the run deletes in: oldest first, which the index of `trashed_at` serves.
    fn push_order(self, builder: &mut QueryBuilder<Sqlite>) {
        match self {
            Self::UpToId { .. } => builder.push(" ORDER BY id"),
            Self::OlderThan { .. } => builder.push(" ORDER BY trashed_at, id"),
        };
    }
}

/// An item a run deletes: only what the progress bar needs, so a million items stay at 16 MB.
#[derive(Debug, FromRow, serde::Serialize, serde::Deserialize)]
struct Doomed {
    id: TrashItemId,
    size: i64,
    volume_id: VolumeId,
    trash_path: String,
}

/// The statement that selects what a run deletes, in the order it deletes it.
pub(super) fn select_query(selector: PurgeSelector) -> QueryBuilder<Sqlite> {
    let mut builder =
        QueryBuilder::<Sqlite>::new("SELECT id, size, volume_id, trash_path FROM trash_items");
    selector.push_where(&mut builder);
    selector.push_order(&mut builder);
    builder
}

impl TrashService {
    /// Queues the permanent deletion of the items up to `request.up_to_id` (the `max_id` of the
    /// summary the user confirmed), on one volume or on all, and returns the task id. What was
    /// trashed after that summary survives.
    ///
    /// Refused: invalid fields (422), an unknown volume (404), an offline volume (409
    /// `volume_offline`) and a selection that matches nothing (409 `trash_already_empty`). A
    /// failure of one item (a read-only volume, a symlink in the way) never stops the next; it
    /// ends the task `failed` with the first failures named.
    pub async fn empty(&self, request: TrashEmptyRequest) -> Result<TaskId, TrashError> {
        request.validate().map_err(TrashError::Validation)?;
        if let Some(volume_id) = request.volume_id {
            self.volumes.get(volume_id)?;
        }
        let selector = PurgeSelector::UpToId {
            max_id: request.up_to_id,
            volume_id: request.volume_id,
        };
        let items = self.count(selector).await?;
        if items == 0 {
            return Err(AppError::conflict(
                "trash_already_empty",
                "no item of the trash matches: it was already emptied",
            )
            .into());
        }
        let params = json!({
            "op": "trash_empty",
            "volume_id": request.volume_id,
            "up_to_id": request.up_to_id,
            "items": items,
        });
        let id = self
            .submit_purge(selector, "Emptying the trash".to_owned(), params)
            .await?;
        tracing::info!(task = id, items, "emptying the trash queued");
        Ok(id)
    }

    /// Submits a purge run. The title is what the task shows while it starts and runs.
    pub(super) async fn submit_purge(
        &self,
        selector: PurgeSelector,
        title: String,
        mut params: Value,
    ) -> Result<TaskId, AppError> {
        let service = self.clone();
        params["selector"] = serde_json::to_value(selector).map_err(AppError::internal)?;
        params["title"] = Value::String(title.clone());
        let spec = TaskSpec::new(TaskKind::TrashEmpty, title.clone())
            .resumable()
            .with_params(params);
        self.tasks
            .submit(spec, move |ctx| async move {
                service.run_purge(ctx, selector, title).await
            })
            .await
    }

    pub(super) async fn count(&self, selector: PurgeSelector) -> Result<u64, AppError> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM trash_items");
        selector.push_where(&mut builder);
        let count: i64 = builder
            .build_query_scalar()
            .fetch_one(self.db.read())
            .await?;
        Ok(to_u64(count))
    }

    pub(super) async fn run_purge(
        &self,
        ctx: TaskCtx,
        selector: PurgeSelector,
        title: String,
    ) -> Result<Value, TaskError> {
        let checkpoint = crate::tasks::Checkpoint::new(&self.db, ctx.id).await?;
        let items = match checkpoint.load::<Vec<Doomed>>("purge-input").await? {
            Some(items) => items,
            None => {
                let items: Vec<Doomed> = select_query(selector)
                    .build_query_as()
                    .fetch_all(self.db.read())
                    .await
                    .map_err(AppError::from)?;
                checkpoint.save("purge-input", &items).await?;
                items
            }
        };
        let total = items.len() as u64;
        let bytes: i64 = items.iter().map(|item| item.size.max(0)).sum();
        ctx.progress.phase(title, Some(to_i64(total)), Some(bytes));

        let cx = self.op_context(&ctx);
        let (next, mut report): (usize, Report) =
            checkpoint.load("purge-progress").await?.unwrap_or_default();
        ctx.progress.add(next as i64, report.bytes as i64);
        for (index, item) in items.iter().enumerate().skip(next) {
            if ctx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            let loc = crate::fsops::Loc::new(
                item.volume_id,
                opm_core::paths::RelPath::new(&item.trash_path).map_err(AppError::internal)?,
            );
            let previous = self
                .fsops
                .completed_bytes(ctx.id, "delete", &loc)
                .await
                .map_err(AppError::from)?;
            let outcome = if let Some(bytes) = previous {
                Ok(crate::fsops::PurgeOutcome { bytes, files: 0 })
            } else {
                self.fsops.purge(item.id, &cx).await
            };
            match outcome {
                Ok(outcome) => {
                    report.done += 1;
                    report.bytes = report.bytes.saturating_add(outcome.bytes);
                }
                // The row vanished since the selection (restored, or removed by a sync): nothing
                // is left to delete.
                Err(FsOpError::SourceMissing) => report.skipped += 1,
                Err(FsOpError::Cancelled) => return Err(TaskError::Cancelled),
                Err(err) => {
                    let path = self.orig_path_of(item.id).await;
                    report.fail(Failure {
                        id: item.id,
                        path,
                        code: err.code().to_owned(),
                        message: err.to_string(),
                    });
                }
            }
            ctx.progress.add(1, item.size.max(0));
            checkpoint
                .save("purge-progress", &(index + 1, &report))
                .await?;
        }

        tracing::info!(
            task = ctx.id,
            deleted = report.done,
            skipped = report.skipped,
            failed = report.failed(),
            bytes = report.bytes,
            "deleting trashed items finished"
        );
        tracing::debug!(summary = %report.summary("deleted"), "purge report");
        ctx.progress.set_title(report.title("Deleted", total));
        report.outcome("deleted", total, "Deleted")
    }

    /// Where a failed item came from, for the error text; the id when its row is gone.
    async fn orig_path_of(&self, id: TrashItemId) -> String {
        let found: Result<Option<String>, sqlx::Error> =
            sqlx::query_scalar("SELECT orig_path FROM trash_items WHERE id = ?")
                .bind(id)
                .fetch_optional(self.db.read())
                .await;
        found.ok().flatten().unwrap_or_else(|| format!("item {id}"))
    }
}
