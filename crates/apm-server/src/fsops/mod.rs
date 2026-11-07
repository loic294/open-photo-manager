//! Safe file operations: the only place that mutates files on the volumes.
//!
//! Owned by todo `implementing-safe-file-ops`. Everything that moves, trashes
//! or removes files (plan execution, trash management, cleanup) goes through
//! the executor here.

pub mod backend;
pub mod executor;
pub mod journal;
pub mod trash;

/// File operations service. Empty until `implementing-safe-file-ops`
/// implements it.
#[derive(Clone, Debug, Default)]
pub struct FsOps;

impl FsOps {
    pub fn new() -> Self {
        Self
    }
}
