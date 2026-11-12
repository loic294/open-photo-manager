use std::collections::{HashMap, HashSet};

use opm_core::paths::FileId;
use opm_core::sidecar::LinkKind;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use super::Source;
use crate::fsops::{SidecarAction, SidecarItem};
use crate::plans::PlanError;

#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub source: Source,
    pub kind: LinkKind,
}

pub(crate) async fn links(
    conn: &mut SqliteConnection,
    ids: &[FileId],
) -> Result<HashMap<FileId, Vec<Link>>, PlanError> {
    let mut result: HashMap<FileId, Vec<Link>> = HashMap::new();
    for chunk in ids.chunks(30_000) {
        let mut qb = QueryBuilder::<Sqlite>::new(format!(
            "SELECT l.file_id AS owner, l.kind AS link_kind, {} FROM file_sidecars l JOIN files f ON f.id = l.sidecar_id LEFT JOIN cameras cam ON cam.id = f.camera_id WHERE l.file_id IN (",
            super::dest::COLUMNS
        ));
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(*id);
        }
        sep.push_unseparated(")");
        for row in qb.build().fetch_all(&mut *conn).await? {
            use sqlx::{FromRow, Row};
            let owner: FileId = row.try_get("owner")?;
            let kind: String = row.try_get("link_kind")?;
            let kind = match kind.as_str() {
                "exact" => LinkKind::Exact,
                "stem" => LinkKind::Stem,
                _ => {
                    return Err(PlanError::Internal(anyhow::anyhow!(
                        "invalid sidecar link kind"
                    )));
                }
            };
            result.entry(owner).or_default().push(Link {
                source: Source::from_row(&row)?,
                kind,
            });
        }
    }
    Ok(result)
}

/// Decisions are frozen from the LIVE links and the effective execution, before the first
/// transfer relinks folders. Each successful owner carries metadata immediately. FsOps
/// deduplicates identical same-path copies; the final carrier moves only while all owners left.
pub(super) async fn carriers(
    conn: &mut SqliteConnection,
    items: &[super::Item],
) -> Result<HashMap<FileId, Vec<SidecarItem>>, PlanError> {
    let ids: Vec<_> = items.iter().map(|i| i.file_id).collect();
    let links = links(conn, &ids).await?;
    let executing: HashSet<_> = ids.iter().copied().collect();
    let mut groups: HashMap<FileId, Vec<(usize, &Link)>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(links) = links.get(&item.file_id) {
            for link in links {
                groups
                    .entry(link.source.id)
                    .or_default()
                    .push((index, link));
            }
        }
    }
    let mut result: HashMap<FileId, Vec<SidecarItem>> = HashMap::new();
    for (sidecar, group) in groups {
        let owners: Vec<FileId> = sqlx::query_scalar(
            "SELECT file_id FROM file_sidecars WHERE sidecar_id = ? ORDER BY file_id",
        )
        .bind(sidecar)
        .fetch_all(&mut *conn)
        .await?;
        let shared = owners.len() > 1;
        let all = owners.iter().all(|id| executing.contains(id));
        let last = group.last().map(|(index, _)| *index);
        for (index, link) in group {
            let action = if !shared || (all && Some(index) == last) {
                SidecarAction::Move
            } else {
                SidecarAction::Copy
            };
            result
                .entry(items[index].file_id)
                .or_default()
                .push(SidecarItem {
                    file_id: Some(sidecar),
                    src: link.source.loc()?,
                    kind: link.kind,
                    action,
                });
        }
    }
    Ok(result)
}
