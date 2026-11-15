//! The ISM trash of bound volumes. Their deletions sit in the sync manager's trash, not in
//! `.opm-trash`: the listing is read from ISM, and restore and purge are ISM operations run as
//! tasks (`fs_move` and `trash_empty`) on the `fs` lane, item by item in one batch, with what ISM
//! settles counted as it arrives. Purge needs the token's `purge` grant and an explicit
//! confirmation; nothing here deletes anything locally.

use std::collections::BTreeMap;

use opm_core::dto::{
    IsmTrashDto, IsmTrashItemDto, IsmTrashPurgeRequest, IsmTrashRestoreRequest, TaskKind,
};
use opm_core::paths::{TaskId, VolumeId};
use serde_json::{Value, json};

use super::report::{Failure, Report};
use super::{TrashError, TrashService, to_i64};
use crate::error::AppError;
use crate::fsops::{Delegated, IsmBinding, Loc};
use crate::ism::wire::TrashEntry;
use crate::tasks::{TaskCtx, TaskError, TaskSpec};

/// Trash ids one request may name.
const MAX_IDS: usize = 10_000;

/// An ISM trash item of a bound volume.
struct Entry {
    item: IsmTrashItemDto,
    loc: Option<Loc>,
}

/// Restore or purge.
#[derive(Clone, Copy)]
enum Action {
    Restore,
    Purge,
}

impl Action {
    fn title(self, count: u64) -> String {
        let items = if count == 1 { "item" } else { "items" };
        match self {
            Self::Restore => format!("Restoring {count} {items} from the sync manager trash"),
            Self::Purge => format!("Deleting {count} {items} from the sync manager trash"),
        }
    }
}

impl TrashService {
    fn ism_service(&self) -> Result<&crate::ism::IsmService, AppError> {
        self.ism
            .as_ref()
            .ok_or_else(|| AppError::conflict("ism_not_connected", "ISM is not connected."))
    }

    fn ism_bound(&self, volume_id: VolumeId) -> Result<IsmBinding, AppError> {
        self.fsops.ism_binding(volume_id).ok_or_else(|| {
            AppError::not_found(
                "ism_not_bound",
                "This volume is not bound to the sync manager",
            )
        })
    }

    /// The ISM trash items of a bound volume, with whether the token may purge them.
    async fn ism_entries(
        &self,
        volume_id: VolumeId,
    ) -> Result<(IsmTrashDto, Vec<Entry>), AppError> {
        let binding = self.ism_bound(volume_id)?;
        let (info, raw) = self.ism_service()?.trash_entries(&binding.root_id).await?;
        let root_name = info
            .roots
            .iter()
            .find(|root| root.root_id == binding.root_id)
            .map_or_else(|| binding.root_id.clone(), |root| root.name.clone());
        let entries: Vec<Entry> = raw
            .into_iter()
            .filter_map(|entry| self.ism_entry(volume_id, &binding, entry))
            .collect();
        let dto = IsmTrashDto {
            volume_id,
            root_id: binding.root_id,
            root_name,
            can_purge: info.actions.iter().any(|action| action == "purge"),
            items: entries.iter().map(|entry| entry.item.clone()).collect(),
        };
        Ok((dto, entries))
    }

    fn ism_entry(
        &self,
        volume_id: VolumeId,
        binding: &IsmBinding,
        entry: TrashEntry,
    ) -> Option<Entry> {
        if entry.location.root_id != binding.root_id {
            return None;
        }
        let loc = self
            .fsops
            .local_of_ism(&entry.location.root_id, &entry.location.path)
            .filter(|loc| loc.volume_id == volume_id && !loc.rel.is_root())?;
        Some(Entry {
            item: IsmTrashItemDto {
                trash_id: entry.trash_id,
                path: loc.rel.as_str().to_owned(),
                is_folder: entry.is_folder,
                size: entry.size,
                trashed_at: entry.trashed_at,
                asset_count: u32::try_from(entry.asset_ids.len()).unwrap_or(u32::MAX),
            },
            loc: Some(loc),
        })
    }

    /// `GET /api/trash/ism?volume_id=`: what ISM keeps in its trash for a bound volume.
    ///
    /// # Errors
    /// 404 `ism_not_bound`, 409 `ism_not_connected` / `ism_unavailable` / `ism_request_failed`.
    pub async fn ism_list(&self, volume_id: VolumeId) -> Result<IsmTrashDto, AppError> {
        self.ism_entries(volume_id).await.map(|(dto, _)| dto)
    }

    /// Queues the restore of ISM trash items of a bound volume as one `fs_move` task.
    ///
    /// # Errors
    /// 422 for no or too many ids, 404 `ism_not_bound`, 403 `forbidden` without the `trash`
    /// grant, 409 when ISM cannot be reached.
    pub async fn ism_restore(&self, request: IsmTrashRestoreRequest) -> Result<TaskId, TrashError> {
        self.ism_submit(Action::Restore, request.volume_id, request.trash_ids)
            .await
    }

    /// Queues the permanent deletion of ISM trash items as one `trash_empty` task. Only with
    /// `confirm: true` and a token with the `purge` grant.
    ///
    /// # Errors
    /// 400 `confirmation_required`, 403 `forbidden`, and those of [`Self::ism_restore`].
    pub async fn ism_purge(&self, request: IsmTrashPurgeRequest) -> Result<TaskId, TrashError> {
        if !request.confirm {
            return Err(AppError::bad_request(
                "confirmation_required",
                "Deleting for good must be confirmed",
            )
            .into());
        }
        self.ism_submit(Action::Purge, request.volume_id, request.trash_ids)
            .await
    }

    async fn ism_submit(
        &self,
        action: Action,
        volume_id: VolumeId,
        trash_ids: Vec<String>,
    ) -> Result<TaskId, TrashError> {
        let mut ids: Vec<String> = trash_ids
            .into_iter()
            .filter(|id| !id.trim().is_empty())
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() || ids.len() > MAX_IDS {
            return Err(TrashError::Validation(vec![opm_core::dto::FieldError {
                field: "trash_ids".into(),
                message: format!("between 1 and {MAX_IDS} trash ids"),
            }]));
        }
        let (dto, _) = self.ism_entries(volume_id).await?;
        let (grant, kind, op) = match action {
            Action::Restore => ("trash", TaskKind::FsMove, "ism_trash_restore"),
            Action::Purge => ("purge", TaskKind::TrashEmpty, "ism_trash_purge"),
        };
        if matches!(action, Action::Purge) && !dto.can_purge {
            return Err(
                AppError::forbidden("forbidden", "The ISM token does not allow purge").into(),
            );
        }
        let count = ids.len() as u64;
        let params = json!({"op": op, "volume_id": volume_id, "count": count, "grant": grant});
        let service = self.clone();
        let spec = TaskSpec::new(kind, action.title(count)).with_params(params);
        let id = self
            .tasks
            .submit(spec, move |ctx| async move {
                service.run_ism(ctx, action, volume_id, ids).await
            })
            .await?;
        tracing::info!(task = id, items = count, "sync manager trash task queued");
        Ok(id)
    }

    async fn run_ism(
        &self,
        ctx: TaskCtx,
        action: Action,
        volume_id: VolumeId,
        ids: Vec<String>,
    ) -> Result<Value, TaskError> {
        let total = ids.len() as u64;
        let (_, entries) = self.ism_entries(volume_id).await?;
        let mut by_id: BTreeMap<String, Entry> = entries
            .into_iter()
            .map(|entry| (entry.item.trash_id.clone(), entry))
            .collect();
        let binding = self.ism_bound(volume_id)?;
        let mut report = Report::new("left in the trash");
        let mut picked = Vec::new();
        for id in ids {
            match by_id.remove(&id) {
                Some(entry) => picked.push(entry),
                None => {
                    report.fail(Failure {
                        id: 0,
                        path: format!("item {id}"),
                        code: "not_in_trash".to_owned(),
                        message: "the item is not in the sync manager trash (any more)".to_owned(),
                    });
                    ctx.progress.add(1, 0);
                }
            }
        }
        let bytes: u64 = picked.iter().map(|entry| entry.item.size).sum();
        ctx.progress.phase(
            action.title(total),
            Some(to_i64(total)),
            Some(to_i64(bytes)),
        );
        let actions: Vec<Delegated> = picked
            .iter()
            .map(|entry| match action {
                Action::Restore => Delegated::Restore {
                    trash_id: entry.item.trash_id.clone(),
                    root_id: binding.root_id.clone(),
                    loc: entry.loc.clone(),
                    is_dir: entry.item.is_folder,
                },
                Action::Purge => Delegated::Purge {
                    trash_id: entry.item.trash_id.clone(),
                    root_id: binding.root_id.clone(),
                },
            })
            .collect();
        if !actions.is_empty() {
            let cx = self.op_context(&ctx);
            let (label, key) = match action {
                Action::Restore => (
                    "Restore from trash",
                    format!("opm:trash-restore:{}", ctx.id),
                ),
                Action::Purge => ("Delete from trash", format!("opm:trash-purge:{}", ctx.id)),
            };
            let progress = ctx.progress.clone();
            self.fsops
                .delegate(label, &key, &actions, &cx, &mut |index, outcome| {
                    let Some(entry) = picked.get(index) else {
                        return;
                    };
                    match outcome {
                        Ok(_) => {
                            report.done += 1;
                            report.bytes = report.bytes.saturating_add(entry.item.size);
                        }
                        Err(err) => report.fail(Failure {
                            id: 0,
                            path: entry.item.path.clone(),
                            code: err.code().to_owned(),
                            message: err.to_string(),
                        }),
                    }
                    progress.add(1, to_i64(entry.item.size));
                })
                .await;
        }
        if ctx.cancel.is_cancelled() && report.done < total {
            return Err(TaskError::Cancelled);
        }
        let (done_key, verb) = match action {
            Action::Restore => ("restored", "Restored"),
            Action::Purge => ("deleted", "Deleted"),
        };
        tracing::info!(
            task = ctx.id,
            done = report.done,
            failed = report.failed(),
            "sync manager trash task finished"
        );
        ctx.progress.set_title(report.title(verb, total));
        report.outcome(done_key, total, verb)
    }
}
