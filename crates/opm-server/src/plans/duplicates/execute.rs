use std::collections::HashSet;

use opm_core::dto::{
    DuplicateAction, DuplicateFileExecuteOptions, DuplicateParams, MatchMode, PlanResult, PlanSide,
};
use opm_core::hash::{Sha1Digest, from_slice};
use opm_core::matching::{MatchFacts, MatchOptions, is_match, participates};
use opm_core::paths::{FileId, RelPath};
use serde_json::Value;
use sqlx::FromRow;

use super::{options, side_name};
use crate::db::{Db, models::file_kind_from_db};
use crate::fsops::{Expect, FsOpError, Loc, TrashRequest};
use crate::plans::{ExecContext, Plan, PlanError, ResultBuilder, display_path};

#[derive(FromRow)]
struct Live {
    id: FileId,
    volume_id: i64,
    dir_path: String,
    name: String,
    kind: String,
    hash: Option<Vec<u8>>,
    capture_ts: Option<i64>,
    size: i64,
    mtime: i64,
}

impl Live {
    fn loc(&self) -> Result<Loc, PlanError> {
        let rel = RelPath::new(&self.dir_path)
            .and_then(|dir| dir.join(&self.name))
            .map_err(|err| PlanError::Internal(err.into()))?;
        Ok(Loc::new(self.volume_id, rel))
    }
    fn expect(&self) -> Result<Expect, PlanError> {
        Ok(Expect {
            size: u64::try_from(self.size).map_err(|err| PlanError::Internal(err.into()))?,
            mtime: self.mtime,
        })
    }
    fn facts(&self) -> Result<MatchFacts<'_>, PlanError> {
        let hash: Option<Sha1Digest> = self
            .hash
            .as_deref()
            .map(|bytes| {
                from_slice(bytes).ok_or_else(|| {
                    PlanError::Internal(anyhow::anyhow!(
                        "invalid indexed hash for file {}",
                        self.id
                    ))
                })
            })
            .transpose()?;
        Ok(MatchFacts {
            id: self.id,
            name: &self.name,
            hash,
            capture_ts: self.capture_ts,
            size: self.size,
        })
    }
    fn media(&self) -> Result<bool, PlanError> {
        Ok(participates(
            file_kind_from_db(&self.kind).map_err(|err| PlanError::Internal(err.into()))?,
        ))
    }
}

async fn live(db: &Db, id: FileId) -> Result<Option<Live>, PlanError> {
    Ok(sqlx::query_as("SELECT id, volume_id, dir_path, name, kind, hash, capture_ts, size, mtime FROM files WHERE id = ?")
        .bind(id).fetch_optional(db.read()).await?)
}

async fn original_member(
    db: &Db,
    plan: i64,
    side: Option<PlanSide>,
    id: FileId,
) -> Result<bool, PlanError> {
    let mut conn = db.read().acquire().await?;
    if !crate::plans::file_identity::is_live(&mut conn, plan, id).await? {
        return Ok(false);
    }
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM duplicate_matches WHERE plan_id = ? AND (? IS NULL OR side = ?) AND file_id = ? AND live = 1)")
        .bind(plan).bind(side.map(side_name)).bind(side.map(side_name)).bind(id).fetch_one(&mut *conn).await?)
}

async fn counterparts(
    db: &Db,
    plan: i64,
    side: Option<PlanSide>,
    id: FileId,
) -> Result<Vec<(FileId, bool)>, PlanError> {
    let sql = match side {
        Some(PlanSide::Source) => {
            "SELECT comparison_id, 1 FROM duplicate_pairs WHERE plan_id = ? AND source_id = ? ORDER BY comparison_id"
        }
        Some(PlanSide::Comparison) => {
            "SELECT source_id, 0 FROM duplicate_pairs WHERE plan_id = ? AND comparison_id = ? ORDER BY source_id"
        }
        None => {
            "SELECT comparison_id, 1 FROM duplicate_pairs WHERE plan_id = ?1 AND source_id = ?2 \
                 UNION SELECT source_id, 0 FROM duplicate_pairs WHERE plan_id = ?1 AND comparison_id = ?2"
        }
    };
    Ok(sqlx::query_as(sql)
        .bind(plan)
        .bind(id)
        .fetch_all(db.read())
        .await?)
}

pub(super) fn keepers(value: &Value) -> Result<HashSet<FileId>, PlanError> {
    let options: DuplicateFileExecuteOptions = serde_json::from_value(value.clone())
        .map_err(|err| PlanError::invalid_field("keep", err.to_string()))?;
    if options.keep.is_empty()
        || options.keep.len() > super::MAX_FILE_VIEW_MATCHES * 2
        || options.keep.iter().any(|id| *id <= 0)
    {
        return Err(PlanError::invalid_field(
            "keep",
            "select at least one valid keeper (at most 40,000)",
        ));
    }
    Ok(options.keep.into_iter().collect())
}

/// Sidecars are collected before FsOps drops the owner's links, but trashed only AFTER it
/// succeeded and no live owner remains. Failed/skipped owners therefore keep their metadata.
async fn sidecars(db: &Db, owner: FileId) -> Result<Vec<FileId>, PlanError> {
    Ok(
        sqlx::query_scalar("SELECT sidecar_id FROM file_sidecars WHERE file_id = ?")
            .bind(owner)
            .fetch_all(db.read())
            .await?,
    )
}

async fn trash_sidecars(
    ctx: &ExecContext,
    ids: Vec<FileId>,
    result: &mut ResultBuilder,
) -> Result<(), PlanError> {
    for id in ids {
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        let mut identities = ctx.db.read().acquire().await?;
        if !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, id).await? {
            continue;
        }
        drop(identities);
        let owned: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM file_sidecars WHERE sidecar_id = ?)")
                .bind(id)
                .fetch_one(ctx.db.read())
                .await?;
        if owned {
            continue;
        }
        if let Some(row) = live(&ctx.db, id).await? {
            let loc = row.loc()?;
            let path = display_path(&ctx.volumes, loc.volume_id, &loc.rel);
            if ctx.soft_exclusions.contains(loc.volume_id, &loc.rel) {
                result.skip_with_detail(
                    Some(id),
                    path,
                    "soft_excluded",
                    "The folder is soft-excluded from tools",
                );
                continue;
            }
            let request = TrashRequest {
                file_id: Some(id),
                loc,
                expect: Some(row.expect()?),
                batch: ctx.batch.clone(),
                reason: "duplicate_sidecar".to_owned(),
            };
            if let Err(err) = ctx.fsops.trash_file(request, &ctx.op).await {
                result.fail_fs(Some(id), path, &err);
            }
        }
    }
    Ok(())
}

pub(super) async fn execute(
    ctx: ExecContext,
    plan: &Plan,
    value: Value,
) -> Result<PlanResult, PlanError> {
    let (params, file_view) = super::parameters(plan.params()?)?;
    let keep = if file_view.is_some() {
        keepers(&value)?
    } else {
        HashSet::new()
    };
    let side = if file_view.is_some() {
        None
    } else if options(&value)?.action == DuplicateAction::DeleteSource {
        Some(PlanSide::Source)
    } else {
        Some(PlanSide::Comparison)
    };
    let only = ctx.only_ids().await?;
    let checkpoint = crate::tasks::Checkpoint::new(&ctx.db, ctx.task.id).await?;
    // Keep vanished ids as work items so they get an explicit file_gone result.
    let selected: Vec<FileId> =
        sqlx::query_scalar("SELECT DISTINCT file_id FROM duplicate_matches WHERE plan_id = ? AND (? IS NULL OR side = ?)")
            .bind(plan.id)
            .bind(side.map(side_name))
            .bind(side.map(side_name))
            .fetch_all(ctx.db.read())
            .await?;
    let members: HashSet<_> = selected.iter().copied().collect();
    if keep.iter().any(|id| !members.contains(id)) {
        return Err(PlanError::invalid_field(
            "keep",
            "a keeper is not a member of this plan; refresh the search",
        ));
    }
    let deletion: HashSet<FileId> = selected
        .into_iter()
        .filter(|id| !keep.contains(id) && only.as_ref().is_none_or(|only| only.contains(id)))
        .collect();
    let (ordered, bytes) = match checkpoint.load("duplicate-input").await? {
        Some(saved) => saved,
        None => {
            let mut ordered = Vec::with_capacity(deletion.len());
            let mut bytes = 0_u64;
            for id in &deletion {
                if let Some(row) = live(&ctx.db, *id).await? {
                    bytes += row.expect()?.size;
                    ordered.push((row.volume_id, row.dir_path, row.name, *id));
                } else {
                    ordered.push((0, String::new(), String::new(), *id));
                }
            }
            ordered.sort();
            checkpoint
                .save("duplicate-input", &(&ordered, bytes))
                .await?;
            (ordered, bytes)
        }
    };
    ctx.declare_totals(ordered.len() as u64, bytes);
    let (next, mut result): (usize, ResultBuilder) = checkpoint
        .load("duplicate-progress")
        .await?
        .unwrap_or_default();
    ctx.task
        .progress
        .add(next as i64, result.bytes_done() as i64);
    for (index, (volume, dir, name, id)) in ordered.into_iter().enumerate().skip(next) {
        let Some(_mutation) = ctx.fsops.mutation_guard(&ctx.task.cancel).await else {
            result.mark_cancelled();
            break;
        };
        let before = result.counted();
        let previous = if name.is_empty() {
            None
        } else {
            let loc = crate::fsops::Loc::new(
                volume,
                RelPath::new(&dir)
                    .and_then(|dir| dir.join(&name))
                    .map_err(anyhow::Error::from)?,
            );
            ctx.fsops
                .completed_bytes(ctx.task.id, "trash", &loc)
                .await?
        };
        if let Some(bytes) = previous {
            result.done(bytes);
            let companions = checkpoint
                .load::<Vec<FileId>>(&format!("duplicate-sidecars:{id}"))
                .await?
                .unwrap_or_default();
            trash_sidecars(&ctx, companions, &mut result).await?;
        } else {
            process(&ctx, plan, &params, side, id, &deletion, &mut result).await?;
        }
        if result.counted() > before {
            ctx.item_processed();
        }
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        checkpoint
            .save("duplicate-progress", &(index + 1, &result))
            .await?;
    }
    Ok(result.finish())
}

async fn process(
    ctx: &ExecContext,
    plan: &Plan,
    params: &DuplicateParams,
    side: Option<PlanSide>,
    id: FileId,
    deletion: &HashSet<FileId>,
    result: &mut ResultBuilder,
) -> Result<(), PlanError> {
    if !original_member(&ctx.db, plan.id, side, id).await? {
        result.skip_with_detail(
            Some(id),
            format!("file {id}"),
            "file_gone",
            "The original indexed file vanished; a reused id requires a new plan",
        );
        return Ok(());
    }
    let Some(row) = live(&ctx.db, id).await? else {
        result.skip_with_detail(
            Some(id),
            format!("file {id}"),
            "file_gone",
            "The indexed file vanished",
        );
        return Ok(());
    };
    let loc = row.loc()?;
    let path = display_path(&ctx.volumes, loc.volume_id, &loc.rel);
    let candidates: Vec<_> = counterparts(&ctx.db, plan.id, side, id)
        .await?
        .into_iter()
        .filter(|(id, _)| !deletion.contains(id))
        .collect();
    if ctx.soft_exclusions.contains(loc.volume_id, &loc.rel) {
        result.skip_with_detail(
            Some(id),
            path,
            "soft_excluded",
            "The folder is soft-excluded from tools",
        );
        return Ok(());
    }
    if candidates.is_empty() {
        result.skip_with_detail(
            Some(id),
            path,
            "no_survivor",
            "No matched counterpart remains outside the deletion set",
        );
        return Ok(());
    }
    match ctx.fsops.stat(&loc).await {
        Ok(stat) if stat.matches(&row.expect()?) => {}
        Ok(_) | Err(FsOpError::SourceChanged) => {
            result.skip_with_detail(
                Some(id),
                path,
                "stale_index",
                "The file changed since it was indexed",
            );
            return Ok(());
        }
        Err(FsOpError::SourceMissing) => {
            result.skip_with_detail(Some(id), path, "file_gone", "The file vanished from disk");
            return Ok(());
        }
        Err(err) => {
            result.fail_fs(Some(id), path, &err);
            return Ok(());
        }
    }
    let mut survivor = false;
    for (candidate, target_is_source) in candidates {
        if ctx.cancelled() {
            result.mark_cancelled();
            return Ok(());
        }
        if !original_member(&ctx.db, plan.id, side.map(PlanSide::opposite), candidate).await? {
            continue;
        }
        let Some(other) = live(&ctx.db, candidate).await? else {
            continue;
        };
        if ctx
            .soft_exclusions
            .contains(other.volume_id, &other.loc()?.rel)
        {
            continue;
        }
        if !row.media()? || !other.media()? {
            continue;
        }
        let (source, comparison) = if target_is_source {
            (&row, &other)
        } else {
            (&other, &row)
        };
        if !is_match(
            params.mode,
            &source.facts()?,
            &comparison.facts()?,
            MatchOptions {
                strict_prefix: params.strict_prefix,
            },
        ) {
            continue;
        }
        match ctx.fsops.stat(&other.loc()?).await {
            Ok(stat) if params.mode != MatchMode::Hash || stat.matches(&other.expect()?) => {
                survivor = true;
                break;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(
                    file_id = candidate,
                    code = err.code(),
                    "duplicate survivor unavailable"
                );
            }
        }
    }
    if !survivor {
        result.skip_with_detail(
            Some(id),
            path,
            "stale_match",
            "No counterpart still matches the live index and disk",
        );
        return Ok(());
    }
    let companions = sidecars(&ctx.db, id).await?;
    let checkpoint = crate::tasks::Checkpoint::new(&ctx.db, ctx.task.id).await?;
    if !companions.is_empty() {
        let mut tx = ctx.db.write().begin().await?;
        crate::plans::file_identity::track(&mut tx, plan.id, &companions).await?;
        tx.commit().await?;
    }
    checkpoint
        .save(&format!("duplicate-sidecars:{id}"), &companions)
        .await?;
    if ctx.cancelled() {
        result.mark_cancelled();
        return Ok(());
    }
    let request = TrashRequest {
        file_id: Some(id),
        loc,
        expect: Some(row.expect()?),
        batch: ctx.batch.clone(),
        reason: "duplicate".to_owned(),
    };
    match ctx.fsops.trash_file(request, &ctx.op).await {
        Ok(outcome) => {
            result.done(outcome.bytes);
            trash_sidecars(ctx, companions, result).await?;
        }
        Err(FsOpError::SourceChanged) => result.skip_with_detail(
            Some(id),
            path,
            "stale_index",
            "The file changed before it could be trashed",
        ),
        Err(FsOpError::SourceMissing) => result.skip_with_detail(
            Some(id),
            path,
            "file_gone",
            "The file vanished before it could be trashed",
        ),
        Err(FsOpError::Cancelled) => result.mark_cancelled(),
        Err(err) => result.fail_fs(Some(id), path, &err),
    }
    Ok(())
}
