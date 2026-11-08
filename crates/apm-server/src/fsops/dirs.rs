//! Folders: `mkdir_all` and the junk-aware `remove_dir_if_removable`.

/// What `remove_dir_if_removable` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmdirOutcome {
    /// The folder is gone; `junk_trashed` junk entries were trashed on the way.
    Removed { junk_trashed: u64 },
    /// The folder did not exist any more.
    AlreadyGone,
}
