//! The index side of a file operation: what ONE write transaction changes in `files`, `dirs`,
//! `file_sidecars`, `trash_items` and `fs_journal` once the disk work is done.
//!
//! # Rules
//! * One transaction per operation (a group member is an operation of its own). It finalises the
//!   journal row, so "the disk changed" and "the index says so" are never visible apart after a
//!   successful call, and a failure (a hook, the database) rolls everything back and leaves the
//!   journal row `pending` for [`FsOps::reconcile`].
//! * The rows are looked up by LOCATION inside the transaction, never trusted from before the disk
//!   step: the scanner may have indexed the destination, or dropped the source row, in between.
//!   `pre` (a snapshot taken before the disk step) only supplies the data a deleted row had.
//! * A moved file keeps its row id (so ratings, tags, selections and Immich mappings follow); a
//!   copy and a restored file get a NEW row. When a scan already created the destination row
//!   while the file was in flight, the row that has the history wins: the source row is kept and
//!   moved, the scanner's row is deleted (its cascade only removes data the scanner just made).
//! * Content-derived data (hash, metadata) is carried over only when the file on disk still has
//!   the size and mtime the snapshot had; otherwise the row restarts the pipeline like a scanner
//!   rewrite would.
//! * Links to sidecars never leave a folder: a move deletes the links that would cross folders
//!   and relinks the destination folder, but only when something there can link.
//!
//! Nothing here touches the disk, and nothing here may call a method that takes the write
//! connection while the transaction is open (the database has ONE writer connection).

use std::collections::{BTreeMap, BTreeSet};

use opm_core::dto::FileKind;
use opm_core::hash::{self, Sha1Digest};
use opm_core::names::{extension_lower, normalize_name, stem_norm};
use opm_core::paths::{FileId, RelPath, TaskId, VolumeId};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, Transaction};

use super::context::{OpContext, TrashBatch};
use super::error::FsOpError;
use super::failpoints::Step;
use super::hooks::AppliedOp;
use super::journal::Journal;
use super::stat::FileStat;
use super::{FsOps, Loc, Rules};
use crate::db::models::{
    FILE_COLUMNS, FileRow, PipelineState, capture_source_to_db, file_kind_to_db, flag_to_db,
};
use crate::db::now_secs;
use crate::db::subtree::Subtree;
use crate::indexer::links;

const PENDING: i64 = PipelineState::Pending.code();
const DONE: i64 = PipelineState::Done.code();
const NOT_APPLICABLE: i64 = PipelineState::NotApplicable.code();

/// Ids are deleted in chunks of this size (SQLite's bind limit is far higher).
const ID_CHUNK: usize = 30_000;

pub(crate) fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// What the index remembers of a file, in a form that survives the row: the content data and the
/// NAMES of the tags and the camera (ids would dangle after a delete).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snap {
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub btime: Option<i64>,
    pub capture_ts: Option<i64>,
    pub capture_offset_min: Option<i32>,
    pub capture_src: Option<String>,
    pub camera: Option<String>,
    pub rating: u8,
    pub flag: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub mime: Option<String>,
    /// SHA-1, lowercase hex.
    pub hash: Option<String>,
    pub meta_state: i64,
    pub hash_state: i64,
    pub thumb_state: i64,
    pub tags: Vec<String>,
}

impl Snap {
    fn from_row(row: &FileRow, camera: Option<String>, tags: Vec<String>) -> Self {
        Self {
            kind: file_kind_to_db(row.kind).to_owned(),
            size: row.size,
            mtime: row.mtime,
            btime: row.btime,
            capture_ts: row.capture_ts,
            capture_offset_min: row.capture_offset_min,
            capture_src: row
                .capture_src
                .map(|source| capture_source_to_db(source).to_owned()),
            camera,
            rating: row.rating,
            flag: flag_to_db(row.flag),
            width: row.width,
            height: row.height,
            duration_ms: row.duration_ms,
            mime: row.mime.clone(),
            hash: row
                .hash
                .as_deref()
                .and_then(hash::from_slice)
                .map(|digest| hash::to_hex(&digest)),
            meta_state: row.meta_state.code(),
            hash_state: row.hash_state.code(),
            thumb_state: row.thumb_state.code(),
            tags,
        }
    }

    pub(crate) fn digest(&self) -> Option<Sha1Digest> {
        self.hash
            .as_deref()
            .and_then(|text| hash::parse_hex(text).ok())
    }

    /// Whether the file on disk is the one the snapshot describes: the same size and whole-second
    /// mtime, the test the scanner itself uses to keep a hash.
    pub(crate) fn content_matches(&self, stat: &FileStat) -> bool {
        self.size == to_i64(stat.size) && self.mtime == stat.mtime
    }
}

/// `trash_items.meta_json`: what a restore needs to bring the index back without re-extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub(crate) enum TrashMeta {
    File(Box<Snap>),
    Dir { files: u64, bytes: u64 },
}

impl TrashMeta {
    fn parse(json: Option<&str>) -> Option<Self> {
        serde_json::from_str(json?).ok()
    }

    /// The snapshot a trashed file's item carries (`None` for a folder or unreadable metadata).
    pub(crate) fn file_snapshot(json: Option<&str>) -> Option<Snap> {
        match Self::parse(json)? {
            Self::File(snap) => Some(*snap),
            Self::Dir { .. } => None,
        }
    }

    fn json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

/// Which kind of transfer an index change belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Move,
    Copy,
    Restore,
}

/// How links to sidecars in other folders are treated after a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crossing {
    /// A standalone operation: delete the links that now cross folders, and relink.
    Delete,
    /// A member of a group: leave all link work to the group's fix-up transaction.
    Keep,
}

/// The source of a cross-device move that was kept in the trash instead of being deleted.
#[derive(Debug, Clone)]
pub(crate) struct KeptSource {
    pub batch: TrashBatch,
    pub trash_path: RelPath,
}

/// Everything the index transaction of a move, copy or restore needs.
pub(crate) struct TransferApply<'a> {
    pub mode: Mode,
    pub journal_id: i64,
    pub task_id: Option<TaskId>,
    /// For a restore: the trash location.
    pub src: &'a Loc,
    pub dst: &'a Loc,
    /// The data the source row had before the disk step (a restore: the trash snapshot).
    pub pre: Option<&'a Snap>,
    /// The destination file as it is now on disk.
    pub dst_stat: &'a FileStat,
    /// The SHA-1 of the copied content (the copy path only): verified, so it is adopted.
    pub hash: Option<Sha1Digest>,
    /// Restore: the `trash_items` row that goes away.
    pub restore_item: Option<i64>,
    pub kept: Option<&'a KeptSource>,
    pub crossing: Crossing,
    /// `false` for a restored file the scanner would ignore (a junk name): no row at all.
    pub indexed: bool,
}

/// What the index transaction produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Indexed {
    /// The row at the destination (`None` only for an unindexed junk restore).
    pub file_id: Option<FileId>,
    /// The row at the source (a copy's origin; a move's own row).
    pub source_file_id: Option<FileId>,
}

/// Everything the index transaction of a trash operation needs.
pub(crate) struct TrashApply<'a> {
    pub journal_id: i64,
    pub task_id: Option<TaskId>,
    pub volume_id: VolumeId,
    pub orig: &'a RelPath,
    pub trash: &'a RelPath,
    pub batch: &'a TrashBatch,
    pub is_dir: bool,
    /// The size of a file; ignored for a folder (the index knows its total).
    pub size: u64,
    pub reason: Option<&'a str>,
    /// What the index said about the item just before it was renamed into the trash. A scan can
    /// remove the rows between the rename and the transaction; this is what the item's
    /// `meta_json` and size are made of then. `None` when nothing was captured (a recovery) or
    /// the index did not know the item.
    pub known: Option<&'a TrashMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Trashed {
    pub trash_item_id: i64,
    pub files_removed: u64,
    pub bytes: u64,
}

/// A `trash_items` row.
#[derive(Debug, Clone)]
pub(crate) struct TrashItem {
    pub id: i64,
    pub volume_id: VolumeId,
    pub orig_path: RelPath,
    pub trash_path: RelPath,
    pub is_dir: bool,
    pub size: u64,
    pub meta_json: Option<String>,
}

const TRASH_ITEM_COLUMNS: &str = "id, volume_id, orig_path, trash_path, is_dir, size, meta_json";

impl TrashItem {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, FsOpError> {
        let orig: String = row.try_get("orig_path")?;
        let trash: String = row.try_get("trash_path")?;
        let size: i64 = row.try_get("size")?;
        Ok(Self {
            id: row.try_get("id")?,
            volume_id: row.try_get("volume_id")?,
            orig_path: RelPath::new(&orig)
                .map_err(|_| FsOpError::internal("a trash item has an invalid original path"))?,
            trash_path: RelPath::new(&trash)
                .map_err(|_| FsOpError::internal("a trash item has an invalid trash path"))?,
            is_dir: row.try_get::<i64, _>("is_dir")? != 0,
            size: u64::try_from(size).unwrap_or(0),
            meta_json: row.try_get("meta_json")?,
        })
    }

    pub(crate) fn trash_loc(&self) -> Loc {
        Loc::new(self.volume_id, self.trash_path.clone())
    }

    pub(crate) fn orig_loc(&self) -> Loc {
        Loc::new(self.volume_id, self.orig_path.clone())
    }
}

/// Loads a `trash_items` row.
pub(crate) async fn load_trash_item(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<TrashItem>, FsOpError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {TRASH_ITEM_COLUMNS} FROM trash_items WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(TrashItem::from_row).transpose()
}

/// The `trash_items` row of a trash path, if any.
pub(crate) async fn trash_item_at(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    trash_path: &RelPath,
) -> Result<Option<TrashItem>, FsOpError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {TRASH_ITEM_COLUMNS} FROM trash_items WHERE volume_id = ? AND trash_path = ?"
    )))
    .bind(volume_id)
    .bind(trash_path.as_str())
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(TrashItem::from_row).transpose()
}

/// The `files` row at `loc`.
pub(crate) async fn file_at(
    conn: &mut SqliteConnection,
    loc: &Loc,
) -> Result<Option<FileRow>, sqlx::Error> {
    sqlx::query_as::<_, FileRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {FILE_COLUMNS} FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?"
    )))
    .bind(loc.volume_id)
    .bind(loc.dir().as_str().to_owned())
    .bind(loc.name().to_owned())
    .fetch_optional(conn)
    .await
}

/// Snapshots a row (with the camera and tag names).
pub(crate) async fn snapshot_of(
    conn: &mut SqliteConnection,
    row: &FileRow,
) -> Result<Snap, sqlx::Error> {
    let camera: Option<String> = match row.camera_id {
        Some(id) => {
            sqlx::query_scalar("SELECT name FROM cameras WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM file_tags ft JOIN tags t ON t.id = ft.tag_id \
         WHERE ft.file_id = ? ORDER BY t.name",
    )
    .bind(row.id)
    .fetch_all(conn)
    .await?;
    Ok(Snap::from_row(row, camera, tags))
}

/// The row at `loc` and its snapshot.
pub(crate) async fn load_snap(
    conn: &mut SqliteConnection,
    loc: &Loc,
) -> Result<Option<(FileId, Snap)>, sqlx::Error> {
    let Some(row) = file_at(&mut *conn, loc).await? else {
        return Ok(None);
    };
    let snap = snapshot_of(conn, &row).await?;
    Ok(Some((row.id, snap)))
}

/// The folders that need a `dirs` row to hold something in `dir`, shallowest first (the volume
/// root has no row).
pub(crate) fn dir_chain(dir: &RelPath) -> Vec<RelPath> {
    if dir.is_root() {
        return Vec::new();
    }
    let mut chain: Vec<RelPath> = dir
        .ancestors()
        .filter(|ancestor| !ancestor.is_root())
        .collect();
    chain.reverse();
    chain.push(dir.clone());
    chain
}

async fn scan_gen(conn: &mut SqliteConnection, volume_id: VolumeId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT scan_gen FROM volumes WHERE id = ?")
        .bind(volume_id)
        .fetch_one(conn)
        .await
}

/// Gives each folder a row stamped with the volume's current scan generation, so a scan that is
/// finishing does not take the new folder for a removed one.
async fn upsert_dirs(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    dirs: &[RelPath],
    generation: i64,
) -> Result<(), sqlx::Error> {
    for dir in dirs.iter().filter(|dir| !dir.is_root()) {
        let parent = dir.parent().unwrap_or_else(RelPath::root);
        sqlx::query(
            "INSERT INTO dirs (volume_id, path, parent_path, name, mtime, seen_gen) \
             VALUES (?, ?, ?, ?, NULL, ?) ON CONFLICT (volume_id, path) DO UPDATE SET \
             seen_gen = excluded.seen_gen WHERE dirs.seen_gen < excluded.seen_gen",
        )
        .bind(volume_id)
        .bind(dir.as_str())
        .bind(parent.as_str())
        .bind(dir.file_name().unwrap_or_default())
        .bind(generation)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn camera_id(conn: &mut SqliteConnection, name: &str) -> Result<i64, sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO cameras (name) VALUES (?)")
        .bind(name)
        .execute(&mut *conn)
        .await?;
    sqlx::query_scalar("SELECT id FROM cameras WHERE name = ?")
        .bind(name)
        .fetch_one(conn)
        .await
}

/// Writes the content-derived columns of `snap` and its tags onto the row `id`. The thumbnail
/// state is NOT carried: thumbnails are cached per file id, so a row with a new id needs its own.
async fn carry_metadata(
    conn: &mut SqliteConnection,
    id: FileId,
    snap: &Snap,
) -> Result<(), sqlx::Error> {
    let camera = match &snap.camera {
        Some(name) => Some(camera_id(&mut *conn, name).await?),
        None => None,
    };
    sqlx::query(
        "UPDATE files SET capture_ts = ?, capture_offset_min = ?, capture_src = ?, \
         camera_id = ?, width = ?, height = ?, duration_ms = ?, mime = ?, hash = ?, \
         meta_state = ?, hash_state = ? WHERE id = ?",
    )
    .bind(snap.capture_ts)
    .bind(snap.capture_offset_min)
    .bind(snap.capture_src.as_deref())
    .bind(camera)
    .bind(snap.width)
    .bind(snap.height)
    .bind(snap.duration_ms)
    .bind(snap.mime.as_deref())
    .bind(snap.digest().map(|digest| digest.to_vec()))
    .bind(snap.meta_state)
    .bind(snap.hash_state)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    for tag in &snap.tags {
        sqlx::query("INSERT OR IGNORE INTO tags (name) VALUES (?)")
            .bind(tag)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO file_tags (file_id, tag_id) \
             SELECT ?, id FROM tags WHERE name = ?",
        )
        .bind(id)
        .bind(tag)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// The user's own data: the rating and the flag.
async fn carry_user_data(
    conn: &mut SqliteConnection,
    id: FileId,
    snap: &Snap,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE files SET rating = ?, flag = ? WHERE id = ?")
        .bind(snap.rating)
        .bind(snap.flag)
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// The pipeline state a row starts in, as the scanner sets it.
fn initial_state(kind: FileKind) -> i64 {
    if kind == FileKind::Sidecar {
        NOT_APPLICABLE
    } else {
        PENDING
    }
}

/// Inserts a row the way the scanner does for a new file.
async fn insert_fresh(
    conn: &mut SqliteConnection,
    loc: &Loc,
    kind: FileKind,
    stat: &FileStat,
    generation: i64,
) -> Result<FileId, sqlx::Error> {
    let name = loc.name();
    let state = initial_state(kind);
    sqlx::query_scalar(
        "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
         mtime, btime, meta_state, hash_state, thumb_state, seen_gen) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(loc.volume_id)
    .bind(loc.dir().as_str().to_owned())
    .bind(name)
    .bind(normalize_name(name))
    .bind(stem_norm(name))
    .bind(extension_lower(name))
    .bind(file_kind_to_db(kind))
    .bind(to_i64(stat.size))
    .bind(stat.mtime)
    .bind(stat.btime)
    .bind(state)
    .bind(state)
    .bind(state)
    .bind(generation)
    .fetch_one(conn)
    .await
}

/// Moves the row to its new location and name; its id and everything else stay.
async fn relocate(
    conn: &mut SqliteConnection,
    id: FileId,
    dst: &Loc,
    kind: FileKind,
    generation: i64,
) -> Result<(), sqlx::Error> {
    let name = dst.name();
    sqlx::query(
        "UPDATE files SET volume_id = ?, dir_path = ?, name = ?, name_norm = ?, stem_norm = ?, \
         ext = ?, kind = ?, seen_gen = ? WHERE id = ?",
    )
    .bind(dst.volume_id)
    .bind(dst.dir().as_str().to_owned())
    .bind(name)
    .bind(normalize_name(name))
    .bind(stem_norm(name))
    .bind(extension_lower(name))
    .bind(file_kind_to_db(kind))
    .bind(generation)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Restarts the pipeline of a row whose file is not the one it was indexed from.
async fn reset_content(
    conn: &mut SqliteConnection,
    id: FileId,
    kind: FileKind,
    stat: &FileStat,
    keep_btime: Option<i64>,
) -> Result<(), sqlx::Error> {
    let state = initial_state(kind);
    sqlx::query(
        "UPDATE files SET size = ?, mtime = ?, btime = ?, hash = NULL, meta_state = ?, \
         hash_state = ?, thumb_state = ? WHERE id = ?",
    )
    .bind(to_i64(stat.size))
    .bind(stat.mtime)
    .bind(stat.btime.or(keep_btime))
    .bind(state)
    .bind(state)
    .bind(state)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}

async fn set_hash(
    conn: &mut SqliteConnection,
    id: FileId,
    digest: &Sha1Digest,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE files SET hash = ?, hash_state = ? WHERE id = ?")
        .bind(digest.to_vec())
        .bind(DONE)
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// A row nobody has processed since the scanner made it.
fn untouched(row: &FileRow) -> bool {
    row.meta_state == PipelineState::Pending && row.hash_state == PipelineState::Pending
}

/// Carries the snapshot onto a row that already exists at the destination (a scan found the file
/// while the operation was in flight).
async fn adopt_into(
    conn: &mut SqliteConnection,
    existing: &FileRow,
    snap: Option<&Snap>,
    kind: FileKind,
    stat: &FileStat,
    digest: Option<&Sha1Digest>,
) -> Result<(), sqlx::Error> {
    if let Some(snap) = snap {
        carry_user_data(&mut *conn, existing.id, snap).await?;
        if kind != FileKind::Sidecar && untouched(existing) && snap.content_matches(stat) {
            carry_metadata(&mut *conn, existing.id, snap).await?;
        }
    }
    if let Some(digest) = digest
        && kind != FileKind::Sidecar
    {
        set_hash(conn, existing.id, digest).await?;
    }
    Ok(())
}

/// Creates the row of a new file (a copy or a restored file) from a snapshot.
async fn insert_from(
    conn: &mut SqliteConnection,
    dst: &Loc,
    kind: FileKind,
    stat: &FileStat,
    snap: Option<&Snap>,
    digest: Option<&Sha1Digest>,
    generation: i64,
) -> Result<FileId, sqlx::Error> {
    let id = insert_fresh(&mut *conn, dst, kind, stat, generation).await?;
    if let Some(snap) = snap {
        carry_user_data(&mut *conn, id, snap).await?;
        if kind != FileKind::Sidecar && snap.content_matches(stat) {
            carry_metadata(&mut *conn, id, snap).await?;
        }
    }
    if let Some(digest) = digest
        && kind != FileKind::Sidecar
    {
        set_hash(conn, id, digest).await?;
    }
    Ok(id)
}

/// The rows of a copy or a restore: a new row, or the one a scan already made at the destination.
async fn index_new_file(
    conn: &mut SqliteConnection,
    t: &TransferApply<'_>,
    kind: FileKind,
    snap: Option<&Snap>,
    generation: i64,
) -> Result<FileId, sqlx::Error> {
    let digest = t.hash.as_ref();
    match file_at(&mut *conn, t.dst).await? {
        Some(existing) => {
            adopt_into(conn, &existing, snap, kind, t.dst_stat, digest).await?;
            Ok(existing.id)
        }
        None => insert_from(conn, t.dst, kind, t.dst_stat, snap, digest, generation).await,
    }
}

/// The row of a move: the source row goes to the destination, keeping its id.
async fn index_move(
    conn: &mut SqliteConnection,
    t: &TransferApply<'_>,
    kind: FileKind,
    source: &FileRow,
    generation: i64,
) -> Result<FileId, sqlx::Error> {
    if let Some(other) = file_at(&mut *conn, t.dst).await?
        && other.id != source.id
    {
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(other.id)
            .execute(&mut *conn)
            .await?;
    }
    relocate(&mut *conn, source.id, t.dst, kind, generation).await?;
    let changed = source.size != to_i64(t.dst_stat.size)
        || source.mtime != t.dst_stat.mtime
        || source.kind != kind;
    if changed {
        reset_content(&mut *conn, source.id, kind, t.dst_stat, source.btime).await?;
    }
    if let Some(digest) = &t.hash
        && kind != FileKind::Sidecar
    {
        set_hash(conn, source.id, digest).await?;
    }
    Ok(source.id)
}

/// The row part of a move, copy or restore.
async fn index_transfer(
    conn: &mut SqliteConnection,
    rules: &Rules,
    t: &TransferApply<'_>,
    generation: i64,
) -> Result<(Indexed, FileKind), sqlx::Error> {
    let kind = rules.matcher.file_kind(t.dst.name());
    if !t.indexed {
        let nothing = Indexed {
            file_id: None,
            source_file_id: None,
        };
        return Ok((nothing, kind));
    }
    let src_row = match t.mode {
        Mode::Restore => None,
        Mode::Move | Mode::Copy => file_at(&mut *conn, t.src).await?,
    };
    let file_id = match (t.mode, &src_row) {
        (Mode::Move, Some(source)) => index_move(&mut *conn, t, kind, source, generation).await?,
        (Mode::Copy, Some(source)) => {
            let snap = snapshot_of(&mut *conn, source).await?;
            index_new_file(&mut *conn, t, kind, Some(&snap), generation).await?
        }
        // A restore, or the source row is gone (a scan dropped it, or it was never indexed):
        // `pre` has what the row had.
        _ => index_new_file(&mut *conn, t, kind, t.pre, generation).await?,
    };
    let indexed = Indexed {
        file_id: Some(file_id),
        source_file_id: src_row.map(|row| row.id),
    };
    Ok((indexed, kind))
}

/// Deletes the links of `file_id` whose other end is not in `dir_path` of `volume_id`, and
/// requeues their owners. Returns whether the file had any link at all.
async fn drop_crossing_links(
    conn: &mut SqliteConnection,
    file_id: FileId,
    volume_id: VolumeId,
    dir_path: &str,
) -> Result<bool, sqlx::Error> {
    let stored: Vec<(FileId, FileId)> = sqlx::query_as(
        "SELECT file_id, sidecar_id FROM file_sidecars WHERE file_id = ? OR sidecar_id = ?",
    )
    .bind(file_id)
    .bind(file_id)
    .fetch_all(&mut *conn)
    .await?;
    if stored.is_empty() {
        return Ok(false);
    }
    let mut owners = Vec::new();
    for (owner, sidecar) in stored {
        let other = if owner == file_id { sidecar } else { owner };
        let together: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM files WHERE id = ? AND volume_id = ? AND dir_path = ?)",
        )
        .bind(other)
        .bind(volume_id)
        .bind(dir_path)
        .fetch_one(&mut *conn)
        .await?;
        if !together {
            sqlx::query("DELETE FROM file_sidecars WHERE file_id = ? AND sidecar_id = ?")
                .bind(owner)
                .bind(sidecar)
                .execute(&mut *conn)
                .await?;
            owners.push(owner);
        }
    }
    links::reextract(conn, &owners).await?;
    Ok(true)
}

/// Whether a sidecar that could link to a file named `name` sits in the folder.
async fn related_sidecar_in(
    conn: &mut SqliteConnection,
    dir: &Loc,
    name: &str,
    file_id: FileId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM files WHERE volume_id = ? AND dir_path = ? \
         AND kind = 'sidecar' AND stem_norm IN (?, ?) AND id <> ?)",
    )
    .bind(dir.volume_id)
    .bind(dir.dir().as_str().to_owned())
    .bind(normalize_name(name))
    .bind(stem_norm(name))
    .bind(file_id)
    .fetch_one(conn)
    .await
}

/// Keeps the sidecar links of the destination folder right after `file_id` arrived there.
/// `moved_row` is true when the row is the one that was at the source (it may carry links).
async fn maintain_links(
    conn: &mut SqliteConnection,
    rules: &Rules,
    t: &TransferApply<'_>,
    file_id: FileId,
    kind: FileKind,
    moved_row: bool,
) -> Result<(), sqlx::Error> {
    if t.crossing == Crossing::Keep {
        return Ok(());
    }
    let dir = t.dst.dir();
    let mut relink = kind == FileKind::Sidecar;
    if moved_row && drop_crossing_links(&mut *conn, file_id, t.dst.volume_id, dir.as_str()).await? {
        relink = true;
    }
    if !relink {
        relink = related_sidecar_in(&mut *conn, t.dst, t.dst.name(), file_id).await?;
    }
    if relink {
        links::relink_dir(conn, t.dst.volume_id, dir.as_str(), &rules.matcher).await?;
    }
    Ok(())
}

struct NewTrashItem<'a> {
    volume_id: VolumeId,
    batch: &'a str,
    orig: &'a RelPath,
    trash: &'a RelPath,
    is_dir: bool,
    size: u64,
    reason: Option<&'a str>,
    task_id: Option<TaskId>,
    meta: Option<&'a TrashMeta>,
}

/// Inserts the `trash_items` row; a row that is already there (a re-run after a failed
/// transaction) is reused, so the call is idempotent.
async fn insert_trash_item(
    conn: &mut SqliteConnection,
    item: &NewTrashItem<'_>,
) -> Result<i64, sqlx::Error> {
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, reason, \
         task_id, meta_json, trashed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (volume_id, trash_path) DO NOTHING RETURNING id",
    )
    .bind(item.volume_id)
    .bind(item.batch)
    .bind(item.orig.as_str())
    .bind(item.trash.as_str())
    .bind(i64::from(item.is_dir))
    .bind(to_i64(item.size))
    .bind(item.reason)
    .bind(item.task_id)
    .bind(item.meta.and_then(TrashMeta::json))
    .bind(now_secs())
    .fetch_optional(&mut *conn)
    .await?;
    match inserted {
        Some(id) => Ok(id),
        None => {
            sqlx::query_scalar("SELECT id FROM trash_items WHERE volume_id = ? AND trash_path = ?")
                .bind(item.volume_id)
                .bind(item.trash.as_str())
                .fetch_one(conn)
                .await
        }
    }
}

/// Deletes the `files` and `dirs` rows at and below a folder; returns how many files went.
async fn delete_subtree(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    dir: &RelPath,
) -> Result<u64, sqlx::Error> {
    let subtree = Subtree::new(dir);
    let mut files = QueryBuilder::<Sqlite>::new("DELETE FROM files WHERE volume_id = ");
    files.push_bind(volume_id).push(" AND ");
    subtree.push_predicate(&mut files, "dir_path");
    let removed = files.build().execute(&mut *conn).await?.rows_affected();
    let mut dirs = QueryBuilder::<Sqlite>::new("DELETE FROM dirs WHERE volume_id = ");
    dirs.push_bind(volume_id).push(" AND ");
    subtree.push_predicate(&mut dirs, "path");
    dirs.build().execute(&mut *conn).await?;
    Ok(removed)
}

/// What the index knows about a file or folder that is about to be trashed: a file's snapshot
/// (none when it has no row), a folder's totals. Read before the rename; see
/// [`TrashApply::known`].
pub(crate) async fn known_before_trash(
    conn: &mut SqliteConnection,
    loc: &Loc,
    is_dir: bool,
) -> Result<Option<TrashMeta>, sqlx::Error> {
    if is_dir {
        let (files, bytes) = subtree_totals(conn, loc.volume_id, &loc.rel).await?;
        return Ok(Some(TrashMeta::Dir { files, bytes }));
    }
    Ok(load_snap(conn, loc)
        .await?
        .map(|(_, snap)| TrashMeta::File(Box::new(snap))))
}

/// `(files, bytes)` of the indexed files at and below a folder.
async fn subtree_totals(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    dir: &RelPath,
) -> Result<(u64, u64), sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files WHERE volume_id = ",
    );
    qb.push_bind(volume_id).push(" AND ");
    Subtree::new(dir).push_predicate(&mut qb, "dir_path");
    let (count, bytes): (i64, i64) = qb.build_query_as().fetch_one(conn).await?;
    Ok((
        u64::try_from(count).unwrap_or(0),
        u64::try_from(bytes).unwrap_or(0),
    ))
}

async fn subtree_ids(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    dir: &RelPath,
) -> Result<Vec<FileId>, sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT id FROM files WHERE volume_id = ");
    qb.push_bind(volume_id).push(" AND ");
    Subtree::new(dir).push_predicate(&mut qb, "dir_path");
    qb.build_query_scalar().fetch_all(conn).await
}

/// Commits the transaction when the body succeeded, rolls it back otherwise.
pub(crate) async fn conclude<T>(
    tx: Transaction<'static, Sqlite>,
    result: Result<T, sqlx::Error>,
) -> Result<T, FsOpError> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(err) => {
            let _ = tx.rollback().await;
            Err(FsOpError::from(err))
        }
    }
}

/// The folders whose listing a transfer changed: the one the file left, the one it entered, and
/// every folder created on the way together with its parent.
pub(crate) fn touched_dirs(
    src: Option<&Loc>,
    dst: Option<&Loc>,
    created: &[RelPath],
) -> Vec<(VolumeId, RelPath)> {
    let mut touched = Vec::new();
    if let Some(src) = src {
        touched.push((src.volume_id, src.dir()));
    }
    if let Some(dst) = dst {
        touched.push((dst.volume_id, dst.dir()));
        for dir in created {
            touched.push((dst.volume_id, dir.clone()));
            touched.push((dst.volume_id, dir.parent().unwrap_or_else(RelPath::root)));
        }
    }
    touched
}

impl FsOps {
    /// Tells the rest of the system which folders changed, once the change is committed: the
    /// change sink (the live `fs_change` event) and the self-write memory (so a watcher ignores
    /// the events these changes caused).
    pub(crate) fn announce(&self, cx: &OpContext, touched: &[(VolumeId, RelPath)]) {
        let mut by_volume: BTreeMap<VolumeId, BTreeSet<RelPath>> = BTreeMap::new();
        for (volume_id, dir) in touched {
            by_volume.entry(*volume_id).or_default().insert(dir.clone());
        }
        for (volume_id, dirs) in by_volume {
            let dirs: Vec<RelPath> = dirs.into_iter().collect();
            self.note_dirs(volume_id, &dirs);
            cx.changes.dirs_changed(volume_id, &dirs);
        }
    }

    /// Commits the index change of a move, copy or restore.
    pub(crate) async fn apply_transfer(
        &self,
        rules: &Rules,
        t: &TransferApply<'_>,
    ) -> Result<Indexed, FsOpError> {
        self.check(Step::BeforeIndexTx)?;
        let mut tx = self.db.begin_write().await?;
        let result = self.transfer_tx(&mut tx, rules, t).await;
        conclude(tx, result).await
    }

    async fn transfer_tx(
        &self,
        conn: &mut SqliteConnection,
        rules: &Rules,
        t: &TransferApply<'_>,
    ) -> Result<Indexed, sqlx::Error> {
        crate::statistics::record::begin(conn, t.journal_id).await?;
        Journal::finish_tx(&mut *conn, t.journal_id).await?;
        let generation = scan_gen(&mut *conn, t.dst.volume_id).await?;
        upsert_dirs(
            &mut *conn,
            t.dst.volume_id,
            &dir_chain(&t.dst.dir()),
            generation,
        )
        .await?;
        let (indexed, kind) = index_transfer(&mut *conn, rules, t, generation).await?;
        if t.mode == Mode::Move
            && let Some(file) = indexed.file_id
        {
            crate::statistics::record::moved_identity(conn, t.journal_id, file).await?;
        }
        sqlx::query("UPDATE fs_journal SET result_file_id = ?, result_live = 1, result_mtime = ? WHERE id = ?")
            .bind(indexed.file_id).bind(t.dst_stat.mtime).bind(t.journal_id)
            .execute(&mut *conn).await?;
        if let Some(file_id) = indexed.file_id {
            let moved_row = t.mode == Mode::Move && indexed.source_file_id.is_some();
            maintain_links(&mut *conn, rules, t, file_id, kind, moved_row).await?;
        }
        if let Some(kept) = t.kept {
            let meta = t.pre.map(|snap| TrashMeta::File(Box::new(snap.clone())));
            insert_trash_item(
                &mut *conn,
                &NewTrashItem {
                    volume_id: t.src.volume_id,
                    batch: kept.batch.name(),
                    orig: &t.src.rel,
                    trash: &kept.trash_path,
                    is_dir: false,
                    size: t.dst_stat.size,
                    reason: Some("moved"),
                    task_id: t.task_id,
                    meta: meta.as_ref(),
                },
            )
            .await?;
        }
        if let Some(item) = t.restore_item {
            sqlx::query("DELETE FROM trash_items WHERE id = ?")
                .bind(item)
                .execute(&mut *conn)
                .await?;
        }
        let op = match (t.mode, indexed.file_id) {
            (Mode::Move, Some(file_id)) => Some(AppliedOp::Moved {
                file_id,
                from: t.src.clone(),
                to: t.dst.clone(),
            }),
            (Mode::Copy, Some(file_id)) => Some(AppliedOp::Copied {
                source_file_id: indexed.source_file_id,
                file_id,
                from: t.src.clone(),
                to: t.dst.clone(),
            }),
            (Mode::Restore, file_id) => Some(AppliedOp::Restored {
                file_id,
                trash_item_id: t.restore_item.unwrap_or(0),
                from: t.src.clone(),
                to: t.dst.clone(),
                is_dir: false,
            }),
            (Mode::Move | Mode::Copy, None) => None,
        };
        if let Some(op) = op {
            self.run_hooks(&mut *conn, &op).await?;
        }
        let metrics = match t.mode {
            Mode::Move => Vec::new(),
            Mode::Copy if indexed.file_id.is_some() => vec![("added", 1, to_i64(t.dst_stat.size))],
            Mode::Restore => vec![("restored", 1, to_i64(t.dst_stat.size))],
            Mode::Copy => Vec::new(),
        };
        crate::statistics::record::finish(conn, t.journal_id, t.dst.volume_id, &metrics).await?;
        Ok(indexed)
    }

    /// Commits the index change of a trashed file or folder.
    pub(crate) async fn apply_trash(&self, t: &TrashApply<'_>) -> Result<Trashed, FsOpError> {
        self.check(Step::BeforeIndexTx)?;
        let mut tx = self.db.begin_write().await?;
        let result = self.trash_tx(&mut tx, t).await;
        conclude(tx, result).await
    }

    async fn trash_tx(
        &self,
        conn: &mut SqliteConnection,
        t: &TrashApply<'_>,
    ) -> Result<Trashed, sqlx::Error> {
        crate::statistics::record::begin(conn, t.journal_id).await?;
        Journal::finish_tx(&mut *conn, t.journal_id).await?;
        let (ids, meta, size) = if t.is_dir {
            let (files, bytes) = subtree_totals(&mut *conn, t.volume_id, t.orig).await?;
            let ids = subtree_ids(&mut *conn, t.volume_id, t.orig).await?;
            // The rows a scan removed since the rename still counted when the folder was looked
            // at before it, so the larger figure is the better one.
            let (files, bytes) = match t.known {
                Some(TrashMeta::Dir {
                    files: known_files,
                    bytes: known_bytes,
                }) => (files.max(*known_files), bytes.max(*known_bytes)),
                _ => (files, bytes),
            };
            (ids, Some(TrashMeta::Dir { files, bytes }), bytes)
        } else {
            let here = Loc::new(t.volume_id, t.orig.clone());
            match file_at(&mut *conn, &here).await? {
                Some(row) => {
                    let snap = snapshot_of(&mut *conn, &row).await?;
                    (vec![row.id], Some(TrashMeta::File(Box::new(snap))), t.size)
                }
                // No row now: never indexed (a junk file, a file the scanner has not seen yet),
                // or a scan removed it after the rename. What the index knew before is all there
                // is to restore; the row is gone, so the hooks are not told about it.
                None => (
                    Vec::new(),
                    t.known
                        .filter(|meta| matches!(meta, TrashMeta::File(_)))
                        .cloned(),
                    t.size,
                ),
            }
        };
        let trash_item_id = insert_trash_item(
            &mut *conn,
            &NewTrashItem {
                volume_id: t.volume_id,
                batch: t.batch.name(),
                orig: t.orig,
                trash: t.trash,
                is_dir: t.is_dir,
                size,
                reason: t.reason,
                task_id: t.task_id,
                meta: meta.as_ref(),
            },
        )
        .await?;
        // Hooks see the rows while they still exist: the Immich outbox reads their hashes.
        self.run_hooks(
            &mut *conn,
            &AppliedOp::Trashed {
                file_ids: ids.clone(),
                from: Loc::new(t.volume_id, t.orig.clone()),
                trash_item_id,
                trash_path: t.trash.clone(),
                is_dir: t.is_dir,
            },
        )
        .await?;
        let files_removed = if t.is_dir {
            delete_subtree(&mut *conn, t.volume_id, t.orig).await?
        } else {
            // The owners of a trashed sidecar must read their metadata again without it.
            links::reextract_owners_of(&mut *conn, &ids).await?;
            for chunk in ids.chunks(ID_CHUNK) {
                let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM files WHERE id IN (");
                let mut list = qb.separated(", ");
                for id in chunk {
                    list.push_bind(*id);
                }
                qb.push(")");
                qb.build().execute(&mut *conn).await?;
            }
            u64::try_from(ids.len()).unwrap_or(0)
        };
        if t.is_dir {
            crate::statistics::record::trashed_folder(conn, t.journal_id, trash_item_id).await?;
        }
        let count = match &meta {
            Some(TrashMeta::Dir { files, .. }) => to_i64(*files),
            _ => 1,
        };
        let mut metrics = vec![
            ("trashed", count, to_i64(size)),
            ("removed", count, to_i64(size)),
        ];
        match t.reason {
            Some("duplicate") => metrics.push(("duplicates", count, to_i64(size))),
            Some("duplicate_sidecar") => metrics.push(("duplicate_sidecars", count, to_i64(size))),
            _ => {}
        }
        crate::statistics::record::finish(conn, t.journal_id, t.volume_id, &metrics).await?;
        Ok(Trashed {
            trash_item_id,
            files_removed,
            bytes: size,
        })
    }

    /// Commits the deletion of a trash item.
    pub(crate) async fn apply_purge(
        &self,
        journal_id: i64,
        item: &TrashItem,
        bytes: u64,
    ) -> Result<(), FsOpError> {
        self.check(Step::BeforeIndexTx)?;
        let mut tx = self.db.begin_write().await?;
        let result = self.purge_tx(&mut tx, journal_id, item, bytes).await;
        conclude(tx, result).await
    }

    async fn purge_tx(
        &self,
        conn: &mut SqliteConnection,
        journal_id: i64,
        item: &TrashItem,
        bytes: u64,
    ) -> Result<(), sqlx::Error> {
        crate::statistics::record::begin(conn, journal_id).await?;
        Journal::finish_tx(&mut *conn, journal_id).await?;
        sqlx::query("DELETE FROM trash_items WHERE id = ?")
            .bind(item.id)
            .execute(&mut *conn)
            .await?;
        self.run_hooks(
            &mut *conn,
            &AppliedOp::Purged {
                trash_item_id: item.id,
                volume_id: item.volume_id,
                trash_path: item.trash_path.clone(),
                is_dir: item.is_dir,
                bytes,
            },
        )
        .await?;
        let count = match TrashMeta::parse(item.meta_json.as_deref()) {
            Some(TrashMeta::Dir { files, .. }) => to_i64(files),
            _ if item.is_dir => 0,
            _ => 1,
        };
        crate::statistics::record::finish(
            conn,
            journal_id,
            item.volume_id,
            &[("purged", count, to_i64(bytes))],
        )
        .await
    }

    /// Commits the index change of a restored FOLDER: its trash item goes and the folders above it
    /// get their rows. The files below are indexed by the scan the caller runs next.
    pub(crate) async fn apply_restore_dir(
        &self,
        journal_id: i64,
        item: &TrashItem,
        dst: &Loc,
    ) -> Result<(), FsOpError> {
        self.check(Step::BeforeIndexTx)?;
        let mut tx = self.db.begin_write().await?;
        let result = self.restore_dir_tx(&mut tx, journal_id, item, dst).await;
        conclude(tx, result).await
    }

    async fn restore_dir_tx(
        &self,
        conn: &mut SqliteConnection,
        journal_id: i64,
        item: &TrashItem,
        dst: &Loc,
    ) -> Result<(), sqlx::Error> {
        crate::statistics::record::begin(conn, journal_id).await?;
        sqlx::query("UPDATE statistics_intents SET subtree=1,scan_generation=(SELECT scan_gen FROM volumes WHERE id=?) WHERE journal_id=?")
            .bind(dst.volume_id).bind(journal_id)
            .execute(&mut *conn)
            .await?;
        Journal::finish_tx(&mut *conn, journal_id).await?;
        let generation = scan_gen(&mut *conn, dst.volume_id).await?;
        upsert_dirs(&mut *conn, dst.volume_id, &dir_chain(&dst.rel), generation).await?;
        crate::statistics::record::restored_folder(
            conn,
            journal_id,
            item.id,
            &item.orig_path,
            &dst.rel,
        )
        .await?;
        sqlx::query("DELETE FROM trash_items WHERE id = ?")
            .bind(item.id)
            .execute(&mut *conn)
            .await?;
        self.run_hooks(
            &mut *conn,
            &AppliedOp::Restored {
                file_id: None,
                trash_item_id: item.id,
                from: item.trash_loc(),
                to: dst.clone(),
                is_dir: true,
            },
        )
        .await?;
        let (files, bytes) = match TrashMeta::parse(item.meta_json.as_deref()) {
            Some(TrashMeta::Dir { files, bytes }) => (to_i64(files), to_i64(bytes)),
            _ => (0, to_i64(item.size)),
        };
        crate::statistics::record::finish(
            conn,
            journal_id,
            dst.volume_id,
            &[("restored", files, bytes)],
        )
        .await
    }

    /// Commits the removal of an (emptied) folder: its `dirs` row goes.
    pub(crate) async fn apply_rmdir(
        &self,
        journal_id: i64,
        volume_id: VolumeId,
        dir: &RelPath,
    ) -> Result<(), FsOpError> {
        self.check(Step::BeforeIndexTx)?;
        let mut tx = self.db.begin_write().await?;
        let result = async {
            Journal::finish_tx(&mut tx, journal_id).await?;
            delete_subtree(&mut tx, volume_id, dir).await.map(|_| ())
        }
        .await;
        conclude(tx, result).await
    }

    /// Gives a folder `mkdir_all` created, and its parents, their rows.
    pub(crate) async fn apply_mkdir(
        &self,
        volume_id: VolumeId,
        dir: &RelPath,
    ) -> Result<(), FsOpError> {
        let mut tx = self.db.begin_write().await?;
        let result = async {
            let generation = scan_gen(&mut tx, volume_id).await?;
            upsert_dirs(&mut tx, volume_id, &dir_chain(dir), generation).await
        }
        .await;
        conclude(tx, result).await
    }

    /// The link work after the members of a group moved: the links that now cross folders go
    /// (their owners read the metadata again), and every destination folder is relinked.
    pub(crate) async fn apply_group_fixup(
        &self,
        rules: &Rules,
        moved: &[FileId],
        dest_dirs: &BTreeSet<(VolumeId, RelPath)>,
    ) -> Result<(), FsOpError> {
        let mut tx = self.db.begin_write().await?;
        let result = self.fixup_tx(&mut tx, rules, moved, dest_dirs).await;
        conclude(tx, result).await
    }

    async fn fixup_tx(
        &self,
        conn: &mut SqliteConnection,
        rules: &Rules,
        moved: &[FileId],
        dest_dirs: &BTreeSet<(VolumeId, RelPath)>,
    ) -> Result<(), sqlx::Error> {
        for &file_id in moved {
            let placed: Option<(VolumeId, String)> =
                sqlx::query_as("SELECT volume_id, dir_path FROM files WHERE id = ?")
                    .bind(file_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            if let Some((volume_id, dir_path)) = placed {
                drop_crossing_links(&mut *conn, file_id, volume_id, &dir_path).await?;
            }
        }
        for (volume_id, dir) in dest_dirs {
            links::relink_dir(&mut *conn, *volume_id, dir.as_str(), &rules.matcher).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn loc(volume_id: VolumeId, path: &str) -> Loc {
        Loc::new(volume_id, rel(path))
    }

    fn stat(size: u64, mtime: i64) -> FileStat {
        FileStat {
            size,
            mtime,
            mtime_nanos: 7,
            mode: 0o100_644,
            dev: 1,
            ino: 2,
            btime: None,
        }
    }

    fn snap() -> Snap {
        Snap {
            kind: "image".to_owned(),
            size: 1234,
            mtime: 1_700_000_000,
            btime: Some(1_699_999_999),
            capture_ts: Some(1_600_000_000),
            capture_offset_min: Some(120),
            capture_src: Some("exif".to_owned()),
            camera: Some("Canon EOS R5".to_owned()),
            rating: 4,
            flag: 1,
            width: Some(8192),
            height: Some(5464),
            duration_ms: None,
            mime: Some("image/jpeg".to_owned()),
            hash: Some(hash::to_hex(&hash::sha1_bytes(b"content"))),
            meta_state: 1,
            hash_state: 1,
            thumb_state: 1,
            tags: vec!["trip".to_owned(), "family/kids".to_owned()],
        }
    }

    #[test]
    fn a_folder_chain_starts_at_the_top_and_ends_at_the_folder() {
        assert!(dir_chain(&RelPath::root()).is_empty());
        assert_eq!(dir_chain(&rel("a")), vec![rel("a")]);
        assert_eq!(
            dir_chain(&rel("a/b/c")),
            vec![rel("a"), rel("a/b"), rel("a/b/c")]
        );
    }

    #[test]
    fn touched_folders_are_the_two_ends_and_what_was_created_on_the_way() {
        let from = loc(1, "in/x.jpg");
        let to = loc(2, "out/new/x.jpg");
        let touched: BTreeSet<_> = touched_dirs(Some(&from), Some(&to), &[rel("out/new")])
            .into_iter()
            .collect();
        let expected: BTreeSet<_> = [(1, rel("in")), (2, rel("out/new")), (2, rel("out"))]
            .into_iter()
            .collect();
        assert_eq!(touched, expected);
    }

    #[test]
    fn a_top_level_file_touches_the_volume_root() {
        let from = loc(3, "x.jpg");
        assert_eq!(
            touched_dirs(Some(&from), None, &[]),
            vec![(3, RelPath::root())]
        );
    }

    #[test]
    fn nothing_is_touched_without_a_source_or_a_destination() {
        assert!(touched_dirs(None, None, &[rel("created")]).is_empty());
    }

    #[test]
    fn sizes_saturate_instead_of_wrapping() {
        assert_eq!(to_i64(5), 5);
        assert_eq!(to_i64(u64::MAX), i64::MAX);
    }

    #[test]
    fn a_snapshot_matches_a_file_of_the_same_size_and_second() {
        let snap = snap();
        assert!(snap.content_matches(&stat(1234, 1_700_000_000)));
        assert!(!snap.content_matches(&stat(1235, 1_700_000_000)));
        assert!(!snap.content_matches(&stat(1234, 1_700_000_001)));
    }

    #[test]
    fn a_snapshot_reads_its_digest_back() {
        let mut snap = snap();
        assert_eq!(snap.digest(), Some(hash::sha1_bytes(b"content")));
        snap.hash = Some("not hex".to_owned());
        assert_eq!(snap.digest(), None);
        snap.hash = None;
        assert_eq!(snap.digest(), None);
    }

    #[test]
    fn a_file_snapshot_survives_its_trip_through_the_trash_item() {
        let json = TrashMeta::File(Box::new(snap())).json().unwrap();
        assert_eq!(TrashMeta::file_snapshot(Some(&json)), Some(snap()));
    }

    #[test]
    fn a_folder_item_carries_totals_and_no_file_snapshot() {
        let json = TrashMeta::Dir {
            files: 3,
            bytes: 42,
        }
        .json()
        .unwrap();
        assert_eq!(json, r#"{"item":"dir","files":3,"bytes":42}"#);
        assert_eq!(TrashMeta::file_snapshot(Some(&json)), None);
        assert_eq!(
            TrashMeta::parse(Some(&json)),
            Some(TrashMeta::Dir {
                files: 3,
                bytes: 42
            })
        );
    }

    #[test]
    fn metadata_that_cannot_be_read_is_no_snapshot() {
        for json in [None, Some(""), Some("not json"), Some("{}"), Some("[]")] {
            assert_eq!(TrashMeta::file_snapshot(json), None, "{json:?}");
        }
    }

    // A trash item can wait for years and be restored by a newer version: the stored form must
    // stay readable. Optional fields may be absent and unknown ones are ignored.
    #[test]
    fn a_stored_snapshot_from_an_older_or_newer_version_still_reads() {
        let stored = r#"{"item":"file","kind":"video","size":10,"mtime":20,"rating":3,"flag":0,
            "meta_state":2,"hash_state":2,"thumb_state":2,"tags":["a"],"added_later":true}"#;
        let snap = TrashMeta::file_snapshot(Some(stored)).unwrap();
        assert_eq!(
            (snap.kind.as_str(), snap.size, snap.mtime),
            ("video", 10, 20)
        );
        assert_eq!((snap.rating, snap.tags), (3, vec!["a".to_owned()]));
        assert_eq!((snap.btime, snap.camera, snap.hash), (None, None, None));
    }

    #[test]
    fn a_sidecar_has_no_pipeline_work_and_a_photo_has_some() {
        assert_eq!(initial_state(FileKind::Sidecar), NOT_APPLICABLE);
        for kind in [
            FileKind::Image,
            FileKind::Video,
            FileKind::Audio,
            FileKind::Other,
        ] {
            assert_eq!(initial_state(kind), PENDING, "{kind:?}");
        }
    }
}
