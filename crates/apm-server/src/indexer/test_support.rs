//! Helpers shared by the unit tests of the indexer: scratch directories and file creation.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// A scratch directory under the workspace's `target/tmp`, removed when dropped. It is
/// canonicalised, like the roots of real volumes.
pub(crate) fn scratch_dir() -> TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&base).unwrap();
    TempDir::new_in(base.canonicalize().unwrap()).unwrap()
}

/// Creates `root/rel` with `bytes`, and the directories above it.
pub(crate) fn touch(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}
