//! Volumes bound to ISM roots. OPM never changes files on them itself: the local paths refuse
//! with `ism_managed`, and the operations that can be expressed as ISM actions (moves, renames,
//! new folders, trash) are sent to the sync manager in batches through a [`Delegate`]. The index
//! follows only what ISM reports as done.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use opm_core::conflict::unique_within;
use opm_core::paths::{FileId, RelPath, VolumeId};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::context::OpContext;
use super::dirs::{is_ism_private_path, is_taken_or_absent};
use super::error::FsOpError;
use super::executor::{hash_file, stat_source};
use super::stat::Expect;
use super::{FsOps, Loc, blocking};
use crate::db::Db;
use crate::indexer::{NoopProgress, RunContext, ScanRequest, ScanTarget};

/// Where a bound volume sits in its ISM root: `prefix` is the volume root relative to the ISM
/// root ('' when they are the same folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmBinding {
    pub root_id: String,
    pub prefix: String,
}

/// A registry must be installed before file operations and remain stable while they run.
pub trait ManagedVolumes: Send + Sync {
    fn manages(&self, volume_id: VolumeId) -> bool;

    fn binding(&self, _volume_id: VolumeId) -> Option<IsmBinding> {
        None
    }

    /// The volume bound to `root_id`, with its prefix.
    fn volume_of_root(&self, _root_id: &str) -> Option<(VolumeId, String)> {
        None
    }
}

/// Bindings are loaded before recovery; a missing or unreadable table prevents startup.
pub struct ManagedVolumeSnapshot(HashMap<VolumeId, IsmBinding>);

impl ManagedVolumeSnapshot {
    pub async fn load(db: &Db) -> anyhow::Result<Self> {
        let mut tx = db.read().begin().await?;
        let rows: Vec<(VolumeId, String, String)> =
            sqlx::query_as("SELECT volume_id, root_id, root_prefix FROM ism_volume_roots")
                .fetch_all(&mut *tx)
                .await?;
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM ism_volume_roots b
                WHERE EXISTS(SELECT 1 FROM fs_journal j
                    WHERE j.state = 'pending'
                    AND (j.src_volume_id = b.volume_id OR j.dst_volume_id = b.volume_id))
                AND EXISTS(SELECT 1 FROM fs_journal j
                    WHERE j.state = 'pending'
                    AND (j.src_volume_id = b.volume_id OR j.dst_volume_id = b.volume_id)
                    AND NOT EXISTS(SELECT 1 FROM tasks t
                        WHERE t.id = j.task_id
                        AND json_extract(t.params_json, '$.local_only') = 1))
            )",
        )
        .fetch_one(&mut *tx)
        .await?;
        if pending {
            anyhow::bail!(
                "ISM-managed volumes have unresolved legacy work; settle it before enabling the binding"
            );
        }
        tx.commit().await?;
        Ok(Self(
            rows.into_iter()
                .map(|(volume_id, root_id, prefix)| (volume_id, IsmBinding { root_id, prefix }))
                .collect(),
        ))
    }
}

impl ManagedVolumes for ManagedVolumeSnapshot {
    fn manages(&self, volume_id: VolumeId) -> bool {
        self.0.contains_key(&volume_id)
    }

    fn binding(&self, volume_id: VolumeId) -> Option<IsmBinding> {
        self.0.get(&volume_id).cloned()
    }

    fn volume_of_root(&self, root_id: &str) -> Option<(VolumeId, String)> {
        self.0
            .iter()
            .find(|(_, binding)| binding.root_id == root_id)
            .map(|(volume_id, binding)| (*volume_id, binding.prefix.clone()))
    }
}

/// A place in an ISM root, as the client API names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsmLocation {
    pub root_id: String,
    pub path: String,
}

/// One ISM action, serialized exactly as the operations API expects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IsmAction {
    Move {
        source: IsmLocation,
        target: IsmLocation,
        expected_revision: String,
    },
    Rename {
        source: IsmLocation,
        target: IsmLocation,
        expected_revision: String,
    },
    CreateFolder {
        target: IsmLocation,
    },
    MoveFolder {
        source: IsmLocation,
        target: IsmLocation,
    },
    Trash {
        source: IsmLocation,
        /// Servers that do not check it yet ignore it.
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_revision: Option<String>,
    },
    TrashFolder {
        source: IsmLocation,
        /// The files OPM saw in the folder with their revisions (folder cleanup: only junk);
        /// ISM refuses when the folder holds anything else. Servers that do not check it yet
        /// ignore it.
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_entries: Option<Vec<ExpectedEntry>>,
    },
    Restore {
        trash_id: String,
        #[serde(skip)]
        root_id: String,
    },
    Purge {
        trash_id: String,
        #[serde(skip)]
        root_id: String,
    },
}

impl IsmAction {
    /// The roots the action touches.
    pub fn roots(&self) -> Vec<&str> {
        match self {
            Self::Move { source, target, .. }
            | Self::Rename { source, target, .. }
            | Self::MoveFolder { source, target } => {
                vec![source.root_id.as_str(), target.root_id.as_str()]
            }
            Self::CreateFolder { target } => vec![target.root_id.as_str()],
            Self::Trash { source, .. } | Self::TrashFolder { source, .. } => {
                vec![source.root_id.as_str()]
            }
            Self::Restore { root_id, .. } | Self::Purge { root_id, .. } => vec![root_id.as_str()],
        }
    }

    /// The token grant the action needs besides `manage`.
    pub fn grant(&self) -> Option<&'static str> {
        match self {
            Self::Trash { .. } | Self::TrashFolder { .. } | Self::Restore { .. } => Some("trash"),
            Self::Purge { .. } => Some("purge"),
            _ => None,
        }
    }
}

/// The actions of one ISM operation.
#[derive(Debug, Clone)]
pub struct IsmBatch {
    pub label: String,
    /// Names the caller (`opm:plan:<id>`); the idempotency key adds a unique suffix.
    pub key_prefix: String,
    pub actions: Vec<IsmAction>,
}

/// A file ISM reports as moved (`to` is `None` or private when it went to the ISM trash).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IsmFile {
    pub from: Option<IsmLocation>,
    pub to: Option<IsmLocation>,
}

/// How ISM settled the action at `index` of the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmItem {
    pub index: usize,
    pub outcome: Result<Vec<IsmFile>, FsOpError>,
}

/// The sync manager seen from the file operations.
#[async_trait]
pub trait Delegate: Send + Sync {
    /// Runs `batch` as one ISM operation and reports every action exactly once on `items`, as
    /// soon as ISM settles it. Must never change files itself and never retry a refusal.
    /// Cancelling `cancel` forwards the cancel to ISM; actions it did not start come back as
    /// `cancelled`.
    async fn run(
        &self,
        batch: IsmBatch,
        cancel: CancellationToken,
        items: mpsc::UnboundedSender<IsmItem>,
    );
}

/// A change OPM asks ISM to make, in OPM's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delegated {
    Move {
        src: Loc,
        dst: Loc,
        revision: String,
    },
    /// `revision` is `None` only when the caller has no index facts to check against.
    Trash {
        loc: Loc,
        revision: Option<String>,
    },
    /// `entries`: the files the caller checked (folder cleanup), `None` for a folder the user
    /// trashes whole.
    TrashFolder {
        loc: Loc,
        entries: Option<Vec<ExpectedEntry>>,
    },
    MoveFolder {
        src: Loc,
        dst: Loc,
    },
    CreateFolder(Loc),
    /// An item of the ISM trash; `loc` is where it comes back (`None` outside the bound volumes).
    Restore {
        trash_id: String,
        root_id: String,
        loc: Option<Loc>,
        is_dir: bool,
    },
    Purge {
        trash_id: String,
        root_id: String,
    },
}

/// What the index learns from a committed action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExternalChange {
    Moved { from: Loc, to: Loc },
    Removed(Loc),
    RemovedDir(Loc),
    MovedDir { from: Loc, to: Loc },
    CreatedDir(Loc),
}

/// A committed action: where the moved file is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegatedDone {
    pub moved_to: Option<Loc>,
}

pub(crate) type OnDelegated<'a> =
    &'a mut (dyn FnMut(usize, Result<DelegatedDone, FsOpError>) + Send);

fn not_connected() -> FsOpError {
    FsOpError::delegated(
        "ism_not_connected",
        "This volume is managed by the sync manager, which is not connected",
    )
}

/// A file a folder must still hold, exactly, for ISM to trash the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectedEntry {
    /// Relative to the folder, `/`-separated.
    pub path: String,
    pub revision: String,
}

/// `<size>-<mtime_ns>` as ISM computes it, or `None` when it cannot be written exactly.
pub(crate) fn revision(size: u64, mtime: i64, nanos: i64) -> Option<String> {
    if mtime < 0 || !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    let ns = i128::from(mtime) * 1_000_000_000 + i128::from(nanos);
    Some(format!("{size}-{ns}"))
}

impl FsOps {
    /// Installs the sync manager that runs the changes of bound volumes.
    #[must_use]
    pub fn with_delegate(mut self, delegate: Arc<dyn Delegate>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    pub fn is_managed(&self, volume_id: VolumeId) -> bool {
        self.managed_volumes
            .as_ref()
            .is_some_and(|m| m.manages(volume_id))
    }

    /// Where a bound volume sits in its ISM root.
    pub(crate) fn ism_binding(&self, volume_id: VolumeId) -> Option<IsmBinding> {
        self.managed_volumes.as_ref()?.binding(volume_id)
    }

    /// The local place of an ISM location, when it is inside a bound volume.
    pub(crate) fn local_of_ism(&self, root_id: &str, path: &str) -> Option<Loc> {
        self.local_of(&IsmLocation {
            root_id: root_id.to_owned(),
            path: path.to_owned(),
        })
    }

    fn ism_location(&self, loc: &Loc) -> Result<IsmLocation, FsOpError> {
        let binding = self
            .managed_volumes
            .as_ref()
            .and_then(|m| m.binding(loc.volume_id))
            .ok_or(FsOpError::IsmManaged)?;
        let path = match (binding.prefix.is_empty(), loc.rel.is_root()) {
            (true, _) => loc.rel.as_str().to_owned(),
            (false, true) => binding.prefix,
            (false, false) => format!("{}/{}", binding.prefix, loc.rel.as_str()),
        };
        Ok(IsmLocation {
            root_id: binding.root_id,
            path,
        })
    }

    fn local_of(&self, at: &IsmLocation) -> Option<Loc> {
        let (volume_id, prefix) = self.managed_volumes.as_ref()?.volume_of_root(&at.root_id)?;
        let path = at.path.trim_matches('/');
        let rest = if prefix.is_empty() {
            path
        } else if path == prefix {
            ""
        } else {
            path.strip_prefix(&prefix)?.strip_prefix('/')?
        };
        let rel = RelPath::new(rest).ok()?;
        (!is_ism_private_path(&rel)).then(|| Loc::new(volume_id, rel))
    }

    /// The ISM revision of a source, read-only, after checking it is still what the index says.
    ///
    /// # Errors
    /// `source_changed` when it no longer matches `expect`, `source_missing`, `volume_offline`,
    /// or `revision_unavailable` (fail closed) when the time cannot be written exactly.
    pub(crate) async fn revision_of(&self, loc: &Loc, expect: Expect) -> Result<String, FsOpError> {
        let root = self.volume(loc.volume_id)?;
        let rel = loc.rel.clone();
        let stat = blocking(move || stat_source(&root, &rel, Some(expect))).await?;
        revision(stat.size, stat.mtime, stat.mtime_nanos).ok_or_else(|| {
            FsOpError::delegated(
                "revision_unavailable",
                "The file time cannot be sent to the sync manager exactly",
            )
        })
    }

    /// Whether something is at `loc` (read-only).
    pub(crate) async fn exists(&self, loc: &Loc) -> Result<bool, FsOpError> {
        if loc.rel.is_root() {
            return Ok(true);
        }
        let root = self.volume(loc.volume_id)?;
        let (dir, name) = (loc.dir(), loc.name().to_owned());
        blocking(move || is_taken_or_absent(&root, &dir, &name)).await
    }

    /// Whether two files have the same content (read-only; sizes first, then SHA-1).
    pub(crate) async fn identical(
        &self,
        a: &Loc,
        b: &Loc,
        cancel: &CancellationToken,
    ) -> Result<bool, FsOpError> {
        let (root_a, root_b) = (self.volume(a.volume_id)?, self.volume(b.volume_id)?);
        let (rel_a, rel_b, cancel) = (a.rel.clone(), b.rel.clone(), cancel.clone());
        blocking(move || {
            let stat_a = stat_source(&root_a, &rel_a, None)?;
            let stat_b = match stat_source(&root_b, &rel_b, None) {
                Ok(stat) => stat,
                Err(FsOpError::NotARegularFile) => return Ok(false),
                Err(err) => return Err(err),
            };
            if stat_a.size != stat_b.size {
                return Ok(false);
            }
            Ok(hash_file(&root_a, &rel_a, &stat_a, &cancel)?
                == hash_file(&root_b, &rel_b, &stat_b, &cancel)?)
        })
        .await
    }

    /// The first free name for `loc` that nothing uses and no queued action claimed.
    pub(crate) async fn free_loc(
        &self,
        loc: &Loc,
        reserved: &HashSet<Loc>,
    ) -> Result<Loc, FsOpError> {
        let root = self.volume(loc.volume_id)?;
        let max = self.rules().name_max_len();
        let (dir, name, volume_id) = (loc.dir(), loc.name().to_owned(), loc.volume_id);
        let reserved: HashSet<RelPath> = reserved
            .iter()
            .filter(|other| other.volume_id == volume_id)
            .map(|other| other.rel.clone())
            .collect();
        let found = blocking(move || {
            let mut failure = None;
            let found = unique_within(&name, max, |candidate| {
                if failure.is_some() {
                    return true;
                }
                let Ok(rel) = dir.join(candidate) else {
                    return true;
                };
                if reserved.contains(&rel) {
                    return true;
                }
                match is_taken_or_absent(&root, &dir, candidate) {
                    Ok(taken) => taken,
                    Err(err) => {
                        failure = Some(err);
                        true
                    }
                }
            });
            if let Some(err) = failure {
                return Err(err);
            }
            found.map_err(|_| FsOpError::DestinationInvalid("no free name was found"))
        })
        .await?;
        loc.with_name(&found)
    }

    fn wire(&self, action: &Delegated) -> Result<IsmAction, FsOpError> {
        Ok(match action {
            Delegated::Move { src, dst, revision } => {
                let (source, target) = (self.ism_location(src)?, self.ism_location(dst)?);
                let expected_revision = revision.clone();
                if src.volume_id == dst.volume_id && src.dir() == dst.dir() {
                    IsmAction::Rename {
                        source,
                        target,
                        expected_revision,
                    }
                } else {
                    IsmAction::Move {
                        source,
                        target,
                        expected_revision,
                    }
                }
            }
            Delegated::Trash { loc, revision } => IsmAction::Trash {
                source: self.ism_location(loc)?,
                expected_revision: revision.clone(),
            },
            Delegated::TrashFolder { loc, entries } => IsmAction::TrashFolder {
                source: self.ism_location(loc)?,
                expected_entries: entries.clone(),
            },
            Delegated::MoveFolder { src, dst } => {
                if src.volume_id != dst.volume_id {
                    return Err(FsOpError::delegated(
                        "ism_cross_volume_folder",
                        "Folders move only within one volume",
                    ));
                }
                IsmAction::MoveFolder {
                    source: self.ism_location(src)?,
                    target: self.ism_location(dst)?,
                }
            }
            Delegated::CreateFolder(loc) => IsmAction::CreateFolder {
                target: self.ism_location(loc)?,
            },
            Delegated::Restore {
                trash_id, root_id, ..
            } => IsmAction::Restore {
                trash_id: trash_id.clone(),
                root_id: root_id.clone(),
            },
            Delegated::Purge { trash_id, root_id } => IsmAction::Purge {
                trash_id: trash_id.clone(),
                root_id: root_id.clone(),
            },
        })
    }

    /// The index changes ISM's report implies, and the folders to rescan for what OPM cannot
    /// map back (a file moved outside the bound volumes).
    fn external_changes(
        &self,
        action: &Delegated,
        files: &[IsmFile],
    ) -> (Vec<ExternalChange>, Option<Loc>, Vec<(Loc, bool)>) {
        let mut changes = Vec::new();
        let mut rescan = Vec::new();
        let mut moved_to = None;
        match action {
            Delegated::Move { src, dst, .. } => {
                for file in files {
                    let from = file.from.as_ref().and_then(|at| self.local_of(at));
                    let to = file.to.as_ref().and_then(|at| self.local_of(at));
                    match (from, to) {
                        (Some(from), Some(to)) => {
                            if &from == src {
                                moved_to = Some(to.clone());
                            }
                            changes.push(ExternalChange::Moved { from, to });
                        }
                        (Some(from), None) => {
                            rescan.push((Loc::new(from.volume_id, from.dir()), false));
                            changes.push(ExternalChange::Removed(from));
                        }
                        (None, Some(to)) => rescan.push((Loc::new(to.volume_id, to.dir()), false)),
                        (None, None) => {}
                    }
                }
                if moved_to.is_none() {
                    changes.push(ExternalChange::Moved {
                        from: src.clone(),
                        to: dst.clone(),
                    });
                    moved_to = Some(dst.clone());
                }
            }
            Delegated::Trash { loc, .. } => {
                changes.push(ExternalChange::Removed(loc.clone()));
                for from in files
                    .iter()
                    .filter_map(|file| file.from.as_ref().and_then(|at| self.local_of(at)))
                    .filter(|from| from != loc)
                {
                    changes.push(ExternalChange::Removed(from));
                }
            }
            Delegated::TrashFolder { loc, .. } => {
                changes.push(ExternalChange::RemovedDir(loc.clone()));
            }
            Delegated::MoveFolder { src, dst } => {
                changes.push(ExternalChange::MovedDir {
                    from: src.clone(),
                    to: dst.clone(),
                });
                // Companions and names ISM adjusted are picked up by reading the folder again.
                rescan.push((dst.clone(), true));
            }
            Delegated::CreateFolder(loc) => changes.push(ExternalChange::CreatedDir(loc.clone())),
            Delegated::Restore { loc, is_dir, .. } => {
                let restored = files
                    .iter()
                    .filter_map(|file| file.to.as_ref().and_then(|at| self.local_of(at)));
                for to in restored.chain(loc.clone()) {
                    rescan.push((Loc::new(to.volume_id, to.dir()), false));
                    if *is_dir {
                        rescan.push((to, true));
                    }
                }
            }
            Delegated::Purge { .. } => {}
        }
        (changes, moved_to, rescan)
    }

    /// Runs `actions` as one ISM operation. `on_item` hears about each action (by its index in
    /// `actions`) as soon as ISM settles it and the index followed; nothing is ever changed
    /// locally. Actions that cannot be expressed fail on their own.
    pub(crate) async fn delegate(
        &self,
        label: &str,
        key_prefix: &str,
        actions: &[Delegated],
        cx: &OpContext,
        on_item: OnDelegated<'_>,
    ) {
        if cx.local_only {
            for (index, _) in actions.iter().enumerate() {
                on_item(
                    index,
                    Err(FsOpError::delegated(
                        "local_only",
                        "Local-only changes cannot be sent to the sync manager",
                    )),
                );
            }
            return;
        }
        let mut wire = Vec::new();
        let mut indexes = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            match self.wire(action) {
                Ok(action) => {
                    wire.push(action);
                    indexes.push(index);
                }
                Err(err) => on_item(index, Err(err)),
            }
        }
        if wire.is_empty() {
            return;
        }
        let Some(delegate) = self.delegate.clone() else {
            for index in indexes {
                on_item(index, Err(not_connected()));
            }
            return;
        };
        let batch = IsmBatch {
            label: label.to_owned(),
            key_prefix: key_prefix.to_owned(),
            actions: wire,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel::<IsmItem>();
        let rules = self.rules();
        let mut moved: Vec<FileId> = Vec::new();
        let mut dest_dirs = BTreeSet::new();
        let mut rescan: BTreeSet<(VolumeId, RelPath, bool)> = BTreeSet::new();
        let mut reported = vec![false; indexes.len()];
        let follow = async {
            while let Some(item) = receiver.recv().await {
                let Some(&index) = indexes.get(item.index) else {
                    continue;
                };
                if std::mem::replace(&mut reported[item.index], true) {
                    continue;
                }
                let files = match item.outcome {
                    Ok(files) => files,
                    Err(err) => {
                        on_item(index, Err(err));
                        continue;
                    }
                };
                let (changes, moved_to, unmapped) = self.external_changes(&actions[index], &files);
                for (loc, recursive) in unmapped {
                    rescan.insert((loc.volume_id, loc.rel, recursive));
                }
                match self.record_external(&rules, &changes).await {
                    Ok(ids) => moved.extend(ids),
                    Err(err) => {
                        // The files did change: the index catches up through a rescan.
                        tracing::warn!(error = %err, "recording a sync manager change failed; rescanning");
                        for change in &changes {
                            for (volume_id, dir) in touched(change) {
                                rescan.insert((volume_id, dir, false));
                            }
                            if let ExternalChange::MovedDir { to: dir, .. }
                            | ExternalChange::CreatedDir(dir) = change
                            {
                                rescan.insert((dir.volume_id, dir.rel.clone(), true));
                            }
                        }
                    }
                }
                let mut dirs = Vec::new();
                for change in &changes {
                    dirs.extend(touched(change));
                    if let ExternalChange::Moved { to, .. } = change {
                        dest_dirs.insert((to.volume_id, to.dir()));
                    }
                }
                self.announce(cx, &dirs);
                on_item(index, Ok(DelegatedDone { moved_to }));
            }
        };
        tokio::join!(delegate.run(batch, cx.cancel.clone(), sender), follow);
        for (position, done) in reported.iter().enumerate() {
            if !done && let Some(&index) = indexes.get(position) {
                on_item(
                    index,
                    Err(FsOpError::delegated(
                        "ism_error",
                        "The sync manager did not report this change",
                    )),
                );
            }
        }
        if !moved.is_empty()
            && let Err(err) = self.apply_group_fixup(&rules, &moved, &dest_dirs).await
        {
            tracing::warn!(error = %err, "relinking after a sync manager move failed");
        }
        if !rescan.is_empty() {
            let request = ScanRequest {
                targets: rescan
                    .into_iter()
                    .map(|(volume_id, dir, recursive)| ScanTarget {
                        volume_id,
                        dir,
                        recursive,
                    })
                    .collect(),
                ..ScanRequest::default()
            };
            let run = RunContext::new(
                cx.cancel.clone(),
                Arc::new(NoopProgress),
                cx.changes.clone(),
            );
            if let Err(err) = self.indexer.scan(&request, &run).await {
                tracing::warn!(error = %err, "rescanning after a sync manager change failed");
            }
        }
    }

    /// One action on its own.
    pub(crate) async fn delegate_one(
        &self,
        label: &str,
        action: Delegated,
        cx: &OpContext,
    ) -> Result<DelegatedDone, FsOpError> {
        let mut outcome = None;
        self.delegate(
            label,
            "opm:fs",
            std::slice::from_ref(&action),
            cx,
            &mut |_, result| {
                outcome = Some(result);
            },
        )
        .await;
        outcome.unwrap_or_else(|| Err(not_connected()))
    }
}

/// The folders whose listing a change touched.
fn touched(change: &ExternalChange) -> Vec<(VolumeId, RelPath)> {
    match change {
        ExternalChange::Moved { from, to } => {
            vec![(from.volume_id, from.dir()), (to.volume_id, to.dir())]
        }
        ExternalChange::Removed(loc) => vec![(loc.volume_id, loc.dir())],
        ExternalChange::RemovedDir(loc) | ExternalChange::CreatedDir(loc) => {
            vec![(loc.volume_id, loc.dir()), (loc.volume_id, loc.rel.clone())]
        }
        ExternalChange::MovedDir { from, to } => vec![
            (from.volume_id, from.dir()),
            (from.volume_id, from.rel.clone()),
            (to.volume_id, to.dir()),
            (to.volume_id, to.rel.clone()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_size_and_nanoseconds() {
        assert_eq!(revision(12, 3, 4).as_deref(), Some("12-3000000004"));
        assert_eq!(revision(0, 0, 0).as_deref(), Some("0-0"));
    }

    #[test]
    fn a_revision_that_cannot_be_exact_is_refused() {
        assert_eq!(revision(1, -1, 0), None);
        assert_eq!(revision(1, 1, 1_000_000_000), None);
    }

    #[test]
    fn actions_serialize_as_the_operations_api_expects() {
        let at = |path: &str| IsmLocation {
            root_id: "photos".into(),
            path: path.into(),
        };
        let json = serde_json::to_value(IsmAction::Rename {
            source: at("a/b.jpg"),
            target: at("a/c.jpg"),
            expected_revision: "1-2".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "rename", "source": {"root_id": "photos", "path": "a/b.jpg"},
                "target": {"root_id": "photos", "path": "a/c.jpg"}, "expected_revision": "1-2"})
        );
        let json = serde_json::to_value(IsmAction::TrashFolder {
            source: at("x"),
            expected_entries: Some(vec![ExpectedEntry {
                path: ".DS_Store".into(),
                revision: "1-2".into(),
            }]),
        })
        .unwrap();
        assert_eq!(json["kind"], "trash_folder");
        assert_eq!(
            json["expected_entries"],
            serde_json::json!([{"path": ".DS_Store", "revision": "1-2"}])
        );
        let json = serde_json::to_value(IsmAction::Trash {
            source: at("a.jpg"),
            expected_revision: None,
        })
        .unwrap();
        assert!(json.get("expected_revision").is_none());
        let json = serde_json::to_value(IsmAction::Restore {
            trash_id: "t1".into(),
            root_id: "photos".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "restore", "trash_id": "t1"})
        );
    }
}
