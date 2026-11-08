//! When the scheduler starts a pass: one pure rule, so the table in the tests is the whole story.
//!
//! The scheduler (`ImmichOutbox::start_background`) only DECIDES and submits one
//! `TaskKind::ImmichCleanup` task. It wakes when the hook enqueues something, every
//! [`TICK`](super::consts::TICK), and when the quiet period of the last enqueue ends.

use std::time::Duration;

use super::consts::{MAX_DEFERRAL_SECS, TICK};

/// Everything the rule looks at. Times are epoch seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Situation {
    pub now: i64,
    /// An Immich connection is stored.
    pub connected: bool,
    /// An `ImmichCleanup` task is queued or running: one pass at a time.
    pub task_active: bool,
    /// After a pass that failed as a whole, no new pass starts before this instant (`0`: none).
    pub failure_backoff_until: i64,
    /// The earliest `next_attempt_at` among the pending rows, due or not; `None` when no row is
    /// pending.
    pub next_due: Option<i64>,
    /// When the hook last enqueued something (since the server started); `None` if it never did.
    pub last_enqueue: Option<i64>,
    /// A task of the file-operations lane is queued or running: the user is moving files.
    pub fs_busy: bool,
    /// The configured quiet period in seconds (`immich_quiet_secs`).
    pub quiet: i64,
    /// The user asked for it (retry): the quiet period does not apply.
    pub forced: bool,
}

/// Whether a pass should be submitted now.
///
/// It must be connected, no pass may be queued or running, and a pending row must be due. Unless
/// forced, the batch must also be QUIET: no enqueue for `quiet` seconds and no file operation in
/// flight, so Immich is not asked to scan libraries in the middle of a reorganisation. A batch
/// that never becomes quiet starts once its oldest due row has waited [`MAX_DEFERRAL_SECS`].
pub fn should_start(situation: &Situation) -> bool {
    let Some(next_due) = situation.next_due else {
        return false;
    };
    if next_due > situation.now || !situation.connected || situation.task_active {
        return false;
    }
    if situation.now < situation.failure_backoff_until && !situation.forced {
        return false;
    }
    if situation.forced {
        return true;
    }
    if situation.now.saturating_sub(next_due) >= MAX_DEFERRAL_SECS {
        return true;
    }
    !situation.fs_busy && quiet_for(situation) >= situation.quiet
}

/// Seconds since the last enqueue; unbounded when there was none.
fn quiet_for(situation: &Situation) -> i64 {
    situation
        .last_enqueue
        .map_or(i64::MAX, |at| situation.now.saturating_sub(at))
}

/// How long the loop may sleep: until the next tick, or sooner when something it waits for ends
/// before: the quiet period of the last enqueue, the back-off of a failed pass, or the time a
/// row that is not due yet becomes due. Never less than a second.
pub fn sleep_for(situation: &Situation) -> Duration {
    let ends = [
        situation
            .last_enqueue
            .map(|at| at.saturating_add(situation.quiet)),
        Some(situation.failure_backoff_until),
        situation.next_due,
    ];
    ends.into_iter()
        .flatten()
        .filter(|end| *end > situation.now)
        .filter_map(|end| u64::try_from(end - situation.now).ok())
        .map(|secs| Duration::from_secs(secs.max(1)))
        .fold(TICK, Duration::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn ready() -> Situation {
        Situation {
            now: NOW,
            connected: true,
            task_active: false,
            failure_backoff_until: 0,
            next_due: Some(NOW - 5),
            last_enqueue: Some(NOW - 20),
            fs_busy: false,
            quiet: 15,
            forced: false,
        }
    }

    #[test]
    fn a_quiet_batch_with_a_due_row_starts() {
        assert!(should_start(&ready()));
    }

    #[test]
    fn nothing_due_never_starts() {
        let none = Situation {
            next_due: None,
            ..ready()
        };
        assert!(!should_start(&none));
        assert!(!should_start(&Situation {
            forced: true,
            ..none
        }));
    }

    #[test]
    fn not_connected_never_starts() {
        let off = Situation {
            connected: false,
            ..ready()
        };
        assert!(!should_start(&off));
        assert!(!should_start(&Situation {
            forced: true,
            ..off
        }));
        let old = Situation {
            next_due: Some(NOW - 10 * MAX_DEFERRAL_SECS),
            ..off
        };
        assert!(!should_start(&old));
    }

    #[test]
    fn one_pass_at_a_time() {
        let busy = Situation {
            task_active: true,
            ..ready()
        };
        assert!(!should_start(&busy));
        assert!(!should_start(&Situation {
            forced: true,
            ..busy
        }));
        assert!(!should_start(&Situation {
            next_due: Some(NOW - 10 * MAX_DEFERRAL_SECS),
            ..busy
        }));
    }

    #[test]
    fn the_quiet_period_is_counted_from_the_last_enqueue() {
        let just_now = Situation {
            last_enqueue: Some(NOW - 14),
            ..ready()
        };
        assert!(!should_start(&just_now));
        let exactly = Situation {
            last_enqueue: Some(NOW - 15),
            ..ready()
        };
        assert!(should_start(&exactly));
        let never = Situation {
            last_enqueue: None,
            ..ready()
        };
        assert!(
            should_start(&never),
            "rows left from before a restart are due"
        );
    }

    #[test]
    fn a_file_operation_in_flight_postpones_the_batch() {
        let moving = Situation {
            fs_busy: true,
            ..ready()
        };
        assert!(!should_start(&moving));
    }

    #[test]
    fn the_oldest_due_row_overrides_quiet_and_fs_busy_after_the_max_deferral() {
        let waited = |secs| Situation {
            next_due: Some(NOW - secs),
            last_enqueue: Some(NOW - 1),
            fs_busy: true,
            ..ready()
        };
        assert!(!should_start(&waited(MAX_DEFERRAL_SECS - 1)));
        assert!(should_start(&waited(MAX_DEFERRAL_SECS)));
        assert!(should_start(&waited(MAX_DEFERRAL_SECS + 3_600)));
    }

    #[test]
    fn a_retry_skips_the_quiet_period() {
        let forced = Situation {
            last_enqueue: Some(NOW),
            fs_busy: true,
            forced: true,
            ..ready()
        };
        assert!(should_start(&forced));
    }

    #[test]
    fn a_failed_pass_holds_the_next_one_back_until_its_backoff_ends() {
        let backing_off = Situation {
            failure_backoff_until: NOW + 60,
            ..ready()
        };
        assert!(!should_start(&backing_off));
        let later = Situation {
            now: NOW + 60,
            next_due: Some(NOW - 5),
            last_enqueue: Some(NOW - 20),
            ..backing_off
        };
        assert!(should_start(&later));
        assert!(should_start(&Situation {
            forced: true,
            ..backing_off
        }));
    }

    #[test]
    fn a_row_that_is_not_due_yet_does_not_start_a_pass() {
        let later = Situation {
            next_due: Some(NOW + 1),
            ..ready()
        };
        assert!(!should_start(&later));
        assert!(!should_start(&Situation {
            forced: true,
            ..later
        }));
        let due = Situation {
            next_due: Some(NOW),
            ..ready()
        };
        assert!(should_start(&due));
    }

    #[test]
    fn the_loop_sleeps_until_the_quiet_period_ends_when_that_is_sooner_than_a_tick() {
        let soon = Situation {
            last_enqueue: Some(NOW - 10),
            ..ready()
        };
        assert_eq!(sleep_for(&soon), Duration::from_secs(5));
        let long_quiet = Situation {
            quiet: 600,
            last_enqueue: Some(NOW),
            ..ready()
        };
        assert_eq!(sleep_for(&long_quiet), TICK);
        let over = Situation {
            last_enqueue: Some(NOW - 100),
            ..ready()
        };
        assert_eq!(sleep_for(&over), TICK);
        let never = Situation {
            last_enqueue: None,
            ..ready()
        };
        assert_eq!(sleep_for(&never), TICK);
    }

    #[test]
    fn the_loop_sleeps_until_the_next_row_is_due_or_the_backoff_ends() {
        let waiting = Situation {
            next_due: Some(NOW + 12),
            ..ready()
        };
        assert_eq!(sleep_for(&waiting), Duration::from_secs(12));
        let far = Situation {
            next_due: Some(NOW + 3_600),
            ..ready()
        };
        assert_eq!(sleep_for(&far), TICK);
        let backing_off = Situation {
            failure_backoff_until: NOW + 7,
            ..ready()
        };
        assert_eq!(sleep_for(&backing_off), Duration::from_secs(7));
        let nothing = Situation {
            next_due: None,
            last_enqueue: None,
            ..ready()
        };
        assert_eq!(sleep_for(&nothing), TICK);
    }

    #[test]
    fn the_loop_never_sleeps_less_than_a_second() {
        let almost = Situation {
            next_due: Some(NOW + 1),
            failure_backoff_until: NOW + 1,
            last_enqueue: Some(NOW - 14),
            ..ready()
        };
        assert_eq!(sleep_for(&almost), Duration::from_secs(1));
    }
}
