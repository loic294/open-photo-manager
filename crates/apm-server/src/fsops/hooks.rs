//! Hooks that run INSIDE the transaction that makes an operation's index change final.
//!
//! The Immich outbox and the trash management register one through
//! [`FsOps::with_hook`](super::FsOps::with_hook) so that "the file moved" and "an outbox row was
//! queued" commit or roll back together.
//!
//! # Contract
//! * The hook runs on the transaction connection: use `&mut *tx` for every statement, never the
//!   pool (the database has one writer connection and the transaction holds it).
//! * It runs once per applied operation, after the `files` rows reached their final state, with
//!   ONE exception: [`AppliedOp::Trashed`] is delivered BEFORE the `files` rows are deleted (the
//!   foreign keys cascade, so the rows and what hangs on them must still be readable).
//! * Hooks are called in registration order. A hook that fails rolls the transaction back; the
//!   file operation then returns `internal` and leaves its journal row `pending`, so `reconcile`
//!   runs the hooks again. Hooks therefore MUST be idempotent.
//! * Keep a hook short: it holds the only write connection.

use apm_core::paths::{FileId, RelPath, VolumeId};
use futures_util::future::BoxFuture;
use sqlx::SqliteConnection;

use super::Loc;

/// What an operation did, as the hooks see it. The data is owned so a hook can keep it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedOp {
    /// A file moved or was renamed. `file_id` is the row that now lives at `to`.
    Moved {
        file_id: FileId,
        from: Loc,
        to: Loc,
    },
    /// A file was copied. `source_file_id` is the row of the original when it is indexed.
    Copied {
        source_file_id: Option<FileId>,
        file_id: FileId,
        from: Loc,
        to: Loc,
    },
    /// A file or folder went into the trash. `file_ids` lists the `files` rows that are about to
    /// be deleted (every file below a folder); they still exist while the hook runs.
    Trashed {
        file_ids: Vec<FileId>,
        from: Loc,
        trash_item_id: i64,
        trash_path: RelPath,
        is_dir: bool,
    },
    /// A trash item came back. `file_id` is the recreated row (none for a folder: its files are
    /// indexed by a scan right after).
    Restored {
        file_id: Option<FileId>,
        trash_item_id: i64,
        from: Loc,
        to: Loc,
        is_dir: bool,
    },
    /// A trash item was deleted for good.
    Purged {
        trash_item_id: i64,
        volume_id: VolumeId,
        trash_path: RelPath,
        is_dir: bool,
        bytes: u64,
    },
}

/// See the module docs.
pub trait OpHook: Send + Sync {
    fn on_applied<'a>(
        &'a self,
        tx: &'a mut SqliteConnection,
        op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>>;
}
