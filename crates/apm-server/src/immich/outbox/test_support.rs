//! Helpers shared by the unit tests of the outbox modules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use apm_core::paths::VolumeId;
use sqlx::FromRow;
use sqlx::sqlite::SqliteRow;

use super::hook::OutboxHook;
use super::signal::OutboxSignal;
use super::store::{self, Row};
use crate::db::{Db, TempDb};
use crate::fsops::{AppliedOp, OpHook};
use crate::immich::store as immich_store;

/// A temporary database, optionally with a stored Immich connection, plus the line between the hook
/// and the scheduler.
pub(super) struct Fixture {
    _temp: TempDb,
    pub db: Db,
    pub signal: OutboxSignal,
}

impl Fixture {
    /// No connection to Immich.
    pub async fn new() -> Self {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        Self {
            _temp: temp,
            db,
            signal: OutboxSignal::new(),
        }
    }

    /// With the connection of an administrator stored.
    pub async fn connected() -> Self {
        let fx = Self::new().await;
        fx.connect(true).await;
        fx
    }

    pub async fn connect(&self, is_admin: bool) {
        immich_store::replace(
            &self.db,
            &immich_store::NewConnection {
                server_url: "http://immich:2283",
                email: "me@example.org",
                user_id: "user-1",
                user_name: "Me",
                is_admin,
                api_key_id: "key-1",
                api_key_enc: "v1.encrypted",
                allow_insecure_tls: false,
                server_version: Some("v3.4.0"),
                connected_at: 1_700_000_000,
            },
        )
        .await
        .unwrap();
    }

    pub async fn volume(&self, name: &str) -> VolumeId {
        sqlx::query_scalar(
            "INSERT INTO volumes (name, root, writable, online, created_at) \
             VALUES (?, ?, 1, 1, 0) RETURNING id",
        )
        .bind(name)
        .bind(format!("/volumes/{name}"))
        .fetch_one(self.db.write())
        .await
        .unwrap()
    }

    /// A volume mapped to the library `lib-<name>` under `/mnt/<name>`.
    pub async fn mapped_volume(&self, name: &str) -> VolumeId {
        let volume_id = self.volume(name).await;
        sqlx::query(
            "INSERT INTO immich_volume_map (volume_id, library_id, library_name, path_prefix) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(volume_id)
        .bind(format!("lib-{name}"))
        .bind(format!("Library {name}"))
        .bind(format!("/mnt/{name}"))
        .execute(self.db.write())
        .await
        .unwrap();
        volume_id
    }

    /// Runs the hook for `op` in a transaction that is committed, as `FsOps` does.
    pub async fn apply(&self, op: &AppliedOp) {
        let hook = OutboxHook::new(self.signal.clone());
        let mut tx = self.db.begin_write().await.unwrap();
        hook.on_applied(&mut tx, op).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Every row, whatever its state, by id.
    pub async fn rows(&self) -> Vec<Row> {
        store::all(&self.db).await.unwrap()
    }

    /// The `file_id` of every row, sorted.
    pub async fn file_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = sqlx::query_scalar("SELECT file_id FROM immich_outbox")
            .fetch_all(self.db.read())
            .await
            .unwrap();
        ids.sort_unstable();
        ids
    }

    pub async fn execute(&self, sql: &'static str) {
        sqlx::query(sql).execute(self.db.write()).await.unwrap();
    }

    /// The one row `sql` selects, as a tuple or a struct.
    pub async fn query<T>(&self, sql: &'static str) -> T
    where
        T: for<'r> FromRow<'r, SqliteRow> + Send + Unpin,
    {
        sqlx::query_as(sql).fetch_one(self.db.read()).await.unwrap()
    }
}
