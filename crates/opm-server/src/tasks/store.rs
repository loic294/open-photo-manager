//! Persistence of tasks: every statement that touches the `tasks` table.
//!
//! The engine writes a row when a task is submitted, flips it to `running`, saves the counters now
//! and then, and writes the final state. Updates that move a row forward are guarded by the state
//! the row must still be in, so a late write (a runner that finishes after a newer server instance
//! already marked the row `interrupted`) never resurrects or overwrites a finished task.

use opm_core::dto::{TaskDto, TaskState};
use opm_core::paths::TaskId;
use serde_json::Value;

use super::model::{
    INTERRUPTED_BY_RESTART, TaskSpec, kind_from_db, kind_to_db, state_from_db, state_to_db,
};
use super::progress::ProgressView;
use crate::db::models::InvalidDbValue;
use crate::db::{Db, DbError};

/// What the engine writes when a task reaches a final state.
pub(super) struct Finish<'a> {
    pub state: TaskState,
    pub error: Option<&'a str>,
    /// Stored as `result_json`; a JSON `null` is stored as SQL `NULL`.
    pub result: Option<&'a Value>,
    pub finished_at: i64,
    /// The last counters, for a task that ran; `None` leaves the stored ones alone.
    pub progress: Option<&'a ProgressView>,
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: i64,
    kind: String,
    title: String,
    state: String,
    items_done: i64,
    items_total: Option<i64>,
    bytes_done: i64,
    bytes_total: Option<i64>,
    error: Option<String>,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

impl TaskRow {
    /// A stored task has no rate: speed and ETA only exist while it runs.
    fn into_dto(self) -> Result<TaskDto, InvalidDbValue> {
        Ok(TaskDto {
            id: self.id,
            kind: kind_from_db(&self.kind)?,
            title: self.title,
            state: state_from_db(&self.state)?,
            items_done: self.items_done,
            items_total: self.items_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            speed: None,
            eta_secs: None,
            error: self.error,
            started_at: self.started_at,
            finished_at: self.finished_at,
        })
    }
}

/// A row this build cannot read (written by another version) is skipped rather than failing the
/// whole listing.
fn decode(row: TaskRow) -> Option<TaskDto> {
    let id = row.id;
    match row.into_dto() {
        Ok(dto) => Some(dto),
        Err(err) => {
            tracing::warn!(task = id, error = %err, "ignoring a task row that cannot be decoded");
            None
        }
    }
}

macro_rules! task_columns {
    () => {
        "id, kind, title, state, items_done, items_total, bytes_done, bytes_total, error, \
         started_at, finished_at"
    };
}

/// Inserts a task in the `queued` state and returns its id.
pub(super) async fn insert_queued(
    db: &Db,
    spec: &TaskSpec,
    created_at: i64,
) -> Result<TaskId, DbError> {
    // `params_json` must be valid JSON and a bare `null` is not an object worth keeping.
    let params = if spec.params.is_null() {
        "{}".to_owned()
    } else {
        spec.params.to_string()
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (kind, title, lane, state, params_json, created_at, resumable) \
         VALUES (?, ?, ?, 'queued', ?, ?, ?) RETURNING id",
    )
    .bind(kind_to_db(spec.kind))
    .bind(&spec.title)
    .bind(spec.lane.as_str())
    .bind(params)
    .bind(created_at)
    .bind(spec.resumable)
    .fetch_one(db.write())
    .await?;
    Ok(id)
}

/// `queued` -> `running`. Returns whether the row moved.
pub(super) async fn mark_running(db: &Db, id: TaskId, started_at: i64) -> Result<bool, DbError> {
    let moved = sqlx::query(
        "UPDATE tasks SET state = 'running', started_at = ? WHERE id = ? AND state = 'queued'",
    )
    .bind(started_at)
    .bind(id)
    .execute(db.write())
    .await?
    .rows_affected();
    Ok(moved > 0)
}

/// Saves the counters and the title of a running task.
pub(super) async fn save_progress(db: &Db, id: TaskId, view: &ProgressView) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE tasks SET title = ?, items_done = ?, items_total = ?, bytes_done = ?, \
         bytes_total = ? WHERE id = ? AND state = 'running'",
    )
    .bind(&view.title)
    .bind(view.items_done)
    .bind(view.items_total)
    .bind(view.bytes_done)
    .bind(view.bytes_total)
    .bind(id)
    .execute(db.write())
    .await?;
    Ok(())
}

/// Writes the final state of a task that is still `queued` or `running`. Returns whether the row
/// moved (`false`: somebody else already finished it).
pub(super) async fn finish(db: &Db, id: TaskId, outcome: &Finish<'_>) -> Result<bool, DbError> {
    let state = state_to_db(outcome.state);
    let result = outcome
        .result
        .filter(|value| !value.is_null())
        .map(Value::to_string);
    let moved = match outcome.progress {
        Some(view) => sqlx::query(
            "UPDATE tasks SET state = ?, error = ?, result_json = ?, finished_at = ?, \
                 title = ?, items_done = ?, items_total = ?, bytes_done = ?, bytes_total = ? \
                 WHERE id = ? AND state IN ('queued', 'running')",
        )
        .bind(state)
        .bind(outcome.error)
        .bind(result)
        .bind(outcome.finished_at)
        .bind(&view.title)
        .bind(view.items_done)
        .bind(view.items_total)
        .bind(view.bytes_done)
        .bind(view.bytes_total)
        .bind(id)
        .execute(db.write())
        .await?
        .rows_affected(),
        None => sqlx::query(
            "UPDATE tasks SET state = ?, error = ?, result_json = ?, finished_at = ? \
                 WHERE id = ? AND state IN ('queued', 'running')",
        )
        .bind(state)
        .bind(outcome.error)
        .bind(result)
        .bind(outcome.finished_at)
        .bind(id)
        .execute(db.write())
        .await?
        .rows_affected(),
    };
    Ok(moved > 0)
}

/// Startup recovery: a row left `queued` or `running` belongs to a server that is gone.
pub(super) async fn recover_interrupted(db: &Db, now: i64) -> Result<u64, DbError> {
    let recovered = sqlx::query(
        "UPDATE tasks SET state = 'interrupted', error = ?, finished_at = ? \
         WHERE state IN ('queued', 'running')",
    )
    .bind(INTERRUPTED_BY_RESTART)
    .bind(now)
    .execute(db.write())
    .await?
    .rows_affected();
    Ok(recovered)
}

/// Retention: deletes finished tasks that ended before `cutoff` or that are not among the newest
/// `keep` finished ones. Active tasks are never touched. Returns the number of rows removed.
pub(super) async fn prune(db: &Db, cutoff: i64, keep: i64) -> Result<u64, DbError> {
    let removed = sqlx::query(
        "DELETE FROM tasks WHERE state NOT IN ('queued', 'running') \
         AND NOT (state = 'interrupted' AND resumable = 1) AND ( \
             COALESCE(finished_at, created_at) < ? \
             OR id NOT IN ( \
                 SELECT id FROM tasks WHERE state NOT IN ('queued', 'running') \
                 ORDER BY COALESCE(finished_at, created_at) DESC, id DESC LIMIT ? \
             ) \
         )",
    )
    .bind(cutoff)
    .bind(keep)
    .execute(db.write())
    .await?
    .rows_affected();
    Ok(removed)
}

/// One task by id.
pub(super) async fn get(db: &Db, id: TaskId) -> Result<Option<TaskDto>, DbError> {
    let row = sqlx::query_as::<_, TaskRow>(concat!(
        "SELECT ",
        task_columns!(),
        " FROM tasks WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.read())
    .await?;
    Ok(row.and_then(decode))
}

/// The most recently finished tasks, newest first.
pub(super) async fn list_finished(db: &Db, limit: i64) -> Result<Vec<TaskDto>, DbError> {
    let rows = sqlx::query_as::<_, TaskRow>(concat!(
        "SELECT ",
        task_columns!(),
        " FROM tasks WHERE state NOT IN ('queued', 'running') \
          ORDER BY COALESCE(finished_at, created_at) DESC, id DESC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(db.read())
    .await?;
    Ok(rows.into_iter().filter_map(decode).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tasks::model::Lane;
    use opm_core::dto::TaskKind;

    const DAY: i64 = 24 * 3600;

    async fn db() -> crate::db::TempDb {
        Db::open_temp().await.unwrap()
    }

    fn spec(title: &str) -> TaskSpec {
        TaskSpec::new(TaskKind::FsMove, title)
    }

    fn view(done: i64) -> ProgressView {
        ProgressView {
            title: "Moving".to_owned(),
            items_done: done,
            items_total: Some(10),
            bytes_done: done * 100,
            bytes_total: None,
            speed: Some(1.0),
            eta_secs: Some(2.0),
        }
    }

    async fn column<T>(db: &Db, id: TaskId, column: &'static str) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
    {
        let sql = format!("SELECT {column} FROM tasks WHERE id = ?");
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    async fn finish_at(db: &Db, id: TaskId, state: TaskState, finished_at: i64) {
        let moved = finish(
            db,
            id,
            &Finish {
                state,
                error: None,
                result: None,
                finished_at,
                progress: None,
            },
        )
        .await
        .unwrap();
        assert!(moved);
    }

    #[tokio::test]
    async fn a_task_is_stored_queued_with_its_lane_and_params() {
        let db = db().await;
        let spec = spec("Move 3 files")
            .with_params(json!({ "files": 3 }))
            .with_lane(Lane::Index);
        let id = insert_queued(&db, &spec, 1_000).await.unwrap();

        assert_eq!(column::<String>(&db, id, "state").await, "queued");
        assert_eq!(column::<String>(&db, id, "kind").await, "fs_move");
        assert_eq!(column::<String>(&db, id, "lane").await, "index");
        assert_eq!(
            column::<String>(&db, id, "params_json").await,
            r#"{"files":3}"#
        );
        assert_eq!(column::<i64>(&db, id, "created_at").await, 1_000);

        let dto = get(&db, id).await.unwrap().unwrap();
        assert_eq!(dto.id, id);
        assert_eq!(dto.title, "Move 3 files");
        assert_eq!(dto.state, TaskState::Queued);
        assert_eq!((dto.started_at, dto.finished_at), (None, None));
        assert_eq!((dto.speed, dto.eta_secs), (None, None));
        assert_eq!(get(&db, id + 1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn null_params_are_stored_as_an_empty_object() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t").with_params(Value::Null), 1)
            .await
            .unwrap();
        assert_eq!(column::<String>(&db, id, "params_json").await, "{}");
    }

    #[tokio::test]
    async fn running_then_every_final_state_is_accepted_by_the_table() {
        let db = db().await;
        for (state, stored) in [
            (TaskState::Done, "succeeded"),
            (TaskState::Failed, "failed"),
            (TaskState::Cancelled, "cancelled"),
            (TaskState::Interrupted, "interrupted"),
        ] {
            let id = insert_queued(&db, &spec("t"), 10).await.unwrap();
            assert!(mark_running(&db, id, 20).await.unwrap());
            assert_eq!(column::<String>(&db, id, "state").await, "running");
            assert_eq!(column::<i64>(&db, id, "started_at").await, 20);

            finish_at(&db, id, state, 30).await;
            assert_eq!(column::<String>(&db, id, "state").await, stored);
            let dto = get(&db, id).await.unwrap().unwrap();
            assert_eq!(dto.state, state);
            assert_eq!((dto.started_at, dto.finished_at), (Some(20), Some(30)));
        }
    }

    #[tokio::test]
    async fn marking_running_only_moves_a_queued_row() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
        assert!(mark_running(&db, id, 2).await.unwrap());
        assert!(!mark_running(&db, id, 3).await.unwrap(), "already running");
        assert_eq!(column::<i64>(&db, id, "started_at").await, 2);
    }

    #[tokio::test]
    async fn progress_is_saved_only_while_running() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
        save_progress(&db, id, &view(1)).await.unwrap();
        assert_eq!(
            column::<i64>(&db, id, "items_done").await,
            0,
            "still queued"
        );

        mark_running(&db, id, 2).await.unwrap();
        save_progress(&db, id, &view(4)).await.unwrap();
        let dto = get(&db, id).await.unwrap().unwrap();
        assert_eq!((dto.items_done, dto.items_total), (4, Some(10)));
        assert_eq!((dto.bytes_done, dto.bytes_total), (400, None));
        assert_eq!(dto.title, "Moving");

        finish_at(&db, id, TaskState::Done, 3).await;
        save_progress(&db, id, &view(9)).await.unwrap();
        assert_eq!(column::<i64>(&db, id, "items_done").await, 4, "finished");
    }

    #[tokio::test]
    async fn finishing_stores_error_result_and_the_last_counters() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
        mark_running(&db, id, 2).await.unwrap();
        let result = json!({ "moved": 7 });
        let moved = finish(
            &db,
            id,
            &Finish {
                state: TaskState::Failed,
                error: Some("disk full"),
                result: Some(&result),
                finished_at: 9,
                progress: Some(&view(7)),
            },
        )
        .await
        .unwrap();
        assert!(moved);

        let dto = get(&db, id).await.unwrap().unwrap();
        assert_eq!(dto.state, TaskState::Failed);
        assert_eq!(dto.error.as_deref(), Some("disk full"));
        assert_eq!(dto.items_done, 7);
        assert_eq!(
            column::<String>(&db, id, "result_json").await,
            r#"{"moved":7}"#
        );
    }

    #[tokio::test]
    async fn a_null_result_is_stored_as_sql_null() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
        let result = Value::Null;
        finish(
            &db,
            id,
            &Finish {
                state: TaskState::Done,
                error: None,
                result: Some(&result),
                finished_at: 2,
                progress: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(column::<Option<String>>(&db, id, "result_json").await, None);
    }

    #[tokio::test]
    async fn a_finished_task_is_never_overwritten() {
        let db = db().await;
        let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
        mark_running(&db, id, 2).await.unwrap();
        finish_at(&db, id, TaskState::Cancelled, 3).await;

        let late = finish(
            &db,
            id,
            &Finish {
                state: TaskState::Done,
                error: None,
                result: None,
                finished_at: 4,
                progress: Some(&view(10)),
            },
        )
        .await
        .unwrap();
        assert!(!late);
        let dto = get(&db, id).await.unwrap().unwrap();
        assert_eq!(dto.state, TaskState::Cancelled);
        assert_eq!(dto.finished_at, Some(3));
        assert_eq!(dto.items_done, 0);
    }

    #[tokio::test]
    async fn recovery_interrupts_active_rows_and_leaves_finished_ones() {
        let db = db().await;
        let queued = insert_queued(&db, &spec("queued"), 1).await.unwrap();
        let running = insert_queued(&db, &spec("running"), 1).await.unwrap();
        mark_running(&db, running, 2).await.unwrap();
        let done = insert_queued(&db, &spec("done"), 1).await.unwrap();
        finish_at(&db, done, TaskState::Done, 5).await;

        assert_eq!(recover_interrupted(&db, 100).await.unwrap(), 2);
        for id in [queued, running] {
            let dto = get(&db, id).await.unwrap().unwrap();
            assert_eq!(dto.state, TaskState::Interrupted);
            assert_eq!(dto.error.as_deref(), Some("interrupted by restart"));
            assert_eq!(dto.finished_at, Some(100));
        }
        let dto = get(&db, done).await.unwrap().unwrap();
        assert_eq!((dto.state, dto.finished_at), (TaskState::Done, Some(5)));
        assert_eq!(recover_interrupted(&db, 200).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pruning_removes_old_finished_rows_and_keeps_active_ones() {
        let db = db().await;
        let now = 100 * DAY;
        let old = insert_queued(&db, &spec("old"), 1).await.unwrap();
        finish_at(&db, old, TaskState::Done, now - 8 * DAY).await;
        let recent = insert_queued(&db, &spec("recent"), 1).await.unwrap();
        finish_at(&db, recent, TaskState::Failed, now - DAY).await;
        let active = insert_queued(&db, &spec("active"), 1).await.unwrap();
        mark_running(&db, active, 2).await.unwrap();
        let waiting = insert_queued(&db, &spec("waiting"), 1).await.unwrap();

        assert_eq!(prune(&db, now - 7 * DAY, 500).await.unwrap(), 1);
        assert_eq!(get(&db, old).await.unwrap(), None);
        for id in [recent, active, waiting] {
            assert!(get(&db, id).await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn pruning_keeps_only_the_newest_finished_rows() {
        let db = db().await;
        let mut ids = Vec::new();
        for n in 0..6 {
            let id = insert_queued(&db, &spec("t"), 1).await.unwrap();
            finish_at(&db, id, TaskState::Done, 1_000 + n).await;
            ids.push(id);
        }
        let active = insert_queued(&db, &spec("active"), 1).await.unwrap();

        assert_eq!(prune(&db, 0, 4).await.unwrap(), 2);
        for id in &ids[..2] {
            assert_eq!(get(&db, *id).await.unwrap(), None);
        }
        for id in &ids[2..] {
            assert!(get(&db, *id).await.unwrap().is_some());
        }
        assert!(
            get(&db, active).await.unwrap().is_some(),
            "active rows do not count"
        );
        assert_eq!(prune(&db, 0, 4).await.unwrap(), 0, "idempotent");
    }

    #[tokio::test]
    async fn pruning_with_a_zero_keep_removes_every_finished_row() {
        let db = db().await;
        let done = insert_queued(&db, &spec("t"), 1).await.unwrap();
        finish_at(&db, done, TaskState::Done, 5).await;
        let waiting = insert_queued(&db, &spec("w"), 1).await.unwrap();
        assert_eq!(prune(&db, 0, 0).await.unwrap(), 1);
        assert!(get(&db, waiting).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn finished_tasks_are_listed_newest_first_with_a_limit() {
        let db = db().await;
        let first = insert_queued(&db, &spec("first"), 1).await.unwrap();
        finish_at(&db, first, TaskState::Done, 50).await;
        let second = insert_queued(&db, &spec("second"), 1).await.unwrap();
        finish_at(&db, second, TaskState::Failed, 70).await;
        let tie = insert_queued(&db, &spec("tie"), 1).await.unwrap();
        finish_at(&db, tie, TaskState::Cancelled, 70).await;
        let active = insert_queued(&db, &spec("active"), 1).await.unwrap();
        mark_running(&db, active, 2).await.unwrap();

        let all = list_finished(&db, 10).await.unwrap();
        let ids: Vec<TaskId> = all.iter().map(|task| task.id).collect();
        assert_eq!(ids, [tie, second, first], "ties break on the newest id");
        let limited = list_finished(&db, 2).await.unwrap();
        assert_eq!(limited.len(), 2);
        assert!(list_finished(&db, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_row_this_build_cannot_decode_is_skipped() {
        let db = db().await;
        let good = insert_queued(&db, &spec("good"), 1).await.unwrap();
        finish_at(&db, good, TaskState::Done, 5).await;
        let odd = insert_queued(&db, &spec("odd"), 1).await.unwrap();
        finish_at(&db, odd, TaskState::Done, 6).await;
        sqlx::query("UPDATE tasks SET kind = 'teleport' WHERE id = ?")
            .bind(odd)
            .execute(db.write())
            .await
            .unwrap();

        let listed = list_finished(&db, 10).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, good);
        assert_eq!(get(&db, odd).await.unwrap(), None);
    }
}
