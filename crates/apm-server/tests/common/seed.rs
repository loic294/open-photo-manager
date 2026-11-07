//! Seeds a test database with volumes, directories, files, cameras, tags, sidecar links and
//! selections, straight through SQL: no scanner, no files on disk. Every integration test that
//! needs an index to read (the query engine, the browse API, the plan engine, ...) can share it.
//!
//! ```ignore
//! mod common;
//! use common::seed::Seed;
//!
//! let db = Db::open_temp().await.unwrap();
//! let seed = Seed::new(&db);
//! let volume = seed.volume("photos").await;
//! let id = seed
//!     .file(volume, "2024/05", "IMG_0001.CR3")
//!     .camera("Canon EOS R5")
//!     .rating(4)
//!     .tags(["holiday", "family"])
//!     .capture(1_714_000_000)
//!     .insert()
//!     .await;
//! ```
//!
//! A file's directory (and every ancestor of it) is added to `dirs` when the file is inserted, the
//! way the scanner records it; [`Seed::dir`] adds a directory that holds no file. Rows are written
//! through the writer pool, so wait for no extra commit before reading.
//!
//! Defaults of a [`SeedFile`]: size 1000, mtime 1_700_000_000, no birth time, no capture time (a
//! capture time without a source is `exif`), no camera, no tag, unrated, unflagged, no dimensions,
//! no mime type, no hash, and the kind the extension says (`xmp`, `aae` and `thm` are sidecars).

use apm_core::dto::{CaptureSource, FileKind, FlagFilter};
use apm_core::hash::Sha1Digest;
use apm_core::kind::classify_name;
use apm_core::names::{extension_lower, normalize_name, stem_norm};
use apm_core::paths::{FileId, SelectionId, VolumeId};
use apm_server::db::models::{capture_source_to_db, file_kind_to_db, flag_to_db};
use apm_server::db::{Db, now_secs};

/// Extensions the default classification treats as sidecars.
pub const SIDECAR_EXTENSIONS: &[&str] = &["xmp", "aae", "thm"];

/// The default modification time of a seeded file.
pub const DEFAULT_MTIME: i64 = 1_700_000_000;

/// Writes rows into one database.
#[derive(Clone, Copy)]
pub struct Seed<'a> {
    db: &'a Db,
}

impl<'a> Seed<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// An online, writable volume called `name` with its root directory row.
    pub async fn volume(&self, name: &str) -> VolumeId {
        let id: VolumeId = sqlx::query_scalar(
            "INSERT INTO volumes (name, root, writable, online, created_at) \
             VALUES (?, ?, 1, 1, ?) RETURNING id",
        )
        .bind(name)
        .bind(format!("/volumes/{name}"))
        .bind(now_secs())
        .fetch_one(self.db.write())
        .await
        .unwrap();
        self.dir(id, "").await;
        id
    }

    /// The directory `path` of a volume (`""` is the root) and every ancestor of it; a directory
    /// that already exists is left alone.
    pub async fn dir(&self, volume_id: VolumeId, path: &str) {
        self.insert_dir(volume_id, "", None, "").await;
        let mut current = String::new();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            let parent = current.clone();
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(component);
            self.insert_dir(volume_id, &current, Some(&parent), component)
                .await;
        }
    }

    async fn insert_dir(&self, volume_id: VolumeId, path: &str, parent: Option<&str>, name: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO dirs (volume_id, path, parent_path, name) VALUES (?, ?, ?, ?)",
        )
        .bind(volume_id)
        .bind(path)
        .bind(parent)
        .bind(name)
        .execute(self.db.write())
        .await
        .unwrap();
    }

    /// Starts a file named `name` in directory `dir` (`""` is the root) of a volume; call
    /// [`SeedFile::insert`] to write it.
    pub fn file(&self, volume_id: VolumeId, dir: &str, name: &str) -> SeedFile<'a> {
        SeedFile {
            db: self.db,
            volume_id,
            dir: dir.to_owned(),
            name: name.to_owned(),
            kind: None,
            size: 1000,
            mtime: DEFAULT_MTIME,
            btime: None,
            capture_ts: None,
            capture_src: None,
            camera: None,
            rating: 0,
            flag: None,
            width: None,
            height: None,
            duration_ms: None,
            mime: None,
            hash: None,
            tags: Vec::new(),
        }
    }

    /// Records that `sidecar_id` is a sidecar of `file_id`; `kind` is `exact` or `stem`.
    pub async fn sidecar(&self, file_id: FileId, sidecar_id: FileId, kind: &str) {
        sqlx::query("INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES (?, ?, ?)")
            .bind(file_id)
            .bind(sidecar_id)
            .bind(kind)
            .execute(self.db.write())
            .await
            .unwrap();
    }

    /// A server-side selection holding `file_ids`.
    pub async fn selection(&self, file_ids: &[FileId]) -> SelectionId {
        let now = now_secs();
        let id: SelectionId = sqlx::query_scalar(
            "INSERT INTO selections (created_at, expires_at, item_count) VALUES (?, ?, ?) \
             RETURNING id",
        )
        .bind(now)
        .bind(now + 3600)
        .bind(i64::try_from(file_ids.len()).unwrap())
        .fetch_one(self.db.write())
        .await
        .unwrap();
        for file_id in file_ids {
            sqlx::query("INSERT INTO selection_items (selection_id, file_id) VALUES (?, ?)")
                .bind(id)
                .bind(file_id)
                .execute(self.db.write())
                .await
                .unwrap();
        }
        id
    }

    /// The id of the camera called `name` (the case of the first spelling wins), created if needed.
    pub async fn camera(&self, name: &str) -> i64 {
        self.named_id("cameras", name).await
    }

    /// The id of the tag called `name` (the case of the first spelling wins), created if needed.
    pub async fn tag(&self, name: &str) -> i64 {
        self.named_id("tags", name).await
    }

    async fn named_id(&self, table: &'static str, name: &str) -> i64 {
        // `table` is one of the two literals above; the name is always bound.
        let insert = format!("INSERT OR IGNORE INTO {table} (name) VALUES (?)");
        sqlx::query(sqlx::AssertSqlSafe(insert))
            .bind(name)
            .execute(self.db.write())
            .await
            .unwrap();
        let select = format!("SELECT id FROM {table} WHERE name = ?");
        sqlx::query_scalar(sqlx::AssertSqlSafe(select))
            .bind(name)
            .fetch_one(self.db.write())
            .await
            .unwrap()
    }
}

/// A file about to be written; see [`Seed::file`].
pub struct SeedFile<'a> {
    db: &'a Db,
    volume_id: VolumeId,
    dir: String,
    name: String,
    kind: Option<FileKind>,
    size: i64,
    mtime: i64,
    btime: Option<i64>,
    capture_ts: Option<i64>,
    capture_src: Option<CaptureSource>,
    camera: Option<String>,
    rating: u8,
    flag: Option<FlagFilter>,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<i64>,
    mime: Option<String>,
    hash: Option<Sha1Digest>,
    tags: Vec<String>,
}

impl SeedFile<'_> {
    /// Overrides the kind the extension implies.
    pub fn kind(mut self, kind: FileKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn size(mut self, size: i64) -> Self {
        self.size = size;
        self
    }

    pub fn mtime(mut self, mtime: i64) -> Self {
        self.mtime = mtime;
        self
    }

    pub fn btime(mut self, btime: i64) -> Self {
        self.btime = Some(btime);
        self
    }

    /// The naive-local capture time (epoch seconds as if UTC).
    pub fn capture(mut self, capture_ts: i64) -> Self {
        self.capture_ts = Some(capture_ts);
        self
    }

    pub fn capture_src(mut self, source: CaptureSource) -> Self {
        self.capture_src = Some(source);
        self
    }

    pub fn camera(mut self, name: &str) -> Self {
        self.camera = Some(name.to_owned());
        self
    }

    /// 0 (unrated) to 5.
    pub fn rating(mut self, rating: u8) -> Self {
        self.rating = rating;
        self
    }

    pub fn flag(mut self, flag: FlagFilter) -> Self {
        self.flag = Some(flag);
        self
    }

    pub fn dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    pub fn duration_ms(mut self, duration_ms: i64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub fn mime(mut self, mime: &str) -> Self {
        self.mime = Some(mime.to_owned());
        self
    }

    pub fn hash(mut self, hash: Sha1Digest) -> Self {
        self.hash = Some(hash);
        self
    }

    /// Adds tags (created when they do not exist yet).
    pub fn tags<'t>(mut self, tags: impl IntoIterator<Item = &'t str>) -> Self {
        self.tags.extend(tags.into_iter().map(str::to_owned));
        self
    }

    /// Writes the file, its directories, its camera and its tags and returns its id.
    pub async fn insert(self) -> FileId {
        let seed = Seed::new(self.db);
        seed.dir(self.volume_id, &self.dir).await;
        let camera_id = match &self.camera {
            Some(name) => Some(seed.camera(name).await),
            None => None,
        };
        let kind = self
            .kind
            .unwrap_or_else(|| classify_name(&self.name, SIDECAR_EXTENSIONS));
        let capture_src = self
            .capture_src
            .or(self.capture_ts.map(|_| CaptureSource::Exif));

        let id: FileId = sqlx::query_scalar(
            "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
             mtime, btime, capture_ts, capture_src, camera_id, rating, flag, width, height, \
             duration_ms, mime, hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(self.volume_id)
        .bind(&self.dir)
        .bind(&self.name)
        .bind(normalize_name(&self.name))
        .bind(stem_norm(&self.name))
        .bind(extension_lower(&self.name))
        .bind(file_kind_to_db(kind))
        .bind(self.size)
        .bind(self.mtime)
        .bind(self.btime)
        .bind(self.capture_ts)
        .bind(capture_src.map(capture_source_to_db))
        .bind(camera_id)
        .bind(i64::from(self.rating))
        .bind(flag_to_db(self.flag))
        .bind(self.width.map(i64::from))
        .bind(self.height.map(i64::from))
        .bind(self.duration_ms)
        .bind(&self.mime)
        .bind(self.hash.map(|digest| digest.to_vec()))
        .fetch_one(self.db.write())
        .await
        .unwrap();

        for tag in &self.tags {
            let tag_id = seed.tag(tag).await;
            sqlx::query("INSERT OR IGNORE INTO file_tags (file_id, tag_id) VALUES (?, ?)")
                .bind(id)
                .bind(tag_id)
                .execute(self.db.write())
                .await
                .unwrap();
        }
        id
    }
}
