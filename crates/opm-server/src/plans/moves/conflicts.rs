use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use opm_core::conflict::{self, ConflictDecision, ContentInfo};
use opm_core::dto::ConflictPolicy;
use opm_core::hash::{Sha1Digest, sha1_reader};
use opm_core::names::normalize_name;
use opm_core::paths::{FileId, RelPath, VolumeId};
use sqlx::SqliteConnection;

use super::{COLUMNS, Link, Source, Wanted};
use crate::plans::{BuildContext, PlanError};

const HASH_BYTES: u64 = 1024 * 1024 * 1024;
const HASH_TIME: Duration = Duration::from_secs(20);

pub(crate) struct HashBudget {
    bytes: u64,
    deadline: Instant,
    cache: HashMap<FileId, Option<Sha1Digest>>,
    pub unknown: u64,
}

impl HashBudget {
    pub fn new() -> Self {
        Self {
            bytes: 0,
            deadline: Instant::now() + HASH_TIME,
            cache: HashMap::new(),
            unknown: 0,
        }
    }
    async fn digest(
        &mut self,
        ctx: &BuildContext,
        row: &Source,
    ) -> Result<Option<Sha1Digest>, PlanError> {
        if let Some(hash) = row.digest()? {
            return Ok(Some(hash));
        }
        if let Some(hash) = self.cache.get(&row.id) {
            return Ok(*hash);
        }
        if self.bytes.saturating_add(row.size as u64) > HASH_BYTES
            || Instant::now() >= self.deadline
        {
            return Ok(None);
        }
        let root = ctx.volumes.get(row.volume_id)?;
        let path = row.loc()?.rel;
        let size = row.size;
        let mtime = row.mtime;
        let deadline = self.deadline;
        let cancel = ctx.cancel.clone();
        let remaining = HASH_BYTES - self.bytes;
        let (hash, bytes) = tokio::task::spawn_blocking(move || -> Result<_, PlanError> {
            let file = match root.open_read(&path) {
                Ok(file) => file,
                Err(e) => { tracing::debug!(code = "hash_unknown", error = %e, "move conflict hash unavailable"); return Ok((None, 0)); }
            };
            let before = file.metadata().map_err(anyhow::Error::from)?;
            use std::os::unix::fs::MetadataExt;
            if before.len() != size as u64 || before.mtime() != mtime { return Ok((None, 0)); }
            let mut bytes = 0;
            let hash = sha1_reader(&file, |n| {
                bytes += n;
                if cancel.is_cancelled() || Instant::now() >= deadline || bytes > remaining {
                    ControlFlow::Break(())
                } else { ControlFlow::Continue(()) }
            });
            let hash = match hash {
                Ok(hash) => hash,
                Err(e) => { tracing::debug!(code = "hash_unknown", error = %e, "move conflict hash failed"); None }
            };
            let after = file.metadata().map_err(anyhow::Error::from)?;
            Ok((hash.filter(|_| before.len() == after.len() && before.mtime() == after.mtime() && before.mtime_nsec() == after.mtime_nsec()), bytes))
        }).await.map_err(anyhow::Error::from)??;
        self.bytes += bytes;
        ctx.check_cancelled()?;
        self.cache.insert(row.id, hash);
        Ok(hash)
    }
}

#[derive(Default)]
pub(crate) struct Directory {
    files: HashMap<String, Source>,
    folders: HashSet<String>,
    claimed: HashMap<String, FileId>,
}

impl Directory {
    pub async fn load(
        conn: &mut SqliteConnection,
        volume: VolumeId,
        dir: &RelPath,
    ) -> Result<Self, PlanError> {
        let rows = sqlx::query_as::<_, Source>(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM files f LEFT JOIN cameras cam ON cam.id = f.camera_id WHERE f.volume_id = ? AND f.dir_path = ? ORDER BY f.name")))
            .bind(volume).bind(dir.as_str()).fetch_all(&mut *conn).await?;
        let folders: Vec<String> =
            sqlx::query_scalar("SELECT name FROM dirs WHERE volume_id = ? AND parent_path = ?")
                .bind(volume)
                .bind(dir.as_str())
                .fetch_all(conn)
                .await?;
        let mut directory = Self {
            folders: folders.into_iter().map(|s| normalize_name(&s)).collect(),
            ..Self::default()
        };
        for row in rows {
            let key = normalize_name(&row.name);
            match directory.files.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    // Multiple case/normalisation variants are ambiguous replace targets.
                    directory.folders.insert(entry.key().clone());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(row);
                }
            }
        }
        Ok(directory)
    }

    fn taken(&self, key: &str, allowed: FileId) -> bool {
        self.folders.contains(key)
            || self.files.get(key).is_some_and(|f| f.id != allowed)
            || self.claimed.get(key).is_some_and(|id| *id != allowed)
    }

    #[allow(clippy::too_many_arguments)] // One conflict decision includes the owner's companions and the build's shared budgets.
    pub async fn resolve(
        &mut self,
        ctx: &BuildContext,
        row: &Source,
        wanted: &Wanted,
        links: &[Link],
        policy: ConflictPolicy,
        sources: &HashSet<FileId>,
        budget: &mut HashBudget,
    ) -> Result<super::Resolved, PlanError> {
        let norm = normalize_name(&wanted.name);
        let existing = self.files.get(&norm).filter(|f| f.id != row.id);
        let claimed = self.claimed.contains_key(&norm);
        let folder = self.folders.contains(&norm);
        let mut incoming = ContentInfo {
            size: row.size as u64,
            hash: row.digest()?,
        };
        let mut info = existing
            .map(|f| {
                Ok::<_, PlanError>(ContentInfo {
                    size: f.size as u64,
                    hash: f.digest()?,
                })
            })
            .transpose()?;
        if conflict::needs_hashes(policy, info, incoming) && !claimed && !folder {
            incoming.hash = budget.digest(ctx, row).await?;
            if let Some(file) = existing {
                info = Some(ContentInfo {
                    size: file.size as u64,
                    hash: budget.digest(ctx, file).await?,
                });
            }
            if incoming.hash.is_none() || info.is_some_and(|i| i.hash.is_none()) {
                budget.unknown += 1;
            }
        }
        let mut decision = conflict::decide(policy, info, incoming);
        if claimed
            || folder
            || (policy == ConflictPolicy::Replace
                && existing.is_some_and(|f| sources.contains(&f.id)))
        {
            decision = if policy == ConflictPolicy::Skip {
                ConflictDecision::Skip(conflict::SkipReason::Policy)
            } else {
                ConflictDecision::Rename
            };
        }
        let status = match decision {
            ConflictDecision::Skip(conflict::SkipReason::Identical) => "identical_at_destination",
            ConflictDecision::Skip(conflict::SkipReason::Policy) => "skipped_by_policy",
            _ => "move",
        };
        if status != "move" {
            return Ok(Resolved {
                name: wanted.name.clone(),
                status,
                replaced_id: None,
            });
        }
        let tail = links
            .iter()
            .map(|l| conflict::sidecar_destination(l.kind, &row.name, "", &l.source.name).len())
            .max()
            .unwrap_or(0);
        let max = usize::from(ctx.settings.name_max_len).saturating_sub(tail);
        let free = |key: &str| {
            let owner_taken = self.taken(key, row.id);
            owner_taken
                || key.len() > max
                || links.iter().any(|link| {
                    let sidecar = conflict::sidecar_destination(
                        link.kind,
                        &normalize_name(&row.name),
                        key,
                        &normalize_name(&link.source.name),
                    );
                    self.taken(&sidecar, link.source.id)
                })
        };
        let name = if decision == ConflictDecision::Rename
            || wanted.name.len() > max
            || links.iter().any(|link| {
                let sidecar = normalize_name(&conflict::sidecar_destination(
                    link.kind,
                    &row.name,
                    &wanted.name,
                    &link.source.name,
                ));
                self.taken(&sidecar, link.source.id)
            }) {
            // For Rename the original name is explicitly occupied, including folder and
            // same-plan collisions that have no ContentInfo.
            conflict::unique_case_insensitive_within(&wanted.name, max, |key| {
                key == norm || free(key)
            })
            .map_err(|e| PlanError::invalid_field("destination", e.to_string()))?
        } else {
            wanted.name.clone()
        };
        let replaced_id = if decision == ConflictDecision::ReplaceExisting && name == wanted.name {
            existing.map(|f| f.id)
        } else {
            None
        };
        self.claimed.insert(normalize_name(&name), row.id);
        for link in links {
            self.claimed.insert(
                normalize_name(&conflict::sidecar_destination(
                    link.kind,
                    &row.name,
                    &name,
                    &link.source.name,
                )),
                link.source.id,
            );
        }
        Ok(Resolved {
            name,
            status,
            replaced_id,
        })
    }
}

pub(crate) struct Resolved {
    pub name: String,
    pub status: &'static str,
    pub replaced_id: Option<FileId>,
}
