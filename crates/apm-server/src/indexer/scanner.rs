//! Stage 1 of the indexer: brings `dirs`, `files` and `file_sidecars` in line with the disk.
//!
//! Owned by todo `implementing-scanner`. What a scan decides is described in the [`super`] docs;
//! this file is the driver. It plans the volumes and scopes of a request, runs the walker
//! ([`super::walk`]) on a blocking thread, compares every directory with its stored rows
//! ([`super::diff`]), has the changes written ([`super::store`]) and applies the removals at the end.
//!
//! # One volume
//! 1. Take the volume's lock and bump `scan_gen`.
//! 2. For every scope, in order: walk it and handle the listings in batches of up to 64 directories.
//!    A batch is compared with its stored rows (a few queries), written in transactions of whole
//!    directories (about a thousand rows each) and reported to the [`ChangeSink`] once the
//!    transaction committed.
//! 3. When every scope was walked to its end: remove what is gone, unless the guard holds it back.
//! 4. Record `last_scan_at` (scans of the whole volume) and the held count.
//!
//! A scan that is cancelled or fails stops between two transactions and removes nothing: step 3
//! only runs after the last listing was handled.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io;
use std::sync::Arc;

use apm_core::junk::JunkRules;
use apm_core::paths::{FileId, RelPath, VolumeId};
use apm_core::sidecar::SidecarMatcher;
use futures_util::StreamExt as _;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::changes::ChangeSink;
use super::diff::{self, StoredFile, guard_trips};
use super::progress::{IndexPhase, PhaseProgress};
use super::store::{self, DELETE_CHUNK, DirUnit, HeldUpdate, Stored};
use super::volumes::Lookup;
use super::walk::{self, DirListing, WalkEnd, WalkSpec};
use super::{HeldRemovals, IndexError, Indexer, RunContext, ScanRequest, ScanTarget};
use crate::db::subtree::Subtree;
use crate::db::{Db, DbError};
use crate::security::VolumeRoot;

/// Listings the walker may be ahead of the database.
const WALK_QUEUE: usize = 64;
/// Most listings that are compared and written together.
const BATCH_DIRS: usize = 64;
/// Rows after which a write transaction is closed.
const GROUP_ROWS: usize = 1000;
/// A scan that added, changed or removed more rows than this refreshes the planner statistics.
const OPTIMIZE_AFTER_ROWS: u64 = 100;

/// What a scan did, as counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanSummary {
    /// Volumes that were scanned (an offline volume is not).
    pub volumes_scanned: u64,
    /// Volumes that were skipped because they are offline; their rows are untouched.
    pub volumes_offline: u64,
    /// Directories that were listed.
    pub dirs_visited: u64,
    /// Directories that got a row.
    pub dirs_added: u64,
    /// Directories whose row was removed (with their files).
    pub dirs_removed: u64,
    /// Files found on disk in the directories that were listed.
    pub files_seen: u64,
    pub added: u64,
    pub changed: u64,
    pub unchanged: u64,
    /// File rows removed because the file is gone.
    pub removed: u64,
    /// File rows the mass-disappearance guard kept; see the [`super`] docs.
    pub held: u64,
    /// Entries skipped because their name is not valid UTF-8.
    pub skipped_invalid: u64,
    /// Directories that exist but could not be read; their rows are left as they are.
    pub unreadable_dirs: u64,
}

impl ScanSummary {
    fn merge(&mut self, other: &ScanSummary) {
        self.volumes_scanned += other.volumes_scanned;
        self.volumes_offline += other.volumes_offline;
        self.dirs_visited += other.dirs_visited;
        self.dirs_added += other.dirs_added;
        self.dirs_removed += other.dirs_removed;
        self.files_seen += other.files_seen;
        self.added += other.added;
        self.changed += other.changed;
        self.unchanged += other.unchanged;
        self.removed += other.removed;
        self.held += other.held;
        self.skipped_invalid += other.skipped_invalid;
        self.unreadable_dirs += other.unreadable_dirs;
    }
}

/// One directory of a volume to walk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    dir: RelPath,
    recursive: bool,
}

/// What to do on one volume: its scopes, parents before children, none inside a recursive one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumePlan {
    volume_id: VolumeId,
    scopes: Vec<Scope>,
}

impl VolumePlan {
    /// Does a scope cover the whole volume?
    fn whole_volume(&self) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.dir.is_root() && scope.recursive)
    }
}

/// Groups the targets by volume and drops the ones that are already covered. No targets means
/// every volume in `online`, completely.
fn plan_targets(targets: &[ScanTarget], online: &[VolumeId]) -> Vec<VolumePlan> {
    if targets.is_empty() {
        return online
            .iter()
            .map(|&volume_id| VolumePlan {
                volume_id,
                scopes: vec![Scope {
                    dir: RelPath::root(),
                    recursive: true,
                }],
            })
            .collect();
    }
    let mut by_volume: BTreeMap<VolumeId, Vec<Scope>> = BTreeMap::new();
    for target in targets {
        by_volume.entry(target.volume_id).or_default().push(Scope {
            dir: target.dir.clone(),
            recursive: target.recursive,
        });
    }
    by_volume
        .into_iter()
        .map(|(volume_id, scopes)| VolumePlan {
            volume_id,
            scopes: merge_scopes(scopes),
        })
        .collect()
}

/// Sorts scopes (a directory before what lies below it, a recursive scope before a non-recursive
/// one of the same directory) and drops every scope that a kept one already covers.
fn merge_scopes(mut scopes: Vec<Scope>) -> Vec<Scope> {
    scopes.sort_by(|a, b| {
        a.dir
            .cmp(&b.dir)
            .then_with(|| b.recursive.cmp(&a.recursive))
    });
    let mut kept: Vec<Scope> = Vec::new();
    for scope in scopes {
        let covered = kept.iter().any(|other| {
            other.dir == scope.dir || (other.recursive && scope.dir.starts_with(&other.dir))
        });
        if !covered {
            kept.push(scope);
        }
    }
    kept
}

/// The directories among `vanished` that have no ancestor in the set: removing those (with
/// everything below them) removes all the others.
fn reduce_to_topmost(vanished: &BTreeSet<RelPath>) -> Vec<RelPath> {
    vanished
        .iter()
        .filter(|dir| !dir.ancestors().any(|ancestor| vanished.contains(&ancestor)))
        .cloned()
        .collect()
}

/// `paths` as directories, plus their parents when `with_parents`; paths that are not valid
/// directories (never the case for stored ones) are left out.
fn rel_paths(paths: &BTreeSet<String>, with_parents: bool) -> BTreeSet<RelPath> {
    let mut dirs = BTreeSet::new();
    for path in paths {
        let Ok(dir) = RelPath::new(path) else {
            continue;
        };
        if with_parents {
            dirs.extend(dir.parent());
        }
        dirs.insert(dir);
    }
    dirs
}

/// The more important of two failures: anything but a cancellation wins over a cancellation (a
/// failing volume cancels the others).
fn worse(current: Option<IndexError>, new: IndexError) -> IndexError {
    match current {
        Some(current) if !matches!(current, IndexError::Cancelled) => current,
        _ => new,
    }
}

/// What every volume of one scan shares.
struct Shared {
    indexer: Indexer,
    matcher: SidecarMatcher,
    junk: JunkRules,
    progress: PhaseProgress,
    changes: Arc<dyn ChangeSink>,
    /// Cancelled by the caller, or by the first volume that fails.
    cancel: CancellationToken,
    force: bool,
    confirm: bool,
    /// The request covers whole volumes only, so the rows they had are a fair estimate of the work.
    estimate_total: bool,
}

impl Indexer {
    /// Stage 1 of a run: walks the requested directories and brings the index in line with the
    /// disk. See the [`super`] docs for what it decides.
    ///
    /// Volumes are scanned at most `settings.index.concurrency` at a time, directories one after
    /// the other. An offline volume is skipped (and counted), a volume id that does not exist
    /// fails the request before anything is scanned.
    pub async fn scan(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
    ) -> Result<ScanSummary, IndexError> {
        let settings = self.settings().get();
        let shared = Shared {
            indexer: self.clone(),
            matcher: SidecarMatcher::new(&settings.sidecar_extensions),
            junk: JunkRules::new(&settings.junk_names, &settings.junk_prefixes),
            progress: PhaseProgress::new(Arc::clone(&ctx.progress), IndexPhase::Walk),
            changes: Arc::clone(&ctx.changes),
            cancel: ctx.cancel.child_token(),
            force: req.force,
            confirm: req.confirm_removals,
            estimate_total: req
                .targets
                .iter()
                .all(|target| target.recursive && target.dir.is_root()),
        };
        let concurrency = usize::from(settings.index.concurrency).max(1);
        let result = scan_volumes(&shared, req, concurrency).await;
        shared.progress.finish();
        result
    }

    /// The removals the mass-disappearance guard is holding back, per volume. A scan with
    /// `confirm_removals` applies them.
    pub async fn held_removals(&self) -> Result<Vec<HeldRemovals>, DbError> {
        let held = store::held_removals(self.db()).await?;
        Ok(held
            .into_iter()
            .map(|(volume_id, volume_name, files)| HeldRemovals {
                volume_id,
                volume_name,
                files,
            })
            .collect())
    }
}

async fn scan_volumes(
    shared: &Shared,
    req: &ScanRequest,
    concurrency: usize,
) -> Result<ScanSummary, IndexError> {
    let volumes = shared.indexer.volumes();
    let online: Vec<VolumeId> = volumes.online().iter().map(|volume| volume.id).collect();
    let mut summary = ScanSummary::default();
    if req.targets.is_empty() {
        let offline = volumes.list().len().saturating_sub(online.len());
        summary.volumes_offline = offline as u64;
    }

    let mut runnable = Vec::new();
    for plan in plan_targets(&req.targets, &online) {
        match volumes.lookup(plan.volume_id) {
            Lookup::Online(root) => runnable.push((plan, root)),
            Lookup::Offline => {
                tracing::warn!(
                    volume_id = plan.volume_id,
                    "the volume is offline, not scanning it"
                );
                summary.volumes_offline += 1;
            }
            Lookup::Unknown => return Err(IndexError::UnknownVolume(plan.volume_id)),
        }
    }

    let mut results = futures_util::stream::iter(runnable)
        .map(|(plan, root)| scan_volume(shared, plan, root))
        .buffer_unordered(concurrency);
    let mut failure: Option<IndexError> = None;
    let mut optimize = false;
    let mut rows_changed = 0;
    while let Some(result) = results.next().await {
        match result {
            Ok(outcome) => {
                optimize |= outcome.first_scan;
                rows_changed +=
                    outcome.summary.added + outcome.summary.changed + outcome.summary.removed;
                summary.merge(&outcome.summary);
            }
            Err(err) => {
                shared.cancel.cancel();
                failure = Some(worse(failure.take(), err));
            }
        }
    }
    drop(results);
    if let Some(err) = failure {
        return Err(err);
    }

    if (optimize || rows_changed > OPTIMIZE_AFTER_ROWS)
        && let Err(err) = shared.indexer.db().optimize().await
    {
        tracing::warn!(error = %err, "refreshing the planner statistics failed");
    }
    Ok(summary)
}

struct VolumeOutcome {
    summary: ScanSummary,
    /// No scan of the whole volume had completed before this one.
    first_scan: bool,
}

async fn scan_volume(
    shared: &Shared,
    plan: VolumePlan,
    root: Arc<VolumeRoot>,
) -> Result<VolumeOutcome, IndexError> {
    let indexer = &shared.indexer;
    let volume_id = plan.volume_id;
    let lock = indexer.volume_lock(volume_id);
    let _one_scan_at_a_time = tokio::select! {
        biased;
        () = shared.cancel.cancelled() => return Err(IndexError::Cancelled),
        guard = lock.lock_owned() => guard,
    };

    let db = indexer.db();
    let Some(generation) = store::begin_generation(db, volume_id).await? else {
        return Err(IndexError::UnknownVolume(volume_id));
    };
    let rows_before = store::count_files(db, volume_id).await?;
    if shared.estimate_total && rows_before > 0 {
        shared.progress.add_items_total(rows_before);
    }
    let name = indexer
        .volumes()
        .volume(volume_id)
        .map_or_else(|| format!("volume {volume_id}"), |volume| volume.name);

    let mut scan = VolumeScan {
        shared,
        db,
        volume_id,
        name,
        root,
        generation: generation.value,
        rows_before,
        summary: ScanSummary::default(),
        removed_files: Vec::new(),
        vanished: BTreeSet::new(),
    };
    scan.run(&plan).await?;
    Ok(VolumeOutcome {
        summary: scan.summary,
        first_scan: generation.first_scan,
    })
}

/// How the walk of one scope ended.
enum ScopeEnd {
    Done,
    /// The scope's directory cannot be read: its rows are left alone and the scan of the volume
    /// does not count as complete.
    Skipped,
    /// The volume's root cannot be read.
    Offline,
}

struct VolumeScan<'a> {
    shared: &'a Shared,
    db: &'a Db,
    volume_id: VolumeId,
    /// For the progress detail and the log.
    name: String,
    root: Arc<VolumeRoot>,
    generation: i64,
    /// File rows the volume had before the scan: what the guard compares the removals with.
    rows_before: u64,
    summary: ScanSummary,
    /// Rows of listed directories whose file is not on disk (any more).
    removed_files: Vec<FileId>,
    /// Directories that are gone. They are removed, with everything below them, after the walk.
    vanished: BTreeSet<RelPath>,
}

impl VolumeScan<'_> {
    async fn run(&mut self, plan: &VolumePlan) -> Result<(), IndexError> {
        let mut complete = true;
        for scope in &plan.scopes {
            match self.scan_scope(scope).await? {
                ScopeEnd::Done => {}
                ScopeEnd::Skipped => complete = false,
                ScopeEnd::Offline => {
                    self.summary.volumes_offline += 1;
                    return Ok(());
                }
            }
        }

        let held = self.apply_removals().await?;
        let whole_volume = plan.whole_volume();
        let held_update = match (whole_volume, held) {
            (true, held) => HeldUpdate::Set(held),
            (false, 0) => HeldUpdate::Keep,
            (false, held) => HeldUpdate::Raise(held),
        };
        store::finish_volume(
            self.db,
            self.volume_id,
            whole_volume && complete,
            held_update,
        )
        .await?;

        self.summary.volumes_scanned += 1;
        self.summary.held += held;
        if self.summary.skipped_invalid > 0 {
            tracing::warn!(
                volume = %self.name,
                count = self.summary.skipped_invalid,
                "skipped entries whose name is not valid UTF-8"
            );
        }
        tracing::info!(
            volume = %self.name,
            dirs = self.summary.dirs_visited,
            files = self.summary.files_seen,
            added = self.summary.added,
            changed = self.summary.changed,
            removed = self.summary.removed,
            held = self.summary.held,
            "scan finished"
        );
        Ok(())
    }

    fn check_cancelled(&self) -> Result<(), IndexError> {
        if self.shared.cancel.is_cancelled() {
            Err(IndexError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Tells the sinks which directories changed. Call it only after the transaction committed.
    fn report(&self, dirs: BTreeSet<RelPath>) {
        if dirs.is_empty() {
            return;
        }
        let dirs: Vec<RelPath> = dirs.into_iter().collect();
        self.shared.changes.dirs_changed(self.volume_id, &dirs);
    }

    /// Walks one scope and handles its listings.
    async fn scan_scope(&mut self, scope: &Scope) -> Result<ScopeEnd, IndexError> {
        self.check_cancelled()?;
        let spec = WalkSpec {
            root: Arc::clone(&self.root),
            start: scope.dir.clone(),
            recursive: scope.recursive,
            junk: self.shared.junk.clone(),
            sidecars: self.shared.matcher.clone(),
        };
        let cancel = self.shared.cancel.clone();
        let (tx, mut rx) = mpsc::channel(WALK_QUEUE);
        let walker = tokio::task::spawn_blocking(move || walk::walk(&spec, &cancel, &tx));

        let handled = self.handle_listings(&mut rx, scope).await;
        // The walker blocks while the channel is full: it must lose its receiver before it is awaited.
        drop(rx);
        let end = walker
            .await
            .map_err(|err| IndexError::Worker(format!("the directory walker failed: {err}")))?;
        handled?;

        match end {
            WalkEnd::Complete => Ok(ScopeEnd::Done),
            WalkEnd::Cancelled => Err(IndexError::Cancelled),
            WalkEnd::ReceiverGone => Err(IndexError::Worker(
                "the directory walker lost its receiver".to_owned(),
            )),
            WalkEnd::RootUnavailable(kind) => {
                tracing::warn!(volume = %self.name, error = ?kind, "the volume cannot be read, skipping it");
                Ok(ScopeEnd::Offline)
            }
            WalkEnd::StartGone if scope.dir.is_root() => Ok(ScopeEnd::Offline),
            WalkEnd::StartGone => {
                self.vanished.insert(scope.dir.clone());
                Ok(ScopeEnd::Done)
            }
            WalkEnd::StartUnreadable(kind) => {
                tracing::warn!(
                    volume = %self.name,
                    dir = %scope.dir,
                    error = ?kind,
                    "cannot read the directory to scan, leaving its entries alone"
                );
                Ok(ScopeEnd::Skipped)
            }
        }
    }

    /// Handles the listings as the walker produces them: whatever is waiting, up to a batch.
    async fn handle_listings(
        &mut self,
        rx: &mut mpsc::Receiver<DirListing>,
        scope: &Scope,
    ) -> Result<(), IndexError> {
        let mut batch = Vec::with_capacity(BATCH_DIRS);
        loop {
            let received = tokio::select! {
                biased;
                () = self.shared.cancel.cancelled() => return Err(IndexError::Cancelled),
                received = rx.recv_many(&mut batch, BATCH_DIRS) => received,
            };
            if received == 0 {
                return Ok(());
            }
            self.handle_batch(std::mem::take(&mut batch), scope).await?;
        }
    }

    async fn handle_batch(
        &mut self,
        listings: Vec<DirListing>,
        scope: &Scope,
    ) -> Result<(), IndexError> {
        let listed: Vec<&str> = listings
            .iter()
            .map(|listing| listing.dir.as_str())
            .collect();
        let healthy: Vec<&str> = listings
            .iter()
            .filter(|listing| listing.error.is_none())
            .map(|listing| listing.dir.as_str())
            .collect();
        let stored = store::load_stored(self.db, self.volume_id, &listed, &healthy).await?;
        if listings.iter().any(|listing| listing.dir == scope.dir) {
            self.ensure_ancestors(&scope.dir).await?;
        }

        let mut files = 0;
        let mut bytes = 0;
        let mut detail = None;
        let mut units = Vec::with_capacity(listings.len());
        for listing in listings {
            if listing.error.is_none() {
                files += listing.files.len() as u64;
                bytes += listing
                    .files
                    .iter()
                    .map(|file| file.size.unsigned_abs())
                    .sum::<u64>();
                detail = Some(self.detail(&listing.dir));
            }
            units.push(self.plan_dir(listing, &stored, scope));
        }
        self.write_units(units).await?;
        self.shared
            .progress
            .advance(files, bytes, detail.as_deref());
        Ok(())
    }

    /// `volume/dir`, for the progress display.
    fn detail(&self, dir: &RelPath) -> String {
        if dir.is_root() {
            self.name.clone()
        } else {
            format!("{}/{dir}", self.name)
        }
    }

    /// Gives the ancestors of a scope's directory a row when they have none, so a scan of a
    /// sub-folder can be the first scan of a volume.
    async fn ensure_ancestors(&mut self, start: &RelPath) -> Result<(), IndexError> {
        let ancestors: Vec<RelPath> = start.ancestors().collect();
        if ancestors.is_empty() {
            return Ok(());
        }
        let paths: Vec<&str> = ancestors.iter().map(RelPath::as_str).collect();
        let known = store::load_dir_rows(self.db, self.volume_id, &paths).await?;
        let missing: Vec<&RelPath> = ancestors
            .iter()
            .filter(|dir| !known.contains_key(dir.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        self.check_cancelled()?;
        let missing_paths: Vec<&str> = missing.iter().map(|dir| dir.as_str()).collect();
        let mut tx = self.db.begin_write().await?;
        store::ensure_dirs(&mut tx, self.volume_id, self.generation, &missing_paths).await?;
        tx.commit().await?;

        self.summary.dirs_added += missing.len() as u64;
        let mut touched = BTreeSet::new();
        for dir in missing {
            touched.extend(dir.parent());
            touched.insert(dir.clone());
        }
        self.report(touched);
        Ok(())
    }

    /// Compares one listing with the stored rows and decides what to write for it. Collects what
    /// is gone (files now, directories after the walk) without removing anything.
    fn plan_dir(&mut self, listing: DirListing, stored: &Stored, scope: &Scope) -> DirUnit {
        let mut unit = DirUnit::new(listing);
        let dir = unit.listing.dir.clone();
        if let Some(kind) = unit.listing.error {
            self.plan_unlisted_dir(&mut unit, kind, stored, scope);
            return unit;
        }

        self.summary.dirs_visited += 1;
        self.summary.skipped_invalid += unit.listing.skipped_invalid;
        let rows: &[StoredFile] = stored.files.get(dir.as_str()).map_or(&[], Vec::as_slice);
        let diff = diff::diff_dir(&unit.listing, rows, self.shared.force);
        self.summary.files_seen += unit.listing.files.len() as u64;
        self.summary.added += diff.added;
        self.summary.changed += diff.changed;
        self.summary.unchanged += diff.unchanged;
        self.removed_files
            .extend(diff.removed.iter().map(|row| row.id));
        unit.relink = diff.relink;
        unit.changed_sidecars = diff.changed_sidecars;
        unit.ids = diff.ids;
        unit.writes = diff.writes;
        if !unit.writes.is_empty() {
            unit.touched.push(dir.clone());
        }

        let children = stored.children.get(dir.as_str());
        for name in children.into_iter().flatten() {
            if unit.listing.subdirs.binary_search(name).is_err()
                && let Ok(child) = dir.join(name)
            {
                self.vanished.insert(child);
            }
        }

        match stored.dirs.get(dir.as_str()) {
            None => {
                unit.set_dir = Some(unit.listing.mtime);
                self.note_new_dir(&mut unit, &dir);
            }
            Some(&mtime) if mtime != unit.listing.mtime => unit.set_dir = Some(unit.listing.mtime),
            Some(_) => {}
        }

        if !scope.recursive {
            // Nothing lists the sub-directories of a non-recursive scan, so give them their rows here.
            let known: HashSet<&str> = children.into_iter().flatten().map(String::as_str).collect();
            let new: Vec<RelPath> = unit
                .listing
                .subdirs
                .iter()
                .filter(|name| !known.contains(name.as_str()))
                .filter_map(|name| dir.join(name).ok())
                .collect();
            for child in new {
                unit.ensure_dirs.push(child.as_str().to_owned());
                self.note_new_dir(&mut unit, &child);
            }
        }
        unit
    }

    /// A directory that could not be listed: a vanished one is removed after the walk, one that
    /// cannot be read keeps what it has.
    fn plan_unlisted_dir(
        &mut self,
        unit: &mut DirUnit,
        kind: io::ErrorKind,
        stored: &Stored,
        scope: &Scope,
    ) {
        let dir = unit.listing.dir.clone();
        if kind == io::ErrorKind::NotFound && dir != scope.dir {
            self.vanished.insert(dir);
            return;
        }
        self.summary.unreadable_dirs += 1;
        tracing::warn!(
            volume = %self.name,
            dir = %dir,
            error = ?kind,
            "cannot read the directory, leaving its entries alone"
        );
        if !stored.dirs.contains_key(dir.as_str()) {
            unit.ensure_dirs.push(dir.as_str().to_owned());
            self.note_new_dir(unit, &dir);
        }
    }

    /// A directory gets its row: it and its parent change in the listing of the file panes.
    fn note_new_dir(&mut self, unit: &mut DirUnit, dir: &RelPath) {
        self.summary.dirs_added += 1;
        unit.touched.push(dir.clone());
        unit.touched.extend(dir.parent());
    }

    /// Writes the units that have something to write, in transactions of about [`GROUP_ROWS`] rows
    /// made of whole directories.
    async fn write_units(&mut self, units: Vec<DirUnit>) -> Result<(), IndexError> {
        let mut group = Vec::new();
        let mut rows = 0;
        for unit in units {
            if unit.is_idle() {
                continue;
            }
            rows += unit.weight().max(1);
            group.push(unit);
            if rows >= GROUP_ROWS {
                self.write_group(std::mem::take(&mut group)).await?;
                rows = 0;
            }
        }
        if group.is_empty() {
            Ok(())
        } else {
            self.write_group(group).await
        }
    }

    async fn write_group(&mut self, mut group: Vec<DirUnit>) -> Result<(), IndexError> {
        self.check_cancelled()?;
        let mut tx = self.db.begin_write().await?;
        store::write_group(
            &mut tx,
            self.volume_id,
            self.generation,
            &self.shared.matcher,
            &mut group,
        )
        .await?;
        tx.commit().await?;
        self.report(
            group
                .iter()
                .flat_map(|unit| unit.touched.iter().cloned())
                .collect(),
        );
        Ok(())
    }

    /// Removes what is gone, unless the guard holds it back. Returns the number of file rows it
    /// holds back (0 when it removed everything).
    async fn apply_removals(&mut self) -> Result<u64, IndexError> {
        let roots = reduce_to_topmost(&std::mem::take(&mut self.vanished));
        let subtrees: Vec<Subtree> = roots.iter().map(Subtree::new).collect();
        let mut gone = self.removed_files.len() as u64;
        for subtree in &subtrees {
            gone += store::count_subtree_files(self.db, self.volume_id, self.generation, subtree)
                .await?;
        }

        if gone > 0 && guard_trips(gone, self.rows_before) && !self.shared.confirm {
            tracing::warn!(
                volume = %self.name,
                files = gone,
                rows = self.rows_before,
                "too many files disappeared at once: keeping them until the removal is confirmed"
            );
            return Ok(gone);
        }

        let removed_files = std::mem::take(&mut self.removed_files);
        for chunk in removed_files.chunks(DELETE_CHUNK) {
            self.check_cancelled()?;
            let mut tx = self.db.begin_write().await?;
            let removed = store::delete_files(&mut tx, self.generation, chunk).await?;
            tx.commit().await?;
            self.summary.removed += removed.rows;
            self.report(rel_paths(&removed.dirs, false));
        }
        for subtree in &subtrees {
            self.remove_subtree(subtree).await?;
        }
        Ok(0)
    }

    /// Removes the rows of a vanished directory and everything below it, a transaction at a time:
    /// the files first, then the directories.
    async fn remove_subtree(&mut self, subtree: &Subtree) -> Result<(), IndexError> {
        let chunk = DELETE_CHUNK as u64;
        loop {
            self.check_cancelled()?;
            let mut tx = self.db.begin_write().await?;
            let removed =
                store::delete_subtree_files(&mut tx, self.volume_id, self.generation, subtree)
                    .await?;
            tx.commit().await?;
            self.summary.removed += removed.rows;
            self.report(rel_paths(&removed.dirs, false));
            if removed.rows < chunk {
                break;
            }
        }
        loop {
            self.check_cancelled()?;
            let mut tx = self.db.begin_write().await?;
            let removed =
                store::delete_subtree_dirs(&mut tx, self.volume_id, self.generation, subtree)
                    .await?;
            tx.commit().await?;
            self.summary.dirs_removed += removed.rows;
            self.report(rel_paths(&removed.dirs, true));
            if removed.rows < chunk {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn scope(path: &str, recursive: bool) -> Scope {
        Scope {
            dir: rel(path),
            recursive,
        }
    }

    fn plan(targets: &[ScanTarget]) -> Vec<(VolumeId, Vec<(String, bool)>)> {
        plan_targets(targets, &[7, 8])
            .into_iter()
            .map(|plan| {
                let scopes = plan
                    .scopes
                    .iter()
                    .map(|scope| (scope.dir.as_str().to_owned(), scope.recursive))
                    .collect();
                (plan.volume_id, scopes)
            })
            .collect()
    }

    #[test]
    fn no_targets_means_every_online_volume_completely() {
        let plans = plan_targets(&[], &[7, 8]);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].volume_id, 7);
        assert_eq!(plans[1].scopes, [scope("", true)]);
        assert!(plans.iter().all(VolumePlan::whole_volume));
    }

    #[test]
    fn targets_are_grouped_by_volume() {
        let plans = plan(&[
            ScanTarget::subtree(8, rel("x")),
            ScanTarget::subtree(2, rel("a")),
            ScanTarget::dir(8, rel("y")),
        ]);
        assert_eq!(
            plans,
            [
                (2, vec![("a".to_owned(), true)]),
                (8, vec![("x".to_owned(), true), ("y".to_owned(), false)]),
            ]
        );
    }

    #[test]
    fn a_scope_inside_a_recursive_one_is_dropped() {
        let plans = plan(&[
            ScanTarget::subtree(1, rel("a/b/c")),
            ScanTarget::dir(1, rel("a/b")),
            ScanTarget::subtree(1, rel("a")),
            ScanTarget::subtree(1, rel("a/b")),
        ]);
        assert_eq!(plans, [(1, vec![("a".to_owned(), true)])]);
    }

    #[test]
    fn only_a_path_prefix_that_is_a_parent_covers() {
        let plans = plan(&[
            ScanTarget::subtree(1, rel("2019")),
            ScanTarget::subtree(1, rel("2019.old")),
            ScanTarget::subtree(1, rel("2019/Rome")),
        ]);
        assert_eq!(
            plans,
            [(
                1,
                vec![("2019".to_owned(), true), ("2019.old".to_owned(), true)]
            )]
        );
    }

    #[test]
    fn a_non_recursive_scope_does_not_cover_what_is_below_it() {
        let plans = plan(&[
            ScanTarget::dir(1, rel("a")),
            ScanTarget::subtree(1, rel("a/b")),
            ScanTarget::dir(1, rel("a")),
        ]);
        assert_eq!(
            plans,
            [(1, vec![("a".to_owned(), false), ("a/b".to_owned(), true)])]
        );
    }

    #[test]
    fn a_recursive_scope_absorbs_the_non_recursive_one_of_the_same_directory() {
        let plans = plan(&[
            ScanTarget::dir(1, rel("a")),
            ScanTarget::subtree(1, rel("a")),
        ]);
        assert_eq!(plans, [(1, vec![("a".to_owned(), true)])]);
    }

    #[test]
    fn the_whole_volume_covers_everything() {
        let plans = plan_targets(
            &[
                ScanTarget::dir(1, rel("a")),
                ScanTarget::volume(1),
                ScanTarget::subtree(1, rel("b")),
            ],
            &[],
        );
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].scopes, [scope("", true)]);
        assert!(plans[0].whole_volume());
        assert!(!plan_targets(&[ScanTarget::dir(1, RelPath::root())], &[])[0].whole_volume());
    }

    #[test]
    fn only_the_topmost_vanished_directories_are_kept() {
        let vanished: BTreeSet<RelPath> = ["a/b/c", "a/b", "x", "a.old", "a/b/d/e"]
            .into_iter()
            .map(rel)
            .collect();
        assert_eq!(
            reduce_to_topmost(&vanished),
            [rel("a.old"), rel("a/b"), rel("x")]
        );
        assert!(reduce_to_topmost(&BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_hard_failure_wins_over_a_cancellation() {
        assert!(matches!(
            worse(None, IndexError::Cancelled),
            IndexError::Cancelled
        ));
        assert!(matches!(
            worse(Some(IndexError::Cancelled), IndexError::Worker("x".into())),
            IndexError::Worker(_)
        ));
        assert!(matches!(
            worse(Some(IndexError::Worker("x".into())), IndexError::Cancelled),
            IndexError::Worker(_)
        ));
        assert!(matches!(
            worse(
                Some(IndexError::UnknownVolume(1)),
                IndexError::Worker("x".into())
            ),
            IndexError::UnknownVolume(1)
        ));
    }

    #[test]
    fn removed_directories_are_reported_with_their_parents() {
        let paths: BTreeSet<String> = ["a/b".to_owned(), "c".to_owned()].into();
        let with: Vec<String> = rel_paths(&paths, true)
            .iter()
            .map(|dir| dir.as_str().to_owned())
            .collect();
        assert_eq!(with, ["", "a", "a/b", "c"]);
        let without: Vec<String> = rel_paths(&paths, false)
            .iter()
            .map(|dir| dir.as_str().to_owned())
            .collect();
        assert_eq!(without, ["a/b", "c"]);
    }
}
