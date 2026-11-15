//! FsOps accounting runs on the finalizing transaction, including during recovery.

use crate::db::Db;
use opm_core::paths::RelPath;
use sqlx::SqliteConnection;

pub(crate) async fn scanned(
    db: &Db,
    volume: i64,
    scope: &RelPath,
    generation: i64,
) -> Result<(), crate::db::DbError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key,dst_path FROM statistics_intents WHERE dst_volume_id=? AND subtree=1 AND state='done' AND scan_generation < ?")
        .bind(volume).bind(generation).fetch_all(db.read()).await?;
    for (key, path) in rows {
        let path = RelPath::new(&path).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        if path.starts_with(scope) {
            let mut tx = db.begin_write().await?;
            sqlx::query("UPDATE statistics_intents SET subtree=0 WHERE key=?")
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM statistics_folder_members WHERE operation_key=?")
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

pub(crate) async fn trashed_folder(
    conn: &mut SqliteConnection,
    journal: i64,
    trash: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO statistics_folder_members(operation_key,trash_item_id,path) SELECT e.operation_key,?,e.path FROM statistics_events e JOIN statistics_intents i ON i.key=e.operation_key WHERE i.journal_id=? AND e.action='removed' AND e.path IS NOT NULL GROUP BY e.operation_key,e.path ON CONFLICT DO NOTHING")
        .bind(trash).bind(journal).execute(conn).await?;
    Ok(())
}

pub(crate) async fn restored_folder(
    conn: &mut SqliteConnection,
    journal: i64,
    trash: i64,
    original: &RelPath,
    destination: &RelPath,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE statistics_folder_members SET operation_key=(SELECT key FROM statistics_intents WHERE journal_id=?),trash_item_id=NULL,path=? || substr(path,length(?)+1) WHERE trash_item_id=?")
        .bind(journal).bind(destination.as_str()).bind(original.as_str()).bind(trash)
        .execute(&mut *conn).await?;
    // A watcher can see unrelated new children while the restore is still pending.
    sqlx::query("UPDATE statistics_events SET operation_key=NULL WHERE operation_key=(SELECT key FROM statistics_intents WHERE journal_id=?) AND action IN ('added','modified') AND NOT EXISTS(SELECT 1 FROM statistics_folder_members m WHERE m.operation_key=statistics_events.operation_key AND m.path=statistics_events.path)")
        .bind(journal).execute(conn).await?;
    Ok(())
}

pub(crate) async fn begin(conn: &mut SqliteConnection, journal: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE statistics_context SET operation_key = (SELECT key FROM statistics_intents WHERE journal_id = ?) WHERE id = 1")
        .bind(journal).execute(conn).await?;
    Ok(())
}

pub(crate) async fn moved_identity(
    conn: &mut SqliteConnection,
    journal: i64,
    file: i64,
) -> Result<(), sqlx::Error> {
    let identity: Option<String> = sqlx::query_scalar(
        "SELECT e.identity FROM statistics_events e JOIN statistics_intents i ON i.key=e.operation_key WHERE i.journal_id=? AND e.action='removed' AND e.volume_id=i.src_volume_id AND e.path=i.src_path ORDER BY e.id LIMIT 1",
    ).bind(journal).fetch_optional(&mut *conn).await?;
    if let Some(identity) = identity {
        sqlx::query("UPDATE statistics_identities SET identity=? WHERE file_id=?")
            .bind(identity)
            .bind(file)
            .execute(conn)
            .await?;
    }
    Ok(())
}

pub(crate) async fn finish(
    conn: &mut SqliteConnection,
    journal: i64,
    volume: i64,
    metrics: &[(&str, i64, i64)],
) -> Result<(), sqlx::Error> {
    let key: String = sqlx::query_scalar("SELECT key FROM statistics_intents WHERE journal_id = ?")
        .bind(journal)
        .fetch_one(&mut *conn)
        .await?;
    let first =
        sqlx::query("INSERT INTO statistics_operations(key) VALUES(?) ON CONFLICT DO NOTHING")
            .bind(&key)
            .execute(&mut *conn)
            .await?
            .rows_affected()
            != 0;
    if first {
        for (action, count, bytes) in metrics {
            sqlx::query("INSERT INTO statistics_events(volume_id,identity,action,count,bytes,operation_key) VALUES(?,?,?,?,?,?)")
                .bind(volume).bind(&key).bind(format!("op_{action}")).bind(count).bind(bytes)
                .bind(&key).execute(&mut *conn).await?;
        }
    }
    sqlx::query("UPDATE statistics_context SET operation_key = NULL WHERE id = 1")
        .execute(conn)
        .await?;
    Ok(())
}
