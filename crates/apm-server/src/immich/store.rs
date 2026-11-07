//! The tables of the Immich integration: `immich_connection` (at most one row) and
//! `immich_volume_map` (one row per mapped volume). Plain functions over [`Db`]; the service decides
//! when to call them. Migration `0100_immich_connection.sql` documents the columns.
//!
//! Nothing here touches the network or the file system, and the only secret held is the encrypted
//! API key, which [`StoredConnection`] keeps out of its `Debug` output.

use std::fmt;

use apm_core::dto::ImmichMappingDto;
use apm_core::paths::VolumeId;

use crate::db::{Db, DbError};

/// The stored connection, as the table holds it.
#[derive(Clone, sqlx::FromRow)]
pub(super) struct StoredConnection {
    pub server_url: String,
    pub email: String,
    pub user_id: String,
    pub user_name: String,
    pub is_admin: bool,
    /// Immich's id of the API key, needed to revoke it.
    pub api_key_id: String,
    /// The API key, encrypted by `SecretBox`.
    pub api_key_enc: String,
    pub allow_insecure_tls: bool,
    pub server_version: Option<String>,
    pub connected_at: i64,
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
    pub key_rejected: bool,
}

impl fmt::Debug for StoredConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredConnection")
            .field("user_id", &self.user_id)
            .field("is_admin", &self.is_admin)
            .field("api_key_id", &self.api_key_id)
            .field("server_version", &self.server_version)
            .field("connected_at", &self.connected_at)
            .finish_non_exhaustive()
    }
}

/// What a successful connect stores. Health starts clean: the connection was just verified.
pub(super) struct NewConnection<'a> {
    pub server_url: &'a str,
    pub email: &'a str,
    pub user_id: &'a str,
    pub user_name: &'a str,
    pub is_admin: bool,
    pub api_key_id: &'a str,
    pub api_key_enc: &'a str,
    pub allow_insecure_tls: bool,
    pub server_version: Option<&'a str>,
    pub connected_at: i64,
}

const SELECT_CONNECTION: &str = "SELECT server_url, email, user_id, user_name, is_admin, \
     api_key_id, api_key_enc, allow_insecure_tls, server_version, connected_at, last_ok_at, \
     last_error, key_rejected FROM immich_connection WHERE id = 1";

pub(super) async fn load(db: &Db) -> Result<Option<StoredConnection>, DbError> {
    Ok(sqlx::query_as(SELECT_CONNECTION)
        .fetch_optional(db.read())
        .await?)
}

/// Stores `connection` as the connection, replacing the previous one.
pub(super) async fn replace(db: &Db, connection: &NewConnection<'_>) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO immich_connection (id, server_url, email, user_id, user_name, is_admin, \
         api_key_id, api_key_enc, allow_insecure_tls, server_version, connected_at, last_ok_at, \
         last_error, key_rejected) \
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, 0) \
         ON CONFLICT (id) DO UPDATE SET \
         server_url = excluded.server_url, email = excluded.email, user_id = excluded.user_id, \
         user_name = excluded.user_name, is_admin = excluded.is_admin, \
         api_key_id = excluded.api_key_id, api_key_enc = excluded.api_key_enc, \
         allow_insecure_tls = excluded.allow_insecure_tls, \
         server_version = excluded.server_version, connected_at = excluded.connected_at, \
         last_ok_at = excluded.last_ok_at, last_error = NULL, key_rejected = 0",
    )
    .bind(connection.server_url)
    .bind(connection.email)
    .bind(connection.user_id)
    .bind(connection.user_name)
    .bind(connection.is_admin)
    .bind(connection.api_key_id)
    .bind(connection.api_key_enc)
    .bind(connection.allow_insecure_tls)
    .bind(connection.server_version)
    .bind(connection.connected_at)
    .bind(connection.connected_at)
    .execute(db.write())
    .await?;
    Ok(())
}

/// Removes the connection; whether there was one. The mappings stay.
pub(super) async fn delete(db: &Db) -> Result<bool, DbError> {
    let done = sqlx::query("DELETE FROM immich_connection WHERE id = 1")
        .execute(db.write())
        .await?;
    Ok(done.rows_affected() > 0)
}

/// What the latest calls showed, to keep across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HealthRecord {
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
    pub key_rejected: bool,
}

/// Records `health` for the connection of `api_key_id`. A connection that was replaced since the
/// caller loaded it (another key) is left alone.
pub(super) async fn record_health(
    db: &Db,
    api_key_id: &str,
    health: &HealthRecord,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE immich_connection SET last_ok_at = ?, last_error = ?, key_rejected = ? \
         WHERE id = 1 AND api_key_id = ?",
    )
    .bind(health.last_ok_at)
    .bind(&health.last_error)
    .bind(health.key_rejected)
    .bind(api_key_id)
    .execute(db.write())
    .await?;
    Ok(())
}

/// Records the server version the connection of `api_key_id` was last seen with.
pub(super) async fn record_version(
    db: &Db,
    api_key_id: &str,
    version: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE immich_connection SET server_version = ? WHERE id = 1 AND api_key_id = ? \
         AND server_version IS NOT ?",
    )
    .bind(version)
    .bind(api_key_id)
    .bind(version)
    .execute(db.write())
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct MappingRow {
    volume_id: VolumeId,
    volume_name: String,
    library_id: Option<String>,
    library_name: String,
    path_prefix: String,
}

impl From<MappingRow> for ImmichMappingDto {
    fn from(row: MappingRow) -> Self {
        Self {
            volume_id: row.volume_id,
            volume_name: row.volume_name,
            library_id: row.library_id,
            library_name: row.library_name,
            path_prefix: row.path_prefix,
        }
    }
}

/// The stored mappings, ordered by volume name.
pub(super) async fn mappings(db: &Db) -> Result<Vec<ImmichMappingDto>, DbError> {
    let rows: Vec<MappingRow> = sqlx::query_as(
        "SELECT m.volume_id, v.name AS volume_name, m.library_id, m.library_name, m.path_prefix \
         FROM immich_volume_map m JOIN volumes v ON v.id = m.volume_id \
         ORDER BY v.name COLLATE NOCASE, m.volume_id",
    )
    .fetch_all(db.read())
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// One mapping to store.
pub(super) struct MappingInput {
    pub volume_id: VolumeId,
    pub library_id: Option<String>,
    pub library_name: String,
    pub path_prefix: String,
}

/// Replaces every stored mapping by `inputs`, in one transaction.
pub(super) async fn replace_mappings(db: &Db, inputs: &[MappingInput]) -> Result<(), DbError> {
    let mut tx = db.begin_write().await?;
    sqlx::query("DELETE FROM immich_volume_map")
        .execute(&mut *tx)
        .await?;
    for input in inputs {
        sqlx::query(
            "INSERT INTO immich_volume_map (volume_id, library_id, library_name, path_prefix) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(input.volume_id)
        .bind(&input.library_id)
        .bind(&input.library_name)
        .bind(&input.path_prefix)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Updates the remembered name of the library `library_id` in every mapping that uses it.
pub(super) async fn record_library_name(
    db: &Db,
    library_id: &str,
    name: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE immich_volume_map SET library_name = ? WHERE library_id = ? AND library_name <> ?",
    )
    .bind(name)
    .bind(library_id)
    .bind(name)
    .execute(db.write())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn volume(db: &Db, name: &str) -> VolumeId {
        sqlx::query_scalar(
            "INSERT INTO volumes (name, root, writable, online, created_at) \
             VALUES (?, ?, 1, 1, 0) RETURNING id",
        )
        .bind(name)
        .bind(format!("/volumes/{name}"))
        .fetch_one(db.write())
        .await
        .unwrap()
    }

    fn connection(key_id: &str) -> NewConnection<'_> {
        NewConnection {
            server_url: "http://immich:2283",
            email: "me@example.org",
            user_id: "user-1",
            user_name: "Me",
            is_admin: true,
            api_key_id: key_id,
            api_key_enc: "v1.encrypted",
            allow_insecure_tls: false,
            server_version: Some("v3.4.0"),
            connected_at: 1_700_000_000,
        }
    }

    fn input(volume_id: VolumeId, library: Option<&str>, prefix: &str) -> MappingInput {
        MappingInput {
            volume_id,
            library_id: library.map(str::to_owned),
            library_name: library
                .map(|id| format!("name of {id}"))
                .unwrap_or_default(),
            path_prefix: prefix.to_owned(),
        }
    }

    #[tokio::test]
    async fn the_connection_is_stored_replaced_and_deleted() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        assert!(load(&db).await.unwrap().is_none());
        assert!(!delete(&db).await.unwrap());

        replace(&db, &connection("key-1")).await.unwrap();
        let stored = load(&db).await.unwrap().unwrap();
        assert_eq!(stored.server_url, "http://immich:2283");
        assert_eq!(stored.email, "me@example.org");
        assert_eq!(stored.user_name, "Me");
        assert!(stored.is_admin);
        assert_eq!(stored.api_key_id, "key-1");
        assert_eq!(stored.api_key_enc, "v1.encrypted");
        assert_eq!(stored.server_version.as_deref(), Some("v3.4.0"));
        assert_eq!(stored.last_ok_at, Some(1_700_000_000));
        assert_eq!(stored.last_error, None);
        assert!(!stored.key_rejected && !stored.allow_insecure_tls);

        replace(
            &db,
            &NewConnection {
                is_admin: false,
                allow_insecure_tls: true,
                server_version: None,
                ..connection("key-2")
            },
        )
        .await
        .unwrap();
        let stored = load(&db).await.unwrap().unwrap();
        assert_eq!(stored.api_key_id, "key-2");
        assert!(!stored.is_admin && stored.allow_insecure_tls);
        assert_eq!(stored.server_version, None);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_connection")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(rows, 1, "a second connect replaces the first");

        assert!(delete(&db).await.unwrap());
        assert!(load(&db).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_new_connect_clears_the_old_health() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        replace(&db, &connection("key-1")).await.unwrap();
        record_health(
            &db,
            "key-1",
            &HealthRecord {
                last_ok_at: Some(5),
                last_error: Some("Immich answered 500".to_owned()),
                key_rejected: true,
            },
        )
        .await
        .unwrap();
        let stored = load(&db).await.unwrap().unwrap();
        assert_eq!(stored.last_ok_at, Some(5));
        assert_eq!(stored.last_error.as_deref(), Some("Immich answered 500"));
        assert!(stored.key_rejected);

        replace(&db, &connection("key-2")).await.unwrap();
        let stored = load(&db).await.unwrap().unwrap();
        assert_eq!(stored.last_ok_at, Some(1_700_000_000));
        assert_eq!(stored.last_error, None);
        assert!(!stored.key_rejected);
    }

    #[tokio::test]
    async fn health_and_version_only_change_the_connection_they_were_loaded_from() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        replace(&db, &connection("key-2")).await.unwrap();
        let stale = HealthRecord {
            last_ok_at: Some(1),
            last_error: Some("old failure".to_owned()),
            key_rejected: true,
        };
        record_health(&db, "key-1", &stale).await.unwrap();
        record_version(&db, "key-1", "v1.0.0").await.unwrap();
        let stored = load(&db).await.unwrap().unwrap();
        assert_eq!(stored.last_error, None);
        assert_eq!(stored.server_version.as_deref(), Some("v3.4.0"));

        record_version(&db, "key-2", "v3.5.0").await.unwrap();
        assert_eq!(
            load(&db).await.unwrap().unwrap().server_version.as_deref(),
            Some("v3.5.0")
        );
    }

    #[tokio::test]
    async fn the_secret_stays_out_of_debug_output() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        replace(&db, &connection("key-1")).await.unwrap();
        let shown = format!("{:?}", load(&db).await.unwrap().unwrap());
        assert!(!shown.contains("v1.encrypted"), "{shown}");
        assert!(!shown.contains("immich:2283"), "{shown}");
        assert!(!shown.contains("me@example.org"), "{shown}");
    }

    #[tokio::test]
    async fn mappings_are_replaced_as_a_whole_and_listed_by_volume_name() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let video = volume(&db, "video").await;
        let photo = volume(&db, "Photo").await;
        assert!(mappings(&db).await.unwrap().is_empty());

        replace_mappings(
            &db,
            &[
                input(video, Some("lib-v"), "/mnt/video"),
                input(photo, None, "/mnt/photo"),
            ],
        )
        .await
        .unwrap();
        let stored = mappings(&db).await.unwrap();
        let names: Vec<_> = stored.iter().map(|m| m.volume_name.as_str()).collect();
        assert_eq!(names, ["Photo", "video"], "case-insensitive order by name");
        assert_eq!(stored[0].library_id, None);
        assert_eq!(stored[0].library_name, "");
        assert_eq!(stored[1].library_id.as_deref(), Some("lib-v"));
        assert_eq!(stored[1].library_name, "name of lib-v");
        assert_eq!(stored[1].path_prefix, "/mnt/video");

        replace_mappings(&db, &[input(photo, Some("lib-p"), "/srv/photos")])
            .await
            .unwrap();
        let stored = mappings(&db).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].path_prefix, "/srv/photos");

        replace_mappings(&db, &[]).await.unwrap();
        assert!(mappings(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_database_refuses_what_the_validation_should_have_caught() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let a = volume(&db, "a").await;
        let b = volume(&db, "b").await;

        let relative = replace_mappings(&db, &[input(a, None, "mnt/a")])
            .await
            .unwrap_err();
        assert!(relative.is_check_violation(), "{relative}");

        let same = replace_mappings(&db, &[input(a, None, "/mnt/x"), input(b, None, "/mnt/x")])
            .await
            .unwrap_err();
        assert!(same.is_unique_violation(), "{same}");

        let twice = replace_mappings(&db, &[input(a, None, "/mnt/x"), input(a, None, "/mnt/y")])
            .await
            .unwrap_err();
        assert!(twice.is_unique_violation(), "{twice}");

        let missing = replace_mappings(&db, &[input(999, None, "/mnt/z")])
            .await
            .unwrap_err();
        assert!(missing.is_foreign_key_violation(), "{missing}");
        assert!(
            mappings(&db).await.unwrap().is_empty(),
            "a refused replacement changes nothing"
        );
    }

    #[tokio::test]
    async fn mappings_follow_their_volume_when_it_is_deleted() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let a = volume(&db, "a").await;
        replace_mappings(&db, &[input(a, None, "/mnt/a")])
            .await
            .unwrap();
        sqlx::query("DELETE FROM volumes WHERE id = ?")
            .bind(a)
            .execute(db.write())
            .await
            .unwrap();
        assert!(mappings(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_library_name_is_refreshed_everywhere_it_is_used() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let a = volume(&db, "a").await;
        let b = volume(&db, "b").await;
        replace_mappings(
            &db,
            &[
                input(a, Some("lib-1"), "/mnt/a"),
                input(b, Some("lib-2"), "/mnt/b"),
            ],
        )
        .await
        .unwrap();
        record_library_name(&db, "lib-1", "Renamed").await.unwrap();
        record_library_name(&db, "unknown", "Nothing")
            .await
            .unwrap();
        let stored = mappings(&db).await.unwrap();
        assert_eq!(stored[0].library_name, "Renamed");
        assert_eq!(stored[1].library_name, "name of lib-2");
    }
}
