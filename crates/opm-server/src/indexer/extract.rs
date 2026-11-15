//! Metadata extraction (stage 2 of [`Indexer::run`](super::Indexer::run)): capture time, camera,
//! rating, flag, keywords, dimensions, duration and MIME type of the files with
//! `meta_state = pending`.
//!
//! Owned by todo `extracting-metadata`. The pure rules live in `opm_core::metadata` (camera names,
//! keyword flattening, rating and flag, the merge of embedded metadata with sidecars) and
//! `opm_core::datetime` (the capture-date priority list); `xmp.rs` reads sidecars; `exiftool.rs`
//! is the production [`MetadataExtractor`]. This file holds the seam ([`MetadataExtractor`],
//! [`FakeExtractor`]) and the stage ([`Indexer::extract_pending`]).
//!
//! # The stage
//! Pages of 500 files are read from the shared stage queue (`PendingQueue` on `meta_state`). Per
//! page: kinds `audio` and `other` never reach the extractor (their metadata is the file system's:
//! `capture_src = fs`, MIME from the name); images and videos are batched to the extractor; the
//! `.xmp` sidecars linked to the files (`file_sidecars`, exact before stem, then by `name_norm`)
//! are each parsed once per page. The embedded metadata and the sidecars are merged field by field
//! and the capture date is resolved with the indexer's time zone (the server's `TZ`, UTC when
//! unset or unknown) and the file's birth and modification times. A sidecar's
//! `exif:DateTimeOriginal` replaces the embedded one and then `capture_src` is `xmp`; its
//! `xmp:CreateDate` / `photoshop:DateCreated` rank just below the embedded `DateTimeOriginal`.
//!
//! One transaction per page writes the rows with `size = ? AND mtime = ?` guards (a file changed
//! since the scan keeps `pending` for the next run), replaces the file's keywords and creates
//! cameras and tags on demand. After the commit the stage reports the directories it changed. A
//! file the extractor could not read gets `meta_state = failed` and keeps its other columns; a
//! sidecar that cannot be parsed is ignored and its owner still completes. At the end of a
//! complete pass unused cameras and tags are deleted. Running the stage again does nothing.
//!
//! If the extractor is not usable (exiftool missing or too old) the stage logs once, leaves every
//! file pending and reports [`ExtractSummary::unavailable`] (surfaced as
//! `IndexSummary::extractor_unavailable`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use chrono_tz::Tz;
use futures_util::future::BoxFuture;
use opm_core::datetime::{CaptureInputs, parse_datetime, resolve_capture};
use opm_core::dto::{CaptureSource, FileKind};
use opm_core::metadata::{SourceMetadata, merge, normalize_camera, rating_and_flag};
use opm_core::paths::{RelPath, VolumeId};
use sqlx::{QueryBuilder, Row as _, Sqlite, SqliteConnection};

use super::pending::{PendingFile, PendingKey, PendingQueue, PipelineColumn};
use super::progress::{IndexPhase, PhaseProgress};
use super::xmp::read_xmp_from;
use super::{IndexError, Indexer, RunContext, ScanRequest};
use crate::db::models::{PipelineState, capture_source_to_db};
use crate::security::VolumeRoot;

mod exiftool;

pub use exiftool::{BATCH_FILES, ExiftoolExtractor, MIN_VERSION, TAGS, base_args};

const PAGE: usize = 500;

/// What to read from one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractRequest {
    pub id: i64,
    /// Absolute path, resolved through the volume root by the caller.
    pub path: PathBuf,
    pub kind: FileKind,
}

/// What the extractor read from one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embedded {
    pub meta: SourceMetadata,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub mime: Option<String>,
}

/// Why a file has no metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractFailure {
    /// The tool reported an error for the file, or gave no result.
    Unreadable(String),
    Timeout,
    Crashed(String),
}

pub type ExtractOutcome = Result<Embedded, ExtractFailure>;

/// Reads embedded metadata. Implemented by [`ExiftoolExtractor`] and [`FakeExtractor`].
pub trait MetadataExtractor: Send + Sync {
    /// Is the tool usable? The error says why not.
    fn probe(&self) -> BoxFuture<'_, Result<(), String>>;

    /// One outcome per request, in the same order.
    fn extract(&self, requests: Vec<ExtractRequest>) -> BoxFuture<'_, Vec<ExtractOutcome>>;
}

type Script = dyn Fn(&ExtractRequest) -> ExtractOutcome + Send + Sync;

/// An extractor for tests: answers with a function of the request and records what it was asked.
pub struct FakeExtractor {
    script: Box<Script>,
    unavailable: Option<String>,
    calls: Mutex<Vec<Vec<PathBuf>>>,
}

impl FakeExtractor {
    pub fn new(script: impl Fn(&ExtractRequest) -> ExtractOutcome + Send + Sync + 'static) -> Self {
        Self {
            script: Box::new(script),
            unavailable: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// An extractor whose [`probe`](MetadataExtractor::probe) fails with `reason`.
    pub fn unavailable(reason: &str) -> Self {
        let mut fake = Self::new(|_| Err(ExtractFailure::Crashed("unavailable".into())));
        fake.unavailable = Some(reason.to_owned());
        fake
    }

    /// The paths of every `extract` call so far.
    pub fn calls(&self) -> Vec<Vec<PathBuf>> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl MetadataExtractor for FakeExtractor {
    fn probe(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.unavailable.clone().map_or(Ok(()), Err) })
    }

    fn extract(&self, requests: Vec<ExtractRequest>) -> BoxFuture<'_, Vec<ExtractOutcome>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(requests.iter().map(|r| r.path.clone()).collect());
            requests
                .iter()
                .map(|request| (self.script)(request))
                .collect()
        })
    }
}

/// What the extract stage did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    pub extracted: u64,
    pub failed: u64,
    /// The extractor could not be used; nothing was changed.
    pub unavailable: bool,
}

/// The columns written for one file.
struct Row {
    id: i64,
    size: i64,
    mtime: i64,
    volume_id: VolumeId,
    dir_path: String,
    outcome: Option<Values>,
}

struct Values {
    has_description: bool,
    capture_ts: i64,
    capture_offset_min: Option<i32>,
    capture_src: CaptureSource,
    camera: Option<String>,
    rating: u8,
    flag: i8,
    width: Option<i64>,
    height: Option<i64>,
    duration_ms: Option<i64>,
    mime: Option<String>,
    keywords: Vec<String>,
}

/// The parsed `.xmp` sidecars of a page, and which belong to which file in precedence order.
#[derive(Default)]
struct Sidecars {
    owners: HashMap<i64, Vec<i64>>,
    parsed: HashMap<i64, SourceMetadata>,
}

impl Sidecars {
    fn of(&self, file_id: i64) -> Vec<&SourceMetadata> {
        self.owners
            .get(&file_id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.parsed.get(id))
            .collect()
    }
}

impl Indexer {
    /// Stage 2: see the module docs.
    pub(crate) async fn extract_pending(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
    ) -> Result<ExtractSummary, IndexError> {
        let mut summary = ExtractSummary::default();
        let db = self.db();
        let queue = PendingQueue::new(PipelineColumn::Meta, self.stage_scope(req));
        let totals = queue.totals(db).await?;
        if totals.files == 0 {
            self.delete_unused_names().await?;
            return Ok(summary);
        }
        if let Err(reason) = self.extractor().probe().await {
            tracing::warn!(%reason, "metadata extraction is unavailable; files stay pending");
            summary.unavailable = true;
            return Ok(summary);
        }
        let progress = PhaseProgress::new(Arc::clone(&ctx.progress), IndexPhase::Extract);
        progress.set_totals(Some(totals.files), Some(totals.bytes));

        let mut after: Option<PendingKey> = None;
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(IndexError::Cancelled);
            }
            let page = queue.page(db, after.as_ref(), PAGE).await?;
            let Some(last) = page.last() else { break };
            after = Some(PendingKey::from(last));
            let bytes: u64 = page
                .iter()
                .map(|file| u64::try_from(file.size).unwrap_or(0))
                .sum();
            let count = page.len() as u64;
            let (extracted, failed) = self.extract_page(&page, ctx).await?;
            summary.extracted += extracted;
            summary.failed += failed;
            progress.advance(count, bytes, None);
        }
        progress.finish();
        self.delete_unused_names().await?;
        Ok(summary)
    }

    async fn extract_page(
        &self,
        page: &[PendingFile],
        ctx: &RunContext,
    ) -> Result<(u64, u64), IndexError> {
        let mut by_volume: BTreeMap<VolumeId, Vec<&PendingFile>> = BTreeMap::new();
        for file in page {
            by_volume.entry(file.volume_id).or_default().push(file);
        }
        let tz = self.timezone();
        let mut rows = Vec::with_capacity(page.len());
        for (volume_id, files) in by_volume {
            // An offline volume leaves its files pending.
            let Ok(root) = self.volumes().get(volume_id) else {
                continue;
            };
            let sidecars = self.load_sidecars(&root, &files).await?;
            let mut requests = Vec::new();
            let mut slots: Vec<(&PendingFile, Option<usize>)> = Vec::new();
            for file in files {
                let path = RelPath::new(&file.dir_path)
                    .and_then(|dir| dir.join(&file.name))
                    .ok()
                    .and_then(|rel| root.resolve_existing(&rel).ok());
                let Some(path) = path else {
                    rows.push(row_of(file, None));
                    continue;
                };
                if matches!(file.kind, FileKind::Image | FileKind::Video) {
                    slots.push((file, Some(requests.len())));
                    requests.push(ExtractRequest {
                        id: file.id,
                        path,
                        kind: file.kind,
                    });
                } else {
                    slots.push((file, None));
                }
            }
            let mut outcomes: Vec<Option<ExtractOutcome>> = if requests.is_empty() {
                Vec::new()
            } else {
                self.extractor()
                    .extract(requests)
                    .await
                    .into_iter()
                    .map(Some)
                    .collect()
            };
            for (file, slot) in slots {
                let outcome = match slot {
                    Some(index) => outcomes
                        .get_mut(index)
                        .and_then(Option::take)
                        .unwrap_or_else(|| Err(ExtractFailure::Unreadable("no result".into()))),
                    None => Ok(Embedded::default()),
                };
                let values = match outcome {
                    Ok(embedded) => Some(values_of(file, &embedded, &sidecars.of(file.id), tz)),
                    Err(failure) => {
                        tracing::debug!(file = file.id, ?failure, "metadata extraction failed");
                        None
                    }
                };
                rows.push(row_of(file, values));
            }
        }
        self.write_rows(&rows, ctx).await
    }

    async fn write_rows(&self, rows: &[Row], ctx: &RunContext) -> Result<(u64, u64), IndexError> {
        if rows.is_empty() {
            return Ok((0, 0));
        }
        let mut tx = self.db().begin_write().await?;
        let mut cameras: HashMap<String, i64> = HashMap::new();
        let mut tags: HashMap<String, i64> = HashMap::new();
        let (mut extracted, mut failed) = (0, 0);
        let mut changed: BTreeMap<VolumeId, BTreeSet<&str>> = BTreeMap::new();
        for row in rows {
            let applied = match &row.outcome {
                Some(values) => {
                    let applied =
                        write_values(&mut tx, row, values, &mut cameras, &mut tags).await?;
                    extracted += u64::from(applied);
                    applied
                }
                None => {
                    let result = sqlx::query(
                        "UPDATE files SET meta_state = ? WHERE id = ? AND size = ? AND mtime = ?",
                    )
                    .bind(PipelineState::Failed.code())
                    .bind(row.id)
                    .bind(row.size)
                    .bind(row.mtime)
                    .execute(&mut *tx)
                    .await?;
                    let applied = result.rows_affected() == 1;
                    failed += u64::from(applied);
                    applied
                }
            };
            if applied {
                changed
                    .entry(row.volume_id)
                    .or_default()
                    .insert(row.dir_path.as_str());
            }
        }
        tx.commit().await?;
        for (volume_id, dirs) in changed {
            let dirs: Vec<RelPath> = dirs
                .into_iter()
                .filter_map(|dir| RelPath::new(dir).ok())
                .collect();
            ctx.changes.dirs_changed(volume_id, &dirs);
        }
        Ok((extracted, failed))
    }

    /// The `.xmp` sidecars of `files`, each parsed once; unreadable ones are left out.
    async fn load_sidecars(
        &self,
        root: &Arc<VolumeRoot>,
        files: &[&PendingFile],
    ) -> Result<Sidecars, IndexError> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT fs.file_id, s.id, s.dir_path, s.name FROM file_sidecars fs \
             JOIN files s ON s.id = fs.sidecar_id WHERE s.ext = 'xmp' AND fs.file_id IN (",
        );
        let mut ids = qb.separated(", ");
        for file in files {
            ids.push_bind(file.id);
        }
        qb.push(") ORDER BY fs.file_id, (fs.kind = 'stem'), s.name_norm");
        let found = qb
            .build()
            .try_map(|row: sqlx::sqlite::SqliteRow| {
                Ok((
                    row.try_get::<i64, _>(0)?,
                    row.try_get::<i64, _>(1)?,
                    row.try_get::<String, _>(2)?,
                    row.try_get::<String, _>(3)?,
                ))
            })
            .fetch_all(self.db().read())
            .await?;

        let mut sidecars = Sidecars::default();
        let mut to_read: BTreeMap<i64, RelPath> = BTreeMap::new();
        for (owner, sidecar, dir, name) in found {
            sidecars.owners.entry(owner).or_default().push(sidecar);
            if let Ok(rel) = RelPath::new(&dir).and_then(|dir| dir.join(&name)) {
                to_read.insert(sidecar, rel);
            }
        }
        if to_read.is_empty() {
            return Ok(sidecars);
        }
        let root = Arc::clone(root);
        sidecars.parsed = tokio::task::spawn_blocking(move || {
            to_read
                .into_iter()
                .filter_map(|(id, rel)| {
                    let file = root.open_read(&rel).ok()?;
                    match read_xmp_from(file) {
                        Ok(meta) => Some((id, meta)),
                        Err(err) => {
                            tracing::debug!(sidecar = id, %err, "ignoring an unusable XMP sidecar");
                            None
                        }
                    }
                })
                .collect::<HashMap<_, _>>()
        })
        .await
        .map_err(|err| IndexError::Worker(err.to_string()))?;
        Ok(sidecars)
    }

    async fn delete_unused_names(&self) -> Result<(), IndexError> {
        let mut tx = self.db().begin_write().await?;
        sqlx::query("DELETE FROM tags WHERE NOT EXISTS (SELECT 1 FROM file_tags t WHERE t.tag_id = tags.id)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM cameras WHERE NOT EXISTS (SELECT 1 FROM files f WHERE f.camera_id = cameras.id)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn row_of(file: &PendingFile, outcome: Option<Values>) -> Row {
    Row {
        id: file.id,
        size: file.size,
        mtime: file.mtime,
        volume_id: file.volume_id,
        dir_path: file.dir_path.clone(),
        outcome,
    }
}

/// Merges, resolves the capture date and normalises everything a row stores.
fn values_of(
    file: &PendingFile,
    embedded: &Embedded,
    sidecars: &[&SourceMetadata],
    tz: Tz,
) -> Values {
    let merged = merge(&embedded.meta, sidecars);
    let meta = &merged.meta;
    let inputs = CaptureInputs {
        sub_sec_date_time_original: meta.sub_sec_date_time_original.as_deref(),
        date_time_original: meta.date_time_original.as_deref(),
        offset_time_original: meta.offset_time_original.as_deref(),
        sidecar_date: meta.sidecar_date.as_deref(),
        create_date: meta.create_date.as_deref(),
        quicktime_creation_date: meta.quicktime_creation_date.as_deref(),
        quicktime_create_date: meta.quicktime_create_date.as_deref(),
        media_create_date: meta.media_create_date.as_deref(),
        birth_time: file.btime,
        modified_time: file.mtime,
    };
    let mut capture = resolve_capture(&inputs, &tz);
    if merged.original_from_sidecar
        && capture.src == CaptureSource::Exif
        && meta
            .date_time_original
            .as_deref()
            .is_some_and(|text| parse_datetime(text).is_ok())
    {
        capture.src = CaptureSource::Xmp;
    }
    let (rating, flag) = rating_and_flag(meta.rating, meta.pick);
    let mime = embedded.mime.clone().or_else(|| {
        mime_guess::from_path(&file.name)
            .first()
            .map(|mime| mime.essence_str().to_owned())
    });
    Values {
        has_description: meta
            .description
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty()),
        capture_ts: capture.naive_local,
        capture_offset_min: capture.offset_min,
        capture_src: capture.src,
        camera: normalize_camera(meta.make.as_deref(), meta.model.as_deref()),
        rating,
        flag,
        width: embedded.width,
        height: embedded.height,
        duration_ms: embedded.duration_ms,
        mime,
        keywords: meta.keywords.clone().unwrap_or_default(),
    }
}

async fn name_id(
    conn: &mut SqliteConnection,
    table: &'static str,
    cache: &mut HashMap<String, i64>,
    name: &str,
) -> Result<i64, sqlx::Error> {
    if let Some(id) = cache.get(name) {
        return Ok(*id);
    }
    let query = match table {
        "cameras" => sqlx::query_scalar(
            "INSERT INTO cameras (name) VALUES (?) ON CONFLICT (name) DO UPDATE SET name = name RETURNING id",
        ),
        _ => sqlx::query_scalar(
            "INSERT INTO tags (name) VALUES (?) ON CONFLICT (name) DO UPDATE SET name = name RETURNING id",
        ),
    };
    let id: i64 = query.bind(name).fetch_one(conn).await?;
    cache.insert(name.to_owned(), id);
    Ok(id)
}

/// Writes one file's metadata; false when the file changed since the scan (nothing written).
async fn write_values(
    conn: &mut SqliteConnection,
    row: &Row,
    values: &Values,
    cameras: &mut HashMap<String, i64>,
    tags: &mut HashMap<String, i64>,
) -> Result<bool, sqlx::Error> {
    let camera_id = match &values.camera {
        Some(name) => Some(name_id(conn, "cameras", cameras, name).await?),
        None => None,
    };
    let result = sqlx::query(
        "UPDATE files SET capture_ts = ?, capture_offset_min = ?, capture_src = ?, camera_id = ?, \
         rating = ?, flag = ?, width = ?, height = ?, duration_ms = ?, mime = ?, meta_state = ?, has_description = ? \
         WHERE id = ? AND size = ? AND mtime = ?",
    )
    .bind(values.capture_ts)
    .bind(values.capture_offset_min)
    .bind(capture_source_to_db(values.capture_src))
    .bind(camera_id)
    .bind(i64::from(values.rating))
    .bind(i64::from(values.flag))
    .bind(values.width)
    .bind(values.height)
    .bind(values.duration_ms)
    .bind(&values.mime)
    .bind(PipelineState::Done.code())
    .bind(values.has_description)
    .bind(row.id)
    .bind(row.size)
    .bind(row.mtime)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query("DELETE FROM file_tags WHERE file_id = ?")
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    for keyword in &values.keywords {
        let tag_id = name_id(conn, "tags", tags, keyword).await?;
        sqlx::query("INSERT OR IGNORE INTO file_tags (file_id, tag_id) VALUES (?, ?)")
            .bind(row.id)
            .bind(tag_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(true)
}
