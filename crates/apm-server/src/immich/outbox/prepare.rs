//! Phases 1 and 2 of a pass: look at what Immich holds for the OLD place of each row (and keep a
//! snapshot of its albums, favorite, rating and tags while the asset is still live), then ask Immich
//! to scan the libraries the rows touch.
//!
//! The order matters. Immich turns the old asset offline at its next library scan and hides what
//! it knew about it, so the snapshot is persisted BEFORE any scan is requested, and a row whose
//! snapshot could not be taken for a live asset never reaches the scan.
//!
//! A scan that this pass asked after it read its rows covers all of them. A recent scan of an
//! earlier pass (at most one request per library and minute is asked, see [`ScanThrottle`]) covers
//! a row only when it was asked strictly after the latest change of the row (`created_at`): any
//! other row waits for the throttle to open again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};

use super::carry::Snapshot;
use super::consts::{SCAN_SETTLE_SECS, SCAN_THROTTLE_SECS, row_backoff_secs};
use super::merge::{Effect, Place};
use super::probe::place_path;
use super::step::{
    Failure, Finish, Note, Shared, Step, Then, classify, code, failed, retry_later, storage_failed,
};
use super::store::{Patch, Row};
use crate::db::{Db, DbError};
use crate::immich::assets;
use crate::immich::client::ImmichClient;
use crate::immich::error::ImmichError;
use crate::immich::paths::PathMap;

/// The Immich libraries of the volumes `effect` touches: the ones a scan has to cover.
pub(super) fn libraries_of(map: &PathMap, effect: &Effect) -> BTreeSet<String> {
    [&effect.old, &effect.new]
        .into_iter()
        .flatten()
        .filter_map(|place| map.library_for(place.volume_id))
        .map(str::to_owned)
        .collect()
}

// ----- phase 1: what Immich holds for the old place ---------------------------------------------

/// Looks at the old place of `row` in Immich. The step is [`Then::Prepared`] with what the scan
/// phase needs, or the end of the row (nothing of it lives in Immich's world) or a failure.
pub(super) async fn prepare(sh: Shared<'_>, row: &Row) -> Step {
    let effect = &row.effect;
    let old_path = effect
        .old
        .as_ref()
        .and_then(|place| place_path(sh.map, place));
    let new_is_mapped = effect
        .new
        .as_ref()
        .is_some_and(|place| place_path(sh.map, place).is_some());
    if old_path.is_none() && !new_is_mapped {
        return Step::finished(Finish::Dropped);
    }
    let mut patch = Patch::default();
    let mut tracked = false;
    if let (Some(old), Some(old_path)) = (&effect.old, &old_path) {
        match inspect_old(sh, row, old, old_path, new_is_mapped).await {
            Ok(Some(found)) => {
                patch = found;
                tracked = true;
            }
            Ok(None) => {}
            Err(step) => return *step,
        }
    }
    Step::new(
        patch,
        Then::Prepared {
            libraries: libraries_of(sh.map, effect),
            tracked,
        },
    )
}

/// The asset Immich holds for `old` (a fresh read, never the cache alone) and, when a new asset is
/// going to appear, the snapshot of what the old one carries. `Ok(None)`: Immich holds none.
async fn inspect_old(
    sh: Shared<'_>,
    row: &Row,
    old: &Place,
    old_path: &str,
    wants_snapshot: bool,
) -> Result<Option<Patch>, Box<Step>> {
    let asset_id = match &row.asset_id {
        Some(id) => id.clone(),
        None => match known_asset(sh.db, old).await {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(None),
            Err(err) => return Err(storage_failed(&err)),
        },
    };
    let asset = match sh.client.get_asset(&asset_id).await {
        Ok(Some(asset)) if asset.original_path == old_path => asset,
        Ok(_) => return Ok(None),
        Err(err) => return Err(failed(row, sh.now, &err, code::IMMICH_ERROR)),
    };
    let mut patch = Patch::default();
    if row.asset_id.as_deref() != Some(asset.id.as_str()) {
        patch.asset_id = Some(asset.id.clone());
    }
    // An offline asset keeps the snapshot of its live days; one that has none is read as it is.
    if wants_snapshot && (!asset.is_offline || row.snapshot_json.is_none()) {
        match sh.client.albums_for_asset(&asset.id).await {
            Ok(albums) => patch.snapshot_json = Some(Snapshot::of(&asset, &albums).to_json()),
            Err(err) if asset.is_offline && classify(&err) != Failure::Fatal => {
                patch.snapshot_json = Some(Snapshot::of(&asset, &[]).to_json());
            }
            Err(err) => return Err(failed(row, sh.now, &err, code::IMMICH_ERROR)),
        }
    }
    Ok(Some(patch))
}

/// The id of the asset the stored copy of Immich's list shows at `place`, a live one first.
async fn known_asset(db: &Db, place: &Place) -> Result<Option<String>, DbError> {
    let records = assets::find_by_location(db, place.volume_id, &place.dir, &place.name).await?;
    Ok(records.into_iter().next().map(|record| record.asset_id))
}

// ----- phase 2: ask Immich to scan --------------------------------------------------------------

/// What asking Immich to scan one library came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Scan {
    /// A scan was requested at `at`. `fresh`: by this pass, which read every row it works on before
    /// asking, so the scan covers all of them. Otherwise the throttle reused a recent request of an
    /// earlier pass, which covers a row only when it was asked strictly after its latest change
    /// (times are whole seconds, so the same second says nothing about the order).
    Asked { at: i64, fresh: bool },
    /// It cannot be requested: the account is no administrator, or Immich refuses it for this
    /// library. The rows wait for Immich's own schedule.
    Cannot,
    /// Immich failed for a passing reason, with this message.
    Retry(String),
}

/// How a failed `POST /libraries/{id}/scan` is understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScanFailure {
    /// Immich refuses it for this library; nothing will change by asking again.
    Cannot,
    /// Every call would fail the same way: the pass ends.
    Fatal,
    /// A passing failure: the rows try again later.
    Retry,
}

/// The order matters: a refusal about THIS library (403, 404, 4xx) must not end the pass, while a
/// connection that is down or a key that is gone must.
pub(super) fn scan_failure(err: &ImmichError) -> ScanFailure {
    match err {
        ImmichError::Forbidden
        | ImmichError::AdminRequired
        | ImmichError::NotFound
        | ImmichError::Validation(_)
        | ImmichError::InvalidUrl(_) => ScanFailure::Cannot,
        ImmichError::Upstream { status, .. } if (400..500).contains(status) && *status != 429 => {
            ScanFailure::Cannot
        }
        _ if err.affects_every_call() => ScanFailure::Fatal,
        _ => ScanFailure::Retry,
    }
}

/// When a library was last asked to scan, in memory: a restart forgets it and costs one scan.
#[derive(Debug, Default)]
pub(super) struct ScanThrottle {
    last: Mutex<HashMap<String, i64>>,
}

impl ScanThrottle {
    /// The time of the scan requested for `library` less than [`SCAN_THROTTLE_SECS`] before `now`.
    pub fn recent(&self, library: &str, now: i64) -> Option<i64> {
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        last.get(library)
            .copied()
            .filter(|at| (0..SCAN_THROTTLE_SECS).contains(&now.saturating_sub(*at)))
    }

    pub fn record(&self, library: &str, at: i64) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        last.insert(library.to_owned(), at);
    }

    /// Forgets every request: a person asked to try now, and the throttle only protects Immich
    /// from the loop.
    pub fn forget(&self) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        last.clear();
    }
}

/// Asks Immich to scan `library` at `now`, unless the account cannot or a scan was asked for within
/// the throttle window. `Err` is a failure that ends the pass (the message is fit for a person).
pub(super) async fn scan_library(
    client: &ImmichClient,
    throttle: &ScanThrottle,
    library: &str,
    admin: bool,
    now: i64,
) -> Result<Scan, String> {
    if !admin {
        return Ok(Scan::Cannot);
    }
    if let Some(at) = throttle.recent(library, now) {
        return Ok(Scan::Asked { at, fresh: false });
    }
    match client.scan_library(library).await {
        Ok(()) => {
            throttle.record(library, now);
            Ok(Scan::Asked {
                at: now,
                fresh: true,
            })
        }
        Err(err) => match scan_failure(&err) {
            ScanFailure::Cannot => {
                tracing::warn!(library, error = %err.summary(), "Immich would not scan a library");
                Ok(Scan::Cannot)
            }
            ScanFailure::Fatal => Err(err.summary()),
            ScanFailure::Retry => Ok(Scan::Retry(err.summary())),
        },
    }
}

/// What the scans of the libraries of a row mean for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AfterScan {
    /// A scan that covers the row was requested.
    Asked,
    /// The only scans were reused ones asked before the latest change of the row: a new one may be
    /// asked at `until`.
    Wait { until: i64 },
    /// A scan failed for a passing reason.
    Retry(String),
    /// No scan can be requested: Immich scans when it wants to.
    NoScan,
}

pub(super) fn after_scan(
    row: &Row,
    libraries: &BTreeSet<String>,
    scans: &BTreeMap<String, Scan>,
) -> AfterScan {
    let mut asked = false;
    let mut until: Option<i64> = None;
    for library in libraries {
        match scans.get(library) {
            Some(Scan::Retry(why)) => return AfterScan::Retry(why.clone()),
            Some(Scan::Asked { at, fresh }) if *fresh || *at > row.created_at => asked = true,
            Some(Scan::Asked { at, .. }) => {
                let reopens = at.saturating_add(SCAN_THROTTLE_SECS);
                until = Some(until.map_or(reopens, |other| other.max(reopens)));
            }
            Some(Scan::Cannot) | None => {}
        }
    }
    match until {
        Some(until) => AfterScan::Wait { until },
        None if asked => AfterScan::Asked,
        None => AfterScan::NoScan,
    }
}

/// The end of phase 2 for one row: the step that moves it on. `tracked`: Immich holds an asset for
/// its old place.
pub(super) fn conclude(row: &Row, tracked: bool, outcome: AfterScan, now: i64) -> Step {
    match outcome {
        AfterScan::Retry(why) => retry_later(
            row,
            now,
            code::IMMICH_UNAVAILABLE,
            Some(&why),
            Patch::default(),
        ),
        AfterScan::Wait { until } => Step::stays(
            Patch {
                next_attempt_at: Some(until),
                ..Patch::default()
            },
            Note::Waiting,
        ),
        AfterScan::Asked if !tracked => Step::finished(Finish::Scanned),
        AfterScan::NoScan if !tracked => Step::finished(Finish::Dropped),
        AfterScan::Asked => Step::stays(
            Patch {
                stage: Some(1),
                attempts: Some(0),
                scan_requested_at: Some(now),
                next_attempt_at: Some(now + SCAN_SETTLE_SECS),
                last_error: Some(None),
                ..Patch::default()
            },
            Note::Waiting,
        ),
        AfterScan::NoScan => Step::stays(
            Patch {
                stage: Some(1),
                attempts: Some(0),
                next_attempt_at: Some(now + row_backoff_secs(0)),
                last_error: Some(None),
                ..Patch::default()
            },
            Note::Waiting,
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use apm_core::dto::ImmichMappingEntry;

    use super::*;
    use crate::immich::outbox::consts::{ATTENTION_AFTER_NO_SCAN_SECS, ATTENTION_MIN_ATTEMPTS};

    const NOW: i64 = 20_000_000;

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
            ImmichMappingEntry {
                volume_id: 3,
                library_id: None,
                path_prefix: "/mnt/limited".to_owned(),
            },
        ])
        .unwrap()
    }

    fn row(created_at: i64) -> Row {
        Row {
            id: 1,
            effect: Effect::moved(Place::new(1, "", "a.jpg"), Place::new(2, "", "a.jpg")),
            asset_id: None,
            new_asset_id: None,
            snapshot_json: None,
            stage: 0,
            carried: false,
            attempts: 0,
            offline_seen_at: None,
            scan_requested_at: None,
            created_at,
        }
    }

    fn libs(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn scans(entries: &[(&str, Scan)]) -> BTreeMap<String, Scan> {
        entries
            .iter()
            .map(|(name, scan)| ((*name).to_owned(), scan.clone()))
            .collect()
    }

    fn upstream(status: u16) -> ImmichError {
        ImmichError::Upstream {
            status,
            message: "boom".to_owned(),
        }
    }

    // ----- libraries -------------------------------------------------------------------------

    #[test]
    fn a_move_touches_the_libraries_of_both_places_once() {
        let moved = Effect::moved(Place::new(1, "", "a.jpg"), Place::new(2, "x", "a.jpg"));
        assert_eq!(
            libraries_of(&map(), &moved),
            libs(&["lib-archive", "lib-photos"])
        );
        let inside = Effect::moved(Place::new(1, "", "a.jpg"), Place::new(1, "x", "a.jpg"));
        assert_eq!(libraries_of(&map(), &inside), libs(&["lib-photos"]));
    }

    #[test]
    fn a_trash_and_an_addition_touch_one_library_and_unmapped_places_none() {
        let removed = Effect::removed(Place::new(2, "", "a.jpg"));
        assert_eq!(libraries_of(&map(), &removed), libs(&["lib-archive"]));
        let added = Effect::added(Place::new(1, "", "a.jpg"));
        assert_eq!(libraries_of(&map(), &added), libs(&["lib-photos"]));
        let outside = Effect::moved(Place::new(9, "", "a.jpg"), Place::new(1, "", "a.jpg"));
        assert_eq!(libraries_of(&map(), &outside), libs(&["lib-photos"]));
        assert!(libraries_of(&map(), &Effect::removed(Place::new(9, "", "a"))).is_empty());
    }

    #[test]
    fn a_volume_mapped_without_a_library_has_none_to_scan() {
        let limited = Effect::removed(Place::new(3, "", "a.jpg"));
        assert!(libraries_of(&map(), &limited).is_empty());
    }

    // ----- scan failures ---------------------------------------------------------------------

    #[test]
    fn a_refusal_about_one_library_does_not_end_the_pass() {
        for err in [
            ImmichError::Forbidden,
            ImmichError::AdminRequired,
            ImmichError::NotFound,
            ImmichError::Validation(Vec::new()),
            ImmichError::InvalidUrl(String::new()),
            upstream(400),
            upstream(409),
            upstream(422),
        ] {
            assert_eq!(scan_failure(&err), ScanFailure::Cannot, "{err:?}");
        }
    }

    #[test]
    fn a_connection_that_is_down_or_a_key_that_is_gone_ends_the_pass() {
        for err in [
            ImmichError::NotConnected,
            ImmichError::NeedsReconnect,
            ImmichError::Unreachable("down".to_owned()),
            ImmichError::Unauthorized,
            ImmichError::Redirected {
                to: "https://other".to_owned(),
            },
        ] {
            assert_eq!(scan_failure(&err), ScanFailure::Fatal, "{err:?}");
        }
    }

    #[test]
    fn a_server_error_is_retried() {
        for err in [
            upstream(429),
            upstream(500),
            upstream(503),
            ImmichError::Protocol("odd".to_owned()),
            ImmichError::Internal("odd".to_owned()),
        ] {
            assert_eq!(scan_failure(&err), ScanFailure::Retry, "{err:?}");
        }
    }

    // ----- the throttle ----------------------------------------------------------------------

    #[test]
    fn a_library_is_asked_once_per_window() {
        let throttle = ScanThrottle::default();
        assert_eq!(throttle.recent("lib", NOW), None);
        throttle.record("lib", NOW);
        assert_eq!(throttle.recent("lib", NOW), Some(NOW));
        assert_eq!(
            throttle.recent("lib", NOW + SCAN_THROTTLE_SECS - 1),
            Some(NOW)
        );
        assert_eq!(throttle.recent("lib", NOW + SCAN_THROTTLE_SECS), None);
        assert_eq!(throttle.recent("other", NOW), None, "per library");
        assert_eq!(
            throttle.recent("lib", NOW - 1),
            None,
            "a clock that went back asks again"
        );
    }

    #[test]
    fn a_person_asking_to_try_now_opens_the_window_again() {
        let throttle = ScanThrottle::default();
        throttle.record("lib", NOW);
        throttle.record("other", NOW);
        throttle.forget();
        assert_eq!(throttle.recent("lib", NOW), None);
        assert_eq!(throttle.recent("other", NOW), None);
    }

    // ----- what the scans mean for a row -----------------------------------------------------

    fn fresh(at: i64) -> Scan {
        Scan::Asked { at, fresh: true }
    }

    fn reused(at: i64) -> Scan {
        Scan::Asked { at, fresh: false }
    }

    #[test]
    fn a_scan_this_pass_asked_covers_every_row_it_read_even_in_the_same_second() {
        for created_at in [NOW - 5, NOW, NOW + 1] {
            let outcome = after_scan(
                &row(created_at),
                &libs(&["lib-photos"]),
                &scans(&[("lib-photos", fresh(NOW))]),
            );
            assert_eq!(outcome, AfterScan::Asked, "created at {created_at}");
        }
    }

    #[test]
    fn a_reused_scan_covers_a_row_only_when_it_was_asked_strictly_after_the_change() {
        let covered = after_scan(
            &row(NOW - 5),
            &libs(&["lib-photos"]),
            &scans(&[("lib-photos", reused(NOW))]),
        );
        assert_eq!(covered, AfterScan::Asked);
        // A whole second says nothing about the order: the change may have come after the scan.
        let same_second = after_scan(
            &row(NOW),
            &libs(&["lib-photos"]),
            &scans(&[("lib-photos", reused(NOW))]),
        );
        assert_eq!(
            same_second,
            AfterScan::Wait {
                until: NOW + SCAN_THROTTLE_SECS
            }
        );
    }

    #[test]
    fn a_scan_asked_before_the_change_does_not_cover_it_and_the_row_waits_for_the_throttle() {
        let outcome = after_scan(
            &row(NOW),
            &libs(&["lib-photos"]),
            &scans(&[("lib-photos", reused(NOW - 10))]),
        );
        assert_eq!(
            outcome,
            AfterScan::Wait {
                until: NOW - 10 + SCAN_THROTTLE_SECS
            }
        );
    }

    #[test]
    fn a_move_between_two_libraries_is_covered_when_both_were_asked_by_this_pass() {
        // The livelock this guards against: the second library's scan is asked a moment after the
        // first one's, so a rule that compared with a row time moved by the pass would never agree.
        let outcome = after_scan(
            &row(NOW - 1),
            &libs(&["lib-archive", "lib-photos"]),
            &scans(&[("lib-photos", fresh(NOW)), ("lib-archive", fresh(NOW + 1))]),
        );
        assert_eq!(outcome, AfterScan::Asked);
    }

    #[test]
    fn one_library_that_was_asked_too_early_makes_the_row_wait_for_the_latest_reopening() {
        let outcome = after_scan(
            &row(NOW),
            &libs(&["lib-archive", "lib-photos"]),
            &scans(&[
                ("lib-photos", fresh(NOW + 1)),
                ("lib-archive", reused(NOW - 30)),
            ]),
        );
        assert_eq!(
            outcome,
            AfterScan::Wait {
                until: NOW - 30 + SCAN_THROTTLE_SECS
            }
        );
    }

    #[test]
    fn a_row_that_waited_for_the_throttle_is_covered_when_it_reopens() {
        // The two-library move that converges: the second pass asks the first library again and
        // reuses the second one's scan, which is now later than the row's change.
        let outcome = after_scan(
            &row(NOW - 30),
            &libs(&["lib-archive", "lib-photos"]),
            &scans(&[
                ("lib-photos", fresh(NOW)),
                ("lib-archive", reused(NOW - 20)),
            ]),
        );
        assert_eq!(outcome, AfterScan::Asked);
    }

    #[test]
    fn a_library_that_cannot_be_scanned_is_ignored_when_another_was() {
        let outcome = after_scan(
            &row(NOW - 5),
            &libs(&["lib-archive", "lib-photos"]),
            &scans(&[("lib-photos", fresh(NOW)), ("lib-archive", Scan::Cannot)]),
        );
        assert_eq!(outcome, AfterScan::Asked);
    }

    #[test]
    fn without_any_scan_the_row_waits_for_immich_itself() {
        let cannot = scans(&[("lib-photos", Scan::Cannot)]);
        assert_eq!(
            after_scan(&row(NOW), &libs(&["lib-photos"]), &cannot),
            AfterScan::NoScan
        );
        assert_eq!(
            after_scan(&row(NOW), &BTreeSet::new(), &BTreeMap::new()),
            AfterScan::NoScan
        );
    }

    #[test]
    fn a_failed_scan_fails_the_row_for_now() {
        let outcome = after_scan(
            &row(NOW - 5),
            &libs(&["lib-archive", "lib-photos"]),
            &scans(&[
                ("lib-photos", fresh(NOW)),
                ("lib-archive", Scan::Retry("busy".to_owned())),
            ]),
        );
        assert_eq!(outcome, AfterScan::Retry("busy".to_owned()));
    }

    // ----- the end of phase 2 ----------------------------------------------------------------

    #[test]
    fn a_tracked_row_moves_to_stage_one_and_is_looked_at_after_the_scan_settled() {
        let step = conclude(&row(NOW - 5), true, AfterScan::Asked, NOW);
        assert_eq!(step.patch.stage, Some(1));
        assert_eq!(step.patch.attempts, Some(0));
        assert_eq!(step.patch.scan_requested_at, Some(NOW));
        assert_eq!(step.patch.next_attempt_at, Some(NOW + SCAN_SETTLE_SECS));
        assert_eq!(step.patch.last_error, Some(None));
        assert!(matches!(step.then, Then::Stays(Note::Waiting)));
    }

    #[test]
    fn a_row_nobody_can_scan_for_moves_to_stage_one_without_a_scan_time() {
        let step = conclude(&row(NOW - 5), true, AfterScan::NoScan, NOW);
        assert_eq!(step.patch.stage, Some(1));
        assert_eq!(step.patch.scan_requested_at, None);
        assert_eq!(step.patch.next_attempt_at, Some(NOW + 15));
    }

    #[test]
    fn an_untracked_row_is_finished_by_the_scan_alone() {
        let scanned = conclude(&row(NOW - 5), false, AfterScan::Asked, NOW);
        assert!(matches!(scanned.then, Then::Finished(Finish::Scanned)));
        let dropped = conclude(&row(NOW - 5), false, AfterScan::NoScan, NOW);
        assert!(matches!(dropped.then, Then::Finished(Finish::Dropped)));
    }

    #[test]
    fn a_row_that_waits_for_the_throttle_keeps_its_stage_and_its_tries() {
        let step = conclude(&row(NOW), true, AfterScan::Wait { until: NOW + 40 }, NOW);
        assert_eq!(
            step.patch,
            Patch {
                next_attempt_at: Some(NOW + 40),
                ..Patch::default()
            }
        );
        assert!(matches!(step.then, Then::Stays(Note::Waiting)));
    }

    #[test]
    fn a_failed_scan_backs_the_row_off_and_names_why() {
        let step = conclude(
            &row(NOW - 5),
            true,
            AfterScan::Retry("busy".to_owned()),
            NOW,
        );
        assert_eq!(step.patch.attempts, Some(1));
        assert_eq!(step.patch.next_attempt_at, Some(NOW + 15));
        assert_eq!(
            step.patch.last_error,
            Some(Some("immich_unavailable".to_owned()))
        );
        assert!(matches!(step.then, Then::Stays(Note::Retry(ref why)) if why == "busy"));
    }

    #[test]
    fn a_scan_that_keeps_failing_for_days_needs_a_person() {
        let stuck = Row {
            attempts: ATTENTION_MIN_ATTEMPTS,
            created_at: NOW - ATTENTION_AFTER_NO_SCAN_SECS,
            ..row(NOW)
        };
        let step = conclude(&stuck, true, AfterScan::Retry("busy".to_owned()), NOW);
        assert!(matches!(step.then, Then::Stays(Note::Attention)));
    }
}
