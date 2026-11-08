//! Tuning constants of the outbox. Each one says what it protects; change them together with the
//! tests that name them.

use std::time::Duration;

/// How often the scheduler looks at the queue even when nothing woke it (the hook wakes it on
/// every enqueue, so this only covers rows that become due by the clock).
pub const TICK: Duration = Duration::from_secs(30);

/// After a wake-up the scheduler waits this long before it looks at the queue: a burst of
/// enqueues (one per moved file) costs a handful of looks, not thousands.
pub const WAKE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A batch that never becomes quiet (the user keeps moving files) still starts once the oldest
/// due row has been due for this long, so a busy day cannot postpone Immich forever.
pub const MAX_DEFERRAL_SECS: i64 = 10 * 60;

/// After a pass asked for library scans, the stage-1 rows are looked at again this long
/// afterwards: a scan of a normal library takes seconds, and the pass after it is cheap.
pub const SCAN_SETTLE_SECS: i64 = 15;

/// At most one `POST /libraries/{id}/scan` per library in this window: a burst of passes must not
/// queue scans faster than Immich runs them.
pub const SCAN_THROTTLE_SECS: i64 = 60;

/// A row whose old asset is offline but for which Immich shows no new asset (the file left the
/// libraries, or sits in an excluded folder) is removed from Immich once this long has passed since
/// the old asset was first seen offline.
pub const NEW_ASSET_GRACE_SECS: i64 = 10 * 60;

/// A row still unresolved this long after its scan was requested needs a human when WE can ask
/// Immich to scan (administrator account).
pub const ATTENTION_AFTER_SCAN_SECS: i64 = 2 * 60 * 60;

/// The same when we cannot ask for a scan and have to wait for Immich's own schedule, which runs
/// at most daily: three days leave room for a weekend.
pub const ATTENTION_AFTER_NO_SCAN_SECS: i64 = 72 * 60 * 60;

/// The wait before a row is tried again, by the number of attempts it already had; the last entry
/// repeats.
pub const ROW_BACKOFF_SECS: [i64; 8] = [15, 30, 60, 120, 300, 600, 1_200, 1_800];

/// The wait after a pass that failed as a whole (Immich unreachable, key rejected, server error),
/// by the number of such passes in a row; the last entry repeats.
pub const FAILURE_BACKOFF_SECS: [i64; 6] = [60, 120, 300, 900, 1_800, 3_600];

/// Rows read and checked per round of live calls: bounds the memory of a pass and how long one
/// transaction of progress can take.
pub const CHUNK: usize = 200;

/// Rows worked on at the same time. Immich is a home server: four requests in flight keep it busy
/// without starving the web UI of the person using it.
pub const CONCURRENCY: usize = 4;

/// Due rows one pass takes (the rest waits for the next pass, which starts at once when rows are
/// left). Large on purpose: a library scan turns the old asset of EVERY moved file offline, so a
/// row that is not snapshotted in the same pass may lose what Immich knew about its asset. The
/// pass is one cancellable task that persists chunk by chunk, so it can safely run for a while.
pub const MAX_ROWS_PER_PASS: usize = 50_000;

/// Ids per statement when rows are updated or deleted by id: well below SQLite's limit of binds.
pub const ID_CHUNK: usize = 5_000;

/// The longest error text kept on a row or in the state.
pub const MAX_ERROR_CHARS: usize = 300;

/// A row is only flagged for a person after this many tries, whatever its age: a row that was
/// retried by hand after it outgrew its limit gets a real new chance instead of bouncing back to
/// "needs attention" at its first check.
pub const ATTENTION_MIN_ATTEMPTS: i64 = 3;

/// A pass gives up as a whole when this many rows in a row failed with an error that may pass
/// (Immich is down or overloaded): the rows that follow would fail the same way.
pub const BREAKER: usize = 10;

/// Wait before a row is tried again.
pub fn row_backoff_secs(attempts: i64) -> i64 {
    let index = usize::try_from(attempts.max(0)).unwrap_or(usize::MAX);
    ROW_BACKOFF_SECS[index.min(ROW_BACKOFF_SECS.len() - 1)]
}

/// Wait after `failures` passes in a row that failed as a whole (at least 1).
pub fn failure_backoff_secs(failures: i64) -> i64 {
    let index = usize::try_from((failures - 1).max(0)).unwrap_or(usize::MAX);
    FAILURE_BACKOFF_SECS[index.min(FAILURE_BACKOFF_SECS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_backs_off_by_its_attempts_and_the_last_wait_repeats() {
        let waits: Vec<i64> = (0..10).map(row_backoff_secs).collect();
        assert_eq!(
            waits,
            [15, 30, 60, 120, 300, 600, 1_200, 1_800, 1_800, 1_800]
        );
        assert_eq!(row_backoff_secs(-3), 15);
        assert_eq!(row_backoff_secs(i64::MAX), 1_800);
    }

    #[test]
    fn a_failing_pass_backs_off_one_two_five_fifteen_thirty_sixty_minutes() {
        let minutes: Vec<i64> = (1..=8).map(|n| failure_backoff_secs(n) / 60).collect();
        assert_eq!(minutes, [1, 2, 5, 15, 30, 60, 60, 60]);
        assert_eq!(failure_backoff_secs(0), 60);
    }

    #[test]
    fn the_limits_of_a_pass_are_consistent() {
        const {
            assert!(CHUNK <= MAX_ROWS_PER_PASS);
            assert!(CONCURRENCY >= 1);
            assert!(ATTENTION_AFTER_SCAN_SECS < ATTENTION_AFTER_NO_SCAN_SECS);
            assert!(NEW_ASSET_GRACE_SECS < ATTENTION_AFTER_SCAN_SECS);
            assert!(ID_CHUNK <= 30_000);
            assert!(ATTENTION_MIN_ATTEMPTS >= 1);
            assert!(BREAKER >= CONCURRENCY);
        }
    }
}
