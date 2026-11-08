//! Thumbnail stage of the indexer: pre-generates the `sm` thumbnail of every image and video whose
//! `thumb_state` is `pending`. Owned by todo `serving-media`.
//!
//! The indexer does not render: it asks a [`Thumbnailer`] (implemented by `media::MediaService`,
//! so on-demand requests and this stage share one cache, one single flight and one concurrency
//! gate). Work comes from `PendingQueue` (`PipelineColumn::Thumb`).
//!
//! * Audio, sidecars and other files have no picture: the stage marks them `not applicable`.
//! * A rendered or already cached thumbnail becomes `done`; a render that failed becomes `failed`.
//!   A file that is gone, changed, or on an offline volume stays `pending` for the next run.
//! * Every write is `UPDATE ... WHERE id = ? AND size = ? AND mtime = ?`.
//! * Concurrency is `max(1, thumbnail_concurrency / 2)` files here; the shared gate enforces the
//!   real limit. Without a thumbnailer (`Indexer::with_thumbnailer` not called) the stage is a
//!   no-op.
//! * At the end the stage asks the thumbnailer to sweep a slice of its cache.

use std::sync::Arc;

use apm_core::dto::FileKind;
use apm_core::paths::{FileId, RelPath, VolumeId};
use futures_util::future::BoxFuture;
use tokio::task::JoinSet;

use super::pending::{PendingFile, PendingKey, PendingQueue, PipelineColumn};
use super::{IndexError, IndexPhase, Indexer, PhaseProgress, RunContext, ScanRequest};
use crate::db::models::PipelineState;

const PAGE: usize = 500;
/// Cache entries one run's sweep looks at.
const SWEEP_LIMIT: usize = 5_000;

/// One file to give a thumbnail.
#[derive(Debug, Clone)]
pub struct ThumbTarget {
    pub id: FileId,
    pub volume_id: VolumeId,
    pub rel: RelPath,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbOutcome {
    Rendered,
    Cached,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThumbError {
    /// The file cannot be rendered: mark the row failed.
    #[error("{0}")]
    Failed(String),
    /// The file is offline, gone or changed: leave the row pending.
    #[error("{0}")]
    Unavailable(String),
}

/// Produces thumbnails for the stage.
pub trait Thumbnailer: Send + Sync {
    fn ensure(&self, target: ThumbTarget) -> BoxFuture<'_, Result<ThumbOutcome, ThumbError>>;

    /// Drops cache entries that no row refers to; looks at about `limit` entries.
    fn sweep_cache(&self, _limit: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// What the stage did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThumbSummary {
    pub thumbnails: u64,
    pub failed: u64,
}

struct Finished {
    file: PendingFile,
    state: Option<PipelineState>,
}

impl Indexer {
    /// Runs the thumbnail stage over `req`'s scope; see the module docs.
    pub async fn thumbnail_pending(
        &self,
        req: &ScanRequest,
        ctx: &RunContext,
    ) -> Result<ThumbSummary, IndexError> {
        let mut summary = ThumbSummary::default();
        let Some(thumbnailer) = self.thumbnailer().cloned() else {
            return Ok(summary);
        };
        let db = self.db();
        let queue = PendingQueue::new(PipelineColumn::Thumb, self.stage_scope(req));
        let totals = queue.totals(db).await?;
        let progress = PhaseProgress::new(ctx.progress.clone(), IndexPhase::Thumbs);
        progress.set_totals(Some(totals.files), Some(totals.bytes));
        let concurrency =
            (usize::from(self.settings().get().index.thumbnail_concurrency) / 2).max(1);

        let walked = async {
            let mut after: Option<PendingKey> = None;
            loop {
                let page = queue.page(db, after.as_ref(), PAGE).await?;
                let Some(last) = page.last() else { break };
                after = Some(PendingKey::from(last));
                let mut finished = Vec::with_capacity(page.len());
                let mut running: JoinSet<Finished> = JoinSet::new();
                for file in page {
                    if ctx.cancel.is_cancelled() {
                        break;
                    }
                    while running.len() >= concurrency {
                        reap(&mut running, &mut finished).await?;
                    }
                    let thumbnailer = Arc::clone(&thumbnailer);
                    running.spawn(async move {
                        let state = render(thumbnailer.as_ref(), &file).await;
                        Finished { file, state }
                    });
                }
                while !running.is_empty() {
                    reap(&mut running, &mut finished).await?;
                }
                let bytes = finished
                    .iter()
                    .map(|done| done.file.size.max(0) as u64)
                    .sum();
                let items = finished.len() as u64;
                self.write_thumb_states(&finished, &mut summary).await?;
                progress.advance(items, bytes, None);
                if ctx.cancel.is_cancelled() {
                    return Err(IndexError::Cancelled);
                }
            }
            Ok::<(), IndexError>(())
        }
        .await;
        progress.finish();
        walked?;
        thumbnailer.sweep_cache(SWEEP_LIMIT).await;
        Ok(summary)
    }

    async fn write_thumb_states(
        &self,
        finished: &[Finished],
        summary: &mut ThumbSummary,
    ) -> Result<(), IndexError> {
        if finished.iter().all(|done| done.state.is_none()) {
            return Ok(());
        }
        let mut tx = self.db().begin_write().await?;
        for done in finished {
            let Some(state) = done.state else { continue };
            let result = sqlx::query(
                "UPDATE files SET thumb_state = ? WHERE id = ? AND size = ? AND mtime = ?",
            )
            .bind(state)
            .bind(done.file.id)
            .bind(done.file.size)
            .bind(done.file.mtime)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() > 0 {
                match state {
                    PipelineState::Done => summary.thumbnails += 1,
                    PipelineState::Failed => summary.failed += 1,
                    _ => {}
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

async fn reap(
    running: &mut JoinSet<Finished>,
    finished: &mut Vec<Finished>,
) -> Result<(), IndexError> {
    if let Some(joined) = running.join_next().await {
        finished
            .push(joined.map_err(|err| IndexError::Worker(format!("thumbnail worker: {err}")))?);
    }
    Ok(())
}

/// The state to record for `file`, or `None` to leave it pending.
async fn render(thumbnailer: &dyn Thumbnailer, file: &PendingFile) -> Option<PipelineState> {
    if !matches!(file.kind, FileKind::Image | FileKind::Video) {
        return Some(PipelineState::NotApplicable);
    }
    let Ok(rel) = RelPath::new(&file.dir_path).and_then(|dir| dir.join(&file.name)) else {
        return None;
    };
    let target = ThumbTarget {
        id: file.id,
        volume_id: file.volume_id,
        rel,
        kind: file.kind,
        size: file.size,
        mtime: file.mtime,
    };
    match thumbnailer.ensure(target).await {
        Ok(_) => Some(PipelineState::Done),
        Err(ThumbError::Failed(_)) => Some(PipelineState::Failed),
        Err(ThumbError::Unavailable(_)) => None,
    }
}
