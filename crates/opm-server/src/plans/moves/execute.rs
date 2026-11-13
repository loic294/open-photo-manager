//! Shared move executor: identity and source facts are checked before the async destination hook,
//! then its wanted location is compared with the snapshot before any FsOps call. Hooks receive
//! only a read-only map of completed transfers; they return coded skips or infrastructure errors.
//! Other plan kinds reuse `run`, retaining ordering, sidecar carriers, Replace safety and outcomes.

use std::collections::{HashMap, HashSet};

use futures_util::future::BoxFuture;
use opm_core::dto::{ConflictPolicy, MoveParams, MoveType, PlanResult};
use opm_core::matching::{MatchOptions, is_match};
use opm_core::paths::{FileId, RelPath};
use sqlx::QueryBuilder;

use super::dest::{Rules, destination_for};
use super::{COLUMNS, Source, Wanted};
use super::{params, sidecars};
use crate::db::Db;
use crate::fsops::{
    Expect, FsOpError, MoveGroup, MoveOutcome, MoveRequest, SidecarAction, SidecarOutcome,
    SidecarSkip, SkipReason,
};
use crate::plans::{ExecContext, Plan, PlanError, ResultBuilder, display_path};

#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub(crate) struct Item {
    pub(crate) file_id: FileId,
    pub(crate) dst_volume_id: i64,
    pub(crate) dst_dir: String,
    pub(crate) dst_name: String,
    pub(crate) wanted_name: Option<String>,
    pub(crate) counterpart_id: Option<FileId>,
    pub(crate) replaced_id: Option<FileId>,
    pub(crate) src_volume_id: i64,
    pub(crate) src_dir: String,
    pub(crate) src_name: String,
    pub(crate) src_size: i64,
    pub(crate) src_mtime: i64,
}

pub(crate) struct Stored {
    pub(crate) policy: ConflictPolicy,
    pub(crate) name_max_len: usize,
}

impl Stored {
    pub(crate) async fn load(ctx: &ExecContext) -> Result<Self, PlanError> {
        let (policy, max): (String, i64) = sqlx::query_as(
            "SELECT conflict_policy, name_max_len FROM move_plans WHERE plan_id = ?",
        )
        .bind(ctx.plan_id)
        .fetch_one(ctx.db.read())
        .await?;
        Ok(Self {
            policy: serde_json::from_value(serde_json::Value::String(policy))
                .map_err(anyhow::Error::from)?,
            name_max_len: usize::try_from(max).map_err(anyhow::Error::from)?,
        })
    }
}

pub(crate) trait Revalidate: Sync {
    fn wanted<'a>(
        &'a self,
        ctx: &'a ExecContext,
        item: &'a Item,
        row: &'a Source,
        transferred: &'a HashMap<FileId, crate::fsops::Loc>,
    ) -> BoxFuture<'a, Result<Result<Wanted, &'static str>, PlanError>>;
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
    hook: &dyn Revalidate,
    transferred: &HashMap<FileId, crate::fsops::Loc>,
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
    drop(identities);
    let wanted = match hook.wanted(ctx, item, &row, transferred).await? {
        Ok(wanted) => wanted,
        Err(code) => return Ok(Err(code)),
    };
    if wanted.volume_id != item.dst_volume_id
        || wanted.dir.as_str() != item.dst_dir
        || wanted.name != item.wanted_name.as_deref().unwrap_or(&item.dst_name)
    {
        return Ok(Err("stale_plan"));
    }
    Ok(Ok(row))
}

struct MovesHook {
    params: MoveParams,
    rules: Rules,
}

impl super::Revalidate for MovesHook {
    fn wanted<'a>(
        &'a self,
        ctx: &'a ExecContext,
        item: &'a Item,
        row: &'a Source,
        transferred: &'a HashMap<FileId, crate::fsops::Loc>,
    ) -> BoxFuture<'a, Result<Result<Wanted, &'static str>, PlanError>> {
        Box::pin(async move {
            let mut identities = ctx.db.read().acquire().await?;
            let counterpart = if let Some(id) = item.counterpart_id {
                if !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, id).await? {
                    return Ok(Err("stale_match"));
                }
                let Some(other) = live(&ctx.db, id).await? else {
                    return Ok(Err("stale_match"));
                };
                if ctx
                    .soft_exclusions
                    .contains(other.volume_id, &other.loc()?.rel)
                {
                    return Ok(Err("soft_excluded"));
                }
                if !is_match(
                    self.params.match_mode.unwrap_or_default(),
                    &row.facts()?,
                    &other.facts()?,
                    MatchOptions {
                        strict_prefix: self.params.strict_prefix,
                    },
                ) {
                    return Ok(Err("stale_match"));
                }
                Some(RelPath::new(&other.dir_path).map_err(anyhow::Error::from)?)
            } else {
                None
            };
            // A transferred counterpart keeps its plan-time folder, but must still match live facts.
            let original;
            let rules = if self.rules.move_type == MoveType::KeepDestination
                && item
                    .counterpart_id
                    .is_some_and(|id| transferred.contains_key(&id))
            {
                original = Rules {
                    move_type: MoveType::Flat,
                    source_base: self.rules.source_base.clone(),
                    destination_base: RelPath::new(&item.dst_dir).map_err(anyhow::Error::from)?,
                    destination_volume: self.rules.destination_volume,
                    pattern: None,
                    name_max_len: self.rules.name_max_len,
                    junk: self.rules.junk.clone(),
                    excluded: self.rules.excluded.clone(),
                    timezone: self.rules.timezone,
                };
                &original
            } else {
                &self.rules
            };
            let wanted = match destination_for(rules, row, counterpart.as_ref()) {
                Ok(wanted) => wanted,
                Err("metadata_pending") => return Ok(Err("metadata_pending")),
                Err(_) => return Ok(Err("stale_plan")),
            };
            Ok(Ok(wanted))
        })
    }
}

pub(super) async fn execute(ctx: ExecContext, plan: &Plan) -> Result<PlanResult, PlanError> {
    let p: MoveParams = plan.params()?;
    let mut rules = params::rules(&p, &ctx.settings)?;
    let stored = super::Stored::load(&ctx).await?;
    rules.name_max_len = stored.name_max_len;
    super::run(ctx, stored, &MovesHook { params: p, rules }).await
}

pub(crate) async fn run(
    ctx: ExecContext,
    stored: Stored,
    hook: &dyn Revalidate,
) -> Result<PlanResult, PlanError> {
    let policy = stored.policy;
    let only = ctx.only_ids().await?;
    let checkpoint = crate::tasks::Checkpoint::new(&ctx.db, ctx.task.id).await?;
    let saved = checkpoint
        .load::<(Vec<Item>, HashMap<FileId, Vec<crate::fsops::SidecarItem>>)>("move-input")
        .await?;
    let (items, mut companions) = if let Some(saved) = saved {
        saved
    } else {
        let mut qb = QueryBuilder::new(
            "SELECT m.file_id, m.dst_volume_id, m.dst_dir, m.dst_name, m.wanted_name, m.counterpart_id, m.replaced_id, m.src_volume_id, m.src_dir, m.src_name, m.src_size, m.src_mtime FROM move_items m WHERE m.plan_id = ",
        );
        qb.push_bind(ctx.plan_id)
            .push(" AND m.status = 'move' ORDER BY m.src_volume_id, m.src_dir, m.src_name");
        let items: Vec<Item> = qb.build_query_as().fetch_all(ctx.db.read()).await?;
        let items: Vec<_> = items
            .into_iter()
            .filter(|i| only.as_ref().is_none_or(|ids| ids.contains(&i.file_id)))
            .collect();
        let mut read = ctx.db.begin_write().await?;
        let companions = sidecars::carriers(&mut read, &items).await?;
        let ids = companions
            .values()
            .flatten()
            .filter_map(|sidecar| sidecar.file_id)
            .collect::<Vec<_>>();
        crate::plans::file_identity::track(&mut read, ctx.plan_id, &ids).await?;
        read.commit().await?;
        checkpoint
            .save("move-input", &(&items, &companions))
            .await?;
        (items, companions)
    };
    ctx.declare_totals(
        items.len() as u64,
        items.iter().map(|i| i.src_size as u64).sum(),
    );
    let (next, mut result, mut failed_sidecars, mut retained_count, mut retained_paths): (
        usize,
        ResultBuilder,
        HashSet<FileId>,
        u64,
        Vec<String>,
    ) = checkpoint.load("move-progress").await?.unwrap_or_default();
    ctx.task
        .progress
        .add(next as i64, result.bytes_done() as i64);
    let mut transferred: HashMap<FileId, crate::fsops::Loc> = checkpoint
        .entries::<(FileId, crate::fsops::Loc)>("transfer:")
        .await?
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    let all_sources: HashSet<FileId> =
        sqlx::query_scalar("SELECT file_id FROM move_items WHERE plan_id = ?")
            .bind(ctx.plan_id)
            .fetch_all(ctx.db.read())
            .await?
            .into_iter()
            .collect();
    for (index, item) in items.into_iter().enumerate().skip(next) {
        let Some(_mutation) = ctx.fsops.mutation_guard(&ctx.task.cancel).await else {
            result.mark_cancelled();
            break;
        };
        let source_path = RelPath::new(&item.src_dir)
            .and_then(|dir| dir.join(&item.src_name))
            .map_err(anyhow::Error::from)?;
        let display = display_path(&ctx.volumes, item.src_volume_id, &source_path);
        let source = crate::fsops::Loc::new(item.src_volume_id, source_path.clone());
        let completed = match ctx.fsops.completed_move(ctx.task.id, &source).await {
            Ok(completed) => completed,
            Err(FsOpError::SourceChanged | FsOpError::SourceMissing) => {
                result.skip_with_detail(
                    Some(item.file_id),
                    display,
                    "stale_plan",
                    "The previously moved file was changed or removed; build a fresh plan",
                );
                ctx.item_processed();
                checkpoint
                    .save(
                        "move-progress",
                        &(
                            index + 1,
                            &result,
                            &failed_sidecars,
                            retained_count,
                            &retained_paths,
                        ),
                    )
                    .await?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let destination_path = RelPath::new(&item.dst_dir).map_err(anyhow::Error::from)?;
        let validation = if ctx
            .soft_exclusions
            .contains(item.src_volume_id, &source_path)
            || ctx
                .soft_exclusions
                .contains(item.dst_volume_id, &destination_path)
        {
            Err("soft_excluded")
        } else if let Some(MoveOutcome::Moved { file_id, .. }) = &completed {
            let mut conn = ctx.db.read().acquire().await?;
            if *file_id == item.file_id
                && crate::plans::file_identity::is_live(&mut conn, ctx.plan_id, item.file_id)
                    .await?
            {
                drop(conn);
                Ok(live(&ctx.db, item.file_id).await?.ok_or_else(|| {
                    PlanError::Internal(anyhow::anyhow!("recovered move lost its index row"))
                })?)
            } else {
                Err("stale_plan")
            }
        } else {
            validate(&ctx, &item, hook, &transferred).await?
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
                checkpoint
                    .save(
                        "move-progress",
                        &(
                            index + 1,
                            &result,
                            &failed_sidecars,
                            retained_count,
                            &retained_paths,
                        ),
                    )
                    .await?;
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
        let mut safe_sidecars = Vec::with_capacity(sidecars.len());
        let mut identities = ctx.db.read().acquire().await?;
        for sidecar in sidecars {
            if let Some(id) = sidecar.file_id
                && !crate::plans::file_identity::is_live(&mut identities, ctx.plan_id, id).await?
            {
                result.skip_with_detail(
                    Some(id),
                    display_path(&ctx.volumes, sidecar.src.volume_id, &sidecar.src.rel),
                    "sidecar_conflict",
                    "The original sidecar vanished; a replacement requires a fresh plan",
                );
                failed_sidecars.insert(id);
            } else {
                safe_sidecars.push(sidecar);
            }
        }
        drop(identities);
        let sidecars = safe_sidecars;
        // Replace is permitted only for the pre-existing, non-source target recorded by
        // this plan. Newly incoming files and sources are never replace targets.
        let mut effective = policy;
        if completed.is_none() && policy == ConflictPolicy::Replace {
            let target: Option<FileId> = sqlx::query_scalar(
                "SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name_norm = ?",
            )
            .bind(item.dst_volume_id)
            .bind(&item.dst_dir)
            .bind(opm_core::names::normalize_name(&item.dst_name))
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
                    checkpoint
                        .save(
                            "move-progress",
                            &(
                                index + 1,
                                &result,
                                &failed_sidecars,
                                retained_count,
                                &retained_paths,
                            ),
                        )
                        .await?;
                    continue;
                }
            }
        }
        let request = MoveRequest {
            file_id: Some(item.file_id),
            src: source,
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
                        checkpoint
                            .save(&format!("transfer:{}", item.file_id), &(item.file_id, &dst))
                            .await?;
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
            Err(FsOpError::Cancelled) => {
                result.mark_cancelled();
                break;
            }
            Err(err) => result.fail_fs(Some(item.file_id), display, &err),
        }
        ctx.item_processed();
        if ctx.cancelled() {
            result.mark_cancelled();
            break;
        }
        checkpoint
            .save(
                "move-progress",
                &(
                    index + 1,
                    &result,
                    &failed_sidecars,
                    retained_count,
                    &retained_paths,
                ),
            )
            .await?;
    }
    if ctx.cancelled() {
        result.mark_cancelled();
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
