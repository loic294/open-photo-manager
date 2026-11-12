//! Engine tests: scheduling, lifecycle, cancellation, persistence and shutdown.
//!
//! The crate's Tokio has no `test-util` feature, so the clock is never paused: intervals are
//! shortened with `support::fast`, waits poll against a deadline, and the speed and ETA tests
//! inject a manual `Clock`. A check that nothing happens (`support::QUIET`) can only fail when the
//! engine does something it must not.

mod cancel;
mod events;
mod lanes;
mod lifecycle;
mod reporting;
mod restart;
mod shutdown;
mod support;
