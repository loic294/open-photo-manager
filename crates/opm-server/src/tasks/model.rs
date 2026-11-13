//! The types a task runner sees ([`TaskSpec`], [`TaskCtx`], [`TaskError`], [`Lane`]) and the
//! mapping between the wire enums and the `tasks` table.
//!
//! [`TaskState`] and [`TaskKind`] live in `opm_core::dto`, so they cannot carry inherent methods
//! here: the `*_to_db` / `*_from_db` functions below are the one place that maps them to the
//! strings stored in the table. The success state is `Done` on the wire and `succeeded` in the
//! `CHECK` constraint of `0001_core.sql`. The `*_to_db` matches have no wildcard arm, so a new
//! variant breaks the build until the mapping (and the constraint) is extended.

use opm_core::dto::{TaskKind, TaskState};
use opm_core::paths::TaskId;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::progress::TaskProgress;
use crate::db::DbError;
use crate::db::models::InvalidDbValue;
use crate::error::AppError;

/// Error text of a task that was running (or queued) when the server stopped.
pub const INTERRUPTED_BY_RESTART: &str = "interrupted by restart";

/// A queue with its own concurrency limit. The tasks of one lane start in submission order (FIFO)
/// within the engine's shared three-task limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    /// File operations: moves, trash, rmdir, plan execution.
    Fs,
    /// Scanning, metadata extraction, hashing and thumbnails.
    Index,
    /// Immich synchronisation and clean-up.
    Immich,
}

impl Lane {
    /// Every lane, in the order the scheduler considers them.
    pub const ALL: [Self; 3] = [Self::Fs, Self::Index, Self::Immich];

    /// The lane a kind of task belongs to unless the submitter says otherwise.
    pub const fn for_kind(kind: TaskKind) -> Self {
        match kind {
            TaskKind::Index => Self::Index,
            TaskKind::FsMove
            | TaskKind::FsTrash
            | TaskKind::FsRmdir
            | TaskKind::PlanExecute
            | TaskKind::TrashEmpty => Self::Fs,
            TaskKind::ImmichSync | TaskKind::ImmichCleanup => Self::Immich,
        }
    }

    /// Any lane may use all three shared task slots.
    pub const fn default_concurrency(self) -> usize {
        match self {
            Self::Fs | Self::Index | Self::Immich => super::lanes::MAX_RUNNING,
        }
    }

    /// The value stored in `tasks.lane`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fs => "fs",
            Self::Index => "index",
            Self::Immich => "immich",
        }
    }
}

/// What to run: the identity of a task as the engine records it.
#[derive(Clone, Debug)]
pub struct TaskSpec {
    pub resumable: bool,
    pub kind: TaskKind,
    /// Human-readable summary shown in the task panel ("Move 120 files"). A runner can replace it
    /// while it runs with [`TaskProgress::set_title`] or [`TaskProgress::phase`].
    pub title: String,
    pub lane: Lane,
    /// Parameters kept in `tasks.params_json` so that a task can be understood after the fact.
    /// They are JSON and end up in the database: never put a secret in them (a `Redacted<T>` is
    /// not `Serialize`, which is what keeps tokens out of here).
    pub params: Value,
}

impl TaskSpec {
    /// A task of `kind` on its default lane ([`Lane::for_kind`]) without parameters.
    pub fn new(kind: TaskKind, title: impl Into<String>) -> Self {
        Self {
            resumable: false,
            kind,
            title: title.into(),
            lane: Lane::for_kind(kind),
            params: Value::Object(Map::new()),
        }
    }

    /// Records `params` with the task (see [`TaskSpec::params`]).
    #[must_use]
    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }

    /// The caller can reconstruct this runner from its persisted parameters at startup.
    #[must_use]
    pub fn resumable(mut self) -> Self {
        self.resumable = true;
        self
    }

    /// Runs the task on another lane than the default one for its kind.
    #[must_use]
    pub fn with_lane(mut self, lane: Lane) -> Self {
        self.lane = lane;
        self
    }
}

/// What a runner receives when its task starts.
#[derive(Clone, Debug)]
pub struct TaskCtx {
    pub id: TaskId,
    /// Cancelled when the user cancels the task and when the server shuts down. Cancellation is
    /// cooperative: check it between units of work, or `select!` on `cancel.cancelled()`, and
    /// return [`TaskError::Cancelled`].
    pub cancel: CancellationToken,
    /// Reports progress; cheap enough to call per file or per chunk, from any thread.
    pub progress: TaskProgress,
}

/// Why a runner did not finish its work.
#[derive(Debug, Error)]
pub enum TaskError {
    /// The runner noticed `cancel` and stopped.
    #[error("task cancelled")]
    Cancelled,
    /// The task failed; the message is shown to the user, so keep it free of secrets.
    #[error("{0}")]
    Failed(String),
}

impl TaskError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

impl From<AppError> for TaskError {
    /// The message of an `AppError` is client-safe by construction.
    fn from(err: AppError) -> Self {
        Self::Failed(err.message().to_owned())
    }
}

impl From<anyhow::Error> for TaskError {
    /// An `AppError` inside keeps its message; anything else is logged (with URLs scrubbed) and
    /// reported as a generic internal error, exactly as on the HTTP path.
    fn from(err: anyhow::Error) -> Self {
        AppError::from(err).into()
    }
}

impl From<DbError> for TaskError {
    fn from(err: DbError) -> Self {
        AppError::from(err).into()
    }
}

impl From<sqlx::Error> for TaskError {
    fn from(err: sqlx::Error) -> Self {
        AppError::from(err).into()
    }
}

impl From<std::io::Error> for TaskError {
    fn from(err: std::io::Error) -> Self {
        AppError::from(err).into()
    }
}

fn invalid(column: &'static str, value: &str) -> InvalidDbValue {
    InvalidDbValue {
        column,
        value: value.to_owned(),
    }
}

/// `tasks.state`.
pub(super) fn state_to_db(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Done => "succeeded",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
        TaskState::Interrupted => "interrupted",
    }
}

pub(super) fn state_from_db(value: &str) -> Result<TaskState, InvalidDbValue> {
    match value {
        "queued" => Ok(TaskState::Queued),
        "running" => Ok(TaskState::Running),
        "succeeded" => Ok(TaskState::Done),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        "interrupted" => Ok(TaskState::Interrupted),
        other => Err(invalid("tasks.state", other)),
    }
}

/// `tasks.kind`: the same snake_case string as on the wire.
pub(super) fn kind_to_db(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Index => "index",
        TaskKind::FsMove => "fs_move",
        TaskKind::FsTrash => "fs_trash",
        TaskKind::FsRmdir => "fs_rmdir",
        TaskKind::PlanExecute => "plan_execute",
        TaskKind::ImmichSync => "immich_sync",
        TaskKind::ImmichCleanup => "immich_cleanup",
        TaskKind::TrashEmpty => "trash_empty",
    }
}

pub(super) fn kind_from_db(value: &str) -> Result<TaskKind, InvalidDbValue> {
    match value {
        "index" => Ok(TaskKind::Index),
        "fs_move" => Ok(TaskKind::FsMove),
        "fs_trash" => Ok(TaskKind::FsTrash),
        "fs_rmdir" => Ok(TaskKind::FsRmdir),
        "plan_execute" => Ok(TaskKind::PlanExecute),
        "immich_sync" => Ok(TaskKind::ImmichSync),
        "immich_cleanup" => Ok(TaskKind::ImmichCleanup),
        "trash_empty" => Ok(TaskKind::TrashEmpty),
        other => Err(invalid("tasks.kind", other)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const STATES: [(TaskState, &str); 6] = [
        (TaskState::Queued, "queued"),
        (TaskState::Running, "running"),
        (TaskState::Done, "succeeded"),
        (TaskState::Failed, "failed"),
        (TaskState::Cancelled, "cancelled"),
        (TaskState::Interrupted, "interrupted"),
    ];

    const KINDS: [TaskKind; 8] = [
        TaskKind::Index,
        TaskKind::FsMove,
        TaskKind::FsTrash,
        TaskKind::FsRmdir,
        TaskKind::PlanExecute,
        TaskKind::ImmichSync,
        TaskKind::ImmichCleanup,
        TaskKind::TrashEmpty,
    ];

    #[test]
    fn state_mapping_is_a_round_trip_and_done_is_succeeded() {
        for (state, db) in STATES {
            assert_eq!(state_to_db(state), db);
            assert_eq!(state_from_db(db).unwrap(), state);
        }
        // The wire name differs from the column value for exactly one state.
        assert_eq!(state_to_db(TaskState::Done), "succeeded");
        assert_eq!(state_from_db("done").unwrap_err().column, "tasks.state");
        assert!(state_from_db("").is_err());
    }

    #[test]
    fn kind_mapping_is_a_round_trip_and_matches_the_wire_name() {
        for kind in KINDS {
            let db = kind_to_db(kind);
            assert_eq!(kind_from_db(db).unwrap(), kind);
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(db));
        }
        let err = kind_from_db("teleport").unwrap_err();
        assert_eq!((err.column, err.value.as_str()), ("tasks.kind", "teleport"));
    }

    #[test]
    fn lanes_are_stored_under_their_lowercase_name() {
        let names: Vec<&str> = Lane::ALL.iter().map(|lane| lane.as_str()).collect();
        assert_eq!(names, ["fs", "index", "immich"]);
    }

    #[test]
    fn every_kind_has_the_documented_default_lane() {
        let expected = [
            (TaskKind::Index, Lane::Index),
            (TaskKind::FsMove, Lane::Fs),
            (TaskKind::FsTrash, Lane::Fs),
            (TaskKind::FsRmdir, Lane::Fs),
            (TaskKind::PlanExecute, Lane::Fs),
            (TaskKind::TrashEmpty, Lane::Fs),
            (TaskKind::ImmichSync, Lane::Immich),
            (TaskKind::ImmichCleanup, Lane::Immich),
        ];
        assert_eq!(expected.len(), KINDS.len());
        for (kind, lane) in expected {
            assert_eq!(Lane::for_kind(kind), lane, "{kind:?}");
            assert_eq!(TaskSpec::new(kind, "t").lane, lane);
        }
    }

    #[test]
    fn default_concurrency_is_three_per_lane() {
        assert_eq!(Lane::Fs.default_concurrency(), 3);
        assert_eq!(Lane::Index.default_concurrency(), 3);
        assert_eq!(Lane::Immich.default_concurrency(), 3);
    }

    #[test]
    fn a_spec_starts_with_empty_object_params() {
        let spec = TaskSpec::new(TaskKind::FsMove, "Move 2 files");
        assert_eq!(spec.params, json!({}));
        let spec = spec
            .with_params(json!({ "files": 2 }))
            .with_lane(Lane::Index);
        assert_eq!(spec.params, json!({ "files": 2 }));
        assert_eq!(spec.lane, Lane::Index);
        assert_eq!(spec.title, "Move 2 files");
    }

    #[test]
    fn errors_convert_without_leaking_internals() {
        let failed = TaskError::from(AppError::conflict("clash", "name taken"));
        assert!(matches!(&failed, TaskError::Failed(message) if message == "name taken"));

        let internal = TaskError::from(anyhow::anyhow!("secret token=abc at /etc/key"));
        assert!(
            matches!(&internal, TaskError::Failed(message) if message == "internal server error")
        );

        let wrapped = TaskError::from(anyhow::Error::new(AppError::forbidden("denied", "no")));
        assert!(matches!(&wrapped, TaskError::Failed(message) if message == "no"));

        assert_eq!(TaskError::Cancelled.to_string(), "task cancelled");
        assert_eq!(TaskError::failed("boom").to_string(), "boom");
    }
}
