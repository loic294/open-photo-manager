//! User-facing trash management: what the Trash page shows (the listing, the deletions and the
//! summary), restoring, emptying, the retention sweeper and the sync with the disk.
//!
//! Owned by todo `building-trash-management` (with `api/trash.rs`). The physical moves (trash,
//! restore, purge) are `fsops`: nothing here changes a volume path, and the sync only reads
//! `.apm-trash`.
//!
//! # What the trash is
//! A trashed item sits at `<volume root>/.apm-trash/<batch>/<original relative path>` and has one
//! `trash_items` row. A batch is what one operation trashed on one volume (the batch name is
//! `YYYYMMDD-HHMMSS-t<task id>` for a task, `...-r<hex>` for an ad-hoc call and `...-x<journal
//! id>` for the source a copy-and-delete move keeps), so `(volume_id, batch)` is one deletion as
//! the user sees it, and "Restore all" of a deletion is one request. The row of a folder covers
//! its whole content (its `size` is the indexed bytes); a file has its own row. `reason` is free
//! text from the caller: `user`, `replaced`, `junk`, `moved`, or `NULL` for an item found on disk
//! (reconcile, [`TrashService::sync`]). `meta_json` is what the index knew (`NULL` for an adopted
//! item, which then restores like a new file).
//!
//! Item ids come from an `AUTOINCREMENT` column (migration 0047) and are never given out twice:
//! that is what makes `up_to_id` of [`TrashService::empty`] a promise.
//!
//! # Restore
//! [`TrashService::restore`] queues ONE `fs_move` task on the `fs` lane for the ticked ids or for a
//! whole batch. The items come back through `FsOps::restore` to the place the trash remembers,
//! one at a time in id order; a name that is taken again is decided by the conflict policy of the
//! request (`null`: the `default_conflict_policy` setting). A skipped item stays in the trash. A
//! failure of one item never stops the next.
//!
//! # Empty and retention
//! "Empty trash" is the only way data is deleted for good (A15). [`TrashService::empty`] deletes
//! the items with `id <= up_to_id` (the `max_id` of the summary the user was looking at), so what
//! was trashed afterwards survives. The retention sweeper ([`TrashService::sweep_retention`]) is
//! the same runner with another selector (everything trashed before a cutoff). Both go through
//! `FsOps::purge`, one item at a time, and both run as a `trash_empty` task on the `fs` lane.
//!
//! # Outcome of a task
//! A run without failures ends `done` with a JSON result (`restored` or `deleted`, `skipped`,
//! `failed`, `bytes`; `adopted`, `removed`, `held` for a sync) and a title that says what was
//! done. A run with a failure ends `failed`: the error text counts the failures and names the
//! first three with their `FsOpError` code, and the task indicator keeps the task until the user
//! dismisses it. Skips do not fail a task.
//!
//! # Sync with the disk
//! [`TrashService::sync`] (`POST /api/trash/sync`) makes the rows agree with `.apm-trash` after
//! the database was lost or the trash was cleaned by hand: untracked files are adopted, rows whose
//! file is gone are removed (unless that looks like an unmounted share). It is an `index` task on
//! the `fs` lane. See `sync.rs`.
//!
//! # Reading
//! The listing and the deletions are read from the index only and never change state. The page
//! opens on three requests; their timings on a million items in 2,500 deletions (release build,
//! `tests/trash_perf.rs`) are:
//!
//! * the summary, 0.1 ms: items and bytes of each volume are the running totals of
//!   `trash_volume_totals` (kept by triggers, migration 0048) and the highest id is one descent of
//!   the table, so it costs the same at a thousand items as at a million;
//! * the first page of the listing, newest first, 0.3 ms (the same for one volume, for one
//!   deletion and for the oldest first): the `trashed_at` indexes of migration 0048 answer it
//!   without a sort;
//! * the deletions, 0.2 s: the grouped list and its count each read every row once, through a
//!   covering index, so the cost grows with the rows of the trash rather than with the page.
//!
//! `schema_tests.rs` pins these plans. Deliberately not indexed: a deep page (offset 100,000)
//! takes 0.14 s, a sort by original path, size or name 0.6 s, 1.5 s or 2.9 s, and a search 0.15 s
//! when it matches a few items and 1.5 s when it matches nothing, because each reads the table.
//! Nobody pages that deep in a trash, and an index per sort would slow every trashing down.

mod empty;
mod list;
mod report;
mod restore;
mod retention;
#[cfg(test)]
mod schema_tests;
mod sync;

use std::sync::Arc;

use apm_core::dto::FieldError;

use crate::db::Db;
use crate::error::AppError;
use crate::fsops::{FsOps, OpContext, TaskChangeSink};
use crate::indexer::VolumeRegistry;
use crate::settings::SettingsService;
use crate::tasks::{TaskCtx, TaskEngine};

/// Seconds since the epoch; a test replaces the wall clock to age the trash.
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The trash page's service. Cheap to clone.
#[derive(Clone)]
pub struct TrashService {
    db: Db,
    settings: SettingsService,
    tasks: TaskEngine,
    volumes: VolumeRegistry,
    fsops: FsOps,
    clock: Clock,
    /// Held while the retention sweeper decides, so two sweeps never submit two tasks.
    sweeping: Arc<tokio::sync::Mutex<()>>,
}

impl TrashService {
    pub fn new(
        db: Db,
        settings: SettingsService,
        tasks: TaskEngine,
        volumes: VolumeRegistry,
        fsops: FsOps,
    ) -> Self {
        Self {
            db,
            settings,
            tasks,
            volumes,
            fsops,
            clock: Arc::new(crate::db::now_secs),
            sweeping: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Replaces the clock the retention sweeper ages the trash with (epoch seconds).
    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// The context of the file operations of a task. Bytes are counted by the runner per item
    /// (`TaskProgress::add`), so the context's own byte callback stays silent: counting both would
    /// double the bar.
    fn op_context(&self, ctx: &TaskCtx) -> OpContext {
        OpContext::new(
            Some(ctx.id),
            ctx.cancel.clone(),
            Arc::new(|_| {}),
            Arc::new(TaskChangeSink::new(self.tasks.clone())),
        )
    }
}

/// Why a request to the trash was refused.
#[derive(Debug)]
pub enum TrashError {
    /// Fields of the request are invalid (a 422 that names them).
    Validation(Vec<FieldError>),
    /// Anything else, with its own status and code.
    Failed(AppError),
}

impl From<AppError> for TrashError {
    fn from(err: AppError) -> Self {
        Self::Failed(err)
    }
}

/// `i64` for a `u64` the database stores (sizes), saturating.
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `u64` for an `i64` the database returns (counts and sizes are never negative; a corrupt
/// negative value counts as 0).
fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}
