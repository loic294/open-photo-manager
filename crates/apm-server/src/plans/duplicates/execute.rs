use std::collections::HashSet;

use apm_core::dto::{DuplicateAction, DuplicateParams, MatchMode, PlanResult, PlanSide};
use apm_core::hash::{Sha1Digest, from_slice};
use apm_core::matching::{MatchFacts, MatchOptions, is_match, participates};
use apm_core::paths::{FileId, RelPath};
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

async fn counterparts(
    db: &Db,
    plan: i64,
    side: PlanSide,
    id: FileId,
) -> Result<Vec<FileId>, PlanError> {
    let sql = match side {
        PlanSide::Source => {
            "SELECT comparison_id FROM duplicate_pairs WHERE plan_id = ? AND source_id = ? ORDER BY comparison_id"
        }
        PlanSide::Comparison => {
            "SELECT source_id FROM duplicate_pairs WHERE plan_id = ? AND comparison_id = ? ORDER BY source_id"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(plan)
        .bind(id)
        .fetch_all(db.read())
        .await?)
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
    let side = if options(&value)?.action == DuplicateAction::DeleteSource {
        PlanSide::Source
    } else {
        PlanSide::Comparison
    };
    let params: DuplicateParams = plan.params()?;
    let only = ctx.only_ids().await?;
    // Keep vanished ids as work items so they get an explicit file_gone result.
    let selected: Vec<FileId> =
        sqlx::query_scalar("SELECT file_id FROM duplicate_matches WHERE plan_id = ? AND side = ?")
            .bind(plan.id)
            .bind(side_name(side))
            .fetch_all(ctx.db.read())
            .await?;
    let deletion: HashSet<FileId> = selected
        .into_iter()
        .filter(|id| only.as_ref().is_none_or(|only| only.contains(id)))
        .collect();
    let mut ordered = Vec::with_capacity(deletion.len());
    let mut bytes = 0;
    for id in &deletion {
        if let Some(row) = live(&ctx.db, *id).await? {
            bytes += row.expect()?.size;
            ordered.push((row.volume_id, row.dir_path, row.name, *id));
        } else {
            ordered.push((0, String::new(), String::new(), *id));
        }
    }
    ordered.sort();
    ctx.declare_totals(deletion.len() as u64, bytes);
    let mut result = ResultBuilder::new();
    for (_, _, _, id) in ordered {
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        let before = result.counted();
        process(&ctx, plan, &params, side, id, &deletion, &mut result).await?;
        if result.counted() > before {
            ctx.item_processed();
        }
    }
    Ok(result.finish())
}

async fn process(
    ctx: &ExecContext,
    plan: &Plan,
    params: &DuplicateParams,
    side: PlanSide,
    id: FileId,
    deletion: &HashSet<FileId>,
    result: &mut ResultBuilder,
) -> Result<(), PlanError> {
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
        .filter(|id| !deletion.contains(id))
        .collect();
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
    for candidate in candidates {
        if ctx.cancelled() {
            result.mark_cancelled();
            return Ok(());
        }
        let Some(other) = live(&ctx.db, candidate).await? else {
            continue;
        };
        if !row.media()? || !other.media()? {
            continue;
        }
        let (source, comparison) = if side == PlanSide::Source {
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
