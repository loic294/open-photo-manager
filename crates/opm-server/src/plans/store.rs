//! The SQL of the generic `plans` table: ONE place for every statement the engine runs on it.
//!
//! A plan row moves through `building` (invisible: the build runs inside the create request),
//! `ready`, `executing` and `executed`. Every transition is one conditional `UPDATE`, so two
//! requests that race for the same plan cannot both win, and nothing here holds a transaction
//! open. Timestamps are epoch seconds from the engine's clock, passed in by the caller.
//!
//! What a client may see is `VISIBLE`: a plan is hidden while it builds and once it expired,
//! except an `executing` plan, which stays visible until its task ends. Hiding a plan sets
//! `expires_at` to 0, which no claim for execution can beat (see `hide`).
//!
//! Owned by todo `building-plan-engine`.

use std::collections::BTreeMap;

use opm_core::dto::{PlanDto, PlanKind, PlanResult, PlanState, PlanWarning};
use opm_core::paths::{PlanId, TaskId};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};

use super::planner::{Plan, PlanError};
use crate::db::{Db, DbError};

const COLUMNS: &str = "id, kind, state, params_json, stats_json, warnings_json, result_json, \
                       task_id, created_at, expires_at";

/// What a client may see: built, and either not expired or being executed (`?1` is the time).
const VISIBLE: &str = "state <> 'building' AND (expires_at > ?1 OR state = 'executing')";

/// `plans.kind`.
pub(super) const fn kind_to_db(kind: PlanKind) -> &'static str {
    match kind {
        PlanKind::Duplicates => "duplicates",
        PlanKind::Move => "move",
        PlanKind::Cleanup => "cleanup",
        PlanKind::Rename => "rename",
    }
}

pub(super) fn kind_from_db(value: &str) -> Result<PlanKind, PlanError> {
    match value {
        "duplicates" => Ok(PlanKind::Duplicates),
        "move" => Ok(PlanKind::Move),
        "cleanup" => Ok(PlanKind::Cleanup),
        "rename" => Ok(PlanKind::Rename),
        other => Err(PlanError::Internal(anyhow::anyhow!(
            "unknown plan kind {other:?} in the plans table"
        ))),
    }
}

/// A state a client can see; `building` has no wire form and is refused.
pub(super) fn state_from_db(value: &str) -> Result<PlanState, PlanError> {
    match value {
        "ready" => Ok(PlanState::Ready),
        "executing" => Ok(PlanState::Executing),
        "executed" => Ok(PlanState::Executed),
        other => Err(PlanError::Internal(anyhow::anyhow!(
            "a plan in state {other:?} was handed out"
        ))),
    }
}

fn parse<T: DeserializeOwned>(column: &str, plan_id: PlanId, text: &str) -> Result<T, PlanError> {
    serde_json::from_str(text).map_err(|err| {
        PlanError::Internal(anyhow::anyhow!(
            "plan {plan_id}: {column} does not parse: {err}"
        ))
    })
}

fn bad_kind(err: PlanError) -> DbError {
    DbError::Sqlx(sqlx::Error::Decode(err.to_string().into()))
}

/// The JSON text of a value that cannot fail to serialise (string-keyed maps, plain structs).
fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, DbError> {
    serde_json::to_string(value).map_err(|err| DbError::Sqlx(sqlx::Error::Encode(Box::new(err))))
}

/// One row of `plans`.
#[derive(Debug, FromRow)]
pub(super) struct PlanRow {
    id: PlanId,
    kind: String,
    state: String,
    params_json: String,
    stats_json: String,
    warnings_json: String,
    result_json: Option<String>,
    task_id: Option<TaskId>,
    created_at: i64,
    expires_at: i64,
}

impl PlanRow {
    /// The plan as a planner receives it.
    pub(super) fn plan(&self) -> Result<Plan, PlanError> {
        Ok(Plan {
            id: self.id,
            kind: kind_from_db(&self.kind)?,
            params: parse("params_json", self.id, &self.params_json)?,
            stats: parse("stats_json", self.id, &self.stats_json)?,
            warnings: parse("warnings_json", self.id, &self.warnings_json)?,
            state: state_from_db(&self.state)?,
            created_at: self.created_at,
            expires_at: self.expires_at,
        })
    }

    /// The plan as a client sees it: statistics, warnings and result, never the parameters.
    pub(super) fn dto(&self) -> Result<PlanDto, PlanError> {
        let result = match &self.result_json {
            Some(text) => Some(parse::<PlanResult>("result_json", self.id, text)?),
            None => None,
        };
        Ok(PlanDto {
            id: self.id,
            kind: kind_from_db(&self.kind)?,
            state: state_from_db(&self.state)?,
            stats: parse::<BTreeMap<String, u64>>("stats_json", self.id, &self.stats_json)?,
            warnings: parse::<Vec<PlanWarning>>("warnings_json", self.id, &self.warnings_json)?,
            result,
            task_id: self.task_id,
            created_at: self.created_at,
            expires_at: self.expires_at,
        })
    }
}

/// Inserts a `building` row and returns its id. `expires_at` is a placeholder until the build ends.
pub(super) async fn insert_building(
    db: &Db,
    kind: PlanKind,
    params: &Value,
    now: i64,
    expires_at: i64,
) -> Result<PlanId, DbError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO plans (kind, state, params_json, created_at, expires_at) \
         VALUES (?1, 'building', ?2, ?3, ?4) RETURNING id",
    )
    .bind(kind_to_db(kind))
    .bind(json(params)?)
    .bind(now)
    .bind(expires_at)
    .fetch_one(db.write())
    .await?)
}

/// `building` to `ready`, with the statistics and warnings of the build. `false` when the row is
/// not `building` any more (it was swept).
pub(super) async fn mark_ready(
    db: &Db,
    id: PlanId,
    stats: &BTreeMap<String, u64>,
    warnings: &[PlanWarning],
    expires_at: i64,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE plans SET state = 'ready', stats_json = ?2, warnings_json = ?3, expires_at = ?4 \
         WHERE id = ?1 AND state = 'building'",
    )
    .bind(id)
    .bind(json(stats)?)
    .bind(json(warnings)?)
    .bind(expires_at)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The plan a client may see, read on a reader connection.
pub(super) async fn get_visible(
    pool: &SqlitePool,
    id: PlanId,
    now: i64,
) -> Result<Option<PlanRow>, DbError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM plans WHERE id = ?2 AND {VISIBLE}"
    )))
    .bind(now)
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// The state column of a plan, whatever it is (even `building` or expired).
pub(super) async fn raw_state(pool: &SqlitePool, id: PlanId) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar("SELECT state FROM plans WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// Makes a `ready` or `executed` plan invisible and impossible to claim, and returns its kind;
/// `None` when it is `building`, `executing` or gone. `expired_by` limits it to plans that expired
/// by that time (the sweep), so a plan used a moment ago survives.
///
/// Expiry 0 is the point: a claim for execution needs `expires_at` above its own clock reading, so
/// it cannot succeed once this statement ran, even when the claimant read its clock earlier.
pub(super) async fn hide(
    db: &Db,
    id: PlanId,
    expired_by: Option<i64>,
) -> Result<Option<PlanKind>, DbError> {
    let kind: Option<String> = sqlx::query_scalar(
        "UPDATE plans SET expires_at = 0 \
         WHERE id = ?1 AND state IN ('ready', 'executed') AND (?2 IS NULL OR expires_at <= ?2) \
         RETURNING kind",
    )
    .bind(id)
    .bind(expired_by)
    .fetch_optional(db.write())
    .await?;
    kind.map(|kind| kind_from_db(&kind))
        .transpose()
        .map_err(bad_kind)
}

/// Removes the plan row (the rows of a planner's tables go with it by cascade); never an
/// `executing` plan.
pub(super) async fn delete_row(db: &Db, id: PlanId) -> Result<bool, DbError> {
    let done = sqlx::query("DELETE FROM plans WHERE id = ?1 AND state <> 'executing'")
        .bind(id)
        .execute(db.write())
        .await?;
    Ok(done.rows_affected() == 1)
}

/// `ready` to `executing`, once: `false` when the plan is not `ready` or expired. Whoever gets
/// `true` owns the execution.
pub(super) async fn claim(db: &Db, id: PlanId, now: i64, expires_at: i64) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE plans SET state = 'executing', expires_at = ?3 \
         WHERE id = ?1 AND state = 'ready' AND expires_at > ?2",
    )
    .bind(id)
    .bind(now)
    .bind(expires_at)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Gives a claim back when no task could be created for it.
pub(super) async fn release_claim(db: &Db, id: PlanId) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE plans SET state = 'ready' \
         WHERE id = ?1 AND state = 'executing' AND task_id IS NULL",
    )
    .bind(id)
    .execute(db.write())
    .await?;
    Ok(())
}

/// Links the plan to its `plan_execute` task.
pub(super) async fn set_task(db: &Db, id: PlanId, task_id: TaskId) -> Result<(), DbError> {
    sqlx::query("UPDATE plans SET task_id = ?2 WHERE id = ?1")
        .bind(id)
        .bind(task_id)
        .execute(db.write())
        .await?;
    Ok(())
}

/// `executing` to `executed` with the result. `false` when it was settled already: the first
/// writer wins, so the task runner and the watcher that backs it up can both call this.
pub(super) async fn finish(
    db: &Db,
    id: PlanId,
    task_id: Option<TaskId>,
    result: &PlanResult,
    expires_at: i64,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE plans SET state = 'executed', result_json = ?2, expires_at = ?3, \
         task_id = COALESCE(?4, task_id) \
         WHERE id = ?1 AND state = 'executing'",
    )
    .bind(id)
    .bind(json(result)?)
    .bind(expires_at)
    .bind(task_id)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Pushes the expiry of a live plan forward, never back.
pub(super) async fn extend(
    db: &Db,
    id: PlanId,
    now: i64,
    expires_at: i64,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE plans SET expires_at = ?3 \
         WHERE id = ?1 AND state IN ('ready', 'executed') AND expires_at > ?2 AND expires_at < ?3",
    )
    .bind(id)
    .bind(now)
    .bind(expires_at)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Plans of any kind that expired by `now`, oldest first, never an `executing` one.
pub(super) async fn list_expired(
    pool: &SqlitePool,
    now: i64,
    limit: i64,
) -> Result<Vec<(PlanId, PlanKind)>, DbError> {
    let rows: Vec<(PlanId, String)> = sqlx::query_as(
        "SELECT id, kind FROM plans \
         WHERE state IN ('ready', 'executed') AND expires_at <= ?1 \
         ORDER BY expires_at, id LIMIT ?2",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    kinds(rows)
}

/// `building` rows created at or before `created_by` (all of them for `None`).
pub(super) async fn list_building(
    pool: &SqlitePool,
    created_by: Option<i64>,
) -> Result<Vec<(PlanId, PlanKind)>, DbError> {
    let rows: Vec<(PlanId, String)> = sqlx::query_as(
        "SELECT id, kind FROM plans \
         WHERE state = 'building' AND (?1 IS NULL OR created_at <= ?1) ORDER BY id",
    )
    .bind(created_by)
    .fetch_all(pool)
    .await?;
    kinds(rows)
}

fn kinds(rows: Vec<(PlanId, String)>) -> Result<Vec<(PlanId, PlanKind)>, DbError> {
    rows.into_iter()
        .map(|(id, kind)| kind_from_db(&kind).map(|kind| (id, kind)).map_err(bad_kind))
        .collect()
}

/// Settles every `executing` plan after a restart: its task died with the process.
pub(super) async fn settle_interrupted(
    db: &Db,
    result: &PlanResult,
    expires_at: i64,
) -> Result<u64, DbError> {
    let done = sqlx::query(
        "UPDATE plans SET state = 'executed', result_json = ?1, expires_at = ?2 \
         WHERE state = 'executing'",
    )
    .bind(json(result)?)
    .bind(expires_at)
    .execute(db.write())
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TempDb;

    const NOW: i64 = 1_000_000;

    async fn ready_plan(temp: &TempDb, kind: PlanKind, expires_at: i64) -> PlanId {
        let db = temp.handle();
        let id = insert_building(&db, kind, &serde_json::json!({"a": 1}), NOW, expires_at)
            .await
            .unwrap();
        let stats = BTreeMap::from([("pairs".to_owned(), 3_u64)]);
        let warnings = [PlanWarning {
            code: "skipped".to_owned(),
            message: "left out".to_owned(),
            count: 2,
        }];
        assert!(
            mark_ready(&db, id, &stats, &warnings, expires_at)
                .await
                .unwrap()
        );
        id
    }

    async fn visible(db: &Db, id: PlanId, now: i64) -> bool {
        get_visible(db.read(), id, now).await.unwrap().is_some()
    }

    async fn task_row(db: &Db, state: &str) -> TaskId {
        sqlx::query_scalar(
            "INSERT INTO tasks (kind, title, lane, state, created_at) \
             VALUES ('plan_execute', 't', 'fs', ?1, 0) RETURNING id",
        )
        .bind(state)
        .fetch_one(db.write())
        .await
        .unwrap()
    }

    #[test]
    fn kinds_and_states_map_both_ways() {
        for kind in [
            PlanKind::Duplicates,
            PlanKind::Move,
            PlanKind::Cleanup,
            PlanKind::Rename,
        ] {
            assert_eq!(kind_from_db(kind_to_db(kind)).unwrap(), kind);
        }
        assert!(kind_from_db("copy").is_err());
        assert_eq!(state_from_db("ready").unwrap(), PlanState::Ready);
        assert_eq!(state_from_db("executing").unwrap(), PlanState::Executing);
        assert_eq!(state_from_db("executed").unwrap(), PlanState::Executed);
        assert!(state_from_db("building").is_err());
    }

    #[tokio::test]
    async fn a_building_plan_is_invisible_until_it_is_ready() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = insert_building(&db, PlanKind::Move, &serde_json::json!({}), NOW, NOW + 100)
            .await
            .unwrap();
        assert!(!visible(&db, id, NOW).await);
        assert_eq!(
            raw_state(db.read(), id).await.unwrap().as_deref(),
            Some("building")
        );
        assert!(
            mark_ready(&db, id, &BTreeMap::new(), &[], NOW + 100)
                .await
                .unwrap()
        );
        assert!(visible(&db, id, NOW).await);
        assert!(
            !mark_ready(&db, id, &BTreeMap::new(), &[], NOW + 100)
                .await
                .unwrap(),
            "only a building row turns ready"
        );
    }

    #[tokio::test]
    async fn a_row_converts_to_the_plan_and_the_dto_without_the_parameters() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Duplicates, NOW + 100).await;
        let row = get_visible(db.read(), id, NOW).await.unwrap().unwrap();

        let plan = row.plan().unwrap();
        assert_eq!(plan.kind, PlanKind::Duplicates);
        assert_eq!(plan.params, serde_json::json!({"a": 1}));
        assert_eq!(plan.stats.get("pairs"), Some(&3));
        assert_eq!(plan.state, PlanState::Ready);

        let dto = row.dto().unwrap();
        assert_eq!(dto.id, id);
        assert_eq!(dto.stats.get("pairs"), Some(&3));
        assert_eq!(dto.warnings[0].code, "skipped");
        assert_eq!(dto.result, None);
        assert_eq!(dto.task_id, None);
        assert_eq!(dto.expires_at, NOW + 100);
        assert!(
            !serde_json::to_string(&dto).unwrap().contains("params"),
            "the parameters are never echoed"
        );
    }

    #[tokio::test]
    async fn an_expired_plan_is_hidden_but_an_executing_one_is_not() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Cleanup, NOW + 10).await;
        assert!(visible(&db, id, NOW + 9).await);
        assert!(!visible(&db, id, NOW + 10).await);

        assert!(claim(&db, id, NOW, NOW + 10).await.unwrap());
        assert!(
            visible(&db, id, NOW + 5_000).await,
            "an executing plan stays visible past its expiry"
        );
    }

    #[tokio::test]
    async fn only_one_claim_wins_and_an_expired_plan_cannot_be_claimed() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
        assert!(!claim(&db, id, NOW, NOW + 200).await.unwrap());

        let expired = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        assert!(!claim(&db, expired, NOW + 100, NOW + 300).await.unwrap());
    }

    #[tokio::test]
    async fn a_released_claim_makes_the_plan_ready_again() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
        release_claim(&db, id).await.unwrap();
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
    }

    #[tokio::test]
    async fn finishing_stores_the_result_once_and_keeps_the_task_link() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        let task = task_row(&db, "queued").await;
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
        set_task(&db, id, task).await.unwrap();

        let result = PlanResult {
            done: 4,
            bytes: 99,
            ..PlanResult::default()
        };
        assert!(
            finish(&db, id, Some(task), &result, NOW + 300)
                .await
                .unwrap()
        );
        assert!(
            !finish(&db, id, None, &PlanResult::default(), NOW + 400)
                .await
                .unwrap(),
            "the first result wins"
        );

        let dto = get_visible(db.read(), id, NOW)
            .await
            .unwrap()
            .unwrap()
            .dto()
            .unwrap();
        assert_eq!(dto.state, PlanState::Executed);
        assert_eq!(dto.result, Some(result));
        assert_eq!(dto.task_id, Some(task));
        assert_eq!(dto.expires_at, NOW + 300);
    }

    #[tokio::test]
    async fn purging_the_task_row_unlinks_the_plan_instead_of_failing() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        let task = task_row(&db, "succeeded").await;
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
        set_task(&db, id, task).await.unwrap();
        assert!(
            finish(&db, id, None, &PlanResult::default(), NOW + 200)
                .await
                .unwrap()
        );

        sqlx::query("DELETE FROM tasks WHERE id = ?1")
            .bind(task)
            .execute(db.write())
            .await
            .unwrap();
        let dto = get_visible(db.read(), id, NOW)
            .await
            .unwrap()
            .unwrap()
            .dto()
            .unwrap();
        assert_eq!(dto.task_id, None);
        assert_eq!(dto.state, PlanState::Executed);
    }

    #[tokio::test]
    async fn extending_only_pushes_a_live_plan_forward() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        assert!(!extend(&db, id, NOW, NOW + 50).await.unwrap(), "never back");
        assert!(extend(&db, id, NOW, NOW + 500).await.unwrap());
        assert!(
            !extend(&db, id, NOW + 500, NOW + 900).await.unwrap(),
            "an expired plan is not revived"
        );
        let row = get_visible(db.read(), id, NOW).await.unwrap().unwrap();
        assert_eq!(row.dto().unwrap().expires_at, NOW + 500);
    }

    #[tokio::test]
    async fn hiding_blocks_a_late_claim_and_spares_a_plan_that_is_still_live() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Cleanup, NOW + 100).await;

        assert_eq!(hide(&db, id, Some(NOW)).await.unwrap(), None, "still live");
        assert_eq!(hide(&db, id, None).await.unwrap(), Some(PlanKind::Cleanup));
        assert!(!visible(&db, id, NOW).await);
        assert!(
            !claim(&db, id, NOW, NOW + 200).await.unwrap(),
            "a claimant with an old clock reading still loses"
        );
    }

    #[tokio::test]
    async fn an_executing_plan_cannot_be_hidden_or_deleted() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = ready_plan(&temp, PlanKind::Cleanup, NOW + 100).await;
        assert!(claim(&db, id, NOW, NOW + 200).await.unwrap());
        assert_eq!(hide(&db, id, None).await.unwrap(), None);
        assert!(!delete_row(&db, id).await.unwrap());
        assert_eq!(
            raw_state(db.read(), id).await.unwrap().as_deref(),
            Some("executing")
        );
    }

    #[tokio::test]
    async fn expired_and_stale_building_plans_are_listed_for_the_sweep() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let old = ready_plan(&temp, PlanKind::Move, NOW + 10).await;
        let fresh = ready_plan(&temp, PlanKind::Cleanup, NOW + 1_000).await;
        let running = ready_plan(&temp, PlanKind::Move, NOW + 10).await;
        assert!(claim(&db, running, NOW, NOW + 10).await.unwrap());
        let building = insert_building(&db, PlanKind::Duplicates, &Value::Null, NOW, NOW + 10)
            .await
            .unwrap();

        let expired = list_expired(db.read(), NOW + 20, 10).await.unwrap();
        assert_eq!(expired, vec![(old, PlanKind::Move)]);
        assert!(!expired.iter().any(|(id, _)| *id == fresh || *id == running));

        assert_eq!(
            list_building(db.read(), None).await.unwrap(),
            vec![(building, PlanKind::Duplicates)]
        );
        assert!(
            list_building(db.read(), Some(NOW - 1))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(list_building(db.read(), Some(NOW)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_restart_settles_every_executing_plan() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let running = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        let waiting = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        assert!(claim(&db, running, NOW, NOW + 100).await.unwrap());

        let result = PlanResult {
            cancelled: true,
            note: Some("interrupted by restart".to_owned()),
            ..PlanResult::default()
        };
        assert_eq!(
            settle_interrupted(&db, &result, NOW + 500).await.unwrap(),
            1
        );

        let settled = get_visible(db.read(), running, NOW)
            .await
            .unwrap()
            .unwrap()
            .dto()
            .unwrap();
        assert_eq!(settled.state, PlanState::Executed);
        assert_eq!(settled.result, Some(result));
        let untouched = get_visible(db.read(), waiting, NOW)
            .await
            .unwrap()
            .unwrap()
            .dto()
            .unwrap();
        assert_eq!(untouched.state, PlanState::Ready);
    }

    #[tokio::test]
    async fn deleting_the_row_cascades_to_a_planners_table() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        sqlx::query(
            "CREATE TABLE store_children (plan_id INTEGER NOT NULL REFERENCES plans (id) \
             ON DELETE CASCADE, n INTEGER NOT NULL)",
        )
        .execute(db.write())
        .await
        .unwrap();
        let id = ready_plan(&temp, PlanKind::Move, NOW + 100).await;
        sqlx::query("INSERT INTO store_children (plan_id, n) VALUES (?1, 1), (?1, 2)")
            .bind(id)
            .execute(db.write())
            .await
            .unwrap();
        assert!(delete_row(&db, id).await.unwrap());
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_children")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
