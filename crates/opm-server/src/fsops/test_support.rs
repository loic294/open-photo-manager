//! Scratch directories for the unit tests of this module.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

/// A scratch directory under the workspace's `target/tmp` (never `/tmp`), removed when dropped.
/// It is canonicalised, like the roots of real volumes.
pub(crate) fn scratch(label: &str) -> TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix(&format!("fsops-{label}-"))
        .tempdir_in(base.canonicalize().unwrap())
        .unwrap()
}
