//! Mirrors what the file operations do to photos and videos into Immich, without ever making a
//! file operation wait for Immich.
//!
//! # The problem
//!
//! Immich reads photos from external libraries and knows each file by its path. When this
//! application moves, renames or trashes a file, the asset at the old path becomes *offline* at the
//! next library scan, and (for a move) a NEW asset appears at the new path: its albums, favorite,
//! rating and tags are gone. Left alone, Immich fills up with offline duplicates and loses
//! everything the user did there.
//!
//! # How it works
//!
//! ```text
//!  FsOps transaction ──► hook ──► immich_outbox row (same transaction, never blocks)
//!                                        │
//!   quiet for `immich_quiet_secs` ◄──────┘  scheduler (30 s tick + wake-ups)
//!                                        ▼
//!        one `ImmichCleanup` task on the `immich` lane (cancellable, progress, history)
//!   refresh ──► snapshot ──► scan libraries ──► check ──► carry over ──► delete old asset
//! ```
//!
//! - **Hook** (`hook`): one fast statement or two inside the transaction that finalises the
//!   operation. Several changes of one file fold into one row (`merge`).
//! - **Scheduler** (`service`): waits until file operations stopped for the quiet window, no file
//!   operation runs, Immich is connected and no cleanup task is active, then submits ONE task.
//!   A burst of 10,000 moves is one task. A failed pass backs off 1 to 60 minutes.
//! - **Pass** (`pass`): reads the pending rows, re-reads the affected libraries
//!   (`ImmichSync::refresh`), snapshots the albums, favorite, rating and tags of each old asset
//!   while it is still live, asks Immich to scan the libraries ONCE (administrators only), and
//!   then, row by row, waits for the old asset to go offline and the new asset to be imported,
//!   copies the snapshot onto the new asset and deletes the offline one (`decide`, `carry`).
//! - **Status**: pending rows, rows waiting for Immich, rows that need attention and the last
//!   pass are in `GET /api/immich/status` (`outbox`), listed by `GET /api/immich/outbox`, and
//!   retried or dismissed with `POST /api/immich/outbox/retry` and `/dismiss`.
//!
//! # Safety rules (every one has a test)
//!
//! The outbox is the only code that deletes anything in Immich, and it only ever deletes an asset
//! that is already **offline** (Immich lost its file), so no file of the user is at risk:
//!
//! 1. A delete is `DELETE /assets` through `ImmichClient::delete_assets`, whose argument type
//!    (`OfflineAssetId`) can only be built from an asset that a FRESH `GET /assets/{id}` of the
//!    same step showed as offline; never from the local cache. For an offline asset Immich
//!    removes the record only, never the file, so `force` is safe and is what makes the record
//!    leave the trash.
//! 2. Only assets named by an outbox row are ever touched.
//! 3. The old path of the row, translated by the `PathMap`, must be the fresh `originalPath` of
//!    the asset, AND the file must be absent from the volume (`VolumeRoot::stat`): a file that
//!    came back, or whose state cannot be read, is never deleted from.
//! 4. A carry-over that fails for good (Immich refuses it) blocks the delete and puts the row in
//!    `attention`; one that fails for a passing reason is retried with a back-off.
//! 5. The hook cannot break a file operation (it fails only when its statement fails).
//! 6. Every call to Immich runs in the cancellable task, never in a request handler.
//!
//! # Rows
//!
//! A row says "the file at `old_*` is now at `new_*`": a move has both sides, a trash only the old
//! one, a copy or a restore only the new one. A row is deleted when its change is done, so the
//! table only holds work to do or work for a person and never grows with history.
//!
//! - **Stage 0**: no library scan was asked for the change yet. The hook still merges new changes
//!   of the file into the row (the first old place stays, the last new place wins, and a file that
//!   is back where it was cancels the row). The pass snapshots the old asset, asks for the scan
//!   and makes the row stage 1.
//! - **Stage 1**: the scan was asked and the row waits for Immich to notice it. A later pass, at
//!   least `SCAN_SETTLE_SECS` after, checks it. A new change of the file is a NEW row.
//! - **State** `pending` (in work, or waiting for its next try) or `attention` (a person decides).
//!   `last_error` holds one of these codes, which the status page explains:
//!
//!   | code | meaning | the row |
//!   |------|---------|---------|
//!   | `unreadable` | the volume of the file cannot be read | pending, flagged after 3 days |
//!   | `immich_unavailable` | Immich failed or could not be reached | pending, flagged after 3 days |
//!   | `not_offline` | Immich still shows the old asset online after the wait | attention |
//!   | `scan_timeout` | Immich did not notice, or import, the change in time | attention |
//!   | `carry_failed` | Immich refused to take albums, favorite, rating or tags over | attention |
//!   | `path_mismatch` | the asset Immich holds is not at the path of the file | attention |
//!   | `immich_error` | Immich refused a request for good | attention |
//!
//!   A row waits 2 hours after its scan was asked (3 days when the account cannot ask for scans)
//!   and 3 tries before `not_offline` or `scan_timeout` flag it.
//!
//! A pass that cannot go on at all (Immich is down, the key is rejected) ends as a whole, leaves
//! every row it did not settle as it was and backs off 1, 2, 5, 15, 30 and then 60 minutes.
//!
//! # Retry, dismiss and reconnect
//!
//! - **Retry** (`ImmichOutbox::retry`) makes the named rows due now with a fresh count of tries
//!   (without ids: every row that needs attention or waits for a back-off), ends the back-off of
//!   failed passes, lets the next pass ask for a scan even if one was asked a minute ago and
//!   starts a pass without the quiet window. `retried` counts the rows it changed. A pass that a
//!   person cancelled backs off 60 seconds, and a retry ends that too.
//! - **Dismiss** forgets rows without touching Immich: an offline asset stays there for Immich's
//!   own clean-up.
//! - **Connect again**: the same server and user keep the queue and the cached assets and only
//!   end the back-off; another server or account, and a disconnect, clear the queue, which only
//!   means something for the server it was queued for. Nothing is queued while Immich is not
//!   connected, or when neither side of the change is in a mapped volume.
//! - When the queue is drained one incremental sync (`ImmichSync::submit`) reads what changed.
//!
//! # Limits
//!
//! - Scans on demand need an administrator account. Without one the library scan is Immich's own
//!   schedule: rows stay pending until it ran (72 hours before they are flagged).
//! - Immich must exclude `**/.opm-trash/**` from its libraries, or a trashed file comes back into
//!   it as a new asset.
//! - A file moved again AFTER the scan of its first change was asked is a second row, not a
//!   merge: it waits for the scan throttle (or a retry) and carries what the intermediate asset
//!   holds by then, so a chain of moves only ends well while the intermediate asset is online
//!   when the later row is prepared. A trash and a restore cancel out while the row waits for
//!   its scan; afterwards they are two rows and the restore supersedes the trash.
//! - A new asset that Immich takes more than 10 minutes to import is not waited for: when the file
//!   is on disk and no asset shows, the old asset goes without a carry-over.
//! - People, stacks and shared links are not carried over, and an asset that was trashed in
//!   Immich and restored loses its albums. A folder that is trashed or restored is not reflected
//!   file by file: its assets go offline and Immich's own trash retention removes them.
//! - A move into a junk path (`@eaDir`, `#recycle`) is not reported by `fsops`, so it is not
//!   reflected either.
//! - One pass takes at most `MAX_ROWS_PER_PASS` (50,000) due rows and the rest follows at once.
//!   When the refresh of the asset list fails, so does the pass (with its back-off) and no row is
//!   touched.
//! - Assets are found by place only in a volume they resolved to (`assets::find_by_location`), and
//!   an asset that Immich deleted for good only leaves the cache at the daily full sync.
//! - A running application keeps the client and the API key in memory: a lost key file only
//!   bites after a restart.
//! - `created_at` of a row, and the age of the oldest change in the status, are the time of the
//!   LATEST change recorded for it. `carried` of a report counts when the row is deleted.
//!
//! # Tuning
//!
//! `OPM_IMMICH_QUIET_SECS` (default 15, 1 to 600) is the quiet window, counted in whole seconds, so
//! a pass can start up to a second early; the end-to-end tests set 1. The constants of `consts`
//! hold every other time and size (the 15 seconds Immich is given to scan and the 60 seconds
//! between two scans of a library are not configurable).
//!
//! # Measured
//!
//! `tests/immich_outbox_perf.rs` (ignored; release build; the command is in its docs), on the
//! development machine:
//!
//! - The hook adds about 10% to a whole move and 19% to its finalising transaction (median of
//!   10,000 moves each way: 200 to 239 µs; the budget is 25%). A queued operation costs 78 to
//!   91 µs however long the queue is (the last 10,000 of 100,000 cost 1.03 times the first),
//!   one that merges or cancels 79 to 83 µs, and nothing is logged per operation.
//! - 10,000 moves in 5.3 s are ONE task, 5 s after the last move (with a 5 s quiet window).
//! - A queue of 50,000 rows, 5,000 of them assets Immich holds, against a fake that needs 5 ms
//!   per answer: the pass that asks for the scan takes 22 s (about 2,250 rows/s, 10,007
//!   requests) and the pass that checks and carries over 70 s (72 rows/s, 6.8 requests per row,
//!   4 in flight), 92 s together against a budget of 300 s. The passes add about 80 MiB to the
//!   process.
//!
//! # Tests
//!
//! Unit tests sit beside the code (`merge`, `decide`, `schedule`, and `store`, whose plans for a
//! queue of 50,000 rows are checked). The integration tests run the real `FsOps`, task engine,
//! database and client against the fake Immich over real directories: `immich_outbox_flow.rs`
//! (every kind of change, chains, carry-over, the safety rules), `immich_outbox_faults.rs` (Immich
//! down or refusing, key rejected, cancel, disconnect), `immich_outbox_api.rs` (routes, status,
//! reconnect), `immich_outbox_loop.rs` (the scheduler in real time) and `immich_outbox_perf.rs`.
//! Every test that drives the backend asserts that Immich recorded no forced delete of an online
//! asset.

mod carry;
mod check;
mod consts;
mod decide;
mod direct;
mod hook;
mod merge;
mod pass;
mod prepare;
mod probe;
mod schedule;
mod service;
mod signal;
mod step;
mod store;
#[cfg(test)]
mod test_support;

pub use direct::MovePreflight;
pub use hook::OutboxHook;
pub use service::ImmichOutbox;
pub use signal::OutboxSignal;

use opm_core::dto::OutboxSummary;

use crate::db::{Db, DbError};

/// The outbox numbers of `GET /api/immich/status`. `scans_on_demand`: whether the account can ask
/// Immich to scan its libraries.
pub(super) async fn summary(db: &Db, scans_on_demand: bool) -> Result<OutboxSummary, DbError> {
    store::summary(db, scans_on_demand).await
}

/// Forgets every row: what a disconnect does, since the queue only means something for the server
/// it was queued for.
pub(super) async fn clear(db: &Db) -> Result<(), DbError> {
    store::clear(db).await
}

/// Ends the back-off of failed passes: what a new key for the same account does, since a pass
/// fails as a whole while the key is rejected and would wait up to an hour otherwise.
pub(super) async fn reset_failures(db: &Db) -> Result<(), DbError> {
    store::reset_failures(db).await
}
