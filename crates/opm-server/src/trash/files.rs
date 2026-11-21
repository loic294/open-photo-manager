//! Moving a server-side selection of indexed files to the trash.

use opm_core::dto::TaskKind;
use opm_core::paths::{FileId, RelPath, SelectionId, TaskId, VolumeId};
use serde_json::{Value, json};
use sqlx::{FromRow, QueryBuilder, Sqlite};

use super::report::{Failure, Report};
use super::{TrashError, TrashService, to_i64, to_u64};
use crate::error::AppError;
use crate::fsops::{Expect, FsOpError, Loc, OpContext, TrashRequest};
use crate::tasks::{Checkpoint, TaskCtx, TaskError, TaskSpec};

#[derive(Debug, Clone, FromRow, serde::Serialize, serde::Deserialize)]
struct Pending {
    id: FileId,
    volume_id: VolumeId,
    dir_path: String,
    name: String,
    size: i64,
    mtime: i64,
}

impl Pending {
    fn loc(&self) -> Result<Loc, AppError> {
        let rel = RelPath::new(&self.dir_path)
            .and_then(|dir| dir.join(&self.name))
            .map_err(AppError::internal)?;
        Ok(Loc::new(self.volume_id, rel))
    }

    fn expect(&self) -> Expect {
        Expect {
            size: to_u64(self.size),
            mtime: self.mtime,
        }
    }
}

impl TrashService {
    /// Queues a resumable task for the current membership and identity of a selection.
    pub async fn trash_files(
        &self,
        selection_id: SelectionId,
        file_ids: Vec<FileId>,
    ) -> Result<TaskId, TrashError> {
        self.trash_files_with_mode(selection_id, file_ids, false)
            .await
    }

    pub async fn trash_files_with_mode(
        &self,
        selection_id: SelectionId,
        file_ids: Vec<FileId>,
        local_only: bool,
    ) -> Result<TaskId, TrashError> {
        if file_ids.is_empty() {
            return Err(
                AppError::conflict("selection_empty", "the selection contains no files").into(),
            );
        }
        let count = file_ids.len();
        let noun = if count == 1 { "file" } else { "files" };
        let saved_ids = file_ids.clone();
        let mut params = json!({
            "op": "trash_files",
            "selection_id": selection_id,
            "file_ids": saved_ids,
        });
        if local_only {
            params["local_only"] = json!(true);
        }
        let service = self.clone();
        let spec = TaskSpec::new(
            TaskKind::FsTrash,
            format!("Moving {count} selected {noun} to the trash"),
        )
        .resumable()
        .with_params(params);
        let task = self
            .tasks
            .submit(spec, move |ctx| async move {
                service.run_trash_files(ctx, selection_id, file_ids).await
            })
            .await?;
        tracing::info!(
            task,
            selection_id,
            files = count,
            "selected files queued for trash"
        );
        Ok(task)
    }

    pub(super) async fn run_trash_files(
        &self,
        ctx: TaskCtx,
        selection_id: SelectionId,
        file_ids: Vec<FileId>,
    ) -> Result<Value, TaskError> {
        let checkpoint = Checkpoint::new(&self.db, ctx.id).await?;
        let items = match checkpoint.load::<Vec<Pending>>("trash-files-input").await? {
            Some(items) => items,
            None => {
                let items = self.pending_files(selection_id, &file_ids).await?;
                checkpoint.save("trash-files-input", &items).await?;
                items
            }
        };
        let total = items.len() as u64;
        let noun = if total == 1 { "file" } else { "files" };
        let bytes: i64 = items.iter().map(|item| item.size.max(0)).sum();
        ctx.progress.phase(
            format!("Moving {total} selected {noun} to the trash"),
            Some(to_i64(total)),
            Some(bytes),
        );

        let cx = match checkpoint.load::<String>("trash-files-batch").await? {
            Some(batch) => self.op_context(&ctx).with_batch(batch),
            None => {
                let cx = self.op_context(&ctx);
                checkpoint
                    .save("trash-files-batch", &cx.batch().name())
                    .await?;
                cx
            }
        };
        let (next, mut report): (usize, Report) = checkpoint
            .load("trash-files-progress")
            .await?
            .unwrap_or_else(|| (0, Report::new("already gone")));
        ctx.progress.add(next as i64, report.bytes as i64);

        let exclusions =
            opm_core::exclusions::Exclusions::new(&self.settings.get().soft_excluded_folders);
        for (index, item) in items.iter().enumerate().skip(next) {
            let _mutation = self
                .fsops
                .mutation_guard(&ctx.cancel)
                .await
                .ok_or(TaskError::Cancelled)?;
            let loc = item.loc()?;
            let sidecar_key = format!("trash-files-sidecars:{}", item.id);
            let companions = match checkpoint.load::<Vec<Pending>>(&sidecar_key).await? {
                Some(companions) => companions,
                None => {
                    let companions = self.sidecars(item.id).await?;
                    checkpoint.save(&sidecar_key, &companions).await?;
                    companions
                }
            };
            if exclusions.contains(item.volume_id, &loc.rel) {
                report.skipped += 1;
            } else if let Some(previous) = self
                .fsops
                .completed_bytes(ctx.id, "trash", &loc)
                .await
                .map_err(AppError::from)?
            {
                report.done += 1;
                report.bytes = report.bytes.saturating_add(previous);
                self.trash_orphaned_sidecars(ctx.id, companions, &cx, &mut report)
                    .await?;
            } else {
                let still_selected: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM selection_items WHERE selection_id = ? AND file_id = ?)",
                )
                .bind(selection_id)
                .bind(item.id)
                .fetch_one(self.db.read())
                .await?;
                if !still_selected {
                    report.skipped += 1;
                } else {
                    self.trash_owner(ctx.id, item, loc, companions, &cx, &mut report)
                        .await?;
                }
            }
            ctx.progress.add(1, item.size.max(0));
            checkpoint
                .save("trash-files-progress", &(index + 1, &report))
                .await?;
        }

        tracing::info!(
            task = ctx.id,
            trashed = report.done,
            skipped = report.skipped,
            failed = report.failed(),
            bytes = report.bytes,
            "moving selected files to the trash finished"
        );
        ctx.progress.set_title(report.title("Trashed", total));
        report.outcome("trashed", total, "Trashed")
    }

    async fn pending_files(
        &self,
        selection_id: SelectionId,
        file_ids: &[FileId],
    ) -> Result<Vec<Pending>, sqlx::Error> {
        let mut found = Vec::with_capacity(file_ids.len());
        for chunk in file_ids.chunks(5_000) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT f.id, f.volume_id, f.dir_path, f.name, f.size, f.mtime \
                 FROM selection_items si JOIN files f ON f.id = si.file_id \
                 WHERE si.selection_id = ",
            );
            query.push_bind(selection_id).push(" AND f.id IN (");
            {
                let mut separated = query.separated(", ");
                for id in chunk {
                    separated.push_bind(id);
                }
            }
            query.push(") ORDER BY f.id");
            found.extend(
                query
                    .build_query_as::<Pending>()
                    .fetch_all(self.db.read())
                    .await?,
            );
        }
        Ok(found)
    }

    async fn sidecars(&self, owner: FileId) -> Result<Vec<Pending>, sqlx::Error> {
        sqlx::query_as::<_, Pending>(
            "SELECT f.id, f.volume_id, f.dir_path, f.name, f.size, f.mtime \
             FROM file_sidecars fs JOIN files f ON f.id = fs.sidecar_id \
             WHERE fs.file_id = ? ORDER BY f.id",
        )
        .bind(owner)
        .fetch_all(self.db.read())
        .await
    }

    async fn trash_owner(
        &self,
        task_id: TaskId,
        item: &Pending,
        loc: Loc,
        companions: Vec<Pending>,
        cx: &OpContext,
        report: &mut Report,
    ) -> Result<(), TaskError> {
        match self
            .fsops
            .trash_file(
                TrashRequest {
                    file_id: Some(item.id),
                    loc: loc.clone(),
                    expect: Some(item.expect()),
                    batch: cx.batch().clone(),
                    reason: "user".to_owned(),
                },
                cx,
            )
            .await
        {
            Ok(outcome) => {
                report.done += 1;
                report.bytes = report.bytes.saturating_add(outcome.bytes);
                self.trash_orphaned_sidecars(task_id, companions, cx, report)
                    .await?;
            }
            Err(FsOpError::SourceMissing | FsOpError::SourceChanged) => report.skipped += 1,
            Err(FsOpError::Cancelled) => return Err(TaskError::Cancelled),
            Err(err) => report.fail(Failure {
                id: item.id,
                path: self.display_path(&loc),
                code: err.code().to_owned(),
                message: err.to_string(),
            }),
        }
        Ok(())
    }

    async fn trash_orphaned_sidecars(
        &self,
        task_id: TaskId,
        companions: Vec<Pending>,
        cx: &OpContext,
        report: &mut Report,
    ) -> Result<(), TaskError> {
        for item in companions {
            if cx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            let loc = item.loc()?;
            let previous = self
                .fsops
                .completed_bytes(task_id, "trash", &loc)
                .await
                .map_err(AppError::from)?;
            if let Some(bytes) = previous {
                report.bytes = report.bytes.saturating_add(bytes);
                continue;
            }
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM file_sidecars WHERE sidecar_id = ?)",
            )
            .bind(item.id)
            .fetch_one(self.db.read())
            .await?;
            if owned {
                continue;
            }
            match self
                .fsops
                .trash_file(
                    TrashRequest {
                        file_id: Some(item.id),
                        loc: loc.clone(),
                        expect: Some(item.expect()),
                        batch: cx.batch().clone(),
                        reason: "user_sidecar".to_owned(),
                    },
                    cx,
                )
                .await
            {
                Ok(outcome) => report.bytes = report.bytes.saturating_add(outcome.bytes),
                Err(FsOpError::SourceMissing | FsOpError::SourceChanged) => {}
                Err(FsOpError::Cancelled) => return Err(TaskError::Cancelled),
                Err(err) => report.fail(Failure {
                    id: item.id,
                    path: self.display_path(&loc),
                    code: err.code().to_owned(),
                    message: err.to_string(),
                }),
            }
        }
        Ok(())
    }

    fn display_path(&self, loc: &Loc) -> String {
        let name = self
            .volumes
            .volume(loc.volume_id)
            .map(|volume| volume.name)
            .unwrap_or_else(|| format!("volume {}", loc.volume_id));
        format!("{name}/{}", loc.rel.as_str())
    }
}
