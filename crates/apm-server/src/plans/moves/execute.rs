use std::collections::{HashMap, HashSet};

use apm_core::dto::{ConflictPolicy, MoveParams, MoveType, PlanResult};
use apm_core::matching::{MatchOptions, is_match};
use apm_core::paths::{FileId, RelPath};
use sqlx::QueryBuilder;

use super::dest::{COLUMNS, Source, destination_for};
use super::{params, sidecars};
use crate::db::Db;
use crate::fsops::{
    Expect, MoveGroup, MoveOutcome, MoveRequest, SidecarAction, SidecarOutcome, SidecarSkip,
    SkipReason,
};
use crate::plans::{ExecContext, Plan, PlanError, ResultBuilder, display_path};

#[derive(sqlx::FromRow)]
pub(super) struct Item {
    pub file_id: FileId,
    pub dst_volume_id: i64,
    pub dst_dir: String,
    pub dst_name: String,
    wanted_name: Option<String>,
    counterpart_id: Option<FileId>,
    replaced_id: Option<FileId>,
    src_volume_id: i64,
    src_dir: String,
    pub src_name: String,
    src_size: i64,
    src_mtime: i64,
}

async fn live(db: &Db, id: FileId) -> Result<Option<Source>, PlanError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM files f LEFT JOIN cameras cam ON cam.id = f.camera_id WHERE f.id = ?"
    )))
    .bind(id)
    .fetch_optional(db.read())
    .await?)
}

async fn validate(
    ctx: &ExecContext,
    item: &Item,
    p: &MoveParams,
    rules: &super::dest::Rules,
) -> Result<Result<Source, &'static str>, PlanError> {
    let Some(row) = live(&ctx.db, item.file_id).await? else {
        return Ok(Err("file_gone"));
    };
    let mut identities = ctx.db.read().acquire().await?;
    if !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, item.file_id).await? {
        return Ok(Err("stale_plan"));
    }
    if row.volume_id != item.src_volume_id
        || row.dir_path != item.src_dir
        || row.name != item.src_name
        || row.size != item.src_size
        || row.mtime != item.src_mtime
    {
        return Ok(Err("stale_plan"));
    }
    let counterpart = if let Some(id) = item.counterpart_id {
        if !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, id).await? {
            return Ok(Err("stale_match"));
        }
        let Some(other) = live(&ctx.db, id).await? else {
            return Ok(Err("stale_match"));
        };
        if !is_match(
            p.match_mode.unwrap_or_default(),
            &row.facts()?,
            &other.facts()?,
            MatchOptions {
                strict_prefix: p.strict_prefix,
            },
        ) {
            return Ok(Err("stale_match"));
        }
        Some(RelPath::new(&other.dir_path).map_err(anyhow::Error::from)?)
    } else {
        None
    };
    let wanted = match destination_for(rules, &row, counterpart.as_ref()) {
        Ok(wanted) => wanted,
        Err("metadata_pending") => return Ok(Err("metadata_pending")),
        Err(_) => return Ok(Err("stale_plan")),
    };
    if wanted.volume_id != item.dst_volume_id
        || wanted.dir.as_str() != item.dst_dir
        || wanted.name != item.wanted_name.as_deref().unwrap_or(&item.dst_name)
    {
        return Ok(Err("stale_plan"));
    }
    Ok(Ok(row))
}

pub(super) async fn execute(ctx: ExecContext, plan: &Plan) -> Result<PlanResult, PlanError> {
    let p: MoveParams = plan.params()?;
    let mut rules = params::rules(&p, &ctx.settings)?;
    let (policy, max): (String, i64) =
        sqlx::query_as("SELECT conflict_policy, name_max_len FROM move_plans WHERE plan_id = ?")
            .bind(plan.id)
            .fetch_one(ctx.db.read())
            .await?;
    let policy: ConflictPolicy =
        serde_json::from_value(serde_json::Value::String(policy)).map_err(anyhow::Error::from)?;
    rules.name_max_len = usize::try_from(max).map_err(anyhow::Error::from)?;
    let only = ctx.only_ids().await?;
    let mut qb = QueryBuilder::new(
        "SELECT m.file_id, m.dst_volume_id, m.dst_dir, m.dst_name, m.wanted_name, m.counterpart_id, m.replaced_id, m.src_volume_id, m.src_dir, m.src_name, m.src_size, m.src_mtime FROM move_items m WHERE m.plan_id = ",
    );
    qb.push_bind(plan.id)
        .push(" AND m.status = 'move' ORDER BY m.src_volume_id, m.src_dir, m.src_name");
    let items: Vec<Item> = qb.build_query_as().fetch_all(ctx.db.read()).await?;
    let items: Vec<_> = items
        .into_iter()
        .filter(|i| only.as_ref().is_none_or(|ids| ids.contains(&i.file_id)))
        .collect();
    ctx.declare_totals(
        items.len() as u64,
        items.iter().map(|i| i.src_size as u64).sum(),
    );
    let mut read = ctx.db.read().begin().await?;
    let mut companions = sidecars::carriers(&mut read, &items).await?;
    read.commit().await?;
    let mut result = ResultBuilder::new();
    let mut failed_sidecars = HashSet::new();
    let mut retained_count = 0_u64;
    let mut retained_paths = Vec::new();
    let mut transferred: HashMap<FileId, crate::fsops::Loc> = HashMap::new();
    let all_sources: HashSet<FileId> =
        sqlx::query_scalar("SELECT file_id FROM move_items WHERE plan_id = ?")
            .bind(plan.id)
            .fetch_all(ctx.db.read())
            .await?
            .into_iter()
            .collect();
    for item in items {
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        let source_path = RelPath::new(&item.src_dir)
            .and_then(|dir| dir.join(&item.src_name))
            .map_err(anyhow::Error::from)?;
        let display = display_path(&ctx.volumes, item.src_volume_id, &source_path);
        // A counterpart may have been moved earlier by this execution; its plan-time folder
        // remains authoritative, but the live matching facts still have to agree.
        let validation = if rules.move_type == MoveType::KeepDestination
            && item
                .counterpart_id
                .is_some_and(|id| transferred.contains_key(&id))
        {
            let mut original = super::dest::Rules {
                move_type: MoveType::Flat,
                source_base: rules.source_base.clone(),
                destination_base: RelPath::new(&item.dst_dir).map_err(anyhow::Error::from)?,
                destination_volume: rules.destination_volume,
                pattern: None,
                name_max_len: rules.name_max_len,
                junk: rules.junk.clone(),
                timezone: rules.timezone,
            };
            original.move_type = MoveType::Flat;
            validate(&ctx, &item, &p, &original).await?
        } else {
            validate(&ctx, &item, &p, &rules).await?
        };
        let row = match validation {
            Ok(row) => row,
            Err(code) => {
                result.skip_with_detail(
                    Some(item.file_id),
                    display,
                    code,
                    "The planned source or match changed; build a fresh plan",
                );
                ctx.item_processed();
                continue;
            }
        };
        let mut sidecars = companions.remove(&item.file_id).unwrap_or_default();
        for sidecar in &mut sidecars {
            if sidecar.action == SidecarAction::Move
                && let Some(id) = sidecar.file_id
            {
                let owners: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM file_sidecars WHERE sidecar_id = ?")
                        .bind(id)
                        .fetch_one(ctx.db.read())
                        .await?;
                if owners > 1 || failed_sidecars.contains(&id) {
                    sidecar.action = SidecarAction::Copy;
                }
            }
        }
        // Replace is permitted only for the pre-existing, non-source target recorded by
        // this plan. Newly incoming files and sources are never replace targets.
        let mut effective = policy;
        if policy == ConflictPolicy::Replace {
            let target: Option<FileId> = sqlx::query_scalar(
                "SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name_norm = ?",
            )
            .bind(item.dst_volume_id)
            .bind(&item.dst_dir)
            .bind(apm_core::names::normalize_name(&item.dst_name))
            .fetch_optional(ctx.db.read())
            .await?;
            if target.is_some_and(|id| Some(id) != item.replaced_id || all_sources.contains(&id)) {
                effective = ConflictPolicy::Rename;
            }
            if let Some(id) = target
                && Some(id) == item.replaced_id
            {
                let mut identities = ctx.db.read().acquire().await?;
                if !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, id).await? {
                    result.skip_with_detail(
                        Some(item.file_id),
                        display,
                        "stale_plan",
                        "The planned replacement was deleted or replaced; build a fresh plan",
                    );
                    ctx.item_processed();
                    continue;
                }
            }
        }
        let request = MoveRequest {
            file_id: Some(item.file_id),
            src: row.loc()?,
            dst: crate::fsops::Loc::new(
                item.dst_volume_id,
                RelPath::new(&item.dst_dir)
                    .and_then(|dir| dir.join(&item.dst_name))
                    .map_err(anyhow::Error::from)?,
            ),
            policy: effective,
            expect: Some(Expect {
                size: row.size as u64,
                mtime: row.mtime,
            }),
        };
        let companion_ids: Vec<_> = sidecars
            .iter()
            .map(|s| (s.file_id, s.src.clone(), s.action))
            .collect();
        let outcome = ctx
            .fsops
            .move_group(
                MoveGroup {
                    owner: request,
                    sidecars,
                },
                &ctx.op,
            )
            .await;
        match outcome {
            Ok(group) => {
                match group.owner {
                    MoveOutcome::Moved { dst, bytes, .. } => {
                        result.done(bytes);
                        transferred.insert(item.file_id, dst);
                    }
                    MoveOutcome::Skipped(reason) => {
                        let code = match reason {
                            SkipReason::SameLocation => "already_in_place",
                            SkipReason::IdenticalAtDestination => "identical_at_destination",
                            SkipReason::DestinationExists => "skipped_by_policy",
                        };
                        result.skip_with_detail(
                            Some(item.file_id),
                            display.clone(),
                            code,
                            "The file operation left the source unchanged",
                        );
                    }
                }
                for ((id, source, action), sidecar) in companion_ids.into_iter().zip(group.sidecars)
                {
                    match sidecar {
                        SidecarOutcome::Skipped(SidecarSkip::AlreadyPresent)
                            if action == SidecarAction::Move =>
                        {
                            retained_count += 1;
                            if retained_paths.len() < 3 {
                                retained_paths.push(display_path(
                                    &ctx.volumes,
                                    source.volume_id,
                                    &source.rel,
                                ));
                            }
                        }
                        SidecarOutcome::Failed(err) => {
                            if let Some(id) = id {
                                failed_sidecars.insert(id);
                            }
                            result.fail_fs(
                                id,
                                display_path(&ctx.volumes, source.volume_id, &source.rel),
                                &err,
                            );
                        }
                        SidecarOutcome::Skipped(SidecarSkip::SidecarConflict) => {
                            if let Some(id) = id {
                                failed_sidecars.insert(id);
                            }
                            result.skip_with_detail(
                                id,
                                display_path(&ctx.volumes, source.volume_id, &source.rel),
                                "sidecar_conflict",
                                "The sidecar destination is occupied; its original was kept",
                            );
                        }
                        _ => {}
                    }
                }
            }
            Err(err) if err.code() == "immich_move_refused" => {
                result.skip_with_detail(Some(item.file_id), display, err.code(), err.to_string());
            }
            Err(err) => result.fail_fs(Some(item.file_id), display, &err),
        }
        ctx.item_processed();
    }
    if retained_count > 0 {
        result.note(format!(
            "shared_sidecar_retained: assets moved with metadata at their destinations; {retained_count} original metadata copies remain at {}{}",
            retained_paths.join(", "),
            if retained_count > retained_paths.len() as u64 { " (and others)" } else { "" },
        ));
    }
    Ok(result.finish())
}
