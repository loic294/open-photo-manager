//! Opt-in snapshot identity shared by file-based planners.
//!
//! Call `begin` on a writer and COMMIT before opening the build read snapshot. Then `track`
//! snapshot member ids on the writer in bounded chunks. Deletes during this window record
//! tombstones, even before membership exists. Tracking never revives a tombstone; files INSERT
//! has no trigger. Ready/executing members remain protected until the plan is removed.
//!
//! Join `plan_file_tracking` and `plan_file_identities` with `live = 1` for batched overlays,
//! or use `is_live` for individual execution checks. Missing registration or membership fails
//! closed. Location and freshness checks remain the planner's responsibility.
//!
//! Every caller lists `CHILD_TABLES` FIRST in its `Planner::child_tables`, in this order:
//! registration removal prevents concurrent deletes from repopulating tombstones during GC.
//! No file foreign key: deleted snapshot identities deliberately outlive the files row.

use opm_core::paths::{FileId, PlanId};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use super::{PlanError, bulk::CHUNK_ROWS};

pub(crate) const CHILD_TABLES: [&str; 2] = ["plan_file_tracking", "plan_file_identities"];

pub(crate) async fn begin(conn: &mut SqliteConnection, plan_id: PlanId) -> Result<(), PlanError> {
    let inserted = sqlx::query(
        "INSERT INTO plan_file_tracking(plan_id) SELECT id FROM plans WHERE id = ? AND state = 'building' ON CONFLICT(plan_id) DO NOTHING",
    )
    .bind(plan_id)
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(PlanError::Internal(anyhow::anyhow!(
            "identity tracking requires a new building plan"
        )));
    }
    Ok(())
}

pub(crate) async fn track(
    conn: &mut SqliteConnection,
    plan_id: PlanId,
    file_ids: &[FileId],
) -> Result<(), PlanError> {
    let registered: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM plan_file_tracking WHERE plan_id = ?)")
            .bind(plan_id)
            .fetch_one(&mut *conn)
            .await?;
    if !registered {
        return Err(PlanError::Cancelled);
    }
    for chunk in file_ids.chunks(CHUNK_ROWS) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "INSERT INTO plan_file_identities(plan_id,file_id,live) SELECT ",
        );
        qb.push_bind(plan_id)
            .push(", column1, EXISTS(SELECT 1 FROM files WHERE id = column1) FROM (");
        qb.push_values(chunk, |mut row, id| {
            row.push_bind(*id);
        });
        qb.push(") WHERE EXISTS(SELECT 1 FROM plan_file_tracking WHERE plan_id = ")
            .push_bind(plan_id)
            .push(") ON CONFLICT(plan_id,file_id) DO NOTHING");
        qb.build().execute(&mut *conn).await?;
    }
    Ok(())
}

pub(crate) async fn is_live(
    conn: &mut SqliteConnection,
    plan_id: PlanId,
    file_id: FileId,
) -> Result<bool, PlanError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM plan_file_tracking t JOIN plan_file_identities i ON i.plan_id = t.plan_id WHERE t.plan_id = ? AND i.file_id = ? AND i.live = 1)",
    )
    .bind(plan_id)
    .bind(file_id)
    .fetch_one(conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Db,
        testing::{insert_file, insert_volume},
    };
    use crate::plans::bulk::delete_plan_rows;
    use tokio_util::sync::CancellationToken;

    async fn plan(db: &Db) -> PlanId {
        sqlx::query_scalar("INSERT INTO plans(kind,state,params_json,created_at,expires_at) VALUES('move','building','{}',0,1) RETURNING id")
            .fetch_one(db.write()).await.unwrap()
    }

    async fn registered(db: &Db, id: PlanId) {
        let mut tx = db.begin_write().await.unwrap();
        begin(&mut tx, id).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn member(db: &Db, plan: PlanId, file: FileId) -> bool {
        let mut conn = db.read().acquire().await.unwrap();
        is_live(&mut conn, plan, file).await.unwrap()
    }

    #[tokio::test]
    async fn committed_registration_before_snapshot_protects_untracked_deletions() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = insert_volume(&db, "v").await;
        let file = insert_file(&db, volume, "", "one.jpg").await;
        let id = plan(&db).await;
        assert!(!member(&db, id, file).await);
        registered(&db, id).await;
        let mut snapshot = db.read().begin().await.unwrap();
        let original: i64 = sqlx::query_scalar("SELECT id FROM files WHERE id = ?")
            .bind(file)
            .fetch_one(&mut *snapshot)
            .await
            .unwrap();
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(file)
            .execute(db.write())
            .await
            .unwrap();
        let replacement = insert_file(&db, volume, "", "one.jpg").await;
        assert_eq!(replacement, original);
        let mut tx = db.begin_write().await.unwrap();
        track(&mut tx, id, &[original]).await.unwrap();
        tx.commit().await.unwrap();
        snapshot.commit().await.unwrap();
        assert!(!member(&db, id, replacement).await);
        // Repeated registration of membership cannot revive it.
        let mut tx = db.begin_write().await.unwrap();
        track(&mut tx, id, &[replacement]).await.unwrap();
        tx.commit().await.unwrap();
        assert!(!member(&db, id, replacement).await);
        let fresh = plan(&db).await;
        registered(&db, fresh).await;
        let mut tx = db.begin_write().await.unwrap();
        track(&mut tx, fresh, &[replacement]).await.unwrap();
        tx.commit().await.unwrap();
        assert!(member(&db, fresh, replacement).await);
    }

    #[tokio::test]
    async fn ready_identity_and_build_tombstones_follow_rollback_and_volume_cascade() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = insert_volume(&db, "v").await;
        let file = insert_file(&db, volume, "", "one.jpg").await;
        let ready = plan(&db).await;
        registered(&db, ready).await;
        let mut tx = db.begin_write().await.unwrap();
        track(&mut tx, ready, &[file]).await.unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE plans SET state = 'ready' WHERE id = ?")
            .bind(ready)
            .execute(db.write())
            .await
            .unwrap();
        let building = plan(&db).await;
        registered(&db, building).await;
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("DELETE FROM volumes WHERE id = ?")
            .bind(volume)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(member(&db, ready, file).await);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plan_file_identities WHERE plan_id = ?")
                .bind(building)
                .fetch_one(db.read())
                .await
                .unwrap();
        assert_eq!(count, 0);
        sqlx::query("DELETE FROM volumes WHERE id = ?")
            .bind(volume)
            .execute(db.write())
            .await
            .unwrap();
        assert!(!member(&db, ready, file).await);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM plan_file_identities WHERE plan_id = ? AND live = 0",
        )
        .bind(building)
        .fetch_one(db.read())
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn bulk_tracking_stays_within_bind_limits_and_missing_members_fail_closed() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let id = plan(&db).await;
        registered(&db, id).await;
        let ids: Vec<FileId> = (1..40_001).collect();
        let mut tx = db.begin_write().await.unwrap();
        track(&mut tx, id, &ids).await.unwrap();
        tx.commit().await.unwrap();
        let (count, live): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), SUM(live) FROM plan_file_identities WHERE plan_id = ?",
        )
        .bind(id)
        .fetch_one(db.read())
        .await
        .unwrap();
        assert_eq!((count, live), (40_000, 0));
        assert!(!member(&db, id, 1).await);
        assert!(!member(&db, id, 40_001).await);
    }

    #[tokio::test]
    async fn registration_first_cleanup_never_repopulates_cancelled_or_finished_plans() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = insert_volume(&db, "v").await;
        let file = insert_file(&db, volume, "", "one.jpg").await;
        // Failed/cancelled builds, expired ready plans and retained executed plans all use
        // the engine's same bounded child-table cleanup.
        for state in ["building", "building", "ready", "executed"] {
            let id = plan(&db).await;
            registered(&db, id).await;
            let mut tx = db.begin_write().await.unwrap();
            track(&mut tx, id, &[file]).await.unwrap();
            tx.commit().await.unwrap();
            sqlx::query("UPDATE plans SET state = ? WHERE id = ?")
                .bind(state)
                .bind(id)
                .execute(db.write())
                .await
                .unwrap();
            let cancel = CancellationToken::new();
            delete_plan_rows(&db, &cancel, CHILD_TABLES[0], id)
                .await
                .unwrap();
            assert!(!member(&db, id, file).await);
            let mut tx = db.begin_write().await.unwrap();
            assert!(matches!(
                track(&mut tx, id, &[file, file + 1]).await,
                Err(PlanError::Cancelled)
            ));
            tx.commit().await.unwrap();
            delete_plan_rows(&db, &cancel, CHILD_TABLES[1], id)
                .await
                .unwrap();
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM plan_file_identities WHERE plan_id = ?")
                    .bind(id)
                    .fetch_one(db.read())
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            sqlx::query("DELETE FROM plans WHERE id = ?")
                .bind(id)
                .execute(db.write())
                .await
                .unwrap();
        }
        // FK deletion is also correct without generic cleanup.
        let id = plan(&db).await;
        registered(&db, id).await;
        sqlx::query("DELETE FROM plans WHERE id = ?")
            .bind(id)
            .execute(db.write())
            .await
            .unwrap();
        assert!(!member(&db, id, file).await);
    }
}
