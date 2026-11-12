//! Row types for the core tables, [`FILE_COLUMNS`], and the codes the enum columns are stored as.
//!
//! The wire enums ([`FileKind`], [`CaptureSource`], [`FlagFilter`]) live in `opm_core::dto` and are
//! stored as lowercase strings (the flag as `-1`/`0`/`1`). They cannot implement the sqlx traits
//! here (orphan rule), so the `*_to_db` / `*_from_db` functions below are the one place that maps
//! them. The `*_to_db` matches have no wildcard arm: adding a variant breaks the build until the
//! mapping and the `CHECK` constraint of `0001_core.sql` are extended (a test covers the second).

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use opm_core::dto::{CaptureSource, FileKind, FlagFilter};
use opm_core::paths::{FileId, VolumeId};
use sqlx::error::BoxDynError;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};

/// A column holds a value this code does not know: a database written by a newer version, or by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidDbValue {
    pub column: &'static str,
    pub value: String,
}

impl fmt::Display for InvalidDbValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unexpected value {:?} for {}", self.value, self.column)
    }
}

impl Error for InvalidDbValue {}

fn invalid(column: &'static str, value: impl fmt::Display) -> InvalidDbValue {
    InvalidDbValue {
        column,
        value: value.to_string(),
    }
}

/// `files.kind`.
pub fn file_kind_to_db(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Image => "image",
        FileKind::Video => "video",
        FileKind::Audio => "audio",
        FileKind::Sidecar => "sidecar",
        FileKind::Other => "other",
    }
}

pub fn file_kind_from_db(value: &str) -> Result<FileKind, InvalidDbValue> {
    match value {
        "image" => Ok(FileKind::Image),
        "video" => Ok(FileKind::Video),
        "audio" => Ok(FileKind::Audio),
        "sidecar" => Ok(FileKind::Sidecar),
        "other" => Ok(FileKind::Other),
        other => Err(invalid("files.kind", other)),
    }
}

/// `files.capture_src`.
pub fn capture_source_to_db(source: CaptureSource) -> &'static str {
    match source {
        CaptureSource::Exif => "exif",
        CaptureSource::Xmp => "xmp",
        CaptureSource::Video => "video",
        CaptureSource::Fs => "fs",
    }
}

pub fn capture_source_from_db(value: &str) -> Result<CaptureSource, InvalidDbValue> {
    match value {
        "exif" => Ok(CaptureSource::Exif),
        "xmp" => Ok(CaptureSource::Xmp),
        "video" => Ok(CaptureSource::Video),
        "fs" => Ok(CaptureSource::Fs),
        other => Err(invalid("files.capture_src", other)),
    }
}

/// `files.flag`: `-1` rejected, `0` no flag, `1` selected.
pub fn flag_to_db(flag: Option<FlagFilter>) -> i64 {
    match flag {
        None => 0,
        Some(FlagFilter::Selected) => 1,
        Some(FlagFilter::Rejected) => -1,
    }
}

pub fn flag_from_db(value: i64) -> Result<Option<FlagFilter>, InvalidDbValue> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(FlagFilter::Selected)),
        -1 => Ok(Some(FlagFilter::Rejected)),
        other => Err(invalid("files.flag", other)),
    }
}

/// Where a file stands in one stage of the indexing pipeline (`files.meta_state`, `hash_state`,
/// `thumb_state`). Stored as its [`code`](Self::code); `PipelineState::Done as i64` and
/// `.bind(PipelineState::Done)` both work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, sqlx::Type)]
#[repr(i64)]
pub enum PipelineState {
    /// Not processed yet (the column default).
    #[default]
    Pending = 0,
    Done = 1,
    /// Tried and failed; retried only on request or when the file changes.
    Failed = 2,
    /// The stage does not apply to this file (no thumbnail for a text file, ...).
    NotApplicable = 3,
}

impl PipelineState {
    /// The value stored in the column.
    pub const fn code(self) -> i64 {
        self as i64
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::NotApplicable => "not_applicable",
        }
    }
}

impl fmt::Display for PipelineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PipelineState {
    type Err = InvalidDbValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "not_applicable" => Ok(Self::NotApplicable),
            other => Err(invalid("pipeline state", other)),
        }
    }
}

impl TryFrom<i64> for PipelineState {
    type Error = InvalidDbValue;

    fn try_from(code: i64) -> Result<Self, Self::Error> {
        match code {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Done),
            2 => Ok(Self::Failed),
            3 => Ok(Self::NotApplicable),
            other => Err(invalid("pipeline state", other)),
        }
    }
}

/// A row of `volumes`.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Volume {
    pub id: VolumeId,
    /// Directory name under `OPM_VOLUMES_DIR`.
    pub name: String,
    /// Absolute, canonical path of the volume root inside the container.
    pub root: String,
    pub writable: bool,
    pub online: bool,
    pub scan_gen: i64,
    pub last_scan_at: Option<i64>,
    pub created_at: i64,
}

impl Volume {
    /// The root directory as a path.
    pub fn root_path(&self) -> &Path {
        Path::new(&self.root)
    }
}

/// A row of `dirs`.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Dir {
    pub id: i64,
    pub volume_id: VolumeId,
    /// Volume-relative, `""` for the volume root.
    pub path: String,
    /// `None` only for the volume root.
    pub parent_path: Option<String>,
    pub name: String,
    pub mtime: Option<i64>,
    pub seen_gen: i64,
}

/// A row of `files`, every column. Select it with [`FILE_COLUMNS`] (or [`file_columns`]) and
/// `sqlx::query_as::<_, FileRow>`; decoding looks columns up by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub id: FileId,
    pub volume_id: VolumeId,
    pub dir_path: String,
    pub name: String,
    pub name_norm: String,
    pub stem_norm: String,
    pub ext: String,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: i64,
    pub btime: Option<i64>,
    /// Naive local wall-clock capture time, epoch seconds as if it were UTC.
    pub capture_ts: Option<i64>,
    pub capture_offset_min: Option<i32>,
    pub capture_src: Option<CaptureSource>,
    pub camera_id: Option<i64>,
    /// 0 = unrated, otherwise 1 to 5.
    pub rating: u8,
    pub flag: Option<FlagFilter>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub mime: Option<String>,
    /// SHA-1 of the content (20 bytes).
    pub hash: Option<Vec<u8>>,
    pub meta_state: PipelineState,
    pub hash_state: PipelineState,
    pub thumb_state: PipelineState,
    pub seen_gen: i64,
}

/// Every column of `files`, in table order, unqualified: `SELECT {FILE_COLUMNS} FROM files`. Use
/// [`file_columns`] when the table has an alias or is joined.
pub const FILE_COLUMNS: &str = "id, volume_id, dir_path, name, name_norm, stem_norm, ext, kind, \
     size, mtime, btime, capture_ts, capture_offset_min, capture_src, camera_id, rating, flag, \
     width, height, duration_ms, mime, hash, meta_state, hash_state, thumb_state, seen_gen";

/// [`FILE_COLUMNS`] with every column qualified by `alias` (`file_columns("f")` is
/// `f.id, f.volume_id, ...`). `alias` must be a literal, never user input.
pub fn file_columns(alias: &str) -> String {
    FILE_COLUMNS
        .split(',')
        .map(|column| format!("{alias}.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decode_error(column: &str, source: impl Into<BoxDynError>) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: column.to_owned(),
        source: source.into(),
    }
}

fn narrowed<T>(row: &SqliteRow, column: &str) -> Result<T, sqlx::Error>
where
    T: TryFrom<i64>,
    T::Error: Error + Send + Sync + 'static,
{
    let value: i64 = row.try_get(column)?;
    T::try_from(value).map_err(|err| decode_error(column, err))
}

fn narrowed_opt<T>(row: &SqliteRow, column: &str) -> Result<Option<T>, sqlx::Error>
where
    T: TryFrom<i64>,
    T::Error: Error + Send + Sync + 'static,
{
    row.try_get::<Option<i64>, _>(column)?
        .map(T::try_from)
        .transpose()
        .map_err(|err| decode_error(column, err))
}

impl<'r> FromRow<'r, SqliteRow> for FileRow {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let kind: String = row.try_get("kind")?;
        let capture_src: Option<String> = row.try_get("capture_src")?;
        Ok(Self {
            id: row.try_get("id")?,
            volume_id: row.try_get("volume_id")?,
            dir_path: row.try_get("dir_path")?,
            name: row.try_get("name")?,
            name_norm: row.try_get("name_norm")?,
            stem_norm: row.try_get("stem_norm")?,
            ext: row.try_get("ext")?,
            kind: file_kind_from_db(&kind).map_err(|err| decode_error("kind", err))?,
            size: row.try_get("size")?,
            mtime: row.try_get("mtime")?,
            btime: row.try_get("btime")?,
            capture_ts: row.try_get("capture_ts")?,
            capture_offset_min: narrowed_opt(row, "capture_offset_min")?,
            capture_src: capture_src
                .as_deref()
                .map(capture_source_from_db)
                .transpose()
                .map_err(|err| decode_error("capture_src", err))?,
            camera_id: row.try_get("camera_id")?,
            rating: narrowed(row, "rating")?,
            flag: flag_from_db(row.try_get("flag")?).map_err(|err| decode_error("flag", err))?,
            width: narrowed_opt(row, "width")?,
            height: narrowed_opt(row, "height")?,
            duration_ms: row.try_get("duration_ms")?,
            mime: row.try_get("mime")?,
            hash: row.try_get("hash")?,
            meta_state: row.try_get("meta_state")?,
            hash_state: row.try_get("hash_state")?,
            thumb_state: row.try_get("thumb_state")?,
            seen_gen: row.try_get("seen_gen")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::db::Db;
    use crate::db::testing::{insert_file, insert_volume};

    const FILE_KINDS: [FileKind; 5] = [
        FileKind::Image,
        FileKind::Video,
        FileKind::Audio,
        FileKind::Sidecar,
        FileKind::Other,
    ];
    const CAPTURE_SOURCES: [CaptureSource; 4] = [
        CaptureSource::Exif,
        CaptureSource::Xmp,
        CaptureSource::Video,
        CaptureSource::Fs,
    ];
    const PIPELINE_STATES: [PipelineState; 4] = [
        PipelineState::Pending,
        PipelineState::Done,
        PipelineState::Failed,
        PipelineState::NotApplicable,
    ];

    #[test]
    fn enum_mappings_round_trip_and_match_the_wire_names() {
        for kind in FILE_KINDS {
            let stored = file_kind_to_db(kind);
            assert_eq!(file_kind_from_db(stored), Ok(kind));
            let wire = serde_json::to_value(kind).unwrap();
            assert_eq!(wire, stored, "the column and the JSON use the same name");
        }
        for source in CAPTURE_SOURCES {
            let stored = capture_source_to_db(source);
            assert_eq!(capture_source_from_db(stored), Ok(source));
            assert_eq!(serde_json::to_value(source).unwrap(), stored);
        }
        for flag in [None, Some(FlagFilter::Selected), Some(FlagFilter::Rejected)] {
            assert_eq!(flag_from_db(flag_to_db(flag)), Ok(flag));
        }
        assert_eq!(flag_to_db(Some(FlagFilter::Rejected)), -1);
        assert_eq!(flag_to_db(Some(FlagFilter::Selected)), 1);
        assert_eq!(flag_to_db(None), 0);
    }

    #[test]
    fn unknown_values_are_rejected() {
        let err = file_kind_from_db("document").unwrap_err();
        assert_eq!(err.column, "files.kind");
        assert!(err.to_string().contains("document"));
        assert!(file_kind_from_db("Image").is_err(), "case matters");
        assert!(capture_source_from_db("gps").is_err());
        assert!(flag_from_db(2).is_err());
        assert!(PipelineState::try_from(4).is_err());
        assert!("later".parse::<PipelineState>().is_err());
    }

    #[test]
    fn pipeline_state_codes_are_the_documented_ones() {
        assert_eq!(PipelineState::default(), PipelineState::Pending);
        assert_eq!(PipelineState::Pending as i64, 0);
        assert_eq!(PipelineState::Done as i64, 1);
        assert_eq!(PipelineState::Failed as i64, 2);
        assert_eq!(PipelineState::NotApplicable as i64, 3);
        for state in PIPELINE_STATES {
            assert_eq!(PipelineState::try_from(state.code()), Ok(state));
            assert_eq!(state.as_str().parse::<PipelineState>(), Ok(state));
            assert_eq!(state.to_string(), state.as_str());
        }
    }

    #[test]
    fn file_columns_qualifies_every_column() {
        let qualified = file_columns("f");
        let plain: Vec<&str> = FILE_COLUMNS.split(", ").collect();
        let aliased: Vec<&str> = qualified.split(", ").collect();
        assert_eq!(plain.len(), 26);
        assert_eq!(aliased.len(), plain.len());
        for (plain, aliased) in plain.iter().zip(&aliased) {
            assert_eq!(*aliased, format!("f.{plain}"));
        }
    }

    /// Fails when a migration adds, renames or drops a `files` column without updating
    /// `FILE_COLUMNS` and `FileRow` (and the other way round).
    #[tokio::test]
    async fn file_columns_are_exactly_the_columns_of_the_files_table() {
        let db = Db::open_temp().await.unwrap();
        let table: BTreeSet<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('files')")
                .fetch_all(db.read())
                .await
                .unwrap()
                .into_iter()
                .collect();
        let listed: BTreeSet<String> = FILE_COLUMNS.split(", ").map(str::to_owned).collect();
        assert_eq!(
            listed, table,
            "db::models::FILE_COLUMNS and FileRow must list every column of `files`: add the new \
             column to the const, to the struct and to its FromRow impl"
        );
        assert_eq!(
            FILE_COLUMNS.split(", ").count(),
            listed.len(),
            "duplicate column"
        );
    }

    #[tokio::test]
    async fn file_row_decodes_every_column() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let id = insert_file(&db, volume, "2019/Rome", "IMG_0001.CR3").await;
        let hash = vec![7_u8; 20];
        sqlx::query(
            "UPDATE files SET kind = 'image', size = 25000000, mtime = 1700000000, btime = 1600000000, \
             capture_ts = 1560000000, capture_offset_min = -420, capture_src = 'exif', rating = 4, \
             flag = -1, width = 6000, height = 4000, duration_ms = 1500, mime = 'image/x-canon-cr3', \
             hash = ?, meta_state = 1, hash_state = 2, thumb_state = 3, seen_gen = 5 WHERE id = ?",
        )
        .bind(&hash)
        .bind(id)
        .execute(db.write())
        .await
        .unwrap();

        let sql = format!("SELECT {} FROM files f WHERE f.id = ?", file_columns("f"));
        let row: FileRow = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(
            row,
            FileRow {
                id,
                volume_id: volume,
                dir_path: "2019/Rome".into(),
                name: "IMG_0001.CR3".into(),
                name_norm: "img_0001.cr3".into(),
                stem_norm: "img_0001".into(),
                ext: "cr3".into(),
                kind: FileKind::Image,
                size: 25_000_000,
                mtime: 1_700_000_000,
                btime: Some(1_600_000_000),
                capture_ts: Some(1_560_000_000),
                capture_offset_min: Some(-420),
                capture_src: Some(CaptureSource::Exif),
                camera_id: None,
                rating: 4,
                flag: Some(FlagFilter::Rejected),
                width: Some(6000),
                height: Some(4000),
                duration_ms: Some(1500),
                mime: Some("image/x-canon-cr3".into()),
                hash: Some(hash),
                meta_state: PipelineState::Done,
                hash_state: PipelineState::Failed,
                thumb_state: PipelineState::NotApplicable,
                seen_gen: 5,
            }
        );
    }

    #[tokio::test]
    async fn file_row_defaults_and_nulls_decode() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let id = insert_file(&db, volume, "", "notes").await;
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?");
        let row: FileRow = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(row.ext, "");
        assert_eq!(row.kind, FileKind::Other);
        assert_eq!(row.rating, 0);
        assert_eq!(row.flag, None);
        assert_eq!(row.capture_src, None);
        assert_eq!(row.hash, None);
        assert_eq!(
            (row.meta_state, row.hash_state, row.thumb_state),
            (
                PipelineState::Pending,
                PipelineState::Pending,
                PipelineState::Pending
            )
        );
    }

    #[tokio::test]
    async fn a_value_the_code_does_not_know_is_a_decode_error_not_a_panic() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let id = insert_file(&db, volume, "", "a.jpg").await;
        // A newer version (or a hand edit) could store what today's CHECK constraints refuse.
        let mut conn = db.write().acquire().await.unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("UPDATE files SET kind = 'hologram' WHERE id = ?")
            .bind(id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);

        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?");
        let err = sqlx::query_as::<_, FileRow>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, sqlx::Error::ColumnDecode { index, .. } if index == "kind"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn pipeline_state_binds_and_decodes_directly() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let id = insert_file(&db, volume, "", "a.jpg").await;
        sqlx::query("UPDATE files SET hash_state = ? WHERE id = ?")
            .bind(PipelineState::Failed)
            .bind(id)
            .execute(db.write())
            .await
            .unwrap();
        let state: PipelineState = sqlx::query_scalar("SELECT hash_state FROM files WHERE id = ?")
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(state, PipelineState::Failed);
    }

    #[tokio::test]
    async fn volume_and_dir_rows_decode_their_booleans_and_nullable_columns() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        crate::db::testing::insert_dir(&db, volume, "").await;
        crate::db::testing::insert_dir(&db, volume, "2019").await;

        let row: Volume = sqlx::query_as("SELECT * FROM volumes WHERE id = ?")
            .bind(volume)
            .fetch_one(db.read())
            .await
            .unwrap();
        assert!(row.writable && row.online);
        assert_eq!(row.last_scan_at, None);
        assert_eq!(row.root_path(), Path::new("/volumes/photos"));

        let dirs: Vec<Dir> = sqlx::query_as("SELECT * FROM dirs WHERE volume_id = ? ORDER BY path")
            .bind(volume)
            .fetch_all(db.read())
            .await
            .unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(
            (dirs[0].path.as_str(), dirs[0].parent_path.as_deref()),
            ("", None)
        );
        assert_eq!(
            (dirs[1].path.as_str(), dirs[1].parent_path.as_deref()),
            ("2019", Some(""))
        );
        assert_eq!(dirs[1].name, "2019");
    }
}
