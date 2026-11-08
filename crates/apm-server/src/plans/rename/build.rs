use std::collections::{BTreeMap, HashMap, HashSet};

use apm_core::dto::{ConflictPolicy, RenameParams};
use apm_core::names::normalize_name;
use apm_core::paths::{FileId, RelPath};
use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite};

use super::super::moves::{Directory, HashBudget, Source, Wanted, source_statement};
use super::names::{Names, Outcome, issue_field};
use crate::plans::{BuildContext, Built, BulkWriter, PlanError};

fn policy(params: &RenameParams, ctx: &BuildContext) -> Result<ConflictPolicy, PlanError> {
    if params.conflict_policy == Some(ConflictPolicy::Replace) {
        return Err(PlanError::invalid_field(
            "conflict_policy",
            "replace is not available for batch rename",
        ));
    }
    let policy = params
        .conflict_policy
        .unwrap_or(ctx.settings.default_conflict_policy);
    Ok(if policy == ConflictPolicy::Replace {
        ConflictPolicy::Smart
    } else {
        policy
    })
}

fn compile(params: &RenameParams) -> Result<apm_core::rename::CompiledRules, PlanError> {
    match apm_core::rename::compile(&params.rules) {
        Ok(compiled) => Ok(compiled),
        Err(issues) => {
            let issue = issues.first().ok_or_else(|| {
                PlanError::Internal(anyhow::anyhow!(
                    "rename rule compilation returned no issues"
                ))
            })?;
            Err(PlanError::invalid_field(
                format!("rules.{}", issue_field(issue.field)),
                issue.message.clone(),
            ))
        }
    }
}

type WriteRow = (Source, Wanted, String, &'static str, Option<u64>);

struct FolderWriter<'a, F>
where
    F: for<'b> FnMut(sqlx::query_builder::Separated<'b, Sqlite, &'static str>, WriteRow) + Send,
{
    ctx: &'a BuildContext,
    names: &'a Names,
    links: &'a HashMap<FileId, Vec<super::super::moves::Link>>,
    sources: &'a HashSet<FileId>,
    policy: ConflictPolicy,
    directory: &'a mut Option<Directory>,
    conflict_conn: &'a mut sqlx::pool::PoolConnection<Sqlite>,
    budget: &'a mut HashBudget,
    writer: &'a mut BulkWriter<WriteRow, F>,
    stats: &'a mut BTreeMap<String, u64>,
    warnings: &'a mut BTreeMap<&'static str, u64>,
}

impl<F> FolderWriter<'_, F>
where
    F: for<'b> FnMut(sqlx::query_builder::Separated<'b, Sqlite, &'static str>, WriteRow) + Send,
{
    async fn write(&mut self, rows: Vec<Source>) -> Result<(), PlanError> {
        let outcomes = self.names.folder(&rows)?;
        let needs_directory = outcomes.iter().zip(&rows).any(
            |(outcome, row)| matches!(outcome, Outcome::Rename { name, .. } if name != &row.name),
        );
        if needs_directory {
            let row = rows
                .first()
                .ok_or_else(|| PlanError::Internal(anyhow::anyhow!("empty rename folder")))?;
            let dir = RelPath::new(&row.dir_path).map_err(anyhow::Error::from)?;
            *self.directory = Some(Directory::load(self.conflict_conn, row.volume_id, &dir).await?);
        }

        for (row, outcome) in rows.into_iter().zip(outcomes) {
            self.ctx.check_cancelled()?;
            if matches!(outcome, Outcome::Unmatched) {
                continue;
            }
            *self.stats.entry("matched".to_owned()).or_default() += 1;
            if self.names.uses_dates()
                && (row.capture_src.as_deref() == Some("fs") || row.capture_ts.is_none())
                && row.meta_state != crate::db::models::PipelineState::Pending
            {
                *self.warnings.entry("date_from_file_time").or_default() += 1;
            }

            let (wanted, name, status, seq) = match outcome {
                Outcome::Unmatched => continue,
                Outcome::Pending => {
                    *self.stats.entry("metadata_pending".to_owned()).or_default() += 1;
                    *self.warnings.entry("metadata_pending").or_default() += 1;
                    (
                        source_wanted(&row)?,
                        row.name.clone(),
                        "metadata_pending",
                        None,
                    )
                }
                Outcome::Failed(code) => {
                    *self.stats.entry(code.to_owned()).or_default() += 1;
                    *self.warnings.entry(code).or_default() += 1;
                    (source_wanted(&row)?, row.name.clone(), code, None)
                }
                Outcome::Rename { name, seq } if name == row.name => {
                    *self.stats.entry("already_in_place".to_owned()).or_default() += 1;
                    (
                        Wanted {
                            volume_id: row.volume_id,
                            dir: RelPath::new(&row.dir_path).map_err(anyhow::Error::from)?,
                            name: name.clone(),
                        },
                        name,
                        "already_in_place",
                        seq,
                    )
                }
                Outcome::Rename { name, seq } => {
                    let wanted = Wanted {
                        volume_id: row.volume_id,
                        dir: RelPath::new(&row.dir_path).map_err(anyhow::Error::from)?,
                        name: name.clone(),
                    };
                    let resolved = self
                        .directory
                        .as_mut()
                        .ok_or_else(|| {
                            PlanError::Internal(anyhow::anyhow!(
                                "rename conflict directory was not loaded"
                            ))
                        })?
                        .resolve(
                            self.ctx,
                            &row,
                            &wanted,
                            self.links.get(&row.id).map(Vec::as_slice).unwrap_or(&[]),
                            self.policy,
                            self.sources,
                            self.budget,
                        )
                        .await?;
                    let status = resolved.status;
                    if status == "move" {
                        *self.stats.entry("to_rename".to_owned()).or_default() += 1;
                        if resolved.name != name {
                            *self
                                .stats
                                .entry("renamed_by_policy".to_owned())
                                .or_default() += 1;
                        }
                    } else {
                        *self.stats.entry(status.to_owned()).or_default() += 1;
                        if status == "skipped_by_policy" {
                            *self.warnings.entry("skipped_by_policy").or_default() += 1;
                        }
                    }
                    (wanted, resolved.name, status, seq)
                }
            };
            self.writer.push((row, wanted, name, status, seq)).await?;
        }
        Ok(())
    }
}

fn source_wanted(row: &Source) -> Result<Wanted, PlanError> {
    Ok(Wanted {
        volume_id: row.volume_id,
        dir: RelPath::new(&row.dir_path).map_err(anyhow::Error::from)?,
        name: row.name.clone(),
    })
}

pub(super) async fn build(ctx: &BuildContext, value: Value) -> Result<Built, PlanError> {
    let params: RenameParams =
        serde_json::from_value(value).map_err(|error| PlanError::invalid(error.to_string()))?;
    let query = super::super::moves::source_query(&params.source)?;
    super::super::moves::validate_volume(
        "source.scope.volume_id",
        params.source.scope.volume_id,
        &ctx.volumes,
    )?;
    let policy = policy(&params, ctx)?;
    let compiled = compile(&params)?;
    let names = Names::new(compiled, &params.rules.pattern, &ctx.settings)?;

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

    let mut identities = ctx.db.begin_write().await?;
    crate::plans::file_identity::track(&mut identities, ctx.plan_id, &ids).await?;
    identities.commit().await?;
    let policy_text = serde_json::to_value(policy)
        .map_err(anyhow::Error::from)?
        .as_str()
        .ok_or_else(|| PlanError::Internal(anyhow::anyhow!("invalid conflict policy encoding")))?
        .to_owned();
    sqlx::query("INSERT INTO move_plans (plan_id, conflict_policy, name_max_len) VALUES (?, ?, ?)")
        .bind(ctx.plan_id)
        .bind(policy_text)
        .bind(i64::from(ctx.settings.name_max_len))
        .execute(ctx.db.write())
        .await?;

    let links = super::super::moves::links(&mut read, &ids).await?;
    let plan_id = ctx.plan_id;
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
            "seq",
        ],
        move |mut binds, (row, wanted, name, status, seq): WriteRow| {
            let wanted_name = (wanted.name != name).then_some(wanted.name);
            binds
                .push_bind(plan_id)
                .push_bind(row.id)
                .push_bind(wanted.volume_id)
                .push_bind(wanted.dir.as_str().to_owned())
                .push_bind(name.clone())
                .push_bind(normalize_name(&name))
                .push_bind(wanted_name)
                .push_bind(status)
                .push_bind(Option::<FileId>::None)
                .push_bind(Option::<FileId>::None)
                .push_bind(row.volume_id)
                .push_bind(row.dir_path)
                .push_bind(row.name)
                .push_bind(row.size)
                .push_bind(row.mtime)
                .push_bind(seq.map(|count| count as i64));
        },
    )?;

    let mut stats: BTreeMap<String, u64> = [
        "candidates",
        "matched",
        "to_rename",
        "already_in_place",
        "identical_at_destination",
        "skipped_by_policy",
        "metadata_pending",
        "pattern_failed",
        "reserved_destination",
        "renamed_by_policy",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), 0))
    .collect();
    stats.insert("candidates".to_owned(), ids.len() as u64);
    let mut warning_counts = BTreeMap::new();
    let mut hash_budget = HashBudget::new();
    let mut conflict_conn = ctx.db.read().acquire().await?;
    let mut current = Vec::new();
    let mut current_key: Option<(i64, String)> = None;
    let mut directory = None;
    let mut statement = source_statement(&query);
    let mut rows = statement.build_query_as::<Source>().fetch(&mut *read);
    while let Some(row) = rows.try_next().await? {
        ctx.check_cancelled()?;
        let key = (row.volume_id, row.dir_path.clone());
        if current_key
            .as_ref()
            .is_some_and(|existing| existing != &key)
        {
            FolderWriter {
                ctx,
                names: &names,
                links: &links,
                sources: &sources,
                policy,
                directory: &mut directory,
                conflict_conn: &mut conflict_conn,
                budget: &mut hash_budget,
                writer: &mut writer,
                stats: &mut stats,
                warnings: &mut warning_counts,
            }
            .write(std::mem::take(&mut current))
            .await?;
            ctx.check_cancelled()?;
            directory = None;
        }
        current_key = Some(key);
        current.push(row);
    }
    drop(rows);
    if !current.is_empty() {
        FolderWriter {
            ctx,
            names: &names,
            links: &links,
            sources: &sources,
            policy,
            directory: &mut directory,
            conflict_conn: &mut conflict_conn,
            budget: &mut hash_budget,
            writer: &mut writer,
            stats: &mut stats,
            warnings: &mut warning_counts,
        }
        .write(current)
        .await?;
    }
    read.commit().await?;
    let items = writer.finish().await?;

    let incomplete: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM move_items m WHERE m.plan_id = ? AND NOT EXISTS(SELECT 1 FROM plan_file_identities i WHERE i.plan_id = m.plan_id AND i.file_id = m.file_id AND i.live = 1))",
    )
    .bind(ctx.plan_id)
    .fetch_one(ctx.db.read())
    .await?;
    if incomplete {
        return Err(PlanError::Internal(anyhow::anyhow!(
            "rename snapshot identity is incomplete"
        )));
    }

    warning_counts.insert("hash_unknown", hash_budget.unknown);
    let mut built = Built {
        items,
        stats,
        warnings: Vec::new(),
    };
    for (code, count) in warning_counts {
        let message = match code {
            "metadata_pending" => "Files awaiting metadata are not renamed; index them and re-plan",
            "date_from_file_time" => "The rename pattern uses file birth or modification time",
            "pattern_failed" => "The pattern could not produce a valid file name",
            "reserved_destination" => {
                "Reserved or ignored source/destination paths are not renamed"
            }
            "skipped_by_policy" => {
                "Conflicting files were left unchanged by the selected conflict policy"
            }
            "hash_unknown" => {
                "Content could not be hashed within the build budget; conflicts are renamed"
            }
            _ => {
                return Err(PlanError::Internal(anyhow::anyhow!(
                    "unknown rename warning"
                )));
            }
        };
        built = built.warning(code, message, count);
    }
    Ok(built)
}
