//! Filesystem indexer: walks the volumes, extracts metadata, hashes files and
//! keeps the index fresh.
//!
//! `mod.rs` (the [`Indexer`] orchestrator) and `scanner` are owned by todo
//! `implementing-scanner`; every other submodule names its own owner in its
//! docs.

pub mod extract;
pub mod hash;
pub mod scanner;
pub mod watcher;
pub mod xmp;

/// Orchestrates full scans and incremental updates. Empty until
/// `implementing-scanner` implements it.
#[derive(Clone, Debug, Default)]
pub struct Indexer;

impl Indexer {
    pub fn new() -> Self {
        Self
    }
}
