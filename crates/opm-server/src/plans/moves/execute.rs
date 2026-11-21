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
    Delegated, Expect, FsOpError, Loc, MoveGroup, MoveOutcome, MoveRequest, SidecarAction,
    SidecarOutcome, SidecarSkip, SkipReason,
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
        transferred: &'a HashMap<FileId, Loc>,
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
    transferred: &HashMap<FileId, Loc>,
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
        transferred: &'a HashMap<FileId, Loc>,
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

/// How many moves go to the sync manager in one operation.
const ISM_BATCH: usize = 500;

/// A move of a bound volume, queued for the sync manager.
struct Queued {
    file_id: FileId,
    display: String,
    src: Loc,
    dst: Loc,
    bytes: u64,
    revision: String,
}

/// Moves on bound volumes, sent to the sync manager in batches. OPM resolves the conflict policy
/// beforehand and never touches these files itself.
#[derive(Default)]
struct IsmQueue {
    items: Vec<Queued>,
    /// Destinations claimed by queued moves.
    claimed: HashSet<Loc>,
}

impl IsmQueue {
    fn holds(&self, id: FileId) -> bool {
        self.items.iter().any(|queued| queued.file_id == id)
    }

    /// Sends the queued moves as one ISM operation and records each outcome as ISM reports it.
    /// Returns true when the run was cancelled.
    async fn flush(
        &mut self,
        ctx: &ExecContext,
        checkpoint: &crate::tasks::Checkpoint,
        result: &mut ResultBuilder,
        transferred: &mut HashMap<FileId, Loc>,
    ) -> Result<bool, PlanError> {
        let queued = std::mem::take(&mut self.items);
        self.claimed.clear();
        if queued.is_empty() {
            return Ok(false);
        }
        let mut moved = Vec::new();
        let actions: Vec<Delegated> = queued
            .iter()
            .map(|item| Delegated::Move {
                src: item.src.clone(),
                dst: item.dst.clone(),
                revision: item.revision.clone(),
            })
            .collect();
        let mut cancelled = false;
        let key = format!("opm:plan:{}", ctx.plan_id);
        ctx.fsops
            .delegate(
                "Move files",
                &key,
                &actions,
                &ctx.op,
                &mut |index, outcome| {
                    let Some(item) = queued.get(index) else {
                        return;
                    };
                    match outcome {
                        Ok(done) => {
                            result.done(item.bytes);
                            ctx.task
                                .progress
                                .add(0, i64::try_from(item.bytes).unwrap_or(i64::MAX));
                            moved.push((item.file_id, done.moved_to.unwrap_or(item.dst.clone())));
                        }
                        Err(FsOpError::Cancelled) => cancelled = true,
                        Err(err) => result.fail_fs(Some(item.file_id), item.display.clone(), &err),
                    }
                    ctx.item_processed();
                },
            )
            .await;
        for (file_id, dst) in moved {
            checkpoint
                .save(&format!("transfer:{file_id}"), &(file_id, &dst))
                .await?;
            transferred.insert(file_id, dst);
        }
        if cancelled {
            result.mark_cancelled();
        }
        Ok(cancelled)
    }
}

enum Planned {
    Queue(Queued),
    Skip(&'static str, &'static str),
    Fail(FsOpError),
}

/// Settles the conflict policy of a move between bound volumes read-only, the way the local
/// executor would, and reads the revision ISM checks before it moves the file.
async fn plan_delegated(
    ctx: &ExecContext,
    queue: &IsmQueue,
    request: &MoveRequest,
    item: &Item,
    display: &str,
) -> Planned {
    if !(ctx.fsops.is_managed(request.src.volume_id) && ctx.fsops.is_managed(request.dst.volume_id))
    {
        return Planned::Fail(FsOpError::delegated(
            "ism_cross_boundary",
            "Files cannot move between a volume the sync manager manages and one it does not",
        ));
    }
    if request.src == request.dst {
        return Planned::Skip(
            "already_in_place",
            "The file is already where the plan puts it",
        );
    }
    let Some(expect) = request.expect else {
        return Planned::Fail(FsOpError::delegated(
            "revision_unavailable",
            "The planned file facts are missing",
        ));
    };
    let revision = match ctx.fsops.revision_of(&request.src, expect).await {
        Ok(revision) => revision,
        Err(err) => return Planned::Fail(err),
    };
    let mut dst = request.dst.clone();
    let taken = queue.claimed.contains(&dst)
        || match ctx.fsops.exists(&dst).await {
            Ok(taken) => taken,
            Err(err) => return Planned::Fail(err),
        };
    if taken {
        match request.policy {
            ConflictPolicy::Skip => {
                return Planned::Skip(
                    "skipped_by_policy",
                    "Something is already at the destination",
                );
            }
            ConflictPolicy::Replace => {
                return Planned::Fail(FsOpError::delegated(
                    "ism_replace_unsupported",
                    "The sync manager does not replace files; choose rename or skip",
                ));
            }
            ConflictPolicy::Smart if !queue.claimed.contains(&dst) => {
                match ctx
                    .fsops
                    .identical(&request.src, &dst, &ctx.op.cancel)
                    .await
                {
                    Ok(true) => {
                        return Planned::Skip(
                            "identical_at_destination",
                            "An identical file is already at the destination",
                        );
                    }
                    Ok(false) => {}
                    Err(err) => return Planned::Fail(err),
                }
            }
            _ => {}
        }
        dst = match ctx.fsops.free_loc(&dst, &queue.claimed).await {
            Ok(free) => free,
            Err(err) => return Planned::Fail(err),
        };
    }
    Planned::Queue(Queued {
        file_id: item.file_id,
        display: display.to_owned(),
        src: request.src.clone(),
        dst,
        bytes: expect.size,
        revision,
    })
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
    let mut transferred: HashMap<FileId, Loc> = checkpoint
        .entries::<(FileId, Loc)>("transfer:")
        .await?
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    let total = items.len();
    let all_sources: HashSet<FileId> =
        sqlx::query_scalar("SELECT file_id FROM move_items WHERE plan_id = ?")
            .bind(ctx.plan_id)
            .fetch_all(ctx.db.read())
            .await?
            .into_iter()
            .collect();
    let mut queue = IsmQueue::default();
    // Progress is saved only while no move waits in the ISM queue, so a resumed run never skips
    // a queued move that was not sent yet.
    macro_rules! save_progress {
        ($next:expr) => {
            if queue.items.is_empty() {
                checkpoint
                    .save(
                        "move-progress",
                        &(
                            $next,
                            &result,
                            &failed_sidecars,
                            retained_count,
                            &retained_paths,
                        ),
                    )
                    .await?;
            }
        };
    }
    for (index, item) in items.into_iter().enumerate().skip(next) {
        let delegated = !ctx.op.local_only
            && (ctx.fsops.is_managed(item.src_volume_id)
                || ctx.fsops.is_managed(item.dst_volume_id));
        // Later items may depend on where earlier ones went: send the queue first.
        if !queue.items.is_empty()
            && (!delegated || item.counterpart_id.is_some_and(|id| queue.holds(id)))
        {
            if queue
                .flush(&ctx, &checkpoint, &mut result, &mut transferred)
                .await?
            {
                break;
            }
            save_progress!(index);
        }
        let Some(mutation) = ctx.fsops.mutation_guard(&ctx.task.cancel).await else {
            result.mark_cancelled();
            break;
        };
        let source_path = RelPath::new(&item.src_dir)
            .and_then(|dir| dir.join(&item.src_name))
            .map_err(anyhow::Error::from)?;
        let display = display_path(&ctx.volumes, item.src_volume_id, &source_path);
        let source = Loc::new(item.src_volume_id, source_path.clone());
        let completed = match if delegated {
            Ok(None)
        } else {
            ctx.fsops.completed_move(ctx.task.id, &source).await
        } {
            Ok(completed) => completed,
            Err(FsOpError::SourceChanged | FsOpError::SourceMissing) => {
                result.skip_with_detail(
                    Some(item.file_id),
                    display,
                    "stale_plan",
                    "The previously moved file was changed or removed; build a fresh plan",
                );
                ctx.item_processed();
                save_progress!(index + 1);
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
                save_progress!(index + 1);
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
                    save_progress!(index + 1);
                    continue;
                }
            }
        }
        let request = MoveRequest {
            file_id: Some(item.file_id),
            src: source,
            dst: Loc::new(
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
        if delegated {
            // ISM moves the sidecars of the asset with it and reports them.
            match plan_delegated(&ctx, &queue, &request, &item, &display).await {
                Planned::Queue(queued) => {
                    queue.claimed.insert(queued.dst.clone());
                    queue.items.push(queued);
                    if queue.items.len() >= ISM_BATCH {
                        drop(mutation);
                        if queue
                            .flush(&ctx, &checkpoint, &mut result, &mut transferred)
                            .await?
                        {
                            break;
                        }
                    }
                }
                Planned::Skip(code, detail) => {
                    result.skip_with_detail(Some(item.file_id), display, code, detail);
                    ctx.item_processed();
                }
                Planned::Fail(err) => {
                    result.fail_fs(Some(item.file_id), display, &err);
                    ctx.item_processed();
                }
            }
            save_progress!(index + 1);
            continue;
        }
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
        save_progress!(index + 1);
    }
    if !ctx.cancelled()
        && !queue.items.is_empty()
        && !queue
            .flush(&ctx, &checkpoint, &mut result, &mut transferred)
            .await?
    {
        save_progress!(total);
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
