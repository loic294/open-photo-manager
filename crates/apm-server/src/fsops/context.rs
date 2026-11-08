//! What a file operation needs from its caller: cancellation, byte progress, change reporting and
//! the trash batch the task's deletions share.

use std::fmt;
use std::sync::{Arc, OnceLock};

use apm_core::paths::{RelPath, TaskId, VolumeId};
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::error::FsOpError;
use crate::indexer::{ChangeSink, NoopChanges};
use crate::tasks::{TaskCtx, TaskEngine};

/// Adapts the task engine to a [`ChangeSink`]: the directories become one coalesced `fs_change`
/// event.
#[derive(Clone)]
pub struct TaskChangeSink {
    engine: TaskEngine,
}

impl TaskChangeSink {
    pub fn new(engine: TaskEngine) -> Self {
        Self { engine }
    }
}

impl ChangeSink for TaskChangeSink {
    fn dirs_changed(&self, volume_id: VolumeId, dirs: &[RelPath]) {
        self.engine
            .publish_fs_change(volume_id, dirs.iter().map(|dir| dir.as_str().to_owned()));
    }
}

/// The name of one trash batch: the directory `.apm-trash/<name>/` that holds everything one task
/// (or one ad-hoc call) trashed. Sortable by time; the suffix says who made it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrashBatch(String);

impl TrashBatch {
    /// `YYYYMMDD-HHMMSS-t<task id>`: every operation of a task shares it.
    pub fn new(task_id: TaskId) -> Self {
        Self(format!("{}-t{task_id}", stamp(crate::db::now_secs())))
    }

    /// `YYYYMMDD-HHMMSS-r<random>` for a caller that has no task.
    pub fn adhoc() -> Self {
        let random = rand::random::<u32>();
        Self(format!("{}-r{random:08x}", stamp(crate::db::now_secs())))
    }

    /// The batch of a copy-and-delete move that keeps its source in the trash. It is derived from
    /// the journal row (`created_at` and id), so recovery finds the same path again.
    pub(crate) fn for_journal(created_at: i64, journal_id: i64) -> Self {
        Self(format!("{}-x{journal_id}", stamp(created_at)))
    }

    /// Reads a batch name back from a trash path (`.apm-trash/<batch>/...`).
    pub(crate) fn from_trash_path(path: &RelPath) -> Option<Self> {
        let mut components = path.components();
        (components.next()? == apm_core::consts::TRASH_DIR)
            .then(|| components.next().map(|batch| Self(batch.to_owned())))
            .flatten()
    }

    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TrashBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn stamp(epoch_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .unwrap_or_default()
        .format("%Y%m%d-%H%M%S")
        .to_string()
}

/// Everything an operation takes besides its own arguments. Cheap to clone; the clones share the
/// trash batch, so all the deletions of one task land in one batch folder.
#[derive(Clone)]
pub struct OpContext {
    /// The task the operation belongs to, recorded in the journal and the trash rows.
    pub task_id: Option<TaskId>,
    /// Cancelling stops a copy at the next chunk (the part file is deleted, the source is
    /// untouched) and makes the next operation return `cancelled`.
    pub cancel: CancellationToken,
    /// Called with the number of bytes that were just handled: once after a rename (the file's
    /// size), per 1 MiB chunk of a copy. Skipped files report nothing; hashing done to decide a
    /// conflict is not reported.
    pub on_bytes: Arc<dyn Fn(u64) + Send + Sync>,
    /// Told, after each commit, which directories changed.
    pub changes: Arc<dyn ChangeSink>,
    batch: Arc<OnceLock<TrashBatch>>,
}

impl OpContext {
    pub fn new(
        task_id: Option<TaskId>,
        cancel: CancellationToken,
        on_bytes: Arc<dyn Fn(u64) + Send + Sync>,
        changes: Arc<dyn ChangeSink>,
    ) -> Self {
        Self {
            task_id,
            cancel,
            on_bytes,
            changes,
            batch: Arc::new(OnceLock::new()),
        }
    }

    /// The context of a runner of the task engine: its cancellation token, its byte progress
    /// and `fs_change` events through the engine.
    pub fn from_task(task: &TaskCtx, engine: &TaskEngine) -> Self {
        let progress = task.progress.clone();
        Self::new(
            Some(task.id),
            task.cancel.clone(),
            Arc::new(move |bytes| progress.add(0, i64::try_from(bytes).unwrap_or(i64::MAX))),
            Arc::new(TaskChangeSink::new(engine.clone())),
        )
    }

    /// No task, never cancelled, nothing reported: for tests, recovery and one-off calls.
    pub fn detached() -> Self {
        Self::new(
            None,
            CancellationToken::new(),
            Arc::new(|_| {}),
            Arc::new(NoopChanges),
        )
    }

    /// Replaces the sink the changed directories are reported to.
    #[must_use]
    pub fn with_changes(mut self, changes: Arc<dyn ChangeSink>) -> Self {
        self.changes = changes;
        self
    }

    /// Replaces the byte-progress callback.
    #[must_use]
    pub fn with_on_bytes(mut self, on_bytes: Arc<dyn Fn(u64) + Send + Sync>) -> Self {
        self.on_bytes = on_bytes;
        self
    }

    /// The trash batch of this context (and its clones), created on first use.
    pub fn batch(&self) -> &TrashBatch {
        self.batch.get_or_init(|| match self.task_id {
            Some(task_id) => TrashBatch::new(task_id),
            None => TrashBatch::adhoc(),
        })
    }

    pub(crate) fn report_bytes(&self, bytes: u64) {
        if bytes > 0 {
            (self.on_bytes)(bytes);
        }
    }

    /// `Err(Cancelled)` once the context was cancelled; operations call it between their steps.
    pub(crate) fn ensure_active(&self) -> Result<(), FsOpError> {
        if self.cancel.is_cancelled() {
            Err(FsOpError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for OpContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpContext")
            .field("task_id", &self.task_id)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_names_sort_by_time_and_name_their_origin() {
        let batch = TrashBatch::new(42);
        let name = batch.name();
        assert!(name.ends_with("-t42"), "{name}");
        assert_eq!(name.len(), "20240101-120000-t42".len());
        assert!(TrashBatch::adhoc().name().contains("-r"));
        assert_eq!(
            TrashBatch::for_journal(1_700_000_000, 7).name(),
            "20231114-221320-x7"
        );
    }

    #[test]
    fn a_batch_is_read_back_from_its_trash_path() {
        let path = RelPath::new(".apm-trash/20231114-221320-x7/a/b.jpg").unwrap();
        assert_eq!(
            TrashBatch::from_trash_path(&path).unwrap().name(),
            "20231114-221320-x7"
        );
        assert!(TrashBatch::from_trash_path(&RelPath::new("a/b").unwrap()).is_none());
        assert!(TrashBatch::from_trash_path(&RelPath::new(".apm-trash").unwrap()).is_none());
    }

    #[test]
    fn clones_share_one_batch() {
        let cx = OpContext::detached();
        let clone = cx.clone();
        assert_eq!(cx.batch(), clone.batch());
    }

    #[test]
    fn the_task_context_names_its_batch_after_the_task() {
        let cx = OpContext::new(
            Some(9),
            CancellationToken::new(),
            Arc::new(|_| {}),
            Arc::new(NoopChanges),
        );
        assert!(cx.batch().name().ends_with("-t9"));
    }
}
