//! Task DTOs: background task state and the server-sent event stream.
//!
//! Owned by todo `building-task-engine`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::paths::{TaskId, VolumeId};

/// What a background task does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TaskKind {
    Index,
    FsMove,
    FsTrash,
    FsRmdir,
    PlanExecute,
    ImmichSync,
    ImmichCleanup,
    TrashEmpty,
}

/// Lifecycle state of a task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TaskState {
    #[default]
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
    /// The server stopped while the task was running; it was not resumed.
    Interrupted,
}

/// A background task as shown in the task panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskDto {
    pub id: TaskId,
    pub kind: TaskKind,
    /// Human-readable summary, e.g. "Move 120 files".
    pub title: String,
    pub state: TaskState,
    pub items_done: i64,
    pub items_total: Option<i64>,
    pub bytes_done: i64,
    pub bytes_total: Option<i64>,
    /// Bytes per second, or items per second when `bytes_total` is `null`.
    pub speed: Option<f64>,
    /// Estimated seconds remaining.
    pub eta_secs: Option<f64>,
    pub error: Option<String>,
    /// Epoch seconds.
    pub started_at: Option<i64>,
    /// Epoch seconds.
    pub finished_at: Option<i64>,
}

/// Event pushed on `GET /api/events` (server-sent events). The JSON payload is
/// tagged by a `type` field. There are exactly two types: indexing is a task of
/// kind `index`, so its progress arrives as a `task` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerEvent {
    /// A task was created or its progress or state changed.
    Task { task: TaskDto },
    /// Directories whose contents changed; open panes should refresh them.
    FsChange {
        volume_id: VolumeId,
        dirs: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    fn sample_task() -> TaskDto {
        TaskDto {
            id: 5,
            kind: TaskKind::FsMove,
            title: "Move 120 files".to_owned(),
            state: TaskState::Running,
            items_done: 30,
            items_total: Some(120),
            bytes_done: 1_000_000,
            bytes_total: Some(4_000_000),
            speed: Some(250_000.5),
            eta_secs: Some(12.0),
            error: None,
            started_at: Some(1_718_000_000),
            finished_at: None,
        }
    }

    #[test]
    fn enums_use_snake_case() {
        assert_eq!(roundtrip(&TaskKind::ImmichCleanup), json!("immich_cleanup"));
        assert_eq!(roundtrip(&TaskKind::FsRmdir), json!("fs_rmdir"));
        assert_eq!(roundtrip(&TaskState::Interrupted), json!("interrupted"));
    }

    #[test]
    fn task_dto_roundtrips() {
        let wire = roundtrip(&sample_task());
        assert_eq!(wire["kind"], json!("fs_move"));
        assert_eq!(wire["state"], json!("running"));
        assert_eq!(wire["finished_at"], json!(null));
    }

    #[test]
    fn server_events_are_tagged_by_type() {
        let task = roundtrip(&ServerEvent::Task {
            task: sample_task(),
        });
        assert_eq!(task["type"], json!("task"));
        assert_eq!(task["task"]["id"], json!(5));

        let change = roundtrip(&ServerEvent::FsChange {
            volume_id: 1,
            dirs: vec!["2024".to_owned(), "2024/Trip".to_owned()],
        });
        assert_eq!(
            change,
            json!({ "type": "fs_change", "volume_id": 1, "dirs": ["2024", "2024/Trip"] })
        );
    }

    #[test]
    fn indexing_is_a_task_not_a_separate_event_type() {
        let wire = roundtrip(&ServerEvent::Task {
            task: TaskDto {
                kind: TaskKind::Index,
                ..sample_task()
            },
        });
        assert_eq!(wire["type"], json!("task"));
        assert_eq!(wire["task"]["kind"], json!("index"));

        let retired = json!({
            "type": "index",
            "state": "scanning",
            "files_total": 100,
            "files_done": 40,
        });
        assert!(serde_json::from_value::<ServerEvent>(retired).is_err());
    }
}
