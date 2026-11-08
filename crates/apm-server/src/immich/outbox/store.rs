//! Every statement on `immich_outbox` and `immich_outbox_state`: the rest of the outbox never
//! writes SQL, so the indexes the queries rely on are all in one file (migration
//! `0120_immich_outbox.sql` documents the columns).
//!
//! Two callers with opposite needs share it. The `FsOps` hook runs INSIDE the transaction of a
//! file operation, so [`queue`] takes the connection of that transaction and costs one or two
//! small statements. The pass reads and updates rows by the hundred, so its writes are batched
//! per chunk ([`apply`]).

use apm_core::dto::{OutboxItemDto, OutboxKind, OutboxPage, OutboxState, OutboxSummary};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection};

use super::consts::{ID_CHUNK, MAX_ERROR_CHARS};
use super::merge::{Effect, Merged, Place, merge};
use crate::db::{Db, DbError};

/// What [`queue`] did with a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    /// Immich is not connected, or neither volume is mapped: nothing to reflect.
    Ignored,
    /// A new row.
    Inserted,
    /// The change folded into the row that was waiting.
    Merged,
    /// Nothing is left to reflect (the change undid the waiting one): no row.
    Dropped,
}

/// The row [`queue`] found waiting for the file, if any. Every column is nullable because the
/// statement is a left join: relevance is decided by the same statement.
#[derive(FromRow)]
struct Waiting {
    id: Option<i64>,
    old_volume_id: Option<i64>,
    old_dir: Option<String>,
    old_name: Option<String>,
    new_volume_id: Option<i64>,
    new_dir: Option<String>,
    new_name: Option<String>,
}

/// Decides whether the change is relevant (Immich is connected and the old or the new volume is
/// mapped: no row comes back otherwise) and returns the row waiting for the file in the same
/// statement. `stage = 0 AND state = 'pending'` is the WHERE clause of the partial unique index
/// the lookup runs on. A macro because the statements need a string literal (the tests prefix it
/// with `EXPLAIN QUERY PLAN`).
macro_rules! find_waiting_sql {
    () => {
        "SELECT o.id, o.old_volume_id, o.old_dir, o.old_name, \
         o.new_volume_id, o.new_dir, o.new_name \
         FROM (SELECT 1) AS one \
         LEFT JOIN immich_outbox o ON o.file_id = ?1 AND o.stage = 0 AND o.state = 'pending' \
         WHERE EXISTS (SELECT 1 FROM immich_connection WHERE id = 1) \
         AND EXISTS (SELECT 1 FROM immich_volume_map WHERE volume_id IN (?2, ?3))"
    };
}

/// Records that the file `key` changed as `incoming` says, merging it into the change that is
/// still waiting for the file (see `merge`). `key` is the id of the file's row, or a negative
/// number that cannot collide with one when the file has none.
///
/// Runs on the connection of the file operation's transaction. The only errors are those of a
/// statement; every other outcome is a [`Queued`] value.
pub(super) async fn queue(
    conn: &mut SqliteConnection,
    key: i64,
    incoming: &Effect,
    now: i64,
) -> Result<Queued, sqlx::Error> {
    let (first, second) = match (&incoming.old, &incoming.new) {
        (Some(old), Some(new)) => (old.volume_id, new.volume_id),
        (Some(only), None) | (None, Some(only)) => (only.volume_id, only.volume_id),
        (None, None) => return Ok(Queued::Ignored),
    };
    let found: Option<Waiting> = sqlx::query_as(find_waiting_sql!())
        .bind(key)
        .bind(first)
        .bind(second)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(found) = found else {
        return Ok(Queued::Ignored);
    };
    let waiting_id = found.id;
    let waiting = waiting_id.map(|_| Effect {
        old: place(found.old_volume_id, found.old_dir, found.old_name),
        new: place(found.new_volume_id, found.new_dir, found.new_name),
    });
    match (merge(waiting.as_ref(), incoming), waiting_id) {
        (Merged::Drop, Some(id)) => {
            sqlx::query("DELETE FROM immich_outbox WHERE id = ?")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            Ok(Queued::Dropped)
        }
        (Merged::Drop, None) => Ok(Queued::Dropped),
        (Merged::Replace(effect), Some(id)) => {
            sqlx::query(
                "UPDATE immich_outbox SET old_volume_id = ?, old_dir = ?, old_name = ?, \
                 new_volume_id = ?, new_dir = ?, new_name = ?, next_attempt_at = ?, \
                 created_at = ?, last_error = NULL WHERE id = ?",
            )
            .bind(effect.old.as_ref().map(|p| p.volume_id))
            .bind(effect.old.as_ref().map(|p| p.dir.as_str()))
            .bind(effect.old.as_ref().map(|p| p.name.as_str()))
            .bind(effect.new.as_ref().map(|p| p.volume_id))
            .bind(effect.new.as_ref().map(|p| p.dir.as_str()))
            .bind(effect.new.as_ref().map(|p| p.name.as_str()))
            .bind(now)
            .bind(now)
            .bind(id)
            .execute(&mut *conn)
            .await?;
            Ok(Queued::Merged)
        }
        (Merged::Replace(effect), None) => {
            sqlx::query(
                "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, \
                 new_volume_id, new_dir, new_name, next_attempt_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(key)
            .bind(effect.old.as_ref().map(|p| p.volume_id))
            .bind(effect.old.as_ref().map(|p| p.dir.as_str()))
            .bind(effect.old.as_ref().map(|p| p.name.as_str()))
            .bind(effect.new.as_ref().map(|p| p.volume_id))
            .bind(effect.new.as_ref().map(|p| p.dir.as_str()))
            .bind(effect.new.as_ref().map(|p| p.name.as_str()))
            .bind(now)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            Ok(Queued::Inserted)
        }
    }
}

fn place(volume: Option<i64>, dir: Option<String>, name: Option<String>) -> Option<Place> {
    Some(Place {
        volume_id: volume?,
        dir: dir?,
        name: name?,
    })
}

// ----- what a pass works on ---------------------------------------------------------------------

/// A row as a pass reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub id: i64,
    pub effect: Effect,
    pub asset_id: Option<String>,
    pub new_asset_id: Option<String>,
    pub snapshot_json: Option<String>,
    pub stage: i64,
    pub carried: bool,
    pub attempts: i64,
    pub offline_seen_at: Option<i64>,
    pub scan_requested_at: Option<i64>,
    /// When the latest change of the row was recorded: a merge records it again, nothing else
    /// does. A library scan asked for at or after it covers the row.
    pub created_at: i64,
}

impl Row {
    /// The moment the age of the row counts from: the scan request, else the latest change.
    pub fn age_base(&self) -> i64 {
        self.scan_requested_at.unwrap_or(self.created_at)
    }
}

#[derive(FromRow)]
struct RawRow {
    id: i64,
    old_volume_id: Option<i64>,
    old_dir: Option<String>,
    old_name: Option<String>,
    new_volume_id: Option<i64>,
    new_dir: Option<String>,
    new_name: Option<String>,
    asset_id: Option<String>,
    new_asset_id: Option<String>,
    snapshot_json: Option<String>,
    stage: i64,
    carried: i64,
    attempts: i64,
    offline_seen_at: Option<i64>,
    scan_requested_at: Option<i64>,
    created_at: i64,
}

impl From<RawRow> for Row {
    fn from(raw: RawRow) -> Self {
        Self {
            id: raw.id,
            effect: Effect {
                old: place(raw.old_volume_id, raw.old_dir, raw.old_name),
                new: place(raw.new_volume_id, raw.new_dir, raw.new_name),
            },
            asset_id: raw.asset_id,
            new_asset_id: raw.new_asset_id,
            snapshot_json: raw.snapshot_json,
            stage: raw.stage,
            carried: raw.carried != 0,
            attempts: raw.attempts,
            offline_seen_at: raw.offline_seen_at,
            scan_requested_at: raw.scan_requested_at,
            created_at: raw.created_at,
        }
    }
}

/// The columns of [`RawRow`]; a macro because the statements need a string literal.
macro_rules! row_columns {
    () => {
        "id, old_volume_id, old_dir, old_name, new_volume_id, new_dir, new_name, asset_id, \
         new_asset_id, snapshot_json, stage, carried, attempts, offline_seen_at, \
         scan_requested_at, created_at"
    };
}

/// The pending rows that are due at `now`, the longest waiting first, at most `limit`. Served by
/// `immich_outbox_due`.
macro_rules! due_sql {
    () => {
        concat!(
            "SELECT ",
            row_columns!(),
            " FROM immich_outbox WHERE state = 'pending' AND next_attempt_at <= ? \
             ORDER BY next_attempt_at, id LIMIT ?"
        )
    };
}

/// The pending rows that are due at `now`, the longest waiting first, at most `limit`.
pub(super) async fn due(db: &Db, now: i64, limit: usize) -> Result<Vec<Row>, DbError> {
    let rows: Vec<RawRow> = sqlx::query_as(due_sql!())
        .bind(now)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(db.read())
        .await?;
    Ok(rows.into_iter().map(Row::from).collect())
}

/// Every row, whatever its state, by id.
#[cfg(test)]
pub(super) async fn all(db: &Db) -> Result<Vec<Row>, DbError> {
    let rows: Vec<RawRow> = sqlx::query_as(concat!(
        "SELECT ",
        row_columns!(),
        " FROM immich_outbox ORDER BY id"
    ))
    .fetch_all(db.read())
    .await?;
    Ok(rows.into_iter().map(Row::from).collect())
}

/// The earliest `next_attempt_at` among the pending rows, due or not; `None` when no row is
/// pending. The scheduler starts a pass once it has passed and sleeps until it comes.
pub(super) async fn next_due(db: &Db) -> Result<Option<i64>, DbError> {
    Ok(
        sqlx::query_scalar(
            "SELECT MIN(next_attempt_at) FROM immich_outbox WHERE state = 'pending'",
        )
        .fetch_one(db.read())
        .await?,
    )
}

/// Whether any row still waits (pending, due or not).
pub(super) async fn any_pending(db: &Db) -> Result<bool, DbError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM immich_outbox WHERE state = 'pending')")
            .fetch_one(db.read())
            .await?,
    )
}

/// What a pass changes on one row. `None` leaves a column alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Patch {
    pub asset_id: Option<String>,
    pub new_asset_id: Option<String>,
    pub snapshot_json: Option<String>,
    pub stage: Option<i64>,
    pub carried: Option<bool>,
    pub offline_seen_at: Option<i64>,
    pub scan_requested_at: Option<i64>,
    pub next_attempt_at: Option<i64>,
    pub attempts: Option<i64>,
    /// `Some(None)` clears the error.
    pub last_error: Option<Option<String>>,
    pub state: Option<OutboxState>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Try again at `next_attempt_at` with `attempts` tries behind it.
    #[cfg(test)]
    pub fn retry(attempts: i64, next_attempt_at: i64, error: Option<&str>) -> Self {
        Self {
            attempts: Some(attempts),
            next_attempt_at: Some(next_attempt_at),
            last_error: Some(error.map(clip)),
            ..Self::default()
        }
    }

    /// A human has to look at the row: it is no longer worked on.
    pub fn attention(attempts: i64, error: &str) -> Self {
        Self {
            attempts: Some(attempts),
            last_error: Some(Some(clip(error))),
            state: Some(OutboxState::Attention),
            ..Self::default()
        }
    }
}

/// `text` cut to what a row keeps.
pub(super) fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_ERROR_CHARS) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// Applies what a pass learned about a chunk of rows, in one transaction: the `patches` and the
/// rows that are `done` (deleted). A change to a row that changed while the pass worked on it is
/// skipped: a stage-0 row is merged into by the hook until its scan is requested, so its latest
/// change is the guard (see `unchanged`). A skipped row keeps what the hook made of it and is
/// worked on by the next pass. The ids of the rows whose change was skipped come back.
pub(super) async fn apply(
    db: &Db,
    patches: &[(Row, Patch)],
    done: &[Row],
) -> Result<Vec<i64>, DbError> {
    if patches.is_empty() && done.is_empty() {
        return Ok(Vec::new());
    }
    let mut skipped = Vec::new();
    let mut tx = db.begin_write().await?;
    for (row, patch) in patches {
        if patch.is_empty() {
            continue;
        }
        if !patch.apply(&mut tx, row).await? {
            skipped.push(row.id);
        }
    }
    let (merged_into, settled): (Vec<&Row>, Vec<&Row>) =
        done.iter().partition(|row| row.stage == 0);
    for row in merged_into {
        let mut query = QueryBuilder::<Sqlite>::new("DELETE FROM immich_outbox");
        unchanged(&mut query, row);
        if query.build().execute(&mut *tx).await?.rows_affected() == 0 {
            skipped.push(row.id);
        }
    }
    for chunk in settled.chunks(ID_CHUNK) {
        let mut query = QueryBuilder::<Sqlite>::new("DELETE FROM immich_outbox WHERE id IN (");
        let mut ids = query.separated(", ");
        for row in chunk {
            ids.push_bind(row.id);
        }
        ids.push_unseparated(")");
        query.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(skipped)
}

/// ` WHERE id = row.id`, and for a stage-0 row that it is still the row the pass read: the hook
/// only ever changes a row that has not requested its scan yet, and a change moves its new place
/// and its `created_at` (the place alone would not tell a move to B and back to B apart).
fn unchanged(query: &mut QueryBuilder<Sqlite>, row: &Row) {
    query.push(" WHERE id = ").push_bind(row.id);
    if row.stage == 0 {
        let new = row.effect.new.as_ref();
        query
            .push(" AND stage = 0 AND created_at = ")
            .push_bind(row.created_at)
            .push(" AND new_volume_id IS ")
            .push_bind(new.map(|p| p.volume_id))
            .push(" AND new_dir IS ")
            .push_bind(new.map(|p| p.dir.clone()))
            .push(" AND new_name IS ")
            .push_bind(new.map(|p| p.name.clone()));
    }
}

impl Patch {
    /// Whether the row was still the one the pass read.
    async fn apply(&self, conn: &mut SqliteConnection, row: &Row) -> Result<bool, sqlx::Error> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE immich_outbox SET ");
        let mut set = query.separated(", ");
        if let Some(value) = &self.asset_id {
            set.push("asset_id = ")
                .push_bind_unseparated(value.as_str());
        }
        if let Some(value) = &self.new_asset_id {
            set.push("new_asset_id = ")
                .push_bind_unseparated(value.as_str());
        }
        if let Some(value) = &self.snapshot_json {
            set.push("snapshot_json = ")
                .push_bind_unseparated(value.as_str());
        }
        if let Some(value) = self.stage {
            set.push("stage = ").push_bind_unseparated(value);
        }
        if let Some(value) = self.carried {
            set.push("carried = ").push_bind_unseparated(value);
        }
        if let Some(value) = self.offline_seen_at {
            set.push("offline_seen_at = ").push_bind_unseparated(value);
        }
        if let Some(value) = self.scan_requested_at {
            set.push("scan_requested_at = ")
                .push_bind_unseparated(value);
        }
        if let Some(value) = self.next_attempt_at {
            set.push("next_attempt_at = ").push_bind_unseparated(value);
        }
        if let Some(value) = self.attempts {
            set.push("attempts = ").push_bind_unseparated(value);
        }
        if let Some(value) = &self.last_error {
            set.push("last_error = ")
                .push_bind_unseparated(value.as_deref());
        }
        if let Some(value) = self.state {
            set.push("state = ")
                .push_bind_unseparated(state_name(value));
        }
        unchanged(&mut query, row);
        let done = query.build().execute(&mut *conn).await?;
        Ok(done.rows_affected() > 0)
    }
}

fn state_name(state: OutboxState) -> &'static str {
    match state {
        OutboxState::Pending => "pending",
        OutboxState::Attention => "attention",
    }
}

// ----- the pass as a whole ----------------------------------------------------------------------

/// What the latest passes did, as `immich_outbox_state` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq, FromRow)]
pub(super) struct PassState {
    pub last_run_at: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
}

pub(super) async fn pass_state(db: &Db) -> Result<PassState, DbError> {
    Ok(sqlx::query_as(
        "SELECT last_run_at, last_error, consecutive_failures FROM immich_outbox_state WHERE id = 1",
    )
    .fetch_optional(db.read())
    .await?
    .unwrap_or_default())
}

/// A pass ended without a whole-pass failure: the back-off is over.
pub(super) async fn record_success(db: &Db, now: i64) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE immich_outbox_state SET last_run_at = ?, last_error = NULL, \
         consecutive_failures = 0 WHERE id = 1",
    )
    .bind(now)
    .execute(db.write())
    .await?;
    Ok(())
}

/// A pass failed as a whole: its rows stay as they were and the next pass waits longer.
pub(super) async fn record_failure(db: &Db, now: i64, error: &str) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar(
        "UPDATE immich_outbox_state SET last_run_at = ?, last_error = ?, \
         consecutive_failures = consecutive_failures + 1 WHERE id = 1 \
         RETURNING consecutive_failures",
    )
    .bind(now)
    .bind(clip(error))
    .fetch_optional(db.write())
    .await?
    .unwrap_or(1))
}

/// Forgets the back-off of failed passes (a person asked to try now).
pub(super) async fn reset_failures(db: &Db) -> Result<(), DbError> {
    sqlx::query("UPDATE immich_outbox_state SET consecutive_failures = 0 WHERE id = 1")
        .execute(db.write())
        .await?;
    Ok(())
}

// ----- what the routes and the status show ------------------------------------------------------

/// The counts of the status. `scans_on_demand`: whether this account can ask Immich to scan.
pub(super) async fn summary(db: &Db, scans_on_demand: bool) -> Result<OutboxSummary, DbError> {
    let (pending, waiting, attention, oldest): (i64, i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT COALESCE(SUM(state = 'pending'), 0), \
         COALESCE(SUM(state = 'pending' AND stage = 1), 0), \
         COALESCE(SUM(state = 'attention'), 0), \
         MIN(CASE WHEN state = 'pending' THEN created_at END) FROM immich_outbox",
    )
    .fetch_one(db.read())
    .await?;
    let state = pass_state(db).await?;
    Ok(OutboxSummary {
        pending,
        waiting_for_immich: waiting,
        attention,
        oldest_pending_at: oldest,
        last_run_at: state.last_run_at,
        last_error: state.last_error,
        scans_on_demand,
    })
}

#[derive(FromRow)]
struct ListedRow {
    id: i64,
    old_volume_id: Option<i64>,
    old_dir: Option<String>,
    old_name: Option<String>,
    old_volume: Option<String>,
    new_volume_id: Option<i64>,
    new_dir: Option<String>,
    new_name: Option<String>,
    new_volume: Option<String>,
    state: String,
    attempts: i64,
    last_error: Option<String>,
    created_at: i64,
    next_attempt_at: i64,
}

/// One page of rows, oldest first, optionally of one state.
pub(super) async fn list(
    db: &Db,
    state: Option<OutboxState>,
    offset: i64,
    limit: i64,
) -> Result<OutboxPage, DbError> {
    let filter = state.map(state_name);
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM immich_outbox WHERE (?1 IS NULL OR state = ?1)")
            .bind(filter)
            .fetch_one(db.read())
            .await?;
    let rows: Vec<ListedRow> = sqlx::query_as(
        "SELECT o.id, o.old_volume_id, o.old_dir, o.old_name, ov.name AS old_volume, \
         o.new_volume_id, o.new_dir, o.new_name, nv.name AS new_volume, \
         o.state, o.attempts, o.last_error, o.created_at, o.next_attempt_at \
         FROM immich_outbox o \
         LEFT JOIN volumes ov ON ov.id = o.old_volume_id \
         LEFT JOIN volumes nv ON nv.id = o.new_volume_id \
         WHERE (?1 IS NULL OR o.state = ?1) ORDER BY o.id LIMIT ?2 OFFSET ?3",
    )
    .bind(filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(db.read())
    .await?;
    let items = rows.into_iter().map(item_of).collect();
    Ok(OutboxPage { items, total })
}

fn item_of(row: ListedRow) -> OutboxItemDto {
    let from = display(row.old_volume_id, row.old_volume, row.old_dir, row.old_name);
    let to = display(row.new_volume_id, row.new_volume, row.new_dir, row.new_name);
    let kind = match (&from, &to) {
        (Some(_), None) => OutboxKind::Removed,
        (None, _) => OutboxKind::Added,
        (Some(_), Some(_)) => OutboxKind::Moved,
    };
    OutboxItemDto {
        id: row.id,
        kind,
        from,
        to,
        state: if row.state == "attention" {
            OutboxState::Attention
        } else {
            OutboxState::Pending
        },
        attempts: row.attempts,
        last_error: row.last_error,
        created_at: row.created_at,
        next_attempt_at: row.next_attempt_at,
    }
}

/// `volume/folder/file`, without the folder at the volume root; `#id` for a volume that is gone.
fn display(
    volume_id: Option<i64>,
    volume: Option<String>,
    dir: Option<String>,
    name: Option<String>,
) -> Option<String> {
    let (volume_id, name) = (volume_id?, name?);
    let volume = volume.unwrap_or_else(|| format!("#{volume_id}"));
    Some(match dir.as_deref() {
        None | Some("") => format!("{volume}/{name}"),
        Some(dir) => format!("{volume}/{dir}/{name}"),
    })
}

/// Makes rows due now: the `attention` ones go back to `pending` with a fresh count of attempts,
/// the pending ones that wait for a back-off are tried at once. `ids` limits it to some rows. A
/// stage-0 row whose file already has another waiting row stays as it is (the partial unique
/// index refuses the second one, and `OR IGNORE` skips it). Returns how many rows changed.
pub(super) async fn retry(db: &Db, ids: Option<&[i64]>, now: i64) -> Result<u64, DbError> {
    let mut changed = 0;
    match ids {
        None => {
            changed += retry_statement(now, None)
                .build()
                .execute(db.write())
                .await?
                .rows_affected();
        }
        Some(ids) => {
            for chunk in ids.chunks(ID_CHUNK) {
                changed += retry_statement(now, Some(chunk))
                    .build()
                    .execute(db.write())
                    .await?
                    .rows_affected();
            }
        }
    }
    Ok(changed)
}

fn retry_statement(now: i64, ids: Option<&[i64]>) -> QueryBuilder<Sqlite> {
    let mut query = QueryBuilder::new(
        "UPDATE OR IGNORE immich_outbox SET state = 'pending', attempts = 0, \
         last_error = NULL, next_attempt_at = ",
    );
    query.push_bind(now);
    query.push(" WHERE (state = 'attention' OR next_attempt_at > ");
    query.push_bind(now);
    query.push(")");
    if let Some(ids) = ids {
        query.push(" AND id IN (");
        let mut list = query.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
        list.push_unseparated(")");
    }
    query
}

/// Deletes the rows `ids`; how many there were.
pub(super) async fn dismiss(db: &Db, ids: &[i64]) -> Result<u64, DbError> {
    let mut removed = 0;
    for chunk in ids.chunks(ID_CHUNK) {
        let mut query = QueryBuilder::<Sqlite>::new("DELETE FROM immich_outbox WHERE id IN (");
        let mut list = query.separated(", ");
        for id in chunk {
            list.push_bind(*id);
        }
        list.push_unseparated(")");
        removed += query.build().execute(db.write()).await?.rows_affected();
    }
    Ok(removed)
}

/// Forgets everything: what a disconnect does, since the queue only means something for the
/// server it was queued for.
pub(super) async fn clear(db: &Db) -> Result<(), DbError> {
    let mut tx = db.begin_write().await?;
    sqlx::query("DELETE FROM immich_outbox")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE immich_outbox_state SET last_run_at = NULL, last_error = NULL, \
         consecutive_failures = 0 WHERE id = 1",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::immich::outbox::test_support::Fixture;

    fn at(volume: i64, dir: &str, name: &str) -> Place {
        Place::new(volume, dir, name)
    }

    /// The columns of a row a test sets; the others take their defaults.
    struct Seed<'a> {
        file_id: i64,
        old: Option<&'a Place>,
        new: Option<&'a Place>,
        stage: i64,
        state: &'a str,
        next_attempt_at: i64,
        created_at: i64,
    }

    impl<'a> Seed<'a> {
        fn moved(file_id: i64, old: &'a Place, new: &'a Place) -> Self {
            Self {
                file_id,
                old: Some(old),
                new: Some(new),
                stage: 0,
                state: "pending",
                next_attempt_at: 100,
                created_at: 100,
            }
        }

        fn removed(file_id: i64, old: &'a Place) -> Self {
            Self {
                new: None,
                ..Self::moved(file_id, old, old)
            }
        }

        fn added(file_id: i64, new: &'a Place) -> Self {
            Self {
                old: None,
                ..Self::moved(file_id, new, new)
            }
        }

        fn stage(self, stage: i64) -> Self {
            Self { stage, ..self }
        }

        fn state(self, state: &'a str) -> Self {
            Self { state, ..self }
        }

        fn due(self, next_attempt_at: i64) -> Self {
            Self {
                next_attempt_at,
                ..self
            }
        }

        fn created(self, created_at: i64) -> Self {
            Self { created_at, ..self }
        }
    }

    async fn insert(fx: &Fixture, seed: Seed<'_>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, \
             new_volume_id, new_dir, new_name, stage, state, next_attempt_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(seed.file_id)
        .bind(seed.old.map(|p| p.volume_id))
        .bind(seed.old.map(|p| p.dir.as_str()))
        .bind(seed.old.map(|p| p.name.as_str()))
        .bind(seed.new.map(|p| p.volume_id))
        .bind(seed.new.map(|p| p.dir.as_str()))
        .bind(seed.new.map(|p| p.name.as_str()))
        .bind(seed.stage)
        .bind(seed.state)
        .bind(seed.next_attempt_at)
        .bind(seed.created_at)
        .fetch_one(fx.db.write())
        .await
        .unwrap()
    }

    async fn queue_now(fx: &Fixture, key: i64, effect: &Effect, now: i64) -> Queued {
        let mut tx = fx.db.begin_write().await.unwrap();
        let queued = queue(&mut tx, key, effect, now).await.unwrap();
        tx.commit().await.unwrap();
        queued
    }

    async fn plan_of(fx: &Fixture, statement: &'static str, binds: &[i64]) -> String {
        let mut query = sqlx::query_as::<_, (i64, i64, i64, String)>(statement);
        for bind in binds {
            query = query.bind(*bind);
        }
        let rows = query.fetch_all(fx.db.read()).await.unwrap();
        rows.into_iter()
            .map(|row| row.3)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ----- the indexes the queries rely on ------------------------------------------------------

    #[tokio::test]
    async fn the_merge_lookup_runs_on_the_partial_unique_index() {
        let fx = Fixture::connected().await;
        let plan = plan_of(
            &fx,
            concat!("EXPLAIN QUERY PLAN ", find_waiting_sql!()),
            &[1, 1, 2],
        )
        .await;
        assert!(plan.contains("immich_outbox_merge"), "{plan}");
        let scans_the_table = plan
            .lines()
            .any(|line| line == "SCAN o" || line.starts_with("SCAN immich_outbox"));
        assert!(!scans_the_table, "{plan}");
    }

    #[tokio::test]
    async fn the_due_query_runs_on_the_due_index() {
        let fx = Fixture::connected().await;
        let plan = plan_of(&fx, concat!("EXPLAIN QUERY PLAN ", due_sql!()), &[100, 10]).await;
        assert!(plan.contains("immich_outbox_due"), "{plan}");
        let plan = plan_of(
            &fx,
            "EXPLAIN QUERY PLAN SELECT MIN(next_attempt_at) FROM immich_outbox \
             WHERE state = 'pending'",
            &[],
        )
        .await;
        assert!(plan.contains("immich_outbox_due"), "{plan}");
    }

    /// The statistics `ANALYZE` keeps for a queue of the size of a big move must not tip the
    /// planner to a scan: the lookup of every operation and the read of a pass stay on their
    /// indexes (`tests/immich_outbox_perf.rs` times them at that size).
    #[tokio::test]
    async fn the_plans_hold_on_a_queue_of_fifty_thousand_rows() {
        let fx = Fixture::connected().await;
        fx.mapped_volume("photos").await;
        fx.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000) \
             INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, \
             new_volume_id, new_dir, new_name, state, next_attempt_at, created_at) \
             SELECT i, 1, 'a', 'x' || i || '.jpg', 1, 'b', 'x' || i || '.jpg', \
             CASE WHEN i % 10 = 0 THEN 'attention' ELSE 'pending' END, i % 600, 0 FROM n",
        )
        .await;
        fx.execute("ANALYZE").await;

        let merge = plan_of(
            &fx,
            concat!("EXPLAIN QUERY PLAN ", find_waiting_sql!()),
            &[25_000, 1, 2],
        )
        .await;
        assert!(merge.contains("immich_outbox_merge"), "{merge}");
        let due = plan_of(
            &fx,
            concat!("EXPLAIN QUERY PLAN ", due_sql!()),
            &[300, 5_000],
        )
        .await;
        assert!(due.contains("immich_outbox_due"), "{due}");
        for plan in [&merge, &due] {
            assert!(
                !plan
                    .lines()
                    .any(|line| line == "SCAN o" || line.starts_with("SCAN immich_outbox")),
                "{plan}"
            );
        }
        assert!(
            !due.contains("USE TEMP B-TREE"),
            "the pass reads in the order of the index: {due}"
        );
    }

    // ----- the schema's own rules ---------------------------------------------------------------

    #[tokio::test]
    async fn a_side_is_all_or_nothing_and_a_row_has_a_side() {
        let fx = Fixture::connected().await;
        for statement in [
            // A partial old side.
            "INSERT INTO immich_outbox (file_id, old_volume_id, new_volume_id, new_dir, new_name, \
             next_attempt_at, created_at) VALUES (1, 1, 1, '', 'a.jpg', 0, 0)",
            // A partial new side.
            "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, new_volume_id, \
             next_attempt_at, created_at) VALUES (1, 1, '', 'a.jpg', 1, 0, 0)",
            // No side at all.
            "INSERT INTO immich_outbox (file_id, next_attempt_at, created_at) VALUES (2, 0, 0)",
            // Not JSON.
            "INSERT INTO immich_outbox (file_id, new_volume_id, new_dir, new_name, snapshot_json, \
             next_attempt_at, created_at) VALUES (3, 1, '', 'a.jpg', 'not json', 0, 0)",
            // A stage and a state that do not exist.
            "INSERT INTO immich_outbox (file_id, new_volume_id, new_dir, new_name, stage, \
             next_attempt_at, created_at) VALUES (4, 1, '', 'a.jpg', 2, 0, 0)",
            "INSERT INTO immich_outbox (file_id, new_volume_id, new_dir, new_name, state, \
             next_attempt_at, created_at) VALUES (5, 1, '', 'a.jpg', 'done', 0, 0)",
        ] {
            let inserted = sqlx::query(statement).execute(fx.db.write()).await;
            assert!(inserted.is_err(), "{statement}");
        }
        assert!(fx.rows().await.is_empty());
    }

    #[tokio::test]
    async fn one_pending_unscanned_row_per_file() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b)).await;
        let again = sqlx::query(
            "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, next_attempt_at, \
             created_at) VALUES (1, 1, '', 'b.jpg', 0, 0)",
        )
        .execute(fx.db.write())
        .await;
        assert!(again.is_err());
        // A row that had its scan requested, or needs attention, does not take the place.
        insert(&fx, Seed::moved(1, &b, &a).stage(1)).await;
        insert(&fx, Seed::moved(1, &b, &a).state("attention")).await;
        assert_eq!(fx.rows().await.len(), 3);
    }

    // ----- queue --------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_change_without_a_side_is_ignored() {
        let fx = Fixture::connected().await;
        fx.mapped_volume("photos").await;
        let nothing = Effect {
            old: None,
            new: None,
        };
        assert_eq!(queue_now(&fx, 1, &nothing, 0).await, Queued::Ignored);
        assert!(fx.rows().await.is_empty());
    }

    // ----- apply --------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_pass_applies_its_patches_and_deletes_in_one_go() {
        let fx = Fixture::connected().await;
        let (a, b, c) = (
            at(1, "x", "a.jpg"),
            at(1, "x", "b.jpg"),
            at(1, "y", "c.jpg"),
        );
        let first = insert(&fx, Seed::moved(1, &a, &c)).await;
        insert(&fx, Seed::moved(2, &b, &c)).await;
        let rows = fx.rows().await;
        let patch = Patch {
            asset_id: Some("asset-1".to_owned()),
            snapshot_json: Some(r#"{"favorite":true}"#.to_owned()),
            stage: Some(1),
            scan_requested_at: Some(500),
            next_attempt_at: Some(515),
            carried: Some(true),
            offline_seen_at: Some(520),
            new_asset_id: Some("asset-2".to_owned()),
            attempts: Some(3),
            last_error: Some(Some("boom".to_owned())),
            ..Patch::default()
        };
        let skipped = apply(&fx.db, &[(rows[0].clone(), patch)], &rows[1..])
            .await
            .unwrap();
        assert!(skipped.is_empty());
        let rows = fx.rows().await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, first);
        assert_eq!(row.asset_id.as_deref(), Some("asset-1"));
        assert_eq!(row.new_asset_id.as_deref(), Some("asset-2"));
        assert_eq!(row.snapshot_json.as_deref(), Some(r#"{"favorite":true}"#));
        assert_eq!((row.stage, row.carried, row.attempts), (1, true, 3));
        assert_eq!(row.scan_requested_at, Some(500));
        assert_eq!(row.offline_seen_at, Some(520));
        let (next, error): (i64, Option<String>) =
            sqlx::query_as("SELECT next_attempt_at, last_error FROM immich_outbox")
                .fetch_one(fx.db.read())
                .await
                .unwrap();
        assert_eq!((next, error.as_deref()), (515, Some("boom")));
    }

    #[tokio::test]
    async fn a_patch_can_clear_an_error_and_make_a_row_need_attention() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).stage(1)).await;
        let row = fx.rows().await.remove(0);
        apply(
            &fx.db,
            &[(row.clone(), Patch::retry(1, 200, Some("later")))],
            &[],
        )
        .await
        .unwrap();
        apply(&fx.db, &[(row.clone(), Patch::retry(0, 300, None))], &[])
            .await
            .unwrap();
        let error: Option<String> = sqlx::query_scalar("SELECT last_error FROM immich_outbox")
            .fetch_one(fx.db.read())
            .await
            .unwrap();
        assert_eq!(error, None);
        apply(&fx.db, &[(row, Patch::attention(2, "carry_failed"))], &[])
            .await
            .unwrap();
        let (state, attempts, error): (String, i64, Option<String>) =
            sqlx::query_as("SELECT state, attempts, last_error FROM immich_outbox")
                .fetch_one(fx.db.read())
                .await
                .unwrap();
        assert_eq!(
            (state.as_str(), attempts, error.as_deref()),
            ("attention", 2, Some("carry_failed"))
        );
    }

    #[tokio::test]
    async fn an_empty_patch_changes_nothing_and_is_not_a_skip() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b)).await;
        let row = fx.rows().await.remove(0);
        assert!(
            apply(&fx.db, &[(row.clone(), Patch::default())], &[])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(fx.rows().await, vec![row]);
    }

    #[tokio::test]
    async fn a_change_the_hook_made_meanwhile_is_neither_overwritten_nor_deleted() {
        let fx = Fixture::connected().await;
        let v = fx.mapped_volume("photos").await;
        let (a, c, d) = (
            at(v, "x", "a.jpg"),
            at(v, "y", "c.jpg"),
            at(v, "z", "d.jpg"),
        );
        insert(&fx, Seed::moved(1, &a, &c)).await;
        let read_by_the_pass = fx.rows().await.remove(0);
        // The file moves again after the pass read the row.
        let merged = queue_now(&fx, 1, &Effect::moved(c.clone(), d.clone()), 200).await;
        assert_eq!(merged, Queued::Merged);

        let patch = Patch {
            asset_id: Some("asset-1".to_owned()),
            stage: Some(1),
            ..Patch::default()
        };
        let skipped = apply(&fx.db, &[(read_by_the_pass.clone(), patch)], &[])
            .await
            .unwrap();
        assert_eq!(skipped, [1]);
        let skipped = apply(&fx.db, &[], &[read_by_the_pass]).await.unwrap();
        assert_eq!(skipped, [1]);

        let rows = fx.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stage, 0);
        assert_eq!(rows[0].asset_id, None);
        assert_eq!(rows[0].effect, Effect::moved(a, d));
        // The merge recorded the change again: a scan asked before it does not cover the row.
        assert_eq!(rows[0].created_at, 200);
    }

    #[tokio::test]
    async fn a_file_that_moved_away_and_back_meanwhile_still_counts_as_changed() {
        let fx = Fixture::connected().await;
        let v = fx.mapped_volume("photos").await;
        let (a, b, c) = (
            at(v, "x", "a.jpg"),
            at(v, "y", "b.jpg"),
            at(v, "z", "c.jpg"),
        );
        insert(&fx, Seed::moved(1, &a, &b)).await;
        let read_by_the_pass = fx.rows().await.remove(0);
        queue_now(&fx, 1, &Effect::moved(b.clone(), c.clone()), 200).await;
        queue_now(&fx, 1, &Effect::moved(c, b.clone()), 300).await;

        let patch = Patch {
            stage: Some(1),
            ..Patch::default()
        };
        let skipped = apply(&fx.db, &[(read_by_the_pass, patch)], &[])
            .await
            .unwrap();
        assert_eq!(skipped, [1], "the new place is the same, the row is not");
        let rows = fx.rows().await;
        assert_eq!(rows[0].stage, 0);
        assert_eq!(rows[0].effect, Effect::moved(a, b));
        assert_eq!(rows[0].created_at, 300);
    }

    #[tokio::test]
    async fn a_row_that_asked_for_its_scan_is_settled_by_id() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).stage(1)).await;
        insert(&fx, Seed::moved(2, &a, &b).stage(1)).await;
        let rows = fx.rows().await;
        assert!(apply(&fx.db, &[], &rows[..1]).await.unwrap().is_empty());
        assert_eq!(fx.rows().await.len(), 1);
        // The row is gone already (dismissed while the pass ran): nothing to patch, one skip.
        let gone = rows[0].clone();
        let patch = Patch::retry(1, 10, None);
        assert_eq!(
            apply(&fx.db, &[(gone, patch)], &[]).await.unwrap(),
            [rows[0].id]
        );
    }

    #[tokio::test]
    async fn a_large_chunk_of_deletions_stays_under_the_bind_limit() {
        let fx = Fixture::connected().await;
        let mut tx = fx.db.begin_write().await.unwrap();
        for file_id in 0..(ID_CHUNK as i64 + 7) {
            sqlx::query(
                "INSERT INTO immich_outbox (file_id, old_volume_id, old_dir, old_name, \
                 new_volume_id, new_dir, new_name, stage, next_attempt_at, created_at) \
                 VALUES (?, 1, '', 'a.jpg', 1, '', 'b.jpg', 1, 0, 0)",
            )
            .bind(file_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let rows = fx.rows().await;
        assert!(apply(&fx.db, &[], &rows).await.unwrap().is_empty());
        assert!(fx.rows().await.is_empty());
    }

    // ----- retry and dismiss --------------------------------------------------------------------

    #[tokio::test]
    async fn retry_makes_rows_that_need_attention_or_wait_due_now() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        let attention = insert(&fx, Seed::moved(1, &a, &b).state("attention").due(5_000)).await;
        let waiting = insert(&fx, Seed::moved(2, &a, &b).stage(1).due(9_000)).await;
        let due = insert(&fx, Seed::moved(3, &a, &b).due(50)).await;
        fx.execute("UPDATE immich_outbox SET attempts = 4, last_error = 'x'")
            .await;

        assert_eq!(retry(&fx.db, None, 1_000).await.unwrap(), 2);

        let rows: Vec<(i64, String, i64, Option<String>, i64)> = sqlx::query_as(
            "SELECT id, state, attempts, last_error, next_attempt_at FROM immich_outbox ORDER BY id",
        )
        .fetch_all(fx.db.read())
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (attention, "pending".to_owned(), 0, None, 1_000),
                (waiting, "pending".to_owned(), 0, None, 1_000),
                (due, "pending".to_owned(), 4, Some("x".to_owned()), 50),
            ]
        );
    }

    #[tokio::test]
    async fn retry_can_be_limited_to_some_rows() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        let first = insert(&fx, Seed::moved(1, &a, &b).state("attention")).await;
        let second = insert(&fx, Seed::moved(2, &a, &b).state("attention")).await;
        assert_eq!(retry(&fx.db, Some(&[first, 999]), 7).await.unwrap(), 1);
        let states: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, state FROM immich_outbox ORDER BY id")
                .fetch_all(fx.db.read())
                .await
                .unwrap();
        assert_eq!(
            states,
            vec![
                (first, "pending".to_owned()),
                (second, "attention".to_owned())
            ]
        );
        assert_eq!(retry(&fx.db, Some(&[]), 7).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn retry_leaves_a_row_whose_file_has_another_waiting_row() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        let stuck = insert(&fx, Seed::moved(1, &a, &b).state("attention")).await;
        insert(&fx, Seed::moved(1, &b, &a).due(9_000)).await;
        assert_eq!(retry(&fx.db, None, 100).await.unwrap(), 1);
        let state: String = sqlx::query_scalar("SELECT state FROM immich_outbox WHERE id = ?")
            .bind(stuck)
            .fetch_one(fx.db.read())
            .await
            .unwrap();
        assert_eq!(state, "attention");
    }

    #[tokio::test]
    async fn dismiss_deletes_the_rows_it_is_given() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        let first = insert(&fx, Seed::moved(1, &a, &b)).await;
        let second = insert(&fx, Seed::moved(2, &a, &b)).await;
        assert_eq!(dismiss(&fx.db, &[first, 12_345]).await.unwrap(), 1);
        assert_eq!(fx.rows().await.len(), 1);
        assert_eq!(fx.rows().await[0].id, second);
        assert_eq!(dismiss(&fx.db, &[]).await.unwrap(), 0);
    }

    // ----- what the pass and the status read ----------------------------------------------------

    #[tokio::test]
    async fn due_rows_come_longest_waiting_first_and_leave_out_the_rest() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        let late = insert(&fx, Seed::moved(1, &a, &b).due(90)).await;
        let early = insert(&fx, Seed::moved(2, &a, &b).due(10)).await;
        insert(&fx, Seed::moved(3, &a, &b).due(101)).await;
        insert(&fx, Seed::moved(4, &a, &b).state("attention").due(1)).await;
        let rows = due(&fx.db, 100, 10).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [early, late]);
        assert_eq!(due(&fx.db, 100, 1).await.unwrap().len(), 1);
        assert_eq!(next_due(&fx.db).await.unwrap(), Some(10));
        assert!(any_pending(&fx.db).await.unwrap());
    }

    #[tokio::test]
    async fn the_next_due_time_is_the_earliest_of_the_pending_rows_and_none_without_any() {
        let fx = Fixture::connected().await;
        assert_eq!(next_due(&fx.db).await.unwrap(), None);
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).due(500)).await;
        insert(&fx, Seed::moved(2, &a, &b).due(300)).await;
        insert(&fx, Seed::moved(3, &a, &b).state("attention").due(1)).await;
        assert_eq!(next_due(&fx.db).await.unwrap(), Some(300));
    }

    #[tokio::test]
    async fn nothing_pending_when_only_rows_need_attention() {
        let fx = Fixture::connected().await;
        assert!(!any_pending(&fx.db).await.unwrap());
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).state("attention")).await;
        assert!(!any_pending(&fx.db).await.unwrap());
    }

    #[tokio::test]
    async fn the_age_of_a_row_counts_from_its_scan_request_else_its_creation() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).created(40)).await;
        let row = fx.rows().await.remove(0);
        assert_eq!(row.age_base(), 40);
        let patch = Patch {
            scan_requested_at: Some(90),
            ..Patch::default()
        };
        apply(&fx.db, &[(row, patch)], &[]).await.unwrap();
        assert_eq!(fx.rows().await[0].age_base(), 90);
    }

    #[tokio::test]
    async fn the_summary_counts_by_state_and_stage() {
        let fx = Fixture::connected().await;
        let empty = summary(&fx.db, true).await.unwrap();
        assert_eq!(
            (empty.pending, empty.waiting_for_immich, empty.attention),
            (0, 0, 0)
        );
        assert_eq!(empty.oldest_pending_at, None);

        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b).created(300)).await;
        insert(&fx, Seed::moved(2, &a, &b).stage(1).created(200)).await;
        insert(&fx, Seed::moved(3, &a, &b).state("attention").created(10)).await;
        record_failure(&fx.db, 900, "Immich is down").await.unwrap();

        let shown = summary(&fx.db, false).await.unwrap();
        assert_eq!(
            (shown.pending, shown.waiting_for_immich, shown.attention),
            (2, 1, 1)
        );
        assert_eq!(shown.oldest_pending_at, Some(200));
        assert_eq!(shown.last_run_at, Some(900));
        assert_eq!(shown.last_error.as_deref(), Some("Immich is down"));
        assert!(!shown.scans_on_demand);
    }

    #[tokio::test]
    async fn the_list_shows_places_as_volume_folder_file() {
        let fx = Fixture::connected().await;
        let photos = fx.mapped_volume("photos").await;
        let archive = fx.mapped_volume("archive").await;
        let gone = 4_242;
        let moved_from = at(photos, "2019/trip", "a.jpg");
        let moved_to = at(archive, "", "a.jpg");
        let removed = at(gone, "x", "b.jpg");
        let added = at(photos, "new", "c.jpg");
        let first = insert(&fx, Seed::moved(1, &moved_from, &moved_to)).await;
        let second = insert(&fx, Seed::removed(2, &removed).state("attention")).await;
        let third = insert(&fx, Seed::added(3, &added)).await;
        fx.execute(
            "UPDATE immich_outbox SET attempts = 3, last_error = 'asset_not_offline' WHERE id = 2",
        )
        .await;

        let page = list(&fx.db, None, 0, 10).await.unwrap();
        assert_eq!(page.total, 3);
        let shown: Vec<_> = page
            .items
            .iter()
            .map(|item| (item.id, item.kind, item.from.as_deref(), item.to.as_deref()))
            .collect();
        assert_eq!(
            shown,
            vec![
                (
                    first,
                    OutboxKind::Moved,
                    Some("photos/2019/trip/a.jpg"),
                    Some("archive/a.jpg")
                ),
                (second, OutboxKind::Removed, Some("#4242/x/b.jpg"), None),
                (third, OutboxKind::Added, None, Some("photos/new/c.jpg")),
            ]
        );
        assert_eq!(page.items[1].state, OutboxState::Attention);
        assert_eq!(page.items[1].attempts, 3);
        assert_eq!(
            page.items[1].last_error.as_deref(),
            Some("asset_not_offline")
        );
        assert_eq!(page.items[0].state, OutboxState::Pending);
    }

    #[tokio::test]
    async fn the_list_filters_by_state_and_pages() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        for file_id in 1..=5 {
            let state = if file_id % 2 == 0 {
                "attention"
            } else {
                "pending"
            };
            insert(&fx, Seed::moved(file_id, &a, &b).state(state)).await;
        }
        let pending = list(&fx.db, Some(OutboxState::Pending), 0, 10)
            .await
            .unwrap();
        assert_eq!((pending.total, pending.items.len()), (3, 3));
        let attention = list(&fx.db, Some(OutboxState::Attention), 0, 10)
            .await
            .unwrap();
        assert_eq!((attention.total, attention.items.len()), (2, 2));
        let page = list(&fx.db, None, 1, 2).await.unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.items.iter().map(|i| i.id).collect::<Vec<_>>(), [2, 3]);
        assert!(list(&fx.db, None, 50, 10).await.unwrap().items.is_empty());
    }

    // ----- the state of the passes --------------------------------------------------------------

    #[tokio::test]
    async fn failing_passes_are_counted_until_one_works() {
        let fx = Fixture::connected().await;
        assert_eq!(pass_state(&fx.db).await.unwrap(), PassState::default());
        assert_eq!(record_failure(&fx.db, 100, "one").await.unwrap(), 1);
        assert_eq!(record_failure(&fx.db, 200, "two").await.unwrap(), 2);
        let state = pass_state(&fx.db).await.unwrap();
        assert_eq!(
            (
                state.last_run_at,
                state.last_error.as_deref(),
                state.consecutive_failures
            ),
            (Some(200), Some("two"), 2)
        );
        reset_failures(&fx.db).await.unwrap();
        assert_eq!(pass_state(&fx.db).await.unwrap().consecutive_failures, 0);
        record_failure(&fx.db, 300, "three").await.unwrap();
        record_success(&fx.db, 400).await.unwrap();
        let state = pass_state(&fx.db).await.unwrap();
        assert_eq!(
            (
                state.last_run_at,
                state.last_error,
                state.consecutive_failures
            ),
            (Some(400), None, 0)
        );
    }

    #[test]
    fn an_error_is_clipped_to_what_a_row_keeps() {
        let long = "é".repeat(MAX_ERROR_CHARS * 2);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), MAX_ERROR_CHARS);
        assert!(long.starts_with(&clipped));
        assert_eq!(clip("short"), "short");
        assert_eq!(
            clip(&"x".repeat(MAX_ERROR_CHARS)),
            "x".repeat(MAX_ERROR_CHARS)
        );
    }

    #[tokio::test]
    async fn clear_forgets_the_rows_and_the_state() {
        let fx = Fixture::connected().await;
        let (a, b) = (at(1, "", "a.jpg"), at(1, "", "b.jpg"));
        insert(&fx, Seed::moved(1, &a, &b)).await;
        record_failure(&fx.db, 100, "boom").await.unwrap();
        clear(&fx.db).await.unwrap();
        assert!(fx.rows().await.is_empty());
        assert_eq!(pass_state(&fx.db).await.unwrap(), PassState::default());
    }
}
