//! Thumbnail and preview rendering: cache lookup, single flight, the concurrency gate, failure
//! memory and the cache sweep. The behaviour is described in the `media` module docs.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use opm_core::dto::FileKind;
use opm_core::paths::{FileId, RelPath, VolumeId};

use super::cache::{CacheKey, Variant};
use super::render::RenderRequest;
use super::service::{Gate, MediaService};
use crate::db::models::{FileRow, PipelineState};
use crate::error::AppError;
use crate::indexer::{ThumbError, ThumbOutcome, ThumbTarget, Thumbnailer};
use crate::security::FsError;

/// Most ids a sweep looks up in one statement (SQLite allows 32766 binds).
const SWEEP_ID_CHUNK: usize = 30_000;

/// A picture ready to send.
#[derive(Debug, Clone)]
pub struct Rendition {
    pub bytes: Bytes,
    /// The strong `ETag`; `None` for a picture rendered from a file that differs from its row
    /// (sent with `no-store`).
    pub etag: Option<String>,
    /// It came from the cache.
    pub cached: bool,
}

/// Why no picture could be served.
#[derive(Debug)]
pub enum MediaError {
    /// The volume is offline or unknown, the path is not acceptable, ...
    App(AppError),
    /// The row exists but the file is gone from the disk.
    Missing,
    /// The file differs from its row (only for callers that refuse stale pictures).
    Stale,
    /// This kind of file has no picture.
    Unsupported,
    /// The renderer could not produce a picture.
    Failed(String),
}

impl From<AppError> for MediaError {
    fn from(err: AppError) -> Self {
        Self::App(err)
    }
}

/// What a render attempt shares with the requests waiting for it.
#[derive(Debug, Clone)]
pub(super) struct RenderFailure(pub(super) String);

/// What a sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub examined: usize,
    pub removed: usize,
}

#[derive(Clone)]
struct Job {
    key: CacheKey,
    volume_id: VolumeId,
    rel: RelPath,
    kind: FileKind,
    ext: String,
    duration_ms: Option<i64>,
    /// Serve a fresh, uncached picture of a file that differs from its row.
    allow_stale: bool,
    /// Record the outcome in `files.thumb_state`.
    mark: bool,
}

impl MediaService {
    /// The cache key of `variant` for the current version of `row`.
    pub fn key(row: &FileRow, variant: Variant) -> CacheKey {
        CacheKey {
            variant,
            id: row.id,
            mtime: row.mtime,
            size: row.size,
        }
    }

    /// The picture of `row` for `variant`: from the cache, else rendered (once however many ask).
    pub async fn rendition(
        &self,
        row: &FileRow,
        variant: Variant,
    ) -> Result<Rendition, MediaError> {
        let rel = RelPath::new(&row.dir_path)
            .and_then(|dir| dir.join(&row.name))
            .map_err(|_| MediaError::Missing)?;
        self.produce(Job {
            key: Self::key(row, variant),
            volume_id: row.volume_id,
            rel,
            kind: row.kind,
            ext: row.ext.clone(),
            duration_ms: row.duration_ms,
            allow_stale: true,
            mark: true,
        })
        .await
    }

    async fn produce(&self, job: Job) -> Result<Rendition, MediaError> {
        let key = job.key;
        if let Some(bytes) = self.read_cache(key).await {
            return Ok(Rendition {
                bytes,
                etag: Some(key.etag()),
                cached: true,
            });
        }
        if !matches!(job.kind, FileKind::Image | FileKind::Video) {
            return Err(MediaError::Unsupported);
        }
        if let Some(message) = self.failure(&key) {
            return Err(MediaError::Failed(message));
        }

        let root = self.inner.volumes.get(job.volume_id)?;
        let rel = job.rel.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let file = root.open_read(&rel)?;
            let meta = file.metadata().map_err(|err| FsError::Io(err.kind()))?;
            let path = root.resolve_existing(&rel)?;
            Ok::<_, FsError>((path, meta.size(), meta.mtime()))
        })
        .await
        .map_err(|err| MediaError::App(AppError::internal(err)))?;
        let (path, size, mtime) = match opened {
            Ok(found) => found,
            Err(FsError::NotFound(_) | FsError::NotAFile(_)) => return Err(MediaError::Missing),
            Err(err) => return Err(MediaError::App(err.into())),
        };
        let current = i64::try_from(size).is_ok_and(|size| size == key.size) && mtime == key.mtime;
        if !current {
            if !job.allow_stale {
                return Err(MediaError::Stale);
            }
            let bytes = self.render_uncached(&job, path).await?;
            return Ok(Rendition {
                bytes,
                etag: None,
                cached: false,
            });
        }

        let flight = {
            let mut flights = self
                .inner
                .flights
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            Arc::clone(flights.entry(key).or_default())
        };
        let outcome = flight
            .get_or_init(|| self.render_cached(&job, path))
            .await
            .clone();
        {
            let mut flights = self
                .inner
                .flights
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if flights
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
            {
                flights.remove(&key);
            }
        }
        match outcome {
            Ok(bytes) => Ok(Rendition {
                bytes,
                etag: Some(key.etag()),
                cached: false,
            }),
            Err(RenderFailure(message)) => Err(MediaError::Failed(message)),
        }
    }

    fn request(job: &Job, path: PathBuf) -> RenderRequest {
        RenderRequest {
            path,
            kind: job.kind,
            ext: job.ext.clone(),
            max_edge: job.key.variant.max_edge(),
            quality: job.key.variant.quality(),
            duration_ms: job.duration_ms,
        }
    }

    fn limit(&self) -> usize {
        usize::from(self.inner.settings.get().index.thumbnail_concurrency)
    }

    async fn read_cache(&self, key: CacheKey) -> Option<Bytes> {
        let cache = Arc::clone(&self.inner.cache);
        tokio::task::spawn_blocking(move || cache.read(&key))
            .await
            .ok()
            .flatten()
            .map(Bytes::from)
    }

    fn failure(&self, key: &CacheKey) -> Option<String> {
        self.inner
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
    }

    /// Waits for a place at the gate, then renders in a task of its own (so the decoder never
    /// runs on a request's task and a panic stays contained), writes the cache and records the
    /// outcome. Dropping the caller while it waits for the gate starts nothing; dropping it
    /// during the render lets the render finish and fill the cache.
    async fn render_cached(&self, job: &Job, path: PathBuf) -> Result<Bytes, RenderFailure> {
        let permit = Gate::acquire(&self.inner.gate, || self.limit()).await;
        let job = job.clone();
        let service = self.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            service.render_and_store(&job, path).await
        });
        match task.await {
            Ok(outcome) => outcome,
            Err(err) => Err(RenderFailure(format!("the renderer panicked: {err}"))),
        }
    }

    async fn render_and_store(&self, job: &Job, path: PathBuf) -> Result<Bytes, RenderFailure> {
        let rendered = self.inner.renderer.render(Self::request(job, path)).await;
        let message = match rendered {
            Ok(bytes) if !bytes.is_empty() => {
                let bytes = Bytes::from(bytes);
                self.store(job.key, bytes.clone()).await;
                if job.mark && job.key.variant == Variant::ThumbSm {
                    self.mark(&job.key, PipelineState::Done).await;
                }
                return Ok(bytes);
            }
            Ok(_) => "the renderer produced no picture".to_owned(),
            Err(err) => err.to_string(),
        };
        self.remember_failure(job, &message).await;
        Err(RenderFailure(message))
    }

    async fn remember_failure(&self, job: &Job, message: &str) {
        tracing::warn!(
            file_id = job.key.id,
            variant = job.key.variant.dir(),
            error = %message,
            "could not render a picture"
        );
        self.inner
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.key, message.to_owned());
        if job.mark && job.key.variant.is_thumbnail() {
            self.mark(&job.key, PipelineState::Failed).await;
        }
    }

    /// Writes the cache entry; a cache that cannot be written is logged once and skipped.
    async fn store(&self, key: CacheKey, bytes: Bytes) {
        let cache = Arc::clone(&self.inner.cache);
        let written = tokio::task::spawn_blocking(move || cache.write(&key, &bytes)).await;
        let error = match written {
            Ok(Ok(())) => return,
            Ok(Err(err)) => err.to_string(),
            Err(err) => err.to_string(),
        };
        if !self.inner.cache_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(error = %error, "the media cache cannot be written; serving uncached");
        }
    }

    async fn mark(&self, key: &CacheKey, state: PipelineState) {
        let result = sqlx::query(
            "UPDATE files SET thumb_state = ? WHERE id = ? AND size = ? AND mtime = ? \
             AND thumb_state != ?",
        )
        .bind(state)
        .bind(key.id)
        .bind(key.size)
        .bind(key.mtime)
        .bind(state)
        .execute(self.inner.db.write())
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "could not record the thumbnail state");
        }
    }

    /// A render of a file that differs from its row: under the gate, never cached, not
    /// remembered.
    async fn render_uncached(&self, job: &Job, path: PathBuf) -> Result<Bytes, MediaError> {
        let permit = Gate::acquire(&self.inner.gate, || self.limit()).await;
        let renderer = Arc::clone(&self.inner.renderer);
        let request = Self::request(job, path);
        let task = tokio::spawn(async move {
            let _permit = permit;
            renderer.render(request).await
        });
        match task.await {
            Ok(Ok(bytes)) if !bytes.is_empty() => Ok(Bytes::from(bytes)),
            Ok(Ok(_)) => Err(MediaError::Failed(
                "the renderer produced no picture".to_owned(),
            )),
            Ok(Err(err)) => Err(MediaError::Failed(err.to_string())),
            Err(err) => Err(MediaError::Failed(format!("the renderer panicked: {err}"))),
        }
    }

    /// Removes the cache entries no row refers to any more (their file changed or is gone). Looks
    /// at whole shards of the cache, continuing where the last call stopped, until at least
    /// `limit` entries were seen, so the work of one call is bounded.
    pub async fn sweep_cache(&self, limit: usize) -> SweepReport {
        let cache = Arc::clone(&self.inner.cache);
        let start = self.inner.sweep_cursor.load(Ordering::Relaxed);
        let listed = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            let mut shards = 0;
            while shards < 256 && entries.len() < limit {
                entries.extend(cache.entries(start + shards, 1));
                shards += 1;
            }
            (entries, shards)
        })
        .await;
        let Ok((entries, shards)) = listed else {
            return SweepReport::default();
        };
        self.inner
            .sweep_cursor
            .store((start + shards) % 256, Ordering::Relaxed);

        let mut ids: Vec<FileId> = entries
            .iter()
            .filter_map(|(_, entry)| entry.parsed.map(|(id, _, _)| id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let mut current = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(SWEEP_ID_CHUNK) {
            let mut query =
                sqlx::QueryBuilder::new("SELECT id, mtime, size FROM files WHERE id IN (");
            let mut separated = query.separated(", ");
            for id in chunk {
                separated.push_bind(*id);
            }
            separated.push_unseparated(")");
            match query
                .build_query_as::<(FileId, i64, i64)>()
                .fetch_all(self.inner.db.read())
                .await
            {
                Ok(rows) => {
                    current.extend(
                        rows.into_iter()
                            .map(|(id, mtime, size)| (id, (mtime, size))),
                    );
                }
                Err(err) => {
                    tracing::warn!(error = %err, "the cache sweep could not read the index");
                    return SweepReport {
                        examined: entries.len(),
                        removed: 0,
                    };
                }
            }
        }

        let examined = entries.len();
        let removed = tokio::task::spawn_blocking(move || {
            entries
                .into_iter()
                .filter(|(_, entry)| {
                    entry
                        .parsed
                        .is_some_and(|(id, mtime, size)| current.get(&id) != Some(&(mtime, size)))
                })
                .filter(|(path, _)| std::fs::remove_file(path).is_ok())
                .count()
        })
        .await
        .unwrap_or(0);
        SweepReport { examined, removed }
    }
}

impl Thumbnailer for MediaService {
    fn ensure(&self, target: ThumbTarget) -> BoxFuture<'_, Result<ThumbOutcome, ThumbError>> {
        Box::pin(async move {
            let ext = target
                .rel
                .file_name()
                .and_then(|name| name.rsplit_once('.'))
                .map(|(_, ext)| ext.to_ascii_lowercase())
                .unwrap_or_default();
            let duration_ms = if target.kind == FileKind::Video {
                sqlx::query_scalar::<_, Option<i64>>("SELECT duration_ms FROM files WHERE id = ?")
                    .bind(target.id)
                    .fetch_optional(self.inner.db.read())
                    .await
                    .ok()
                    .flatten()
                    .flatten()
            } else {
                None
            };
            let job = Job {
                key: CacheKey {
                    variant: Variant::ThumbSm,
                    id: target.id,
                    mtime: target.mtime,
                    size: target.size,
                },
                volume_id: target.volume_id,
                rel: target.rel,
                kind: target.kind,
                ext,
                duration_ms,
                allow_stale: false,
                mark: false,
            };
            match self.produce(job).await {
                Ok(rendition) if rendition.cached => Ok(ThumbOutcome::Cached),
                Ok(_) => Ok(ThumbOutcome::Rendered),
                Err(MediaError::Failed(message)) => Err(ThumbError::Failed(message)),
                Err(MediaError::Unsupported) => Err(ThumbError::Failed(
                    "no picture for this kind of file".to_owned(),
                )),
                Err(MediaError::Missing) => {
                    Err(ThumbError::Unavailable("the file is gone".to_owned()))
                }
                Err(MediaError::Stale) => {
                    Err(ThumbError::Unavailable("the file changed".to_owned()))
                }
                Err(MediaError::App(err)) => Err(ThumbError::Unavailable(err.to_string())),
            }
        })
    }

    fn sweep_cache(&self, limit: usize) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let report = MediaService::sweep_cache(self, limit).await;
            if report.removed > 0 {
                tracing::debug!(removed = report.removed, "swept the media cache");
            }
        })
    }
}
