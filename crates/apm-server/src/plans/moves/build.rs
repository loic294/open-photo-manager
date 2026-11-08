use std::collections::{BTreeMap, HashMap, HashSet};

use apm_core::dto::{DuplicateParams, MatchMode, MoveParams, MoveType, Scope, SideParams};
use apm_core::matching::Pair;
use apm_core::names::normalize_name;
use apm_core::paths::{FileId, RelPath};
use futures_util::{TryStreamExt, future::BoxFuture};
use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use super::dest::destination_for;
use super::params;
use super::{COLUMNS, Directory, HashBudget, Source, Wanted};
use crate::plans::duplicates::{MAX_PAIRS, OverlapPolicy, PairVisitor, candidate_pairs};
use crate::plans::{BuildContext, Built, BulkWriter, PlanError};

struct PairSink {
    pairs: Vec<Pair>,
}
impl PairVisitor for PairSink {
    fn visit(&mut self, pair: Pair) -> BoxFuture<'_, Result<(), PlanError>> {
        Box::pin(async move {
            self.pairs.push(pair);
            if self.pairs.len() as u64 > MAX_PAIRS {
                return Err(PlanError::TooLarge {
                    items: self.pairs.len() as u64,
                    max: MAX_PAIRS,
                });
            }
            Ok(())
        })
    }
}

pub(super) struct Counterpart {
    pub id: FileId,
    pub dir: RelPath,
    pub ambiguous: bool,
}

async fn counterparts(
    conn: &mut SqliteConnection,
    ctx: &BuildContext,
    p: &MoveParams,
) -> Result<HashMap<FileId, Counterpart>, PlanError> {
    if p.move_type != MoveType::KeepDestination {
        return Ok(HashMap::new());
    }
    let duplicates = DuplicateParams {
        mode: p.match_mode.unwrap_or(MatchMode::Hash),
        strict_prefix: p.strict_prefix,
        source: p.source.clone(),
        comparison: SideParams {
            scope: Scope {
                volume_id: Some(p.destination.volume_id),
                dir: p.destination.dir.clone(),
                recursive: true,
            },
            filter: Default::default(),
        },
    };
    let mut sink = PairSink { pairs: Vec::new() };
    candidate_pairs(
        conn,
        &duplicates,
        OverlapPolicy::Unoriented,
        &ctx.cancel,
        &mut sink,
    )
    .await?;
    let ids: HashSet<_> = sink.pairs.iter().map(|p| p.comparison).collect();
    let ids: Vec<_> = ids.into_iter().collect();
    let mut paths = HashMap::new();
    for chunk in ids.chunks(30_000) {
        ctx.check_cancelled()?;
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT id, volume_id, dir_path, name FROM files WHERE id IN (",
        );
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(*id);
        }
        sep.push_unseparated(")");
        for (id, volume, dir, name) in qb
            .build_query_as::<(FileId, i64, String, String)>()
            .fetch_all(&mut *conn)
            .await?
        {
            paths.insert(id, (volume, dir, name));
        }
    }
    let mut chosen: HashMap<FileId, FileId> = HashMap::new();
    let mut ambiguous = HashSet::new();
    for pair in sink.pairs {
        let Some(path) = paths.get(&pair.comparison) else {
            continue;
        };
        if let Some(previous) = chosen.get_mut(&pair.source) {
            let old = paths
                .get(previous)
                .ok_or_else(|| PlanError::Internal(anyhow::anyhow!("missing counterpart path")))?;
            if old.0 != path.0 || old.1 != path.1 {
                ambiguous.insert(pair.source);
            }
            if path < old {
                *previous = pair.comparison;
            }
        } else {
            chosen.insert(pair.source, pair.comparison);
        }
    }
    chosen
        .into_iter()
        .map(|(source, id)| {
            let (_, dir, _) = paths
                .get(&id)
                .ok_or_else(|| PlanError::Internal(anyhow::anyhow!("missing counterpart")))?;
            Ok((
                source,
                Counterpart {
                    id,
                    dir: RelPath::new(dir).map_err(anyhow::Error::from)?,
                    ambiguous: ambiguous.contains(&source),
                },
            ))
        })
        .collect()
}

pub(crate) fn source_statement(query: &crate::query::FileQuery) -> QueryBuilder<Sqlite> {
    let mut qb = QueryBuilder::new(format!(
        "SELECT {COLUMNS} FROM files f LEFT JOIN cameras cam ON cam.id = f.camera_id WHERE "
    ));
    query.push_where(&mut qb, "f");
    qb.push(" AND f.kind <> 'sidecar' ORDER BY f.volume_id, f.dir_path, f.name");
    qb
}

pub(super) async fn build(ctx: &BuildContext, value: Value) -> Result<Built, PlanError> {
    let p: MoveParams =
        serde_json::from_value(value).map_err(|e| PlanError::invalid(e.to_string()))?;
    let rules = params::rules(&p, &ctx.settings)?;
    params::volumes(&p, &ctx.volumes)?;
    let policy = p
        .conflict_policy
        .unwrap_or(ctx.settings.default_conflict_policy);
    let query = super::source_query(&p.source)?;
    let mut registration = ctx.db.begin_write().await?;
    crate::plans::file_identity::begin(&mut registration, ctx.plan_id).await?;
    registration.commit().await?;
    let mut read = ctx.db.read().begin().await?;
    let mut ids_query = QueryBuilder::<Sqlite>::new("SELECT f.id FROM files f WHERE ");
    query.push_where(&mut ids_query, "f");
    ids_query
        .push(" AND f.kind <> 'sidecar' LIMIT ")
        .push_bind(ctx.max_items.saturating_add(1) as i64);
    let ids: Vec<FileId> = ids_query.build_query_scalar().fetch_all(&mut *read).await?;
    ctx.check_size(ids.len() as u64)?;
    ctx.check_cancelled()?;
    let sources: HashSet<_> = ids.iter().copied().collect();
    let links = super::links(&mut read, &ids).await?;
    let counterparts = counterparts(&mut read, ctx, &p).await?;
    let mut identities = ctx.db.begin_write().await?;
    crate::plans::file_identity::track(&mut identities, ctx.plan_id, &ids).await?;
    let counterpart_ids: Vec<_> = counterparts.values().map(|c| c.id).collect();
    crate::plans::file_identity::track(&mut identities, ctx.plan_id, &counterpart_ids).await?;
    identities.commit().await?;
    sqlx::query("INSERT INTO move_plans (plan_id, conflict_policy, name_max_len) VALUES (?, ?, ?)")
        .bind(ctx.plan_id)
        .bind(
            serde_json::to_value(policy)
                .map_err(anyhow::Error::from)?
                .as_str(),
        )
        .bind(i64::from(ctx.settings.name_max_len))
        .execute(ctx.db.write())
        .await?;
    let plan = ctx.plan_id;
    let mut writer = BulkWriter::new(
        &ctx.db,
        &ctx.cancel,
        "move_items",
        &[
            "plan_id",
            "file_id",
            "dst_volume_id",
            "dst_dir",
            "dst_name",
            "dst_name_norm",
            "wanted_name",
            "status",
            "counterpart_id",
            "replaced_id",
            "src_volume_id",
            "src_dir",
            "src_name",
            "src_size",
            "src_mtime",
        ],
        move |mut b,
              (row, wanted, name, status, counterpart, replaced): (
            Source,
            Wanted,
            String,
            &'static str,
            Option<FileId>,
            Option<FileId>,
        )| {
            let wanted_name = (wanted.name != name).then_some(wanted.name);
            let norm = normalize_name(&name);
            b.push_bind(plan)
                .push_bind(row.id)
                .push_bind(wanted.volume_id)
                .push_bind(wanted.dir.as_str().to_owned())
                .push_bind(name)
                .push_bind(norm)
                .push_bind(wanted_name)
                .push_bind(status)
                .push_bind(counterpart)
                .push_bind(replaced)
                .push_bind(row.volume_id)
                .push_bind(row.dir_path)
                .push_bind(row.name)
                .push_bind(row.size)
                .push_bind(row.mtime);
        },
    )?;
    let mut directories = HashMap::new();
    let mut missing_parents = HashMap::new();
    let mut destination_keys = HashSet::new();
    match rules.move_type {
        MoveType::Flat => {
            destination_keys.insert((rules.destination_volume, rules.destination_base.clone()));
        }
        MoveType::KeepDestination => {
            destination_keys.insert((rules.destination_volume, rules.destination_base.clone()));
            destination_keys.extend(
                counterparts
                    .values()
                    .map(|c| (rules.destination_volume, c.dir.clone())),
            );
        }
        MoveType::KeepSource => {
            let mut statement =
                QueryBuilder::<Sqlite>::new("SELECT DISTINCT f.dir_path FROM files f WHERE ");
            query.push_where(&mut statement, "f");
            statement.push(" AND f.kind <> 'sidecar'");
            let mut dirs = statement.build_query_scalar::<String>().fetch(&mut *read);
            while let Some(dir) = dirs.try_next().await? {
                ctx.check_cancelled()?;
                let current = RelPath::new(&dir).map_err(anyhow::Error::from)?;
                let destination = super::dest::structural_directory_for(&rules, &current, None)
                    .map_err(|code| PlanError::Internal(anyhow::anyhow!(code)))?;
                destination_keys.insert((rules.destination_volume, destination));
            }
        }
        MoveType::Custom => {
            // Derived paths use the shared destination function. Stream on the same
            // snapshot, retaining distinct directories rather than every source row.
            let mut statement = super::source_statement(&query);
            let mut rows = statement.build_query_as::<Source>().fetch(&mut *read);
            while let Some(row) = rows.try_next().await? {
                ctx.check_cancelled()?;
                if let Ok(wanted) = destination_for(&rules, &row, None)
                    && wanted.loc()? != row.loc()?
                {
                    destination_keys.insert((wanted.volume_id, wanted.dir));
                }
            }
        }
    }
    for (volume, dir) in destination_keys {
        ctx.check_cancelled()?;
        directories.insert(
            (volume, dir.clone()),
            Directory::load(&mut read, volume, &dir).await?,
        );
        let mut missing = Vec::new();
        for path in dir.ancestors() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM dirs WHERE volume_id = ? AND path = ?)",
            )
            .bind(volume)
            .bind(path.as_str())
            .fetch_one(&mut *read)
            .await?;
            if exists {
                break;
            }
            missing.push((volume, path));
        }
        missing_parents.insert((volume, dir), missing);
    }
    let mut written_directories = HashSet::new();
    let mut new_folders = HashSet::new();
    let mut budget = HashBudget::new();
    let mut replacement_ids = HashSet::new();
    let mut stats: BTreeMap<String, u64> = [
        "candidates",
        "to_move",
        "bytes_to_move",
        "already_in_place",
        "identical_at_destination",
        "skipped_by_policy",
        "metadata_pending",
        "renamed",
        "replaced",
        "cross_volume_items",
        "cross_volume_bytes",
        "new_folders",
        "destination_folders",
    ]
    .into_iter()
    .map(|s| (s.to_owned(), 0))
    .collect();
    let mut warnings: BTreeMap<&str, u64> = BTreeMap::new();
    let mut statement = super::source_statement(&query);
    let mut rows = statement.build_query_as::<Source>().fetch(&mut *read);
    while let Some(row) = rows.try_next().await? {
        ctx.check_cancelled()?;
        *stats.entry("candidates".into()).or_default() += 1;
        let counterpart = counterparts.get(&row.id);
        if rules.move_type == MoveType::KeepDestination {
            if counterpart.is_none() {
                *warnings.entry("no_counterpart").or_default() += 1;
            }
            if counterpart.is_some_and(|c| c.ambiguous) {
                *warnings.entry("ambiguous_counterpart").or_default() += 1;
            }
            if p.match_mode.unwrap_or(MatchMode::Hash) == MatchMode::Hash && row.hash.is_none() {
                *warnings.entry("unhashed_files").or_default() += 1;
            }
        }
        if rules.uses_dates()
            && (row.capture_src.as_deref() == Some("fs") || row.capture_ts.is_none())
            && row.meta_state != crate::db::models::PipelineState::Pending
        {
            *warnings.entry("date_from_file_time").or_default() += 1;
        }
        let (wanted, name, status, replaced) =
            match destination_for(&rules, &row, counterpart.map(|c| &c.dir)) {
                Err(code) => {
                    *warnings.entry(code).or_default() += 1;
                    (
                        Wanted {
                            volume_id: rules.destination_volume,
                            dir: rules.destination_base.clone(),
                            name: row.name.clone(),
                        },
                        row.name.clone(),
                        code,
                        None,
                    )
                }
                Ok(wanted) if wanted.loc()? == row.loc()? => {
                    (wanted, row.name.clone(), "already_in_place", None)
                }
                Ok(wanted) => {
                    let key = (wanted.volume_id, wanted.dir.clone());
                    let dir = directories.get_mut(&key).ok_or_else(|| {
                        PlanError::Internal(anyhow::anyhow!("missing destination directory"))
                    })?;
                    let resolved = dir
                        .resolve(
                            ctx,
                            &row,
                            &wanted,
                            links.get(&row.id).map(Vec::as_slice).unwrap_or(&[]),
                            policy,
                            &sources,
                            &mut budget,
                        )
                        .await?;
                    if resolved.status == "move" {
                        *stats.entry("bytes_to_move".into()).or_default() += row.size as u64;
                        if resolved.name != wanted.name {
                            *stats.entry("renamed".into()).or_default() += 1;
                        }
                        if resolved.replaced_id.is_some() {
                            *stats.entry("replaced".into()).or_default() += 1;
                        }
                        if row.volume_id != wanted.volume_id {
                            *stats.entry("cross_volume_items".into()).or_default() += 1;
                            *stats.entry("cross_volume_bytes".into()).or_default() +=
                                row.size as u64;
                        }
                        if written_directories.insert((wanted.volume_id, wanted.dir.clone()))
                            && let Some(missing) = missing_parents.get(&key)
                        {
                            new_folders.extend(missing.iter().cloned());
                        }
                    }
                    (wanted, resolved.name, resolved.status, resolved.replaced_id)
                }
            };
        let key = if status == "move" { "to_move" } else { status };
        if let Some(id) = replaced {
            replacement_ids.insert(id);
        }
        *stats.entry(key.into()).or_default() += 1;
        writer
            .push((
                row,
                wanted,
                name,
                status,
                counterpart.map(|c| c.id),
                replaced,
            ))
            .await?;
    }
    drop(rows);
    let items = writer.finish().await?;
    let mut identities = ctx.db.begin_write().await?;
    crate::plans::file_identity::track(
        &mut identities,
        ctx.plan_id,
        &replacement_ids.into_iter().collect::<Vec<_>>(),
    )
    .await?;
    identities.commit().await?;
    *stats.entry("new_folders".into()).or_default() = new_folders.len() as u64;
    let missing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM move_items m WHERE m.plan_id = ? AND m.status = 'move' AND (NOT EXISTS(SELECT 1 FROM plan_file_identities i WHERE i.plan_id = m.plan_id AND i.file_id = m.file_id) OR (m.counterpart_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM plan_file_identities i WHERE i.plan_id = m.plan_id AND i.file_id = m.counterpart_id)) OR (m.replaced_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM plan_file_identities i WHERE i.plan_id = m.plan_id AND i.file_id = m.replaced_id))))",
    ).bind(ctx.plan_id).fetch_one(ctx.db.read()).await?;
    if missing {
        return Err(PlanError::Internal(anyhow::anyhow!(
            "move snapshot identity is incomplete"
        )));
    }
    *stats.entry("destination_folders".into()).or_default() = written_directories.len() as u64;
    warnings.insert("renames", stats["renamed"]);
    warnings.insert("replaces_existing", stats["replaced"]);
    warnings.insert("cross_volume", stats["cross_volume_items"]);
    warnings.insert("hash_unknown", budget.unknown);
    let mut built = Built {
        items,
        stats,
        warnings: Vec::new(),
    };
    for (code, count) in warnings {
        let message = match code {
            "cross_volume" => format!(
                "{} bytes will be copied and verified before deleting the source",
                built.stats["cross_volume_bytes"]
            ),
            "renames" => "Conflicting files will receive numbered names".into(),
            "replaces_existing" => "Existing destination files will move to the trash".into(),
            "metadata_pending" => {
                "Files awaiting metadata are not moved; index them and re-plan".into()
            }
            "date_from_file_time" => "The pattern uses file birth or modification time".into(),
            "ambiguous_counterpart" => {
                "Several counterpart folders match; the first by path is used".into()
            }
            "no_counterpart" => "No matching counterpart; the destination base is used".into(),
            "unhashed_files" => "Sources without a hash cannot match by hash".into(),
            "hash_unknown" => {
                "Content could not be hashed within the build budget; conflicts are renamed".into()
            }
            "pattern_failed" => "The pattern could not produce a valid destination".into(),
            "reserved_destination" => {
                "Reserved or ignored source/destination paths are not moved".into()
            }
            _ => return Err(PlanError::Internal(anyhow::anyhow!("unknown move warning"))),
        };
        built = built.warning(code, message, count);
    }
    Ok(built)
}
