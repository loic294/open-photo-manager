//! One pass of the outbox: the body of the single cancellable `Lane::Immich` task that settles the
//! rows that are due (`service` decides when one starts).
//!
//! 1. **Read Immich.** The stored copy of Immich's list is refreshed once for the volumes whose
//!    rows still need to find an asset by the place of a file (`ImmichSync::refresh`). When that
//!    fails, the pass fails with rows untouched: working from a stale copy is worse than waiting.
//! 2. **Prepare** the rows that have not asked for their scan (stage 0): look at the asset Immich
//!    holds for the old place and persist a snapshot of what it carries. Persisted chunk by chunk,
//!    BEFORE any scan: Immich turns the old asset offline and hides what it knew.
//! 3. **Scan** every library the prepared rows touch, one request per library and pass.
//! 4. **Conclude** the prepared rows: stage 1 (asked and tracked), or done.
//! 5. **Check** the rows that were waiting for their scan at the start of the pass (stage 1):
//!    carry over, then delete the old asset when every proof of the safety rules holds. A row
//!    prepared in this pass waits for the scan to settle (`SCAN_SETTLE_SECS`) and is checked by a
//!    later pass.
//!
//! Rows are worked on [`CONCURRENCY`] at a time and written back in chunks of [`CHUNK`] in one
//! transaction each, so a cancelled or crashed pass loses at most the chunk it was working on. A
//! chunk is stored even when the pass ends in the middle of it.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::pin;

use apm_core::paths::VolumeId;
use futures_util::{StreamExt, stream};
use serde::Serialize;

use super::check::check;
use super::consts::{BREAKER, CHUNK, CONCURRENCY, MAX_ROWS_PER_PASS};
use super::prepare::{Scan, ScanThrottle, after_scan, conclude, prepare, scan_library};
use super::step::{Finish, Note, STORAGE_FAILURE, Shared, Step, Then};
use super::store::{self, Patch, Row};
use crate::db::{Db, DbError, now_secs};
use crate::immich::client::ImmichClient;
use crate::immich::error::ImmichError;
use crate::immich::paths::PathMap;
use crate::immich::{ImmichService, ImmichSync, RefreshError};
use crate::indexer::VolumeRegistry;
use crate::tasks::TaskCtx;

const READING: &str = "Reading Immich";
const PREPARING: &str = "Preparing";
const SCANNING: &str = "Asking Immich to scan";
const CHECKING: &str = "Checking Immich";

/// What a pass works with.
pub(super) struct Env<'a> {
    pub db: &'a Db,
    pub immich: &'a ImmichService,
    pub sync: &'a ImmichSync,
    pub volumes: &'a VolumeRegistry,
    pub throttle: &'a ScanThrottle,
    pub ctx: &'a TaskCtx,
}

/// What a pass did, and the result of its task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct Report {
    /// Rows the pass took.
    pub rows: usize,
    /// Library scans it requested.
    pub scans: usize,
    /// Deleted rows whose albums, favorite, rating and tags were carried over to the new asset
    /// first (in this pass or an earlier one): counted with the delete.
    pub carried: usize,
    /// Old assets it removed from Immich.
    pub deleted: usize,
    /// Id-preserving direct database updates (including confirmed retry).
    pub paths_updated: usize,
    pub paths_unconfirmed: usize,
    /// Rows whose old asset was gone from Immich already.
    pub gone: usize,
    /// Rows the world had overtaken (the file came back, the asset came online again).
    pub superseded: usize,
    /// Rows that needed nothing but a library scan.
    pub scanned: usize,
    /// Rows that touched nothing Immich knows of.
    pub dropped: usize,
    /// Rows that keep waiting for Immich.
    pub waiting: usize,
    /// Rows a call failed for, to be retried.
    pub retried: usize,
    /// Rows a person has to look at.
    pub attention: usize,
    /// Rows that changed while the pass worked on them: the next pass takes them again.
    pub skipped: usize,
}

impl Report {
    /// Whether the pass changed what Immich shows, so that reading its list again is worth it.
    pub fn changed(&self) -> bool {
        self.paths_updated > 0
            || self.scans > 0
            || self.carried > 0
            || self.deleted > 0
            || self.gone > 0
    }

    fn finished(&mut self, finish: Finish) {
        match finish {
            Finish::Dropped => self.dropped += 1,
            Finish::Scanned => self.scanned += 1,
            Finish::Gone => self.gone += 1,
            Finish::Superseded => self.superseded += 1,
            Finish::Deleted { carried } => {
                self.deleted += 1;
                if carried {
                    self.carried += 1;
                }
            }
        }
    }

    fn stays(&mut self, note: &Note) {
        match note {
            Note::Waiting => self.waiting += 1,
            Note::Retry(_) => self.retried += 1,
            Note::Attention => self.attention += 1,
        }
    }
}

/// Why a pass ended before it settled everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PassError {
    /// The task was cancelled, the server is shutting down or the connection was removed.
    Stopped,
    /// The pass could not go on; the message is fit for a person.
    Failed(String),
}

fn storage(err: DbError) -> PassError {
    tracing::error!(error = %err, "the Immich outbox could not use its database");
    PassError::Failed(STORAGE_FAILURE.to_owned())
}

fn connection_lost(err: ImmichError) -> PassError {
    match err {
        ImmichError::NotConnected => PassError::Stopped,
        other => PassError::Failed(other.summary()),
    }
}

fn count(items: usize) -> i64 {
    i64::try_from(items).unwrap_or(i64::MAX)
}

fn ensure_running(ctx: &TaskCtx) -> Result<(), PassError> {
    if ctx.cancel.is_cancelled() {
        Err(PassError::Stopped)
    } else {
        Ok(())
    }
}

/// `work`, unless the task is cancelled first.
async fn cancellable<T>(ctx: &TaskCtx, work: impl Future<Output = T>) -> Result<T, PassError> {
    tokio::select! {
        biased;
        () = ctx.cancel.cancelled() => Err(PassError::Stopped),
        out = work => Ok(out),
    }
}

/// Runs one pass. What it did is written to `report` as it goes, so a pass that ends early still
/// says what it settled.
pub(super) async fn run(env: &Env<'_>, report: &mut Report) -> Result<(), PassError> {
    let rows = store::due(env.db, now_secs(), MAX_ROWS_PER_PASS)
        .await
        .map_err(storage)?;
    report.rows = rows.len();
    if rows.is_empty() {
        return Ok(());
    }
    let mut api_rows = Vec::new();
    for row in rows {
        ensure_running(env.ctx)?;
        if let Some(json) = super::direct::request(env.db, row.id)
            .await
            .map_err(storage)?
        {
            let updated = cancellable(
                env.ctx,
                super::direct::apply(env.db, env.immich, env.volumes, &row, &json),
            )
            .await?
            .map_err(storage)?;
            if updated {
                report.paths_updated += 1;
            } else {
                report.retried += 1;
                report.paths_unconfirmed += 1;
            }
        } else {
            api_rows.push(row);
        }
    }
    if api_rows.is_empty() {
        return direct_result(report);
    }
    let rows = api_rows;
    let client = env.immich.client().await.map_err(connection_lost)?;
    let map = env.immich.path_map().await.map_err(connection_lost)?;
    let Some(connection) = env.immich.connection().await.map_err(connection_lost)? else {
        return Err(PassError::Stopped);
    };
    read_immich(env, &rows, &map).await?;

    let (unscanned, scanned): (Vec<Row>, Vec<Row>) =
        rows.into_iter().partition(|row| row.stage == 0);
    let shared = Shared {
        db: env.db,
        client: &client,
        map: &map,
        volumes: env.volumes,
        now: now_secs(),
    };
    let mut pass = Pass {
        report,
        breaker: Breaker::default(),
        prepared: Vec::new(),
    };
    drive(env, shared, Phase::Prepare, &unscanned, &mut pass).await?;
    let prepared = std::mem::take(&mut pass.prepared);
    let scans = ask_scans(env, &client, &prepared, connection.is_admin, pass.report).await?;
    conclude_prepared(env, &mut pass, &prepared, &scans).await?;
    drive(env, shared, Phase::Check, &scanned, &mut pass).await?;
    direct_result(pass.report)
}

fn direct_result(report: &Report) -> Result<(), PassError> {
    if report.paths_unconfirmed > 0 {
        Err(PassError::Failed(format!(
            "{} files moved, but their Immich path updates are NOT confirmed. They remain durably queued. Check the outbox warnings and database configuration, then retry.",
            report.paths_unconfirmed
        )))
    } else {
        Ok(())
    }
}

/// The volumes whose assets the rows have to look up by the place of a file in the stored copy of
/// Immich's list: the old place of a row that has not found its asset yet, the new place of one
/// that waits for its scan and has not found the asset that replaces it.
fn lookup_volumes(rows: &[Row], map: &PathMap) -> Vec<VolumeId> {
    let mut volumes = BTreeSet::new();
    for row in rows {
        let wanted = if row.stage == 0 {
            row.asset_id.is_none().then_some(row.effect.old.as_ref())
        } else {
            row.new_asset_id
                .is_none()
                .then_some(row.effect.new.as_ref())
        };
        if let Some(Some(place)) = wanted
            && map.prefix_for(place.volume_id).is_some()
        {
            volumes.insert(place.volume_id);
        }
    }
    volumes.into_iter().collect()
}

async fn read_immich(env: &Env<'_>, rows: &[Row], map: &PathMap) -> Result<(), PassError> {
    let volumes = lookup_volumes(rows, map);
    if volumes.is_empty() {
        return Ok(());
    }
    env.ctx.progress.phase(READING, None, None);
    match env.sync.refresh(env.ctx, &volumes).await {
        Ok(_) => Ok(()),
        Err(RefreshError::Stopped) => Err(PassError::Stopped),
        Err(RefreshError::Failed(message)) => Err(PassError::Failed(message)),
    }
}

// ----- working on rows --------------------------------------------------------------------------

/// The rows of phase 1 that go on to the scan.
struct Prepared {
    row: Row,
    libraries: BTreeSet<String>,
    tracked: bool,
}

/// What one chunk of rows came to, written to the database in one transaction.
#[derive(Default)]
struct Chunk {
    patches: Vec<(Row, Patch)>,
    done: Vec<Row>,
}

/// Counts consecutive rows that failed for a passing reason: when Immich is down, the next rows
/// would fail the same way, so the pass gives up instead of failing every row of a long queue.
#[derive(Debug, Default)]
struct Breaker {
    failures: usize,
}

impl Breaker {
    fn ok(&mut self) {
        self.failures = 0;
    }

    /// Whether the breaker trips.
    fn fail(&mut self) -> bool {
        self.failures += 1;
        self.failures >= BREAKER
    }
}

struct Pass<'r> {
    report: &'r mut Report,
    breaker: Breaker,
    prepared: Vec<Prepared>,
}

impl Pass<'_> {
    /// Takes the outcome of one row into `chunk` and the report. `Some` ends the pass (after the
    /// chunk is stored).
    fn absorb(&mut self, row: Row, step: Step, chunk: &mut Chunk) -> Option<PassError> {
        let Step { patch, then } = step;
        match then {
            Then::Prepared { libraries, tracked } => {
                if !patch.is_empty() {
                    chunk.patches.push((row.clone(), patch));
                }
                self.prepared.push(Prepared {
                    row,
                    libraries,
                    tracked,
                });
                self.breaker.ok();
                None
            }
            Then::Finished(finish) => {
                self.report.finished(finish);
                chunk.done.push(row);
                self.breaker.ok();
                None
            }
            Then::Stays(note) => {
                if !patch.is_empty() {
                    chunk.patches.push((row, patch));
                }
                self.report.stays(&note);
                match note {
                    Note::Retry(why) => self.breaker.fail().then_some(PassError::Failed(why)),
                    Note::Waiting | Note::Attention => {
                        self.breaker.ok();
                        None
                    }
                }
            }
            Then::Abort(why) => Some(PassError::Failed(why)),
        }
    }

    /// Writes `chunk` and drops from the scan the prepared rows whose change was skipped: they
    /// changed since the pass read them and are prepared again by the next pass.
    async fn store(&mut self, env: &Env<'_>, chunk: Chunk) -> Result<(), PassError> {
        let skipped = store::apply(env.db, &chunk.patches, &chunk.done)
            .await
            .map_err(storage)?;
        self.report.skipped += skipped.len();
        if !skipped.is_empty() {
            self.prepared
                .retain(|prepared| !skipped.contains(&prepared.row.id));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Prepare,
    Check,
}

/// Works on `rows` with the phase, a chunk at a time.
async fn drive(
    env: &Env<'_>,
    shared: Shared<'_>,
    phase: Phase,
    rows: &[Row],
    pass: &mut Pass<'_>,
) -> Result<(), PassError> {
    if rows.is_empty() {
        return Ok(());
    }
    ensure_running(env.ctx)?;
    let title = match phase {
        Phase::Prepare => PREPARING,
        Phase::Check => CHECKING,
    };
    env.ctx.progress.phase(title, Some(count(rows.len())), None);
    for rows in rows.chunks(CHUNK) {
        let mut chunk = Chunk::default();
        let halt = collect(env, shared, phase, rows, pass, &mut chunk).await;
        pass.store(env, chunk).await?;
        if let Some(error) = halt {
            return Err(error);
        }
    }
    Ok(())
}

/// Works on the rows of one chunk, [`CONCURRENCY`] at a time, and gathers what they came to. The
/// error is why the work ended early.
async fn collect(
    env: &Env<'_>,
    shared: Shared<'_>,
    phase: Phase,
    rows: &[Row],
    pass: &mut Pass<'_>,
    chunk: &mut Chunk,
) -> Option<PassError> {
    let mut results = pin!(
        stream::iter(rows.iter().cloned())
            .map(move |row| async move {
                let shared = Shared {
                    now: now_secs(),
                    ..shared
                };
                let step = match phase {
                    Phase::Prepare => prepare(shared, &row).await,
                    Phase::Check => check(shared, &row).await,
                };
                (row, step)
            })
            .buffer_unordered(CONCURRENCY)
    );
    loop {
        tokio::select! {
            biased;
            () = env.ctx.cancel.cancelled() => return Some(PassError::Stopped),
            next = results.next() => {
                let (row, step) = next?;
                env.ctx.progress.add(1, 0);
                if let Some(error) = pass.absorb(row, step, chunk) {
                    return Some(error);
                }
            }
        }
    }
}

// ----- scanning ---------------------------------------------------------------------------------

/// Asks Immich to scan every library the prepared rows touch.
async fn ask_scans(
    env: &Env<'_>,
    client: &ImmichClient,
    prepared: &[Prepared],
    admin: bool,
    report: &mut Report,
) -> Result<BTreeMap<String, Scan>, PassError> {
    let libraries: BTreeSet<&str> = prepared
        .iter()
        .flat_map(|prepared| prepared.libraries.iter().map(String::as_str))
        .collect();
    let mut scans = BTreeMap::new();
    if libraries.is_empty() {
        return Ok(scans);
    }
    ensure_running(env.ctx)?;
    env.ctx
        .progress
        .phase(SCANNING, Some(count(libraries.len())), None);
    for library in libraries {
        let asked = scan_library(client, env.throttle, library, admin, now_secs());
        let scan = cancellable(env.ctx, asked)
            .await?
            .map_err(PassError::Failed)?;
        if matches!(scan, Scan::Asked { fresh: true, .. }) {
            report.scans += 1;
        }
        scans.insert(library.to_owned(), scan);
        env.ctx.progress.add(1, 0);
    }
    Ok(scans)
}

/// Moves the prepared rows on, by what the scans of their libraries came to.
async fn conclude_prepared(
    env: &Env<'_>,
    pass: &mut Pass<'_>,
    prepared: &[Prepared],
    scans: &BTreeMap<String, Scan>,
) -> Result<(), PassError> {
    for group in prepared.chunks(CHUNK) {
        let now = now_secs();
        let mut chunk = Chunk::default();
        let mut halt = None;
        for item in group {
            let outcome = after_scan(&item.row, &item.libraries, scans);
            let step = conclude(&item.row, item.tracked, outcome, now);
            halt = pass.absorb(item.row.clone(), step, &mut chunk);
            if halt.is_some() {
                break;
            }
        }
        pass.store(env, chunk).await?;
        if let Some(error) = halt {
            return Err(error);
        }
        ensure_running(env.ctx)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use apm_core::dto::ImmichMappingEntry;

    use super::*;
    use crate::immich::outbox::merge::{Effect, Place};

    fn map() -> PathMap {
        PathMap::new(&[
            ImmichMappingEntry {
                volume_id: 1,
                library_id: Some("lib-photos".to_owned()),
                path_prefix: "/mnt/photos".to_owned(),
            },
            ImmichMappingEntry {
                volume_id: 2,
                library_id: Some("lib-archive".to_owned()),
                path_prefix: "/mnt/archive".to_owned(),
            },
        ])
        .unwrap()
    }

    fn row(id: i64, effect: Effect) -> Row {
        Row {
            id,
            effect,
            asset_id: None,
            new_asset_id: None,
            snapshot_json: None,
            stage: 0,
            carried: false,
            attempts: 0,
            offline_seen_at: None,
            scan_requested_at: None,
            created_at: 100,
        }
    }

    fn moved(id: i64, from: VolumeId, to: VolumeId) -> Row {
        row(
            id,
            Effect::moved(Place::new(from, "", "a.jpg"), Place::new(to, "x", "a.jpg")),
        )
    }

    fn settled(mut row: Row) -> Row {
        row.stage = 1;
        row
    }

    // ----- the report ------------------------------------------------------------------------

    #[test]
    fn a_pass_changed_what_immich_shows_when_it_scanned_carried_deleted_or_found_a_gone_asset() {
        assert!(!Report::default().changed());
        for report in [
            Report {
                scans: 1,
                ..Report::default()
            },
            Report {
                carried: 1,
                ..Report::default()
            },
            Report {
                deleted: 1,
                ..Report::default()
            },
            Report {
                gone: 1,
                ..Report::default()
            },
        ] {
            assert!(report.changed(), "{report:?}");
        }
        let idle = Report {
            rows: 9,
            superseded: 2,
            scanned: 1,
            dropped: 1,
            waiting: 3,
            retried: 1,
            attention: 1,
            skipped: 1,
            ..Report::default()
        };
        assert!(!idle.changed());
    }

    // ----- the breaker -----------------------------------------------------------------------

    #[test]
    fn the_breaker_trips_after_a_run_of_failures_and_a_success_restarts_the_count() {
        let mut breaker = Breaker::default();
        for _ in 0..BREAKER - 1 {
            assert!(!breaker.fail());
        }
        breaker.ok();
        for _ in 0..BREAKER - 1 {
            assert!(!breaker.fail());
        }
        assert!(breaker.fail());
    }

    // ----- which volumes are read ------------------------------------------------------------

    #[test]
    fn a_row_that_has_not_found_its_old_asset_reads_the_old_volume() {
        let rows = [moved(1, 1, 2)];
        assert_eq!(lookup_volumes(&rows, &map()), [1]);
    }

    #[test]
    fn a_row_that_knows_its_old_asset_needs_no_read() {
        let mut known = moved(1, 1, 2);
        known.asset_id = Some("asset-1".to_owned());
        assert!(lookup_volumes(&[known], &map()).is_empty());
    }

    #[test]
    fn a_row_waiting_for_its_scan_reads_the_new_volume_until_it_knows_the_new_asset() {
        let waiting = settled(moved(1, 1, 2));
        assert_eq!(lookup_volumes(std::slice::from_ref(&waiting), &map()), [2]);
        let mut found = waiting;
        found.new_asset_id = Some("asset-2".to_owned());
        assert!(lookup_volumes(&[found], &map()).is_empty());
    }

    #[test]
    fn volumes_are_listed_once_and_unmapped_ones_never() {
        let rows = [
            moved(1, 1, 2),
            moved(2, 1, 1),
            settled(moved(3, 1, 2)),
            moved(4, 9, 1),
            row(5, Effect::added(Place::new(1, "", "new.jpg"))),
        ];
        assert_eq!(lookup_volumes(&rows, &map()), [1, 2]);
        assert!(lookup_volumes(&[moved(6, 9, 8)], &map()).is_empty());
    }

    // ----- what the rows came to -------------------------------------------------------------

    fn pass_over(report: &mut Report) -> Pass<'_> {
        Pass {
            report,
            breaker: Breaker::default(),
            prepared: Vec::new(),
        }
    }

    fn patch_of(next_attempt_at: i64) -> Patch {
        Patch {
            next_attempt_at: Some(next_attempt_at),
            ..Patch::default()
        }
    }

    #[test]
    fn finished_rows_are_counted_by_kind_and_leave_the_database() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        for (id, finish) in [
            Finish::Dropped,
            Finish::Scanned,
            Finish::Gone,
            Finish::Superseded,
            Finish::Deleted { carried: false },
            Finish::Deleted { carried: true },
        ]
        .into_iter()
        .enumerate()
        {
            let row = settled(moved(id as i64, 1, 2));
            assert_eq!(pass.absorb(row, Step::finished(finish), &mut chunk), None);
        }
        assert_eq!(chunk.done.len(), 6);
        assert!(chunk.patches.is_empty());
        assert_eq!(
            report,
            Report {
                dropped: 1,
                scanned: 1,
                gone: 1,
                superseded: 1,
                deleted: 2,
                carried: 1,
                ..Report::default()
            }
        );
    }

    #[test]
    fn staying_rows_are_patched_and_counted_by_note() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        let notes = [
            Note::Waiting,
            Note::Retry("busy".to_owned()),
            Note::Attention,
        ];
        for (id, note) in notes.into_iter().enumerate() {
            let step = Step::stays(patch_of(500), note);
            assert_eq!(pass.absorb(moved(id as i64, 1, 2), step, &mut chunk), None);
        }
        assert_eq!(chunk.patches.len(), 3);
        assert!(chunk.done.is_empty());
        assert_eq!(
            (report.waiting, report.retried, report.attention),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_step_that_changes_nothing_adds_no_patch() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        let step = Step::stays(Patch::default(), Note::Waiting);
        assert_eq!(pass.absorb(moved(1, 1, 2), step, &mut chunk), None);
        assert!(chunk.patches.is_empty());
        assert_eq!(report.waiting, 1);
    }

    #[test]
    fn a_prepared_row_keeps_its_snapshot_patch_and_waits_for_the_scan() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        let libraries: BTreeSet<String> = ["lib-photos".to_owned()].into();
        let patch = Patch {
            asset_id: Some("asset-1".to_owned()),
            ..Patch::default()
        };
        let step = Step::new(
            patch,
            Then::Prepared {
                libraries: libraries.clone(),
                tracked: true,
            },
        );
        assert_eq!(pass.absorb(moved(1, 1, 1), step, &mut chunk), None);
        assert_eq!(chunk.patches.len(), 1);
        assert_eq!(pass.prepared.len(), 1);
        assert_eq!(pass.prepared[0].libraries, libraries);
        assert!(pass.prepared[0].tracked);
        let untouched = Step::new(
            Patch::default(),
            Then::Prepared {
                libraries: BTreeSet::new(),
                tracked: false,
            },
        );
        assert_eq!(pass.absorb(moved(2, 1, 1), untouched, &mut chunk), None);
        assert_eq!(chunk.patches.len(), 1, "nothing to write for the second");
        assert_eq!(pass.prepared.len(), 2);
    }

    #[test]
    fn a_run_of_rows_that_failed_for_a_passing_reason_ends_the_pass_with_the_last_message() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        for id in 0..BREAKER as i64 - 1 {
            let step = Step::stays(patch_of(500), Note::Retry("down".to_owned()));
            assert_eq!(pass.absorb(moved(id, 1, 2), step, &mut chunk), None);
        }
        let step = Step::stays(patch_of(500), Note::Retry("still down".to_owned()));
        assert_eq!(
            pass.absorb(moved(99, 1, 2), step, &mut chunk),
            Some(PassError::Failed("still down".to_owned()))
        );
        assert_eq!(
            chunk.patches.len(),
            BREAKER,
            "every failure is written back"
        );
    }

    #[test]
    fn a_row_that_settles_between_failures_keeps_the_pass_going() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        for round in 0..3 {
            for id in 0..BREAKER as i64 - 1 {
                let step = Step::stays(patch_of(500), Note::Retry("down".to_owned()));
                assert_eq!(
                    pass.absorb(moved(round * 100 + id, 1, 2), step, &mut chunk),
                    None
                );
            }
            let row = settled(moved(round * 100 + 50, 1, 2));
            let step = Step::finished(Finish::Superseded);
            assert_eq!(pass.absorb(row, step, &mut chunk), None);
        }
    }

    #[test]
    fn an_abort_ends_the_pass_at_once() {
        let mut report = Report::default();
        let mut chunk = Chunk::default();
        let mut pass = pass_over(&mut report);
        assert_eq!(
            pass.absorb(moved(1, 1, 2), Step::abort("key revoked"), &mut chunk),
            Some(PassError::Failed("key revoked".to_owned()))
        );
        assert!(chunk.patches.is_empty() && chunk.done.is_empty());
    }

    #[test]
    fn a_lost_connection_stops_a_pass_and_any_other_error_fails_it() {
        assert_eq!(
            connection_lost(ImmichError::NotConnected),
            PassError::Stopped
        );
        assert!(matches!(
            connection_lost(ImmichError::NeedsReconnect),
            PassError::Failed(message) if !message.is_empty()
        ));
    }
}
