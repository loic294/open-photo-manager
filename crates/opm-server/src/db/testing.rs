//! Row builders for unit tests that need a few volumes, directories and files without running the
//! scanner. Only the columns that have no default are set; `UPDATE` the rest as the test needs.

use opm_core::names::{extension_lower, normalize_name, split_name};
use opm_core::paths::{FileId, VolumeId};

use super::{Db, now_secs};

/// Inserts an online, writable volume called `name` (root `/volumes/<name>`) and returns its id.
pub(crate) async fn insert_volume(db: &Db, name: &str) -> VolumeId {
    sqlx::query_scalar(
        "INSERT INTO volumes (name, root, writable, online, created_at) VALUES (?, ?, 1, 1, ?) \
         RETURNING id",
    )
    .bind(name)
    .bind(format!("/volumes/{name}"))
    .bind(now_secs())
    .fetch_one(db.write())
    .await
    .unwrap()
}

/// Inserts the directory `path` (`""` is the volume root) and returns its id.
pub(crate) async fn insert_dir(db: &Db, volume_id: VolumeId, path: &str) -> i64 {
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None if path.is_empty() => (None, ""),
        None => (Some(""), path),
    };
    sqlx::query_scalar(
        "INSERT INTO dirs (volume_id, path, parent_path, name) VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(volume_id)
    .bind(path)
    .bind(parent)
    .bind(name)
    .fetch_one(db.write())
    .await
    .unwrap()
}

/// Inserts a file of kind `other`, size 1, with the derived columns (`name_norm`, `stem_norm`,
/// `ext`) filled in the way the scanner does, and returns its id.
pub(crate) async fn insert_file(
    db: &Db,
    volume_id: VolumeId,
    dir_path: &str,
    name: &str,
) -> FileId {
    sqlx::query_scalar(
        "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, mtime) \
         VALUES (?, ?, ?, ?, ?, ?, 'other', 1, 1) RETURNING id",
    )
    .bind(volume_id)
    .bind(dir_path)
    .bind(name)
    .bind(normalize_name(name))
    .bind(normalize_name(split_name(name).0))
    .bind(extension_lower(name))
    .fetch_one(db.write())
    .await
    .unwrap()
}
