//! The retention sweeper: deletes what has been in the trash longer than the
//! `trash_retention_days` setting, as one `trash_empty` task at a time.
//!
//! It looks two minutes after the server started, then every hour, and at once when the setting
//! changes (so a shorter retention takes effect without waiting). The sweep is the same runner as
//! "Empty trash" with the selector "trashed before the cutoff": the cutoff is computed when the
//! sweep is decided and the items are selected when the task starts.

use std::time::Duration;

use apm_core::dto::{TaskKind, TaskState};
use apm_core::paths::TaskId;
use axum::http::StatusCode;
use serde_json::json;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::TrashService;
use super::empty::PurgeSelector;
use crate::error::AppError;

const DAY_SECS: i64 = 86_400;
/// "Is anything older than the cutoff?": one entry of the index of `trashed_at`.
pub(super) const DUE_SQL: &str = "SELECT 1 FROM trash_items WHERE trashed_at < ? LIMIT 1";
/// The first look after the server started: a boot is not slowed down by a clean-up.
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(120);
/// How often the sweeper looks afterwards.
const SWEEP_EVERY: Duration = Duration::from_secs(3_600);

impl TrashService {
    /// Queues the deletion of everything trashed more than `trash_retention_days` ago, when the
    /// setting is on, something is due and no other `trash_empty` task is queued or running.
    /// Returns the task, or `None` when there was nothing to do. A second call while the first
    /// task is active never starts a second one.
    pub async fn sweep_retention(&self) -> Result<Option<TaskId>, AppError> {
        let Some(days) = self.settings.get().trash_retention_days else {
            return Ok(None);
        };
        let _deciding = self.sweeping.lock().await;
        if self.purge_is_active().await? {
            return Ok(None);
        }
        let cutoff = self.now().saturating_sub(i64::from(days) * DAY_SECS);
        let due: Option<i64> = sqlx::query_scalar(DUE_SQL)
            .bind(cutoff)
            .fetch_optional(self.db.read())
            .await?;
        if due.is_none() {
            return Ok(None);
        }
        let unit = if days == 1 { "day" } else { "days" };
        let params = json!({
            "op": "trash_retention",
            "retention_days": days,
            "cutoff": cutoff,
        });
        let id = self
            .submit_purge(
                PurgeSelector::OlderThan { cutoff },
                format!("Removing trash older than {days} {unit}"),
                params,
            )
            .await?;
        tracing::info!(task = id, retention_days = days, "retention sweep queued");
        Ok(Some(id))
    }

    /// Whether a `trash_empty` task is queued or running.
    async fn purge_is_active(&self) -> Result<bool, AppError> {
        let tasks = self.tasks.list(0).await?;
        Ok(tasks.iter().any(|task| {
            task.kind == TaskKind::TrashEmpty
                && matches!(task.state, TaskState::Queued | TaskState::Running)
        }))
    }

    /// Starts the sweeper: first look after two minutes, then every hour, and at once when the
    /// retention setting changes. It ends when `shutdown` is cancelled.
    pub fn start_background(&self, shutdown: CancellationToken) {
        self.start_background_every(shutdown, FIRST_SWEEP_DELAY, SWEEP_EVERY);
    }

    /// [`TrashService::start_background`] with its own timing, for tests.
    pub fn start_background_every(
        &self,
        shutdown: CancellationToken,
        first_delay: Duration,
        every: Duration,
    ) {
        let service = self.clone();
        tokio::spawn(async move { service.sweep_loop(shutdown, first_delay, every).await });
    }

    async fn sweep_loop(self, shutdown: CancellationToken, first_delay: Duration, every: Duration) {
        let mut settings = self.settings.subscribe();
        let mut retention = settings.borrow().trash_retention_days;
        let mut deadline = Instant::now() + first_delay;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep_until(deadline) => {}
                changed = settings.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = settings.borrow_and_update().trash_retention_days;
                    if now == retention {
                        // Another setting changed: the schedule stays as it was.
                        continue;
                    }
                    retention = now;
                }
            }
            self.sweep_once().await;
            deadline = Instant::now() + every;
        }
    }

    async fn sweep_once(&self) {
        match self.sweep_retention().await {
            Ok(_) => {}
            Err(err) if err.status() == StatusCode::SERVICE_UNAVAILABLE => {
                tracing::debug!("retention sweep skipped: the server is stopping");
            }
            Err(err) => tracing::warn!(
                code = err.code(),
                "retention sweep failed: {}",
                err.message()
            ),
        }
    }
}
