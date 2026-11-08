//! The vocabulary the phases of a pass share: what the work on one row came to ([`Step`]), and the
//! rules that turn a wait or a failure into the next state of the row.
//!
//! A phase never writes to the database itself. It reads the facts of ONE row and answers with a
//! [`Step`]: the columns to change and what that means for the pass. The driver (`pass`) collects
//! the steps of a chunk of rows and applies them in one transaction.

use std::collections::BTreeSet;

use apm_core::dto::OutboxState;

use super::consts::{
    ATTENTION_AFTER_NO_SCAN_SECS, ATTENTION_AFTER_SCAN_SECS, ATTENTION_MIN_ATTEMPTS,
    row_backoff_secs,
};
use super::store::{Patch, Row};
use crate::db::Db;
use crate::immich::client::ImmichClient;
use crate::immich::error::ImmichError;
use crate::immich::paths::PathMap;
use crate::indexer::VolumeRegistry;

/// The codes a row keeps in `last_error`; `OutboxItemDto::last_error` explains each to the user.
pub(super) mod code {
    /// The old asset is still online: Immich has not noticed that the file left.
    pub const NOT_OFFLINE: &str = "not_offline";
    /// Immich has not noticed the change (or has not imported the new file) in time.
    pub const SCAN_TIMEOUT: &str = "scan_timeout";
    /// Immich refused to take the albums, favorite, rating or tags over for good.
    pub const CARRY_FAILED: &str = "carry_failed";
    /// The asset Immich holds is not at the path of the file.
    pub const PATH_MISMATCH: &str = "path_mismatch";
    /// Immich refused a request for good.
    pub const IMMICH_ERROR: &str = "immich_error";
    /// The volume of the file could not be read.
    pub const UNREADABLE: &str = "unreadable";
    /// Immich failed or could not be reached.
    pub const IMMICH_UNAVAILABLE: &str = "immich_unavailable";
}

/// What a phase reads while it works on one row. `Copy`: the future of every row owns its copy.
#[derive(Clone, Copy)]
pub(super) struct Shared<'a> {
    pub db: &'a Db,
    pub client: &'a ImmichClient,
    pub map: &'a PathMap,
    pub volumes: &'a VolumeRegistry,
    /// Epoch seconds, read when the work on the row started.
    pub now: i64,
}

/// The outcome for one row.
pub(super) struct Step {
    /// The columns to change; empty when nothing changes.
    pub patch: Patch,
    pub then: Then,
}

/// What a [`Step`] means for the row and for the pass.
pub(super) enum Then {
    /// Phase 1 is done: `libraries` have to be scanned, `tracked` says Immich holds an asset for
    /// the old place of the file.
    Prepared {
        libraries: BTreeSet<String>,
        tracked: bool,
    },
    /// Nothing is left to do for the row: it is deleted.
    Finished(Finish),
    /// The row stays, changed by the patch.
    Stays(Note),
    /// Every other row would fail the same way: the pass ends with this message and every row it
    /// did not settle stays as it was.
    Abort(String),
}

/// Why a row is deleted, for the report of the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Finish {
    /// Nothing of it is in Immich's world (neither place is mapped, or Immich never knew the file).
    Dropped,
    /// An addition (copy, restore) or a move Immich knew nothing of: the library scan was all it
    /// needed.
    Scanned,
    /// The old asset was already gone from Immich.
    Gone,
    /// The file came back or the asset came online again: the change no longer applies.
    Superseded,
    /// The old asset was removed from Immich; `carried` says its state was carried over first.
    Deleted { carried: bool },
}

/// Why a row stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Note {
    /// It waits for Immich (the scan, the import).
    Waiting,
    /// A call failed for a passing reason, with this message.
    Retry(String),
    /// It waited too long or Immich refused: a person decides.
    Attention,
}

impl Step {
    pub fn new(patch: Patch, then: Then) -> Self {
        Self { patch, then }
    }

    pub fn finished(finish: Finish) -> Self {
        Self::new(Patch::default(), Then::Finished(finish))
    }

    pub fn stays(patch: Patch, note: Note) -> Self {
        Self::new(patch, Then::Stays(note))
    }

    pub fn abort(why: impl Into<String>) -> Self {
        Self::new(Patch::default(), Then::Abort(why.into()))
    }

    /// A person has to look at the row; `code` says why.
    pub fn attention(row: &Row, code: &str) -> Self {
        Self::stays(Patch::attention(row.attempts + 1, code), Note::Attention)
    }
}

/// How long a waiting row may stay unresolved before a person is asked: counted from the scan
/// request, two hours when the application asked Immich to scan, three days when it had to wait
/// for Immich's own schedule.
pub(super) fn attention_limit(row: &Row) -> i64 {
    if row.scan_requested_at.is_some() {
        ATTENTION_AFTER_SCAN_SECS
    } else {
        ATTENTION_AFTER_NO_SCAN_SECS
    }
}

/// Whether a row that has been tried `attempts` times and exists since `since` needs a person at
/// `now`: only after a few tries, whatever its age, so a row that was retried by hand gets a real
/// new chance.
pub(super) fn gives_up(attempts: i64, since: i64, now: i64, limit: i64) -> bool {
    attempts >= ATTENTION_MIN_ATTEMPTS && now.saturating_sub(since) >= limit
}

/// The next state of a row that has to wait for Immich: another try after a back-off, or a person
/// after `limit` seconds. `extra` is what the try learned (a first sighting, an asset id).
pub(super) fn wait_a_while(row: &Row, now: i64, limit: i64, code: &str, extra: Patch) -> Step {
    let attempts = row.attempts + 1;
    if gives_up(attempts, row.age_base(), now, limit) {
        let patch = Patch {
            attempts: Some(attempts),
            last_error: Some(Some(code.to_owned())),
            state: Some(OutboxState::Attention),
            ..extra
        };
        return Step::stays(patch, Note::Attention);
    }
    let patch = Patch {
        attempts: Some(attempts),
        next_attempt_at: Some(now + row_backoff_secs(row.attempts)),
        last_error: Some(None),
        ..extra
    };
    Step::stays(patch, Note::Waiting)
}

/// The next state of a row whose try failed for a passing reason: another try after a back-off,
/// with the `code` shown as the reason, or a person after three days. `why` is what the pass
/// reports when Immich failed (such failures add up to the circuit breaker of the pass); `None`
/// is a row that waits for something Immich has no part in (an unreadable volume).
pub(super) fn retry_later(
    row: &Row,
    now: i64,
    code: &str,
    why: Option<&str>,
    extra: Patch,
) -> Step {
    let attempts = row.attempts + 1;
    if gives_up(attempts, row.age_base(), now, ATTENTION_AFTER_NO_SCAN_SECS) {
        let patch = Patch {
            attempts: Some(attempts),
            last_error: Some(Some(code.to_owned())),
            state: Some(OutboxState::Attention),
            ..extra
        };
        return Step::stays(patch, Note::Attention);
    }
    let patch = Patch {
        attempts: Some(attempts),
        next_attempt_at: Some(now + row_backoff_secs(row.attempts)),
        last_error: Some(Some(code.to_owned())),
        ..extra
    };
    Step::stays(
        patch,
        why.map_or(Note::Waiting, |why| Note::Retry(why.to_owned())),
    )
}

/// What a failed call to Immich means for the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    /// Every call would fail the same way (Immich is down, the key is gone): the pass ends.
    Fatal,
    /// A passing reason: the row is tried again later.
    Retry,
    /// Immich refused this request for good: a person decides.
    Permanent,
}

pub(super) fn classify(err: &ImmichError) -> Failure {
    match err {
        // A key that may not do one thing is a matter of this request, not of the connection.
        ImmichError::Forbidden => Failure::Permanent,
        _ if err.affects_every_call() => Failure::Fatal,
        _ if err.is_retryable() => Failure::Retry,
        ImmichError::NotFound
        | ImmichError::Protocol(_)
        | ImmichError::Internal(_)
        | ImmichError::Db(_) => Failure::Retry,
        _ => Failure::Permanent,
    }
}

/// The step for a call that failed with `err`. `permanent` is the code of the row when Immich
/// refused for good. Boxed because it is the error side of a `Result`.
pub(super) fn failed(row: &Row, now: i64, err: &ImmichError, permanent: &str) -> Box<Step> {
    let why = err.summary();
    let failure = classify(err);
    tracing::warn!(row = row.id, ?failure, error = %why, "an Immich call of the outbox failed");
    Box::new(match failure {
        Failure::Fatal => Step::abort(why),
        Failure::Retry => retry_later(
            row,
            now,
            code::IMMICH_UNAVAILABLE,
            Some(&why),
            Patch::default(),
        ),
        Failure::Permanent => Step::attention(row, permanent),
    })
}

/// The step for a database error: logged, never shown (the pass cannot go on without its table).
pub(super) fn storage_failed(err: &impl std::fmt::Display) -> Box<Step> {
    tracing::error!(error = %err, "the Immich outbox could not read its database");
    Box::new(Step::abort(STORAGE_FAILURE))
}

/// What a person sees when the database fails.
pub(super) const STORAGE_FAILURE: &str = "the Immich outbox could not use its database";

#[cfg(test)]
mod tests {
    use apm_core::dto::OutboxState;

    use super::*;
    use crate::immich::outbox::merge::{Effect, Place};

    const NOW: i64 = 10_000_000;

    fn row() -> Row {
        Row {
            id: 7,
            effect: Effect::moved(Place::new(1, "", "a.jpg"), Place::new(1, "", "b.jpg")),
            asset_id: Some("asset".to_owned()),
            new_asset_id: None,
            snapshot_json: None,
            stage: 1,
            carried: false,
            attempts: 0,
            offline_seen_at: None,
            scan_requested_at: Some(NOW - 60),
            created_at: NOW - 120,
        }
    }

    fn unavailable(status: u16) -> ImmichError {
        ImmichError::Upstream {
            status,
            message: "boom".to_owned(),
        }
    }

    #[test]
    fn calls_that_fail_every_time_end_the_pass_and_the_rest_is_judged_by_the_row() {
        for err in [
            ImmichError::NotConnected,
            ImmichError::NeedsReconnect,
            ImmichError::Unreachable("down".to_owned()),
            ImmichError::Unauthorized,
        ] {
            assert_eq!(classify(&err), Failure::Fatal, "{err:?}");
        }
        for err in [unavailable(500), unavailable(503), unavailable(429)] {
            assert_eq!(classify(&err), Failure::Retry, "{err:?}");
        }
        for err in [
            ImmichError::NotFound,
            ImmichError::Protocol("odd".to_owned()),
            ImmichError::Internal("odd".to_owned()),
        ] {
            assert_eq!(classify(&err), Failure::Retry, "{err:?}");
        }
        for err in [
            ImmichError::Forbidden,
            ImmichError::AdminRequired,
            unavailable(400),
            unavailable(422),
            ImmichError::InvalidUrl(String::new()),
            ImmichError::Validation(Vec::new()),
        ] {
            assert_eq!(classify(&err), Failure::Permanent, "{err:?}");
        }
    }

    #[test]
    fn a_waiting_row_backs_off_and_counts_a_try() {
        let step = wait_a_while(&row(), NOW, ATTENTION_AFTER_SCAN_SECS, code::NOT_OFFLINE, {
            Patch {
                offline_seen_at: Some(NOW),
                ..Patch::default()
            }
        });
        assert_eq!(step.then_note(), Some(Note::Waiting));
        assert_eq!(step.patch.attempts, Some(1));
        assert_eq!(step.patch.next_attempt_at, Some(NOW + 15));
        assert_eq!(step.patch.last_error, Some(None));
        assert_eq!(step.patch.offline_seen_at, Some(NOW));
        assert_eq!(step.patch.state, None);
    }

    #[test]
    fn a_row_needs_a_person_only_after_a_few_tries_and_its_age() {
        let limit = ATTENTION_AFTER_SCAN_SECS;
        let old = Row {
            scan_requested_at: Some(NOW - limit),
            attempts: ATTENTION_MIN_ATTEMPTS - 1,
            ..row()
        };
        let step = wait_a_while(&old, NOW, limit, code::SCAN_TIMEOUT, Patch::default());
        assert_eq!(step.then_note(), Some(Note::Attention));
        assert_eq!(step.patch.state, Some(OutboxState::Attention));
        assert_eq!(step.patch.last_error, Some(Some("scan_timeout".to_owned())));

        let few_tries = Row {
            attempts: 0,
            ..old.clone()
        };
        let step = wait_a_while(&few_tries, NOW, limit, code::SCAN_TIMEOUT, Patch::default());
        assert_eq!(step.then_note(), Some(Note::Waiting), "too few tries");

        let young = Row {
            scan_requested_at: Some(NOW - limit + 1),
            ..old
        };
        let step = wait_a_while(&young, NOW, limit, code::SCAN_TIMEOUT, Patch::default());
        assert_eq!(step.then_note(), Some(Note::Waiting), "too young");
    }

    #[test]
    fn the_age_limit_depends_on_whether_a_scan_was_asked() {
        assert_eq!(attention_limit(&row()), ATTENTION_AFTER_SCAN_SECS);
        let silent = Row {
            scan_requested_at: None,
            ..row()
        };
        assert_eq!(attention_limit(&silent), ATTENTION_AFTER_NO_SCAN_SECS);
    }

    #[test]
    fn a_failed_try_keeps_its_code_and_reports_why() {
        let step = retry_later(&row(), NOW, code::UNREADABLE, Some("no"), Patch::default());
        assert_eq!(step.then_note(), Some(Note::Retry("no".to_owned())));
        assert_eq!(step.patch.last_error, Some(Some("unreadable".to_owned())));
        assert_eq!(step.patch.next_attempt_at, Some(NOW + 15));
        let later = Row {
            attempts: 3,
            ..row()
        };
        let step = retry_later(&later, NOW, code::UNREADABLE, Some("no"), Patch::default());
        assert_eq!(step.patch.next_attempt_at, Some(NOW + 120));
    }

    #[test]
    fn a_row_that_waits_for_something_immich_has_no_part_in_is_not_a_failure_of_immich() {
        let step = retry_later(&row(), NOW, code::UNREADABLE, None, Patch::default());
        assert_eq!(step.then_note(), Some(Note::Waiting));
        assert_eq!(step.patch.last_error, Some(Some("unreadable".to_owned())));
    }

    #[test]
    fn a_failed_try_is_flagged_after_three_days_and_a_few_tries() {
        let stuck = Row {
            attempts: 5,
            scan_requested_at: None,
            created_at: NOW - ATTENTION_AFTER_NO_SCAN_SECS,
            ..row()
        };
        let step = retry_later(
            &stuck,
            NOW,
            code::IMMICH_UNAVAILABLE,
            Some("x"),
            Patch::default(),
        );
        assert_eq!(step.then_note(), Some(Note::Attention));
        assert_eq!(step.patch.attempts, Some(6));
    }

    #[test]
    fn a_failed_call_becomes_the_step_its_kind_of_failure_asks_for() {
        let abort = failed(&row(), NOW, &ImmichError::Unauthorized, code::IMMICH_ERROR);
        assert!(matches!(abort.then, Then::Abort(_)));
        assert!(abort.patch.is_empty());

        let retry = failed(&row(), NOW, &unavailable(503), code::IMMICH_ERROR);
        assert_eq!(
            retry.then_note(),
            Some(Note::Retry(unavailable(503).summary()))
        );
        assert_eq!(
            retry.patch.last_error,
            Some(Some("immich_unavailable".to_owned()))
        );

        let refused = failed(&row(), NOW, &unavailable(422), code::CARRY_FAILED);
        assert_eq!(refused.then_note(), Some(Note::Attention));
        assert_eq!(
            refused.patch.last_error,
            Some(Some("carry_failed".to_owned()))
        );
        assert_eq!(refused.patch.state, Some(OutboxState::Attention));
    }

    impl Step {
        fn then_note(&self) -> Option<Note> {
            match &self.then {
                Then::Stays(note) => Some(note.clone()),
                _ => None,
            }
        }
    }
}
