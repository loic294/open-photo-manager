//! What the application knows of the assets of Immich, and the one place that reads and writes it.
//! The sync (`sync`) fills it, the `in_immich` badge of the browse projection reads it, and the
//! outbox of `reflecting-file-ops-in-immich` asks it which asset stands for a local file.
//!
//! Two tables hold the data (migrations `0110_immich_assets` and `0111_immich_sync_state`):
//!
//! - `immich_assets`: one row per asset the sync has read, live, offline or trashed, with the path
//!   Immich reports and the same path resolved through the [`PathMap`] to `(volume_id, dir_path,
//!   name)`. The join to `files` is by that triple, never by a checksum (the checksum of an
//!   external-library asset is derived from its path) and never by a stored file id, so a file
//!   that is moved or indexed again keeps its asset. Assets that were uploaded to Immich instead of
//!   being read from a library are out of scope, and so are locked assets (an API key never sees
//!   them).
//! - `immich_sync_state`: one row per scope, the part of Immich that is read in one go (see
//!   [`plan`]), with where its sync stands and what the last complete pass found.
//!
//! # Rules
//!
//! - Nothing here talks to Immich or the file system, and nothing is ever sent to Immich.
//! - Every write of the sync runs in a transaction opened by [`begin_guarded`], which first checks
//!   that the connection the run started with still exists. The single writer serialises it with
//!   the disconnect's [`clear`], so a run that was going on during a disconnect (or a disconnect
//!   and a reconnect) can never put rows back. A guarded function answers `None` or `false` then,
//!   and the run ends as cancelled without writing anything else.
//! - A write transaction carries at most [`BATCH`] rows and never waits for anything else; the
//!   writer connection is single, so nothing here calls `Db::write` or `Db::begin_write` while a
//!   transaction is open.
//!
//! # What the outbox may call
//!
//! [`find_by_location`] (which assets stand for a file, live ones first), [`forget`] (drop the rows
//! of assets that are gone from Immich), [`clear`], [`summary`], and [`begin_guarded`] for any
//! other write that must not outlive the connection. Rows describe Immich as the last pass saw it:
//! an asset read before a change the outbox made in Immich keeps its old flags until the next pass
//! reads it again, so the outbox asks [`super::ImmichSync::submit`] for a sync after it changed
//! Immich.

use std::collections::{BTreeMap, HashMap};

use opm_core::dto::{ImmichMappingDto, SyncScopeSummary, SyncSummary};
use opm_core::hash::{sha1_bytes, to_hex};
use opm_core::paths::VolumeId;
use sqlx::{QueryBuilder, Sqlite, Transaction};

use super::models::ImmichAsset;
use super::paths::PathMap;
use super::store::{self, StoredConnection};
use crate::db::{Db, DbError};

/// The most rows one write transaction and one statement carry.
const BATCH: usize = 1000;

/// The start of the key of a limited-mode scope; the mapped prefix follows.
const PATH_SCOPE: &str = "path:";

/// One asset as the table holds it, without its resolved location.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct AssetRecord {
    pub asset_id: String,
    pub library_id: Option<String>,
    /// Exactly as Immich reports it, never normalised.
    pub original_path: String,
    /// Immich found the file missing during a library scan.
    pub is_offline: bool,
    /// In Immich's trash (`isTrashed` or `deletedAt`).
    pub is_trashed: bool,
    /// Immich's `updatedAt` in epoch milliseconds; `0` when Immich sent none.
    pub updated_at_ms: i64,
}

impl AssetRecord {
    /// Present in its library and not trashed: what "in Immich" means for the badge.
    pub fn is_live(&self) -> bool {
        !self.is_offline && !self.is_trashed
    }
}

/// Where an asset's path points to in the application: a file of a mapped volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Location {
    pub volume_id: VolumeId,
    /// Same encoding as `files.dir_path`: `""` is the volume root.
    pub dir_path: String,
    pub name: String,
}

/// An asset read from Immich, ready to be stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    pub record: AssetRecord,
    /// `None` for a path outside every mapping, and for the root of a volume itself.
    pub location: Option<Location>,
}

/// Maps the path of `asset` through `map` (the only translation of Immich paths) and keeps what
/// the sync stores. Only the volumes in `volumes` (the ones that feed the scope the asset was read
/// in) count: an asset below the prefix of any other volume stays unresolved, so a scope's rows
/// depend on its own mappings and nothing else (see [`plan`]).
pub(super) fn resolve(asset: &ImmichAsset, map: &PathMap, volumes: &[VolumeId]) -> Resolved {
    let location = map
        .from_immich(&asset.original_path)
        .filter(|(volume_id, _)| volumes.contains(volume_id))
        .and_then(|(volume_id, rel)| {
            let dir_path = rel.parent()?.as_str().to_owned();
            let name = rel.file_name()?.to_owned();
            Some(Location {
                volume_id,
                dir_path,
                name,
            })
        });
    Resolved {
        record: AssetRecord {
            asset_id: asset.id.clone(),
            library_id: asset.library_id.clone(),
            original_path: asset.original_path.clone(),
            is_offline: asset.is_offline,
            is_trashed: asset.trashed(),
            updated_at_ms: asset.updated_at_ms.unwrap_or(0),
        },
        location,
    }
}

// ----- scopes ---------------------------------------------------------------------------------

/// A part of Immich that is read in one go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    /// `immich_sync_state.scope`: the library id, or `path:` and the mapped prefix.
    pub key: String,
    pub kind: ScopeKind,
    /// The volumes whose mappings feed the scope, ascending: the only ones its assets resolve to.
    pub volumes: Vec<VolumeId>,
    /// What the rows of the scope depend on (see [`plan`]). A scope whose stored fingerprint is
    /// another one has to be read in full again.
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ScopeKind {
    /// Full mode (an administrator account): every asset of one library, whichever volumes it is
    /// mapped to.
    Library { id: String, name: String },
    /// Limited mode: no library can be named, so the assets below one mapped prefix.
    Prefix { prefix: String },
}

/// The scopes the current connection and mappings call for, ordered by key.
///
/// With an administrator account a mapping that names a library feeds the scope of that library
/// (several volumes may share one); every other mapping is a scope of its own, read by path
/// prefix. The fingerprint of a scope is a hash of what its rows were resolved with: the
/// connection's server, user and mode, and the `(volume, prefix)` pairs of the mappings that feed
/// this scope. A scope resolves its assets to those volumes only ([`resolve`]), so that is all its
/// rows depend on. The fingerprint leaves out the API key (a new key for the same account changes
/// nothing) and the other scopes (editing one mapping never makes another scope read again).
pub(super) fn plan(stored: &StoredConnection, mappings: &[ImmichMappingDto]) -> Vec<Scope> {
    struct Group<'a> {
        kind: ScopeKind,
        feeds: Vec<(VolumeId, &'a str)>,
    }

    let mut groups: BTreeMap<String, Group<'_>> = BTreeMap::new();
    for mapping in mappings {
        let (key, kind) = match &mapping.library_id {
            Some(id) if stored.is_admin => (
                id.clone(),
                ScopeKind::Library {
                    id: id.clone(),
                    name: mapping.library_name.clone(),
                },
            ),
            _ => (
                format!("{PATH_SCOPE}{}", mapping.path_prefix),
                ScopeKind::Prefix {
                    prefix: mapping.path_prefix.clone(),
                },
            ),
        };
        groups
            .entry(key)
            .or_insert_with(|| Group {
                kind,
                feeds: Vec::new(),
            })
            .feeds
            .push((mapping.volume_id, mapping.path_prefix.as_str()));
    }
    groups
        .into_iter()
        .map(|(key, mut group)| {
            group.feeds.sort_unstable();
            let fingerprint = fingerprint(stored, &key, &group.feeds);
            let mut volumes: Vec<VolumeId> =
                group.feeds.iter().map(|(volume, _)| *volume).collect();
            volumes.dedup();
            Scope {
                key,
                kind: group.kind,
                volumes,
                fingerprint,
            }
        })
        .collect()
}

fn fingerprint(stored: &StoredConnection, key: &str, feeds: &[(VolumeId, &str)]) -> String {
    let mut text = format!(
        "v1\nserver={}\nuser={}\nadmin={}\nscope={key}\n",
        stored.server_url, stored.user_id, stored.is_admin
    );
    for (volume, prefix) in feeds {
        text.push_str(&format!("{volume}\t{prefix}\n"));
    }
    to_hex(&sha1_bytes(text.as_bytes()))
}

// ----- the state of a scope -------------------------------------------------------------------

/// A row of `immich_sync_state`; the migration documents the columns.
#[derive(Clone, Debug, Default, PartialEq, Eq, sqlx::FromRow)]
pub(super) struct ScopeState {
    pub scope: String,
    pub fingerprint: String,
    pub high_water_ms: i64,
    pub run: i64,
    pub last_attempt_at: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_full_at: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub assets_total: i64,
    pub assets_live: i64,
    pub assets_matched: i64,
    pub assets_unmapped: i64,
}

const SELECT_STATES: &str = "SELECT scope, fingerprint, high_water_ms, run, last_attempt_at, \
     last_sync_at, last_full_at, last_error, consecutive_failures, assets_total, assets_live, \
     assets_matched, assets_unmapped FROM immich_sync_state";

/// Every stored scope state, by scope key.
pub(super) async fn states(db: &Db) -> Result<HashMap<String, ScopeState>, DbError> {
    let rows: Vec<ScopeState> = sqlx::query_as(SELECT_STATES).fetch_all(db.read()).await?;
    Ok(rows
        .into_iter()
        .map(|state| (state.scope.clone(), state))
        .collect())
}

// ----- guarded writes -------------------------------------------------------------------------

/// Opens a write transaction, but only while the connection of `api_key_id` is the stored one:
/// `None` (nothing is written, the transaction is gone) when it was disconnected or replaced since
/// the caller loaded it. The one entry to every write that must not outlive the connection.
pub async fn begin_guarded(
    db: &Db,
    api_key_id: &str,
) -> Result<Option<Transaction<'static, Sqlite>>, DbError> {
    let mut tx = db.begin_write().await?;
    let present: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM immich_connection WHERE id = 1 AND api_key_id = ?")
            .bind(api_key_id)
            .fetch_optional(&mut *tx)
            .await?;
    if present.is_none() {
        tx.rollback().await?;
        return Ok(None);
    }
    Ok(Some(tx))
}

/// What starting a pass of a scope tells the pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PassStart {
    /// The `sync_run` of the rows this pass writes.
    pub run: i64,
    /// The rows of the scope were thrown away because what they were resolved with changed: the
    /// pass has to read everything, whatever it planned.
    pub wiped: bool,
}

/// Starts a pass of `scope` at `now`: hands out the next run number and notes the attempt. A
/// stored fingerprint that is not the scope's means the existing rows describe other mappings or
/// another account: they are deleted, and the stored fingerprint and high-water mark reset until a
/// complete full pass sets them again. `None` when the connection is gone.
pub(super) async fn begin_pass(
    db: &Db,
    api_key_id: &str,
    scope: &Scope,
    now: i64,
) -> Result<Option<PassStart>, DbError> {
    let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
        return Ok(None);
    };
    let (run, fingerprint): (i64, String) = sqlx::query_as(
        "INSERT INTO immich_sync_state (scope, run, last_attempt_at) VALUES (?, 1, ?) \
         ON CONFLICT (scope) DO UPDATE SET run = run + 1, \
         last_attempt_at = excluded.last_attempt_at \
         RETURNING run, fingerprint",
    )
    .bind(&scope.key)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    let wiped = fingerprint != scope.fingerprint;
    if wiped {
        sqlx::query(
            "UPDATE immich_sync_state SET fingerprint = '', high_water_ms = 0 WHERE scope = ?",
        )
        .bind(&scope.key)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    if wiped && !purge(db, api_key_id, &scope.key, i64::MAX).await? {
        return Ok(None);
    }
    Ok(Some(PassStart { run, wiped }))
}

/// Notes that the attempt on `scope` failed with `message` (never a secret; see
/// `ImmichError::summary`). `false` when the connection is gone.
pub(super) async fn record_failure(
    db: &Db,
    api_key_id: &str,
    scope: &str,
    message: &str,
    now: i64,
) -> Result<bool, DbError> {
    let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
        return Ok(false);
    };
    sqlx::query(
        "INSERT INTO immich_sync_state (scope, last_attempt_at, last_error, consecutive_failures) \
         VALUES (?, ?, ?, 1) \
         ON CONFLICT (scope) DO UPDATE SET last_attempt_at = excluded.last_attempt_at, \
         last_error = excluded.last_error, consecutive_failures = consecutive_failures + 1",
    )
    .bind(scope)
    .bind(now)
    .bind(message)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Stores one page of a pass of `scope` as run `run`, in one transaction per [`BATCH`] rows. An
/// asset that is already stored (whichever scope it was in) is overwritten, so a page can be
/// stored any number of times. `false` when the connection is gone.
pub(super) async fn upsert_page(
    db: &Db,
    api_key_id: &str,
    scope: &str,
    run: i64,
    rows: &[Resolved],
) -> Result<bool, DbError> {
    for chunk in rows.chunks(BATCH) {
        let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
            return Ok(false);
        };
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO immich_assets (asset_id, scope, library_id, original_path, is_offline, \
             is_trashed, updated_at, sync_run, volume_id, dir_path, name) ",
        );
        query.push_values(chunk, |mut row, item| {
            let location = item.location.as_ref();
            row.push_bind(item.record.asset_id.as_str())
                .push_bind(scope)
                .push_bind(item.record.library_id.as_deref())
                .push_bind(item.record.original_path.as_str())
                .push_bind(item.record.is_offline)
                .push_bind(item.record.is_trashed)
                .push_bind(item.record.updated_at_ms)
                .push_bind(run)
                .push_bind(location.map(|at| at.volume_id))
                .push_bind(location.map(|at| at.dir_path.as_str()))
                .push_bind(location.map(|at| at.name.as_str()));
        });
        query.push(
            " ON CONFLICT (asset_id) DO UPDATE SET scope = excluded.scope, \
             library_id = excluded.library_id, original_path = excluded.original_path, \
             is_offline = excluded.is_offline, is_trashed = excluded.is_trashed, \
             updated_at = excluded.updated_at, sync_run = excluded.sync_run, \
             volume_id = excluded.volume_id, dir_path = excluded.dir_path, name = excluded.name",
        );
        query.build().execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(true)
}

/// What a complete pass of a scope leaves behind.
#[derive(Clone, Copy, Debug)]
pub(super) struct Completion<'a> {
    pub scope: &'a Scope,
    pub run: i64,
    /// A full pass also drops the rows it did not touch and records the scope's fingerprint.
    pub full: bool,
    /// What the pass learned of Immich's clock, in epoch milliseconds (see `sync`). The stored
    /// mark only ever moves forward; a value at or below it changes nothing.
    pub high_water_ms: i64,
    /// Epoch seconds.
    pub now: i64,
}

/// Finishes a complete pass: removes what a full pass did not see, counts what the scope holds and
/// stores the counters, the high-water mark and the success. `false` when the connection is gone.
pub(super) async fn complete(
    db: &Db,
    api_key_id: &str,
    done: &Completion<'_>,
) -> Result<bool, DbError> {
    let scope = done.scope;
    if done.full && !purge(db, api_key_id, &scope.key, done.run).await? {
        return Ok(false);
    }
    let (total, live, matched, unmapped) = count(db, &scope.key).await?;

    let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE immich_sync_state SET high_water_ms = MAX(high_water_ms, ?), assets_total = ?, \
         assets_live = ?, assets_matched = ?, assets_unmapped = ?, last_sync_at = ?, \
         last_error = NULL, consecutive_failures = 0, \
         fingerprint = CASE WHEN ? THEN ? ELSE fingerprint END, \
         last_full_at = CASE WHEN ? THEN ? ELSE last_full_at END \
         WHERE scope = ?",
    )
    .bind(done.high_water_ms)
    .bind(total)
    .bind(live)
    .bind(matched)
    .bind(unmapped)
    .bind(done.now)
    .bind(done.full)
    .bind(&scope.fingerprint)
    .bind(done.full)
    .bind(done.now)
    .bind(&scope.key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// `(total, live, matched, unmapped)` of the rows of `scope`. A row is matched when it resolves to
/// a volume and the index holds a file at that path, whatever the flags of the asset (the join is
/// the one of the `in_immich` badge, which adds the flags).
async fn count(db: &Db, scope: &str) -> Result<(i64, i64, i64, i64), DbError> {
    Ok(sqlx::query_as(
        "SELECT COUNT(*), \
         COALESCE(SUM(a.is_offline = 0 AND a.is_trashed = 0), 0), \
         COALESCE(SUM(a.volume_id IS NOT NULL AND EXISTS (SELECT 1 FROM files f \
             WHERE f.volume_id = a.volume_id AND f.dir_path = a.dir_path AND f.name = a.name)), 0), \
         COALESCE(SUM(a.volume_id IS NULL), 0) \
         FROM immich_assets a WHERE a.scope = ?",
    )
    .bind(scope)
    .fetch_one(db.read())
    .await?)
}

/// Deletes the rows of `scope` written by a run older than `run_below`, [`BATCH`] rows per
/// transaction. `false` when the connection is gone.
async fn purge(db: &Db, api_key_id: &str, scope: &str, run_below: i64) -> Result<bool, DbError> {
    loop {
        let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
            return Ok(false);
        };
        let done = sqlx::query(
            "DELETE FROM immich_assets WHERE asset_id IN (SELECT asset_id FROM immich_assets \
             WHERE scope = ? AND sync_run < ? LIMIT 1000)",
        )
        .bind(scope)
        .bind(run_below)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if done.rows_affected() < BATCH as u64 {
            return Ok(true);
        }
    }
}

/// Drops the rows and the state of every scope that is not in `keep`: mappings that were removed
/// or changed leave no stale `in_immich` badge behind. `false` when the connection is gone.
pub(super) async fn retain_scopes(
    db: &Db,
    api_key_id: &str,
    keep: &[Scope],
) -> Result<bool, DbError> {
    let known: Vec<String> = sqlx::query_scalar("SELECT scope FROM immich_sync_state")
        .fetch_all(db.read())
        .await?;
    for key in known
        .iter()
        .filter(|key| !keep.iter().any(|scope| scope.key == **key))
    {
        if !purge(db, api_key_id, key, i64::MAX).await? {
            return Ok(false);
        }
        let Some(mut tx) = begin_guarded(db, api_key_id).await? else {
            return Ok(false);
        };
        sqlx::query("DELETE FROM immich_sync_state WHERE scope = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(true)
}

// ----- what the outbox and the disconnect use -------------------------------------------------

/// The assets that stand for the file `name` in the directory `dir_path` of `volume_id` (the
/// encoding of `files`: `""` is the volume root), live ones first, then by id. Several assets can
/// share a path when Immich keeps a trashed one next to a new one.
pub async fn find_by_location(
    db: &Db,
    volume_id: VolumeId,
    dir_path: &str,
    name: &str,
) -> Result<Vec<AssetRecord>, DbError> {
    Ok(sqlx::query_as(
        "SELECT asset_id, library_id, original_path, is_offline, is_trashed, \
         updated_at AS updated_at_ms FROM immich_assets \
         WHERE volume_id = ? AND dir_path = ? AND name = ? \
         ORDER BY is_offline, is_trashed, asset_id",
    )
    .bind(volume_id)
    .bind(dir_path)
    .bind(name)
    .fetch_all(db.read())
    .await?)
}

/// Deletes the rows of the assets `asset_ids` (ones that are gone from Immich), [`BATCH`] ids per
/// transaction; how many rows there were. Ids that are not stored are ignored.
pub async fn forget(db: &Db, asset_ids: &[String]) -> Result<u64, DbError> {
    let mut removed = 0;
    for chunk in asset_ids.chunks(BATCH) {
        let mut query =
            QueryBuilder::<Sqlite>::new("DELETE FROM immich_assets WHERE asset_id IN (");
        let mut ids = query.separated(", ");
        for id in chunk {
            ids.push_bind(id.as_str());
        }
        ids.push_unseparated(")");
        let mut tx = db.begin_write().await?;
        removed += query.build().execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
    }
    Ok(removed)
}

/// Deletes every asset and every scope state: what the disconnect does after it removed the
/// connection, so no running sync can write again (see [`begin_guarded`]).
pub async fn clear(db: &Db) -> Result<(), DbError> {
    loop {
        let done = sqlx::query(
            "DELETE FROM immich_assets WHERE rowid IN (SELECT rowid FROM immich_assets LIMIT 1000)",
        )
        .execute(db.write())
        .await?;
        if done.rows_affected() < BATCH as u64 {
            break;
        }
    }
    sqlx::query("DELETE FROM immich_sync_state")
        .execute(db.write())
        .await?;
    Ok(())
}

/// Lets every scope be tried at the next look of the loop, however long it was backing off: the new
/// key of a connection is what a rejected one needed. Rows, counters and the last error stay until
/// an attempt completes.
pub(super) async fn retry_now(db: &Db) -> Result<(), DbError> {
    sqlx::query("UPDATE immich_sync_state SET last_attempt_at = NULL")
        .execute(db.write())
        .await?;
    Ok(())
}

// ----- the summary ----------------------------------------------------------------------------

/// What the sync found, for the status and for the result of a sync task. `None` when no scope of
/// the current mappings has been attempted yet. Stored data only: no network.
pub async fn summary(db: &Db) -> Result<Option<SyncSummary>, DbError> {
    let Some(stored) = store::load(db).await? else {
        return Ok(None);
    };
    let mappings = store::mappings(db).await?;
    status_summary(db, &stored, &mappings).await
}

/// [`summary`] for a connection and mappings the caller has loaded already.
pub(super) async fn status_summary(
    db: &Db,
    stored: &StoredConnection,
    mappings: &[ImmichMappingDto],
) -> Result<Option<SyncSummary>, DbError> {
    let scopes = plan(stored, mappings);
    Ok(summarize(&scopes, &states(db).await?))
}

/// One row per current scope (zeros before its first attempt) and their sums. The newest
/// completed read of any scope is the time of the summary; the first failing scope names the error.
pub(super) fn summarize(
    scopes: &[Scope],
    states: &HashMap<String, ScopeState>,
) -> Option<SyncSummary> {
    if !scopes.iter().any(|scope| states.contains_key(&scope.key)) {
        return None;
    }
    let untouched = ScopeState::default();
    let rows: Vec<SyncScopeSummary> = scopes
        .iter()
        .map(|scope| {
            let state = states.get(&scope.key).unwrap_or(&untouched);
            let (library_id, library_name, path_prefix) = match &scope.kind {
                ScopeKind::Library { id, name } => (
                    Some(id.clone()),
                    (!name.is_empty()).then(|| name.clone()),
                    None,
                ),
                ScopeKind::Prefix { prefix } => (None, None, Some(prefix.clone())),
            };
            SyncScopeSummary {
                scope: scope.key.clone(),
                library_id,
                library_name,
                path_prefix,
                assets_total: state.assets_total,
                assets_live: state.assets_live,
                assets_matched: state.assets_matched,
                assets_unmapped: state.assets_unmapped,
                last_sync_at: state.last_sync_at,
                last_full_at: state.last_full_at,
                last_error: state.last_error.clone(),
                consecutive_failures: state.consecutive_failures,
            }
        })
        .collect();
    Some(SyncSummary {
        assets_total: rows.iter().map(|row| row.assets_total).sum(),
        assets_live: rows.iter().map(|row| row.assets_live).sum(),
        assets_matched: rows.iter().map(|row| row.assets_matched).sum(),
        assets_unmapped: rows.iter().map(|row| row.assets_unmapped).sum(),
        last_sync_at: rows.iter().filter_map(|row| row.last_sync_at).max(),
        last_full_at: rows.iter().filter_map(|row| row.last_full_at).max(),
        last_error: rows.iter().find_map(|row| row.last_error.clone()),
        scopes: rows,
    })
}

#[cfg(test)]
mod tests {
    use opm_core::dto::ImmichMappingEntry;

    use super::*;
    use crate::db::TempDb;
    use crate::db::testing::{insert_file, insert_volume};
    use crate::immich::store::{MappingInput, NewConnection};

    const KEY: &str = "key-1";
    const NOW: i64 = 1_700_000_000;

    fn stored(is_admin: bool) -> StoredConnection {
        StoredConnection {
            server_url: "http://immich:2283".to_owned(),
            email: "me@example.org".to_owned(),
            user_id: "user-1".to_owned(),
            user_name: "Me".to_owned(),
            is_admin,
            api_key_id: KEY.to_owned(),
            api_key_enc: "v1.encrypted".to_owned(),
            allow_insecure_tls: false,
            server_version: Some("v3.4.0".to_owned()),
            connected_at: NOW,
            last_ok_at: Some(NOW),
            last_error: None,
            key_rejected: false,
        }
    }

    fn new_connection(api_key_id: &str) -> NewConnection<'_> {
        NewConnection {
            server_url: "http://immich:2283",
            email: "me@example.org",
            user_id: "user-1",
            user_name: "Me",
            is_admin: true,
            api_key_id,
            api_key_enc: "v1.encrypted",
            allow_insecure_tls: false,
            server_version: Some("v3.4.0"),
            connected_at: NOW,
        }
    }

    fn mapping(volume_id: VolumeId, library: Option<&str>, prefix: &str) -> ImmichMappingDto {
        ImmichMappingDto {
            volume_id,
            volume_name: format!("volume {volume_id}"),
            library_id: library.map(str::to_owned),
            library_name: library
                .map(|id| format!("Library {id}"))
                .unwrap_or_default(),
            path_prefix: prefix.to_owned(),
        }
    }

    fn path_map(entries: &[(VolumeId, &str)]) -> PathMap {
        let entries: Vec<ImmichMappingEntry> = entries
            .iter()
            .map(|(volume_id, prefix)| ImmichMappingEntry {
                volume_id: *volume_id,
                library_id: None,
                path_prefix: (*prefix).to_owned(),
            })
            .collect();
        PathMap::new(&entries).unwrap()
    }

    fn immich_asset(id: &str, path: &str) -> ImmichAsset {
        ImmichAsset {
            id: id.to_owned(),
            original_path: path.to_owned(),
            library_id: Some("lib-a".to_owned()),
            updated_at_ms: Some(1_000),
            ..ImmichAsset::default()
        }
    }

    fn resolved(id: &str, at: Option<(VolumeId, &str, &str)>) -> Resolved {
        Resolved {
            record: AssetRecord {
                asset_id: id.to_owned(),
                library_id: Some("lib-a".to_owned()),
                original_path: format!("/mnt/photo/{id}"),
                is_offline: false,
                is_trashed: false,
                updated_at_ms: 5,
            },
            location: at.map(|(volume_id, dir, name)| Location {
                volume_id,
                dir_path: dir.to_owned(),
                name: name.to_owned(),
            }),
        }
    }

    fn many(volume: VolumeId, count: usize) -> Vec<Resolved> {
        (0..count)
            .map(|i| {
                resolved(
                    &format!("asset-{i:05}"),
                    Some((volume, "trip", &format!("{i:05}.jpg"))),
                )
            })
            .collect()
    }

    struct Fixture {
        temp: TempDb,
        stored: StoredConnection,
        volume: VolumeId,
    }

    impl Fixture {
        async fn new() -> Self {
            let temp = Db::open_temp().await.unwrap();
            store::replace(&temp, &new_connection(KEY)).await.unwrap();
            let stored = store::load(&temp).await.unwrap().unwrap();
            let volume = insert_volume(&temp, "photos").await;
            Self {
                temp,
                stored,
                volume,
            }
        }

        fn db(&self) -> &Db {
            &self.temp
        }

        /// The scope of the library `lib-a`, fed by the volume `photos`.
        fn scope(&self) -> Scope {
            plan(
                &self.stored,
                &[mapping(self.volume, Some("lib-a"), "/mnt/photo")],
            )
            .remove(0)
        }

        /// One complete pass of `scope` that stores `rows`; a pass that had to wipe reads in full.
        async fn run_pass(
            &self,
            scope: &Scope,
            rows: &[Resolved],
            full: bool,
            high_water_ms: i64,
            now: i64,
        ) -> PassStart {
            let start = begin_pass(self.db(), KEY, scope, now)
                .await
                .unwrap()
                .unwrap();
            assert!(
                upsert_page(self.db(), KEY, &scope.key, start.run, rows)
                    .await
                    .unwrap()
            );
            let done = Completion {
                scope,
                run: start.run,
                full: full || start.wiped,
                high_water_ms,
                now,
            };
            assert!(complete(self.db(), KEY, &done).await.unwrap());
            start
        }

        async fn state(&self, scope: &Scope) -> ScopeState {
            states(self.db()).await.unwrap().remove(&scope.key).unwrap()
        }
    }

    async fn assets_count(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM immich_assets")
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    async fn states_count(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM immich_sync_state")
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    async fn asset_ids(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT asset_id FROM immich_assets ORDER BY asset_id")
            .fetch_all(db.read())
            .await
            .unwrap()
    }

    // ----- scopes ----------------------------------------------------------------------------

    #[test]
    fn a_library_is_one_scope_whichever_volumes_are_mapped_to_it() {
        let scopes = plan(
            &stored(true),
            &[
                mapping(2, Some("lib-a"), "/mnt/b"),
                mapping(1, Some("lib-a"), "/mnt/a"),
                mapping(3, Some("lib-b"), "/mnt/c"),
            ],
        );

        let keys: Vec<&str> = scopes.iter().map(|scope| scope.key.as_str()).collect();
        assert_eq!(keys, ["lib-a", "lib-b"]);
        assert_eq!(scopes[0].volumes, [1, 2]);
        assert_eq!(scopes[1].volumes, [3]);
        assert_eq!(
            scopes[0].kind,
            ScopeKind::Library {
                id: "lib-a".to_owned(),
                name: "Library lib-a".to_owned()
            }
        );
    }

    #[test]
    fn without_an_administrator_every_prefix_is_a_scope_of_its_own() {
        // A library id means nothing to an account that may not name libraries.
        let scopes = plan(
            &stored(false),
            &[
                mapping(1, Some("lib-a"), "/mnt/a"),
                mapping(2, None, "/mnt/b"),
            ],
        );

        let keys: Vec<&str> = scopes.iter().map(|scope| scope.key.as_str()).collect();
        assert_eq!(keys, ["path:/mnt/a", "path:/mnt/b"]);
        assert_eq!(
            scopes[0].kind,
            ScopeKind::Prefix {
                prefix: "/mnt/a".to_owned()
            }
        );
        assert_eq!(scopes[1].volumes, [2]);
    }

    #[test]
    fn a_mapping_without_a_library_is_read_by_prefix_even_for_an_administrator() {
        let scopes = plan(&stored(true), &[mapping(1, None, "/mnt/a")]);

        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].key, "path:/mnt/a");
    }

    #[test]
    fn no_mapping_is_no_scope() {
        assert!(plan(&stored(true), &[]).is_empty());
    }

    #[test]
    fn the_fingerprint_names_what_the_rows_were_resolved_with() {
        let fingerprint_of =
            |stored: &StoredConnection, mappings: &[ImmichMappingDto], key: &str| {
                plan(stored, mappings)
                    .into_iter()
                    .find(|scope| scope.key == key)
                    .unwrap()
                    .fingerprint
            };
        let base = [
            mapping(1, Some("lib-a"), "/mnt/a"),
            mapping(3, Some("lib-b"), "/mnt/c"),
        ];
        let original = fingerprint_of(&stored(true), &base, "lib-a");

        let prefix_moved = [mapping(1, Some("lib-a"), "/mnt/z")];
        let volume_swapped = [mapping(2, Some("lib-a"), "/mnt/a")];
        let feed_added = [
            mapping(1, Some("lib-a"), "/mnt/a"),
            mapping(2, Some("lib-a"), "/mnt/b"),
        ];
        for changed in [&prefix_moved[..], &volume_swapped[..], &feed_added[..]] {
            assert_ne!(fingerprint_of(&stored(true), changed, "lib-a"), original);
        }
        let other_server = StoredConnection {
            server_url: "http://other:2283".to_owned(),
            ..stored(true)
        };
        let other_user = StoredConnection {
            user_id: "user-2".to_owned(),
            ..stored(true)
        };
        assert_ne!(fingerprint_of(&other_server, &base, "lib-a"), original);
        assert_ne!(fingerprint_of(&other_user, &base, "lib-a"), original);

        // The key and the health of the connection do not matter, nor do the other scopes.
        let new_key = StoredConnection {
            api_key_id: "key-2".to_owned(),
            last_error: Some("down".to_owned()),
            key_rejected: true,
            ..stored(true)
        };
        assert_eq!(fingerprint_of(&new_key, &base, "lib-a"), original);
        let alone = [mapping(1, Some("lib-a"), "/mnt/a")];
        assert_eq!(fingerprint_of(&stored(true), &alone, "lib-a"), original);
        let neighbour_moved = [
            mapping(1, Some("lib-a"), "/mnt/a"),
            mapping(3, Some("lib-b"), "/mnt/elsewhere"),
        ];
        assert_eq!(
            fingerprint_of(&stored(true), &neighbour_moved, "lib-a"),
            original
        );

        // Being an administrator changes how an asset is found, so it changes the rows.
        let by_prefix = |stored: &StoredConnection| {
            fingerprint_of(stored, &[mapping(1, None, "/mnt/a")], "path:/mnt/a")
        };
        assert_ne!(by_prefix(&stored(true)), by_prefix(&stored(false)));
    }

    // ----- resolving -------------------------------------------------------------------------

    #[test]
    fn a_path_resolves_to_the_file_below_its_prefix() {
        let map = path_map(&[(1, "/mnt/photo"), (2, "/mnt/other")]);

        let nested = resolve(
            &immich_asset("a", "/mnt/photo/trips/2024/IMG_1.jpg"),
            &map,
            &[1],
        );
        assert_eq!(
            nested.location,
            Some(Location {
                volume_id: 1,
                dir_path: "trips/2024".to_owned(),
                name: "IMG_1.jpg".to_owned()
            })
        );
        assert_eq!(
            nested.record.original_path,
            "/mnt/photo/trips/2024/IMG_1.jpg"
        );
        assert_eq!(nested.record.library_id.as_deref(), Some("lib-a"));
        assert_eq!(nested.record.updated_at_ms, 1_000);

        let at_the_root = resolve(&immich_asset("b", "/mnt/photo/IMG_2.jpg"), &map, &[1]);
        assert_eq!(
            at_the_root.location,
            Some(Location {
                volume_id: 1,
                dir_path: String::new(),
                name: "IMG_2.jpg".to_owned()
            })
        );
    }

    #[test]
    fn a_path_that_leads_nowhere_stays_unresolved_but_is_kept() {
        let map = path_map(&[(1, "/mnt/photo"), (2, "/mnt/other")]);

        for path in [
            "/mnt/photos/x.jpg",
            "/elsewhere/x.jpg",
            "/mnt/photo",
            "relative/x.jpg",
        ] {
            let resolved = resolve(&immich_asset("a", path), &map, &[1, 2]);
            assert_eq!(resolved.location, None, "{path}");
            assert_eq!(resolved.record.original_path, path);
        }
        // A volume that does not feed the scope is not a place for its assets.
        let foreign = resolve(&immich_asset("a", "/mnt/other/x.jpg"), &map, &[1]);
        assert_eq!(foreign.location, None);
    }

    #[test]
    fn the_flags_come_from_whichever_field_immich_filled() {
        let map = path_map(&[(1, "/mnt/photo")]);
        let path = "/mnt/photo/a.jpg";
        let record = |asset: ImmichAsset| resolve(&asset, &map, &[1]).record;

        assert!(record(immich_asset("a", path)).is_live());
        let offline = record(ImmichAsset {
            is_offline: true,
            ..immich_asset("a", path)
        });
        assert!(offline.is_offline && !offline.is_trashed && !offline.is_live());
        let trashed = record(ImmichAsset {
            is_trashed: true,
            ..immich_asset("a", path)
        });
        assert!(!trashed.is_offline && trashed.is_trashed && !trashed.is_live());
        let deleted = record(ImmichAsset {
            deleted_at_ms: Some(5),
            ..immich_asset("a", path)
        });
        assert!(deleted.is_trashed && !deleted.is_live());
        let no_clock = record(ImmichAsset {
            updated_at_ms: None,
            ..immich_asset("a", path)
        });
        assert_eq!(no_clock.updated_at_ms, 0);
    }

    #[test]
    fn names_are_carried_byte_for_byte() {
        let map = path_map(&[(1, "/mnt/photo")]);

        let at = resolve(
            &immich_asset("a", "/mnt/photo/Cafe\u{301}/re\u{301}sume\u{301}.jpg"),
            &map,
            &[1],
        )
        .location
        .unwrap();

        assert_eq!(at.dir_path, "Cafe\u{301}");
        assert_eq!(at.name, "re\u{301}sume\u{301}.jpg");
    }

    // ----- the guard -------------------------------------------------------------------------

    #[tokio::test]
    async fn a_guarded_transaction_exists_only_for_the_stored_connection() {
        let fx = Fixture::new().await;

        let tx = begin_guarded(fx.db(), KEY).await.unwrap().unwrap();
        tx.rollback().await.unwrap();
        assert!(
            begin_guarded(fx.db(), "another-key")
                .await
                .unwrap()
                .is_none()
        );

        // A connection with another key replaced it: the run of the old key is out.
        store::replace(fx.db(), &new_connection("key-2"))
            .await
            .unwrap();
        assert!(begin_guarded(fx.db(), KEY).await.unwrap().is_none());
        let tx = begin_guarded(fx.db(), "key-2").await.unwrap().unwrap();
        tx.rollback().await.unwrap();

        store::delete(fx.db()).await.unwrap();
        assert!(begin_guarded(fx.db(), "key-2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn nothing_is_written_once_the_connection_is_gone() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        let start = begin_pass(fx.db(), KEY, &scope, NOW)
            .await
            .unwrap()
            .unwrap();
        let rows = many(fx.volume, 3);
        assert!(
            upsert_page(fx.db(), KEY, &scope.key, start.run, &rows)
                .await
                .unwrap()
        );

        store::delete(fx.db()).await.unwrap();
        clear(fx.db()).await.unwrap();

        assert!(
            !upsert_page(fx.db(), KEY, &scope.key, start.run, &rows)
                .await
                .unwrap()
        );
        assert!(
            !record_failure(fx.db(), KEY, &scope.key, "down", NOW)
                .await
                .unwrap()
        );
        let done = Completion {
            scope: &scope,
            run: start.run,
            full: true,
            high_water_ms: 9,
            now: NOW,
        };
        assert!(!complete(fx.db(), KEY, &done).await.unwrap());
        assert!(
            begin_pass(fx.db(), KEY, &scope, NOW)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(assets_count(fx.db()).await, 0);
        assert_eq!(states_count(fx.db()).await, 0);
    }

    // ----- pages -----------------------------------------------------------------------------

    #[tokio::test]
    async fn a_page_is_stored_in_batches_and_again_without_duplicates() {
        let fx = Fixture::new().await;
        let rows = many(fx.volume, 2_500);

        assert!(upsert_page(fx.db(), KEY, "lib-a", 1, &rows).await.unwrap());
        assert!(upsert_page(fx.db(), KEY, "lib-a", 2, &rows).await.unwrap());

        let (count, oldest_run): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), MIN(sync_run) FROM immich_assets")
                .fetch_one(fx.db().read())
                .await
                .unwrap();
        assert_eq!((count, oldest_run), (2_500, 2));
    }

    #[tokio::test]
    async fn an_asset_that_changes_scope_path_or_flags_is_still_one_row() {
        let fx = Fixture::new().await;
        let first = resolved("a1", Some((fx.volume, "trip", "a.jpg")));
        let mut later = resolved("a1", Some((fx.volume, "moved", "b.jpg")));
        later.record.is_offline = true;
        later.record.updated_at_ms = 9;

        assert!(
            upsert_page(fx.db(), KEY, "lib-a", 1, &[first])
                .await
                .unwrap()
        );
        assert!(
            upsert_page(fx.db(), KEY, "lib-b", 4, &[later])
                .await
                .unwrap()
        );

        let row: (String, i64, String, String, bool, i64) = sqlx::query_as(
            "SELECT scope, sync_run, dir_path, name, is_offline, updated_at FROM immich_assets",
        )
        .fetch_one(fx.db().read())
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "lib-b".to_owned(),
                4,
                "moved".to_owned(),
                "b.jpg".to_owned(),
                true,
                9
            )
        );
    }

    #[tokio::test]
    async fn an_unresolved_asset_keeps_its_row_without_a_location() {
        let fx = Fixture::new().await;

        upsert_page(fx.db(), KEY, "lib-a", 1, &[resolved("far", None)])
            .await
            .unwrap();

        let row: (Option<i64>, Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT volume_id, dir_path, name, original_path FROM immich_assets")
                .fetch_one(fx.db().read())
                .await
                .unwrap();
        assert_eq!(row, (None, None, None, "/mnt/photo/far".to_owned()));
    }

    #[tokio::test]
    async fn the_assets_of_a_location_come_live_first_and_names_are_byte_exact() {
        let fx = Fixture::new().await;
        let at = Some((fx.volume, "trip", "a.jpg"));
        let mut offline = resolved("b-offline", at);
        offline.record.is_offline = true;
        let mut trashed = resolved("a-trashed", at);
        trashed.record.is_trashed = true;
        let rows = [
            offline,
            trashed,
            resolved("z-live", at),
            resolved("nfc", Some((fx.volume, "", "\u{e9}.jpg"))),
            resolved("nfd", Some((fx.volume, "", "e\u{301}.jpg"))),
        ];
        upsert_page(fx.db(), KEY, "lib-a", 1, &rows).await.unwrap();

        let found = find_by_location(fx.db(), fx.volume, "trip", "a.jpg")
            .await
            .unwrap();

        let ids: Vec<&str> = found.iter().map(|asset| asset.asset_id.as_str()).collect();
        assert_eq!(ids, ["z-live", "a-trashed", "b-offline"]);
        assert!(found[0].is_live() && !found[1].is_live() && !found[2].is_live());
        assert_eq!(found[0].library_id.as_deref(), Some("lib-a"));
        assert_eq!(found[0].updated_at_ms, 5);
        let at_other_places = [
            find_by_location(fx.db(), fx.volume + 1, "trip", "a.jpg").await,
            find_by_location(fx.db(), fx.volume, "", "a.jpg").await,
            find_by_location(fx.db(), fx.volume, "trip", "A.jpg").await,
        ];
        for found in at_other_places {
            assert!(found.unwrap().is_empty());
        }
        let nfc = find_by_location(fx.db(), fx.volume, "", "\u{e9}.jpg")
            .await
            .unwrap();
        let nfd = find_by_location(fx.db(), fx.volume, "", "e\u{301}.jpg")
            .await
            .unwrap();
        assert_eq!(nfc.len(), 1);
        assert_eq!(nfc[0].asset_id, "nfc");
        assert_eq!(nfd.len(), 1);
        assert_eq!(nfd[0].asset_id, "nfd");
    }

    #[tokio::test]
    async fn forgetting_assets_removes_their_rows_only() {
        let fx = Fixture::new().await;
        let rows = many(fx.volume, 2_500);
        upsert_page(fx.db(), KEY, "lib-a", 1, &rows).await.unwrap();
        let mut gone: Vec<String> = rows[..2_100]
            .iter()
            .map(|row| row.record.asset_id.clone())
            .collect();
        gone.push("never-stored".to_owned());

        assert_eq!(forget(fx.db(), &gone).await.unwrap(), 2_100);
        assert_eq!(forget(fx.db(), &gone).await.unwrap(), 0);
        assert_eq!(forget(fx.db(), &[]).await.unwrap(), 0);

        let left = asset_ids(fx.db()).await;
        assert_eq!(left.len(), 400);
        assert_eq!(left[0], "asset-02100");
    }

    #[tokio::test]
    async fn clearing_empties_both_tables() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        fx.run_pass(&scope, &many(fx.volume, 1_500), true, 7, NOW)
            .await;
        assert_eq!(assets_count(fx.db()).await, 1_500);
        assert_eq!(states_count(fx.db()).await, 1);

        clear(fx.db()).await.unwrap();

        assert_eq!(assets_count(fx.db()).await, 0);
        assert_eq!(states_count(fx.db()).await, 0);
    }

    // ----- passes ----------------------------------------------------------------------------

    #[tokio::test]
    async fn the_first_pass_of_a_scope_starts_clean_and_a_full_one_completes_it() {
        let fx = Fixture::new().await;
        let scope = fx.scope();

        let first = begin_pass(fx.db(), KEY, &scope, NOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first,
            PassStart {
                run: 1,
                wiped: true
            }
        );
        let attempted = fx.state(&scope).await;
        assert_eq!(attempted.fingerprint, "");
        assert_eq!(attempted.last_attempt_at, Some(NOW));
        assert_eq!(attempted.last_sync_at, None);

        let rows = many(fx.volume, 3);
        upsert_page(fx.db(), KEY, &scope.key, first.run, &rows)
            .await
            .unwrap();
        let done = Completion {
            scope: &scope,
            run: first.run,
            full: true,
            high_water_ms: 5_000,
            now: NOW + 1,
        };
        assert!(complete(fx.db(), KEY, &done).await.unwrap());

        let state = fx.state(&scope).await;
        assert_eq!(state.fingerprint, scope.fingerprint);
        assert_eq!(state.high_water_ms, 5_000);
        assert_eq!(state.last_sync_at, Some(NOW + 1));
        assert_eq!(state.last_full_at, Some(NOW + 1));
        assert_eq!(state.last_error, None);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.assets_total, 3);
        let second = begin_pass(fx.db(), KEY, &scope, NOW + 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            second,
            PassStart {
                run: 2,
                wiped: false
            }
        );
        assert_eq!(assets_count(fx.db()).await, 3);
    }

    #[tokio::test]
    async fn the_counters_tell_apart_live_matched_and_unmapped_assets() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        insert_file(fx.db(), fx.volume, "trip", "live.jpg").await;
        insert_file(fx.db(), fx.volume, "trip", "offline.jpg").await;
        let mut offline = resolved("offline", Some((fx.volume, "trip", "offline.jpg")));
        offline.record.is_offline = true;
        let mut trashed = resolved("trashed", Some((fx.volume, "trip", "trashed.jpg")));
        trashed.record.is_trashed = true;
        let rows = [
            resolved("live", Some((fx.volume, "trip", "live.jpg"))),
            offline,
            trashed,
            resolved("no-file", Some((fx.volume, "trip", "no-file.jpg"))),
            resolved("far", None),
        ];

        fx.run_pass(&scope, &rows, true, 1, NOW).await;

        let state = fx.state(&scope).await;
        assert_eq!(
            (
                state.assets_total,
                state.assets_live,
                state.assets_matched,
                state.assets_unmapped
            ),
            (5, 3, 2, 1)
        );
    }

    #[tokio::test]
    async fn a_full_pass_drops_what_it_did_not_see_and_an_incremental_one_does_not() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        let all = many(fx.volume, 2_500);
        fx.run_pass(&scope, &all, true, 5_000, NOW).await;
        assert_eq!(assets_count(fx.db()).await, 2_500);

        // Incremental: touches two rows, drops none, and does not move the full-pass marks.
        fx.run_pass(&scope, &all[..2], false, 100, NOW + 10).await;
        let state = fx.state(&scope).await;
        assert_eq!(assets_count(fx.db()).await, 2_500);
        assert_eq!(state.high_water_ms, 5_000);
        assert_eq!(state.last_sync_at, Some(NOW + 10));
        assert_eq!(state.last_full_at, Some(NOW));

        // Full: whatever it did not see no longer exists in Immich, however many rows that is.
        fx.run_pass(&scope, &all[..10], true, 6_000, NOW + 20).await;
        let state = fx.state(&scope).await;
        assert_eq!(assets_count(fx.db()).await, 10);
        assert_eq!(state.assets_total, 10);
        assert_eq!(state.high_water_ms, 6_000);
        assert_eq!(state.last_full_at, Some(NOW + 20));
    }

    #[tokio::test]
    async fn a_scope_only_drops_its_own_rows() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        upsert_page(
            fx.db(),
            KEY,
            "lib-b",
            1,
            &[resolved("of-b", Some((fx.volume, "other", "b.jpg")))],
        )
        .await
        .unwrap();

        fx.run_pass(&scope, &many(fx.volume, 2), true, 1, NOW).await;
        fx.run_pass(&scope, &[], true, 2, NOW + 1).await;

        assert_eq!(asset_ids(fx.db()).await, ["of-b"]);
    }

    #[tokio::test]
    async fn the_high_water_mark_never_moves_back() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        fx.run_pass(&scope, &many(fx.volume, 1), true, 9_000, NOW)
            .await;

        fx.run_pass(&scope, &many(fx.volume, 1), false, 4_000, NOW + 1)
            .await;
        assert_eq!(fx.state(&scope).await.high_water_ms, 9_000);

        fx.run_pass(&scope, &many(fx.volume, 1), false, 12_000, NOW + 2)
            .await;
        assert_eq!(fx.state(&scope).await.high_water_ms, 12_000);
    }

    #[tokio::test]
    async fn a_pass_whose_fingerprint_changed_wipes_the_scope_and_its_mark() {
        let fx = Fixture::new().await;
        let scope = fx.scope();
        upsert_page(
            fx.db(),
            KEY,
            "lib-b",
            1,
            &[resolved("of-b", Some((fx.volume, "other", "b.jpg")))],
        )
        .await
        .unwrap();
        fx.run_pass(&scope, &many(fx.volume, 5), true, 5_000, NOW)
            .await;

        let moved = Scope {
            fingerprint: "another mapping".to_owned(),
            ..scope.clone()
        };
        let start = begin_pass(fx.db(), KEY, &moved, NOW + 5)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            start,
            PassStart {
                run: 2,
                wiped: true
            }
        );
        assert_eq!(asset_ids(fx.db()).await, ["of-b"]);
        let state = fx.state(&scope).await;
        assert_eq!(state.fingerprint, "");
        assert_eq!(state.high_water_ms, 0);
        assert_eq!(state.last_sync_at, Some(NOW));
    }

    #[tokio::test]
    async fn failures_are_counted_until_a_pass_completes() {
        let fx = Fixture::new().await;
        let scope = fx.scope();

        assert!(
            record_failure(fx.db(), KEY, &scope.key, "Immich is unreachable", NOW)
                .await
                .unwrap()
        );
        assert!(
            record_failure(fx.db(), KEY, &scope.key, "Immich said no", NOW + 60)
                .await
                .unwrap()
        );

        let failed = fx.state(&scope).await;
        assert_eq!(failed.consecutive_failures, 2);
        assert_eq!(failed.last_error.as_deref(), Some("Immich said no"));
        assert_eq!(failed.last_attempt_at, Some(NOW + 60));
        assert_eq!(failed.last_sync_at, None);

        fx.run_pass(&scope, &many(fx.volume, 1), true, 1, NOW + 120)
            .await;
        let healed = fx.state(&scope).await;
        assert_eq!(healed.consecutive_failures, 0);
        assert_eq!(healed.last_error, None);
        assert_eq!(healed.last_sync_at, Some(NOW + 120));
    }

    #[tokio::test]
    async fn scopes_that_are_no_longer_current_leave_nothing_behind() {
        let fx = Fixture::new().await;
        let kept = fx.scope();
        let other = Scope {
            key: "lib-b".to_owned(),
            ..kept.clone()
        };
        fx.run_pass(&kept, &many(fx.volume, 2), true, 1, NOW).await;
        let others = [resolved("of-b", Some((fx.volume, "other", "b.jpg")))];
        fx.run_pass(&other, &others, true, 1, NOW).await;
        assert_eq!(assets_count(fx.db()).await, 3);

        assert!(
            retain_scopes(fx.db(), KEY, std::slice::from_ref(&kept))
                .await
                .unwrap()
        );

        assert_eq!(states_count(fx.db()).await, 1);
        assert_eq!(asset_ids(fx.db()).await, ["asset-00000", "asset-00001"]);
        assert!(
            retain_scopes(fx.db(), KEY, std::slice::from_ref(&kept))
                .await
                .unwrap()
        );
        assert!(retain_scopes(fx.db(), KEY, &[]).await.unwrap());
        assert_eq!(assets_count(fx.db()).await, 0);
        assert_eq!(states_count(fx.db()).await, 0);
    }

    // ----- the summary -----------------------------------------------------------------------

    #[test]
    fn the_summary_is_empty_until_a_current_scope_was_attempted() {
        let scopes = plan(&stored(true), &[mapping(1, Some("lib-a"), "/mnt/a")]);
        assert_eq!(summarize(&scopes, &HashMap::new()), None);

        // The state of a scope that is no longer current says nothing about the current ones.
        let stale = HashMap::from([(
            "lib-gone".to_owned(),
            ScopeState {
                scope: "lib-gone".to_owned(),
                ..ScopeState::default()
            },
        )]);
        assert_eq!(summarize(&scopes, &stale), None);
    }

    #[test]
    fn the_summary_adds_up_the_scopes_and_names_the_first_failure() {
        let scopes = plan(
            &stored(true),
            &[
                mapping(1, Some("lib-a"), "/mnt/a"),
                mapping(2, Some("lib-b"), "/mnt/b"),
                mapping(3, None, "/mnt/c"),
            ],
        );
        let states = HashMap::from([
            (
                "lib-a".to_owned(),
                ScopeState {
                    scope: "lib-a".to_owned(),
                    assets_total: 10,
                    assets_live: 8,
                    assets_matched: 6,
                    assets_unmapped: 2,
                    last_sync_at: Some(100),
                    last_full_at: Some(90),
                    ..ScopeState::default()
                },
            ),
            (
                "lib-b".to_owned(),
                ScopeState {
                    scope: "lib-b".to_owned(),
                    assets_total: 5,
                    assets_live: 5,
                    assets_matched: 5,
                    last_sync_at: Some(200),
                    last_error: Some("boom".to_owned()),
                    consecutive_failures: 3,
                    ..ScopeState::default()
                },
            ),
        ]);

        let summary = summarize(&scopes, &states).unwrap();

        assert_eq!(
            (
                summary.assets_total,
                summary.assets_live,
                summary.assets_matched,
                summary.assets_unmapped
            ),
            (15, 13, 11, 2)
        );
        assert_eq!(summary.last_sync_at, Some(200));
        assert_eq!(summary.last_full_at, Some(90));
        assert_eq!(summary.last_error.as_deref(), Some("boom"));
        let keys: Vec<&str> = summary
            .scopes
            .iter()
            .map(|row| row.scope.as_str())
            .collect();
        assert_eq!(keys, ["lib-a", "lib-b", "path:/mnt/c"]);
        assert_eq!(
            summary.scopes[0].library_name.as_deref(),
            Some("Library lib-a")
        );
        assert_eq!(summary.scopes[1].consecutive_failures, 3);
        let untouched = &summary.scopes[2];
        assert_eq!(untouched.path_prefix.as_deref(), Some("/mnt/c"));
        assert_eq!(untouched.library_id, None);
        assert_eq!((untouched.assets_total, untouched.last_sync_at), (0, None));
    }

    #[tokio::test]
    async fn the_summary_reads_the_stored_state_for_the_current_mappings() {
        let fx = Fixture::new().await;
        assert_eq!(summary(fx.db()).await.unwrap(), None);
        store::replace_mappings(
            fx.db(),
            &[MappingInput {
                volume_id: fx.volume,
                library_id: Some("lib-a".to_owned()),
                library_name: "Photos".to_owned(),
                path_prefix: "/mnt/photo".to_owned(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(summary(fx.db()).await.unwrap(), None);

        fx.run_pass(&fx.scope(), &many(fx.volume, 4), true, 1, NOW)
            .await;

        let found = summary(fx.db()).await.unwrap().unwrap();
        assert_eq!(found.assets_total, 4);
        assert_eq!(found.last_sync_at, Some(NOW));
        assert_eq!(found.scopes.len(), 1);
        assert_eq!(found.scopes[0].library_name.as_deref(), Some("Photos"));

        // Other mappings are other scopes: the old state is not theirs.
        store::replace_mappings(fx.db(), &[]).await.unwrap();
        assert_eq!(summary(fx.db()).await.unwrap(), None);

        store::delete(fx.db()).await.unwrap();
        assert_eq!(summary(fx.db()).await.unwrap(), None);
    }
}
