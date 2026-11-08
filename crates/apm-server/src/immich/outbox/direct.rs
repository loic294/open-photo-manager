//! Executor preflight, durable direct-move intents and their worker.
//! The hook below is SQLite-only. The worker never requests a scan for a tracked
//! in-place move and never falls back to deleting/reimporting its asset.

use apm_core::dto::OutboxState;
use apm_core::paths::RelPath;
use sqlx::SqliteConnection;

use super::decide::Disk;
use super::merge::Place;
use super::probe::probe_exact_file;
use super::store::{self, Patch, Row};
use crate::db::{Db, now_secs};
use crate::fsops::Loc;
use crate::immich::direct_db::{DbMoveError, MoveSpec, PreparedMove, SidecarMove};
use crate::immich::{ImmichService, assets};
use crate::indexer::VolumeRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePreflight {
    /// The local synced index has no tracked asset. This does not discover locked
    /// assets the API key cannot see; mappings/sync must be kept current.
    Untracked,
    /// A move outside mapped libraries retains the offline-confirmed API delete.
    ApiRemoval,
    DirectReady,
}

/// Called once before this executor admits new preflights. Orphan preparations
/// have no disk journal to recover; pending journals retain their exact intent.
/// Completed proof remains until the corresponding journal is actually pruned.
pub(super) async fn cleanup_at_start(db: &Db) -> Result<(), crate::db::DbError> {
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "DELETE FROM immich_direct_move WHERE (queued = 0 OR queued = 2)
        AND NOT EXISTS(SELECT 1 FROM fs_journal j
            WHERE j.src_volume_id = immich_direct_move.old_volume_id
            AND j.src_path = immich_direct_move.old_rel
            AND j.dst_volume_id = immich_direct_move.new_volume_id
            AND j.dst_path = immich_direct_move.new_rel
            AND ((immich_direct_move.queued = 0 AND j.state = 'pending')
                OR (immich_direct_move.queued = 2 AND j.id = immich_direct_move.journal_id
                    AND j.created_at = immich_direct_move.journal_created_at)))",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn preflight(
    db: &Db,
    immich: &ImmichService,
    src: &Loc,
    dst: &Loc,
    sidecars: &[(Loc, Loc)],
) -> Result<MovePreflight, DbMoveError> {
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM immich_direct_move WHERE queued = 1
        AND new_volume_id = ? AND new_rel = ?)",
    )
    .bind(src.volume_id)
    .bind(src.rel.as_str())
    .fetch_one(db.read())
    .await
    .map_err(|_| DbMoveError::Unavailable)?;
    if pending {
        return Err(DbMoveError::PendingUpdate);
    }
    let records = assets::find_by_location(db, src.volume_id, src.dir().as_str(), src.name())
        .await
        .map_err(|_| DbMoveError::Unavailable)?;
    if records.is_empty() {
        return Ok(MovePreflight::Untracked);
    }
    if records.len() != 1 {
        return Err(DbMoveError::Collision);
    }
    let pending_asset: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM immich_direct_move d
        WHERE json_extract(d.request_json, '$.spec.asset_id') = ?
        AND (d.queued = 1 OR (d.queued = 0 AND EXISTS(SELECT 1 FROM fs_journal j
            WHERE j.state = 'pending' AND j.src_volume_id = d.old_volume_id
            AND j.src_path = d.old_rel AND j.dst_volume_id = d.new_volume_id
            AND j.dst_path = d.new_rel))))")
        .bind(&records[0].asset_id).fetch_one(db.read()).await
        .map_err(|_| DbMoveError::Unavailable)?;
    if pending_asset { return Err(DbMoveError::PendingUpdate); }
    let file_id: i64 = sqlx::query_scalar(
        "SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?")
        .bind(src.volume_id).bind(src.dir().as_str()).bind(src.name())
        .fetch_optional(db.read()).await.map_err(|_| DbMoveError::Unavailable)?
        .ok_or(DbMoveError::Stale)?;
    let map = immich.path_map().await.map_err(|_| DbMoveError::Identity)?;
    let Some(new) = map.to_immich(dst.volume_id, &dst.rel) else {
        return Ok(MovePreflight::ApiRemoval);
    };
    let old = map
        .to_immich(src.volume_id, &src.rel)
        .ok_or(DbMoveError::Identity)?;
    let library = map
        .library_for(src.volume_id)
        .ok_or(DbMoveError::Identity)?;
    if map.library_for(dst.volume_id) != Some(library) {
        return Err(DbMoveError::Destination);
    }
    immich.probe_move_database().await?;
    let connection = super::super::store::load(db)
        .await
        .map_err(|_| DbMoveError::Unavailable)?
        .ok_or(DbMoveError::Identity)?;
    let client = immich
        .client()
        .await
        .map_err(|_| DbMoveError::Unavailable)?;
    let asset = client
        .get_asset(&records[0].asset_id)
        .await
        .map_err(|_| DbMoveError::Unavailable)?
        .ok_or(DbMoveError::Stale)?;
    if asset.original_path != old {
        return Err(DbMoveError::Stale);
    }
    let sidecars = sidecars
        .iter()
        .map(|(from, to)| {
            Ok(SidecarMove {
                old: map
                    .to_immich(from.volume_id, &from.rel)
                    .ok_or(DbMoveError::Sidecar)?,
                new: map
                    .to_immich(to.volume_id, &to.rel)
                    .ok_or(DbMoveError::Sidecar)?,
            })
        })
        .collect::<Result<Vec<_>, DbMoveError>>()?;
    let prepared = immich
        .move_database()
        .prepare(MoveSpec {
            asset_id: asset.id,
            owner_id: connection.user_id,
            library_id: library.to_owned(),
            old,
            new,
            new_name: dst.name().to_owned(),
            sidecars,
        })
        .await?;
    let json = serde_json::to_string(&prepared).map_err(|_| DbMoveError::Configuration)?;
    let mut tx = db
        .begin_write()
        .await
        .map_err(|_| DbMoveError::Unavailable)?;
    let changed = sqlx::query(
        "INSERT INTO immich_direct_move
        (old_volume_id, old_rel, new_volume_id, new_rel, request_json, prepared_at, file_id)
        VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(old_volume_id, old_rel, new_volume_id, new_rel)
        DO UPDATE SET request_json = excluded.request_json, prepared_at = excluded.prepared_at,
        file_id = excluded.file_id, journal_id = NULL, journal_created_at = NULL,
        queued = 0, outbox_id = NULL, sidecar_confirmed = 0 WHERE immich_direct_move.queued <> 1",
    )
    .bind(src.volume_id)
    .bind(src.rel.as_str())
    .bind(dst.volume_id)
    .bind(dst.rel.as_str())
    .bind(json)
    .bind(now_secs())
    .bind(file_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DbMoveError::Unavailable)?;
    if changed.rows_affected() != 1 {
        return Err(DbMoveError::PendingUpdate);
    }
    tx.commit().await.map_err(|_| DbMoveError::Unavailable)?;
    Ok(MovePreflight::DirectReady)
}

pub(super) async fn settled(
    db: &Db,
    from: &Loc,
    to: &Loc,
    identical: bool,
) -> Result<(), crate::db::DbError> {
    let mut tx = db.begin_write().await?;
    if identical {
        confirm_sidecar(&mut tx, from, to).await?;
    }
    sqlx::query(
        "DELETE FROM immich_direct_move WHERE old_volume_id = ? AND old_rel = ?
        AND new_volume_id = ? AND new_rel = ? AND queued = 0
        AND NOT EXISTS(SELECT 1 FROM fs_journal j WHERE j.state = 'pending'
        AND j.src_volume_id = immich_direct_move.old_volume_id
        AND j.src_path = immich_direct_move.old_rel
        AND j.dst_volume_id = immich_direct_move.new_volume_id
        AND j.dst_path = immich_direct_move.new_rel)",
    )
    .bind(from.volume_id)
    .bind(from.rel.as_str())
    .bind(to.volume_id)
    .bind(to.rel.as_str())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn confirm_sidecar(
    tx: &mut SqliteConnection,
    from: &Loc,
    to: &Loc,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE immich_direct_move SET sidecar_confirmed = 1 WHERE queued = 1
        AND json_extract(request_json, '$.sidecar.old') =
        (SELECT rtrim(path_prefix, '/') || '/' || ? FROM immich_volume_map WHERE volume_id = ?)
        AND json_extract(request_json, '$.sidecar.new') =
        (SELECT rtrim(path_prefix, '/') || '/' || ? FROM immich_volume_map WHERE volume_id = ?)",
    )
    .bind(from.rel.as_str())
    .bind(from.volume_id)
    .bind(to.rel.as_str())
    .bind(to.volume_id)
    .execute(tx)
    .await?;
    Ok(())
}

/// Idempotent local hook, called before generic queue/merge. Returns true when
/// this operation has a direct intent, including a replay of a completed one.
pub(super) async fn queue(
    tx: &mut SqliteConnection,
    file_id: i64,
    from: &Loc,
    to: &Loc,
) -> Result<bool, sqlx::Error> {
    let intent: Option<(String, i64, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT request_json, queued, file_id, journal_id, journal_created_at FROM immich_direct_move WHERE
        old_volume_id = ? AND old_rel = ? AND new_volume_id = ? AND new_rel = ?",
    )
    .bind(from.volume_id)
    .bind(from.rel.as_str())
    .bind(to.volume_id)
    .bind(to.rel.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((json, queued, prepared_file_id, recorded_journal, recorded_time)) = intent else {
        return Ok(false);
    };
    if queued != 0 && prepared_file_id != file_id {
        return Ok(false);
    }
    let (journal, journal_created_at): (i64, i64) = sqlx::query_as(
        "SELECT id, created_at FROM fs_journal WHERE src_volume_id = ? AND src_path = ?
        AND dst_volume_id = ? AND dst_path = ? AND op = 'move' AND state = 'done'
        ORDER BY id DESC LIMIT 1")
        .bind(from.volume_id).bind(from.rel.as_str()).bind(to.volume_id).bind(to.rel.as_str())
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| sqlx::Error::Protocol("A direct move is missing its completed filesystem journal".to_owned()))?;
    if queued != 0 {
        return Ok(recorded_journal == Some(journal) && recorded_time == Some(journal_created_at));
    };
    let now = now_secs();
    let outbox_id: i64 = sqlx::query_scalar(
        "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name,
        new_volume_id, new_dir, new_name, stage, next_attempt_at, created_at, last_error)
        VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 'database_update_pending') RETURNING id",
    )
    .bind(file_id)
    .bind(from.volume_id)
    .bind(from.dir().as_str())
    .bind(from.name())
    .bind(to.volume_id)
    .bind(to.dir().as_str())
    .bind(to.name())
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE immich_direct_move SET queued = 1, outbox_id = ?, journal_id = ?, file_id = ?,
        journal_created_at = ?
        WHERE request_json = ? AND old_volume_id = ? AND old_rel = ?
        AND new_volume_id = ? AND new_rel = ?",
    )
    .bind(outbox_id)
    .bind(journal)
    .bind(file_id)
    .bind(journal_created_at)
    .bind(json)
    .bind(from.volume_id)
    .bind(from.rel.as_str())
    .bind(to.volume_id)
    .bind(to.rel.as_str())
    .execute(tx)
    .await?;
    Ok(true)
}

pub(super) async fn request(db: &Db, id: i64) -> Result<Option<String>, crate::db::DbError> {
    Ok(sqlx::query_scalar(
        "SELECT request_json FROM immich_direct_move WHERE outbox_id = ? AND queued = 1",
    )
    .bind(id)
    .fetch_optional(db.read())
    .await?)
}

async fn path_disk(volumes: &VolumeRegistry, immich: &ImmichService, path: &str) -> Disk {
    let Ok(map) = immich.path_map().await else {
        return Disk::Unknown;
    };
    let Some((volume_id, rel)) = map.from_immich(path) else {
        return Disk::Unknown;
    };
    let Some(name) = rel.file_name() else {
        return Disk::Unknown;
    };
    probe_exact_file(
        volumes,
        &Place::new(
            volume_id,
            rel.parent().unwrap_or_else(RelPath::root).as_str(),
            name,
        ),
    )
    .await
}

pub(super) async fn apply(
    db: &Db,
    immich: &ImmichService,
    volumes: &VolumeRegistry,
    row: &Row,
    json: &str,
) -> Result<bool, crate::db::DbError> {
    let result = match serde_json::from_str::<PreparedMove>(json) {
        Err(_) => Err(DbMoveError::Configuration),
        Ok(prepared) => {
            let old = path_disk(volumes, immich, &prepared.spec.old).await;
            let new = path_disk(volumes, immich, &prepared.spec.new).await;
            let paths_match = old == Disk::Absent && new == Disk::Present;
            let sidecar_matches = match &prepared.sidecar {
                Some(moved) => {
                    let confirmed: bool = sqlx::query_scalar(
                        "SELECT sidecar_confirmed FROM immich_direct_move WHERE outbox_id = ?",
                    )
                    .bind(row.id)
                    .fetch_one(db.read())
                    .await?;
                    confirmed && path_disk(volumes, immich, &moved.new).await == Disk::Present
                }
                None => true,
            };
            if old == Disk::Unknown || new == Disk::Unknown {
                Err(DbMoveError::Unavailable)
            } else if !paths_match {
                Err(DbMoveError::Stale)
            } else if !sidecar_matches {
                Err(DbMoveError::SidecarPending)
            } else {
                match immich.probe_move_database().await {
                    Ok(()) => immich.move_database().apply(&prepared).await.map(|_| ()),
                    Err(error) => Err(error),
                }
            }
        }
    };
    match result {
        Ok(()) => {
            // Retain proof for journal replay. SQLite failure leaves the outbox row:
            // the next remote attempt is idempotent, including ambiguous commits.
            let mut tx = db.begin_write().await?;
            sqlx::query(
                "UPDATE immich_assets SET volume_id = ?, dir_path = ?, name = ?,
                original_path = json_extract(?, '$.spec.new')
                WHERE asset_id = json_extract(?, '$.spec.asset_id')",
            )
            .bind(row.effect.new.as_ref().map(|at| at.volume_id))
            .bind(row.effect.new.as_ref().map(|at| at.dir.as_str()))
            .bind(row.effect.new.as_ref().map(|at| at.name.as_str()))
            .bind(json)
            .bind(json)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE immich_direct_move SET queued = 2, outbox_id = NULL WHERE outbox_id = ?")
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM immich_outbox WHERE id = ?")
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(true)
        }
        Err(error) => {
            let message = error.queued_warning();
            tracing::warn!(row = row.id, error = %message, "Immich direct path update is unconfirmed");
            store::apply(
                db,
                &[(
                    row.clone(),
                    Patch {
                        attempts: Some(row.attempts + 1),
                        next_attempt_at: Some(
                            now_secs() + super::consts::row_backoff_secs(row.attempts),
                        ),
                        last_error: Some(Some(message)),
                        // Never abandon transient DB failures. Permanent refusals stay
                        // durably visible with an explicit Retry/Dismiss decision.
                        state: Some(
                            if matches!(
                                error,
                                DbMoveError::Unavailable | DbMoveError::SidecarPending
                            ) {
                                OutboxState::Pending
                            } else {
                                OutboxState::Attention
                            },
                        ),
                        ..Patch::default()
                    },
                )],
                &[],
            )
            .await?;
            Ok(false)
        }
    }
}
