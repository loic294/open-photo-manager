//! Hooks that run INSIDE the transaction that makes an operation's index change final.
//!
//! Register hooks with [`FsOps::with_hook`](super::FsOps::with_hook).

use futures_util::future::BoxFuture;
use opm_core::paths::{FileId, RelPath, VolumeId};
use sqlx::SqliteConnection;

use super::Loc;

/// What an operation did, as the hooks see it. The data is owned so a hook can keep it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedOp {
    /// A file moved or was renamed. `file_id` is the row that now lives at `to`.
    Moved { file_id: FileId, from: Loc, to: Loc },
    /// A file was copied. `source_file_id` is the row of the original when it is indexed.
    Copied {
        source_file_id: Option<FileId>,
        file_id: FileId,
        from: Loc,
        to: Loc,
    },
    /// A file or folder went into the trash. `file_ids` lists the `files` rows that are about to
    /// be deleted (every file below a folder); they still exist while the hook runs. A row a scan
    /// already dropped between the rename and this transaction is not listed: `file_ids` can then
    /// be empty (or short) for a file that was indexed.
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
