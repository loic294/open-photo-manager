//! Helpers shared by the unit tests of the Immich modules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use tempfile::TempDir;

/// A scratch directory below `target/tmp` (never the system temp dir), deleted when dropped.
pub(super) fn scratch_dir() -> TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    std::fs::create_dir_all(&base).unwrap();
    TempDir::new_in(base.canonicalize().unwrap()).unwrap()
}
