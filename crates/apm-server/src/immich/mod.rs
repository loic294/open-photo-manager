//! Immich integration: the API client, the stored connection, the volume-to-library mapping, the
//! asset sync and the outbox that mirrors local file operations to Immich.
//!
//! Everything except `assets`, `sync` and `outbox` is owned by todo `building-immich-client`;
//! `assets` and `sync` belong to `syncing-immich-assets` and `outbox` to
//! `reflecting-file-ops-in-immich`.
//!
//! | module    | what it holds                                                                    |
//! |-----------|----------------------------------------------------------------------------------|
//! | `assets`  | the tables `immich_assets` and `immich_sync_state`: what the sync read of Immich |
//! | `client`  | [`ImmichClient`]: every HTTP request to Immich goes through it                   |
//! | `connect` | connecting (login, API key, verification) and disconnecting                      |
//! | `crypto`  | encryption at rest of the API key                                                |
//! | `detect`  | finding the path prefix Immich uses for a volume                                 |
//! | `error`   | [`ImmichError`] and its mapping to HTTP                                          |
//! | `mapping` | libraries, the volume-to-library mappings, the trash exclusion                   |
//! | `models`  | the shapes of what Immich sends and receives                                     |
//! | `outbox`  | mirroring local file operations to Immich                                        |
//! | `paths`   | [`PathMap`]: the one translation between Immich paths and files                  |
//! | `store`   | the tables `immich_connection` and `immich_volume_map`                           |
//! | `sync`    | [`ImmichSync`]: the read-only sync of the assets, and the loop that triggers it  |
//!
//! # Rules for everything that talks to Immich
//!
//! - Get the client ONLY from [`ImmichService::client`]. It reads and decrypts the API key, applies
//!   the stored TLS choice and server version, and is shared, so its health tracking sees every
//!   call. Never build an [`ImmichClient`] elsewhere (the connect flow is the one exception: it has
//!   no key yet).
//! - Translate paths ONLY with the [`PathMap`] of [`ImmichService::path_map`]; never join or split
//!   an Immich path by hand.
//! - Secrets: the password exists only inside the connect request, the API key only as `Redacted`
//!   in the client and encrypted in the database. Never log a request body, a header or a URL
//!   query; errors from the network are scrubbed before they are kept (see `error`).
//! - None of this is a task-engine task: connecting, status, libraries, mappings and prefix
//!   detection are short calls with hard timeouts, answered while the request waits. The sync and
//!   the outbox cleanup are long and belong on the `immich` lane of the task engine.
//!
//! # The connection
//!
//! At most one connection is stored (`immich_connection`). Connecting is all or nothing: the key is
//! created with the account's password, checked, encrypted and stored, and the Immich session used
//! for it is ended again; a failure after the key exists revokes it. The status
//! ([`ImmichService::status`]) is built from the stored row plus what the client has seen since it
//! was built, and makes no request unless asked to (`check`). When the key cannot be decrypted
//! (the key file was lost or replaced) or Immich rejects it, the status says `needs_reconnect`.
//!
//! # The asset sync
//!
//! `sync` reads the assets of Immich into `immich_assets` (see `assets`), so that the `in_immich`
//! badge of the item pages and the outbox know which files Immich has. It only reads: nothing is
//! ever sent to Immich, and nothing changes on disk.
//!
//! - **What**: pages of 1,000 from the metadata search, trashed and offline assets included (Immich
//!   trashes an asset whose file went missing and flags it offline, so without them a deletion is
//!   invisible). With an administrator account one *scope* per mapped library, otherwise one per
//!   mapped path prefix. A pass is full (everything, then the rows it did not see are dropped) or
//!   incremental (what was updated since the last pass). Locked assets (an API key never sees
//!   them) and assets that were uploaded instead of read from a library are out of scope; an asset
//!   that is deleted from Immich for good leaves the store at the next full pass, a day at most.
//! - **When**: the loop `main` starts looks every 30 s; a scope is read again 15 minutes after its
//!   last pass, in full after a day, and at once when it is new or its mappings, account or server
//!   changed. `POST /api/immich/sync` and [`ImmichSync::submit`] ask for every scope: the Settings
//!   page calls the endpoint after it saved mappings, the outbox calls `submit` when its queue
//!   drains. One sync runs at a time, and requests that arrive while one waits are merged into it.
//! - **Cost**: 200,000 assets in 2.5 s, and the badge adds 0.4 ms to a page of 200 files (the
//!   numbers are in the docs of `sync`).
//! - **Status**: `last_sync_at` and `sync` of [`ImmichService::status`] come from `assets`.
//!   Disconnecting, or connecting to another server or account, clears what was read.
//!
//! # Seams for the outbox
//!
//! `reflecting-file-ops-in-immich` (migration block 0120-0139, module `outbox`):
//! - same client and `PathMap` rules; it calls `OfflineAssetId::confirm` on a fresh `get_asset`
//!   right before any delete;
//! - `assets::find_by_location` tells which assets stand for a file and `assets::forget` drops the
//!   ones it removed; the rows are what the last pass saw, so it reads the affected scopes again
//!   with [`ImmichSync::refresh`] before it looks, and asks [`ImmichSync::submit`] for a sync after
//!   it changed Immich (the docs of `assets` list the rest);
//! - [`ImmichService::status`] fills `outbox_pending`, `needs_attention` and `outbox` from
//!   `outbox::summary`, and `disconnect` empties the queue with `outbox::clear`;
//! - the Immich page of Settings lists the mappings from `GET /api/immich/status`.

pub mod assets;
pub mod client;
mod connect;
pub mod crypto;
mod detect;
pub mod direct_db;
pub mod error;
mod mapping;
pub mod models;
pub mod outbox;
pub mod paths;
mod store;
pub mod sync;
#[cfg(test)]
mod test_support;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use apm_core::dto::{
    ImmichConnectionDto, ImmichMappingDto, ImmichMappingEntry, ImmichMode, ImmichStatusDto,
};
use tokio::sync::Mutex;

use crate::db::Db;
use crate::security::Redacted;

pub use client::ImmichClient;
pub use connect::ConnectInput;
pub use error::ImmichError;
pub use outbox::ImmichOutbox;
pub use paths::PathMap;
pub use sync::{ImmichSync, RefreshError, RefreshStats, SyncRequest, SyncSubmitted};

use client::{API_KEY_NAME, ClientOptions, Health};
use connect::Timing;
use crypto::SecretBox;
use models::{ImmichVersion, SPEC_VERSION};
use store::{HealthRecord, StoredConnection};

/// The oldest Immich major version the application is written for (plan decision A19).
const MIN_MAJOR_VERSION: u32 = 2;

/// The Immich integration, shared by every handler and engine. Cheap to clone.
#[derive(Clone)]
pub struct ImmichService {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    move_database: Arc<dyn direct_db::MoveDatabase>,
    /// Where the key file of `crypto` lives.
    data_dir: PathBuf,
    /// Held for the whole of a connect or a disconnect so two of them never interleave: each
    /// would otherwise revoke the key of the other.
    gate: Mutex<()>,
    /// The client of the stored connection, built on first use.
    cache: Mutex<Option<Cached>>,
    /// Whether an unreadable key was already reported in the log since the last readable one.
    key_warned: AtomicBool,
    /// The time limits of the connect and disconnect flows.
    timing: Timing,
}

struct Cached {
    built_from: Fingerprint,
    client: Arc<ImmichClient>,
}

/// What a client was built from: a stored connection that differs in any of it (a new connect, a
/// version seen by a status check) needs a new client, so the cache never has to be invalidated.
#[derive(PartialEq, Eq)]
struct Fingerprint {
    api_key_id: String,
    server_url: String,
    server_version: Option<String>,
    allow_insecure_tls: bool,
}

impl Fingerprint {
    fn of(stored: &StoredConnection) -> Self {
        Self {
            api_key_id: stored.api_key_id.clone(),
            server_url: stored.server_url.clone(),
            server_version: stored.server_version.clone(),
            allow_insecure_tls: stored.allow_insecure_tls,
        }
    }
}

impl fmt::Debug for ImmichService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmichService").finish_non_exhaustive()
    }
}

impl ImmichService {
    /// Nothing is read or sent here: the stored connection, the key file and the HTTP client are
    /// only touched when first needed.
    pub fn new(db: Db, data_dir: PathBuf) -> Self {
        Self::with_timing(db, data_dir, Timing::default())
    }

    fn with_timing(db: Db, data_dir: PathBuf, timing: Timing) -> Self {
        Self::build(
            db,
            data_dir,
            timing,
            Arc::new(direct_db::PostgresMoves::new(None)),
        )
    }

    pub fn with_move_database(
        db: Db,
        data_dir: PathBuf,
        database: Arc<dyn direct_db::MoveDatabase>,
    ) -> Self {
        Self::build(db, data_dir, Timing::default(), database)
    }

    fn build(
        db: Db,
        data_dir: PathBuf,
        timing: Timing,
        move_database: Arc<dyn direct_db::MoveDatabase>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                move_database,
                data_dir,
                gate: Mutex::new(()),
                cache: Mutex::new(None),
                key_warned: AtomicBool::new(false),
                timing,
            }),
        }
    }

    pub(crate) fn move_database(&self) -> &dyn direct_db::MoveDatabase {
        self.inner.move_database.as_ref()
    }

    /// Executor-only probe; never called by a request handler or local SQLite hook.
    pub(crate) async fn probe_move_database(&self) -> Result<(), direct_db::DbMoveError> {
        let result = match self.client().await {
            Ok(client) => match client.about().await {
                Ok(about) => self.move_database().probe(&about.version).await,
                Err(_) => Err(direct_db::DbMoveError::Unavailable),
            },
            Err(_) => Err(direct_db::DbMoveError::Unavailable),
        };
        let reason = result.map_or_else(|error| error.to_string(),
            |()| "Compatible Immich v3.3.3 database verified. Tracked moves still require per-asset checks.".to_owned());
        let mut tx = self
            .inner
            .db
            .begin_write()
            .await
            .map_err(|_| direct_db::DbMoveError::Unavailable)?;
        sqlx::query(
            "UPDATE immich_direct_db_state SET ready = ?, reason = ?, checked_at = ? WHERE id = 1",
        )
        .bind(result.is_ok())
        .bind(reason)
        .bind(crate::db::now_secs())
        .execute(&mut *tx)
        .await
        .map_err(|_| direct_db::DbMoveError::Unavailable)?;
        tx.commit()
            .await
            .map_err(|_| direct_db::DbMoveError::Unavailable)?;
        result
    }

    /// The client of the stored connection: [`ImmichError::NotConnected`] without one,
    /// [`ImmichError::NeedsReconnect`] when its API key cannot be read. The same client is returned
    /// until the connection changes.
    pub async fn client(&self) -> Result<Arc<ImmichClient>, ImmichError> {
        let stored = self.stored().await?;
        self.client_for(&stored).await
    }

    /// The path mapping of the stored mappings, whether or not a connection exists (check that
    /// with [`ImmichService::client`]). A stored set the rules refuse is an internal error: nobody
    /// may translate paths with a broken mapping.
    pub async fn path_map(&self) -> Result<PathMap, ImmichError> {
        let entries: Vec<ImmichMappingEntry> = store::mappings(&self.inner.db)
            .await?
            .into_iter()
            .map(|mapping| ImmichMappingEntry {
                volume_id: mapping.volume_id,
                library_id: mapping.library_id,
                path_prefix: mapping.path_prefix,
            })
            .collect();
        PathMap::new(&entries).map_err(|problems| {
            let described: Vec<String> = problems.iter().map(ToString::to_string).collect();
            ImmichError::Internal(format!(
                "the stored Immich mappings are invalid: {}",
                described.join("; ")
            ))
        })
    }

    /// The stored connection, `None` when disconnected.
    pub async fn connection(&self) -> Result<Option<ImmichConnectionDto>, ImmichError> {
        Ok(store::load(&self.inner.db)
            .await?
            .map(|stored| connection_dto(&stored)))
    }

    /// The stored mappings with their volume and library names, ordered by volume name.
    pub async fn mappings(&self) -> Result<Vec<ImmichMappingDto>, ImmichError> {
        Ok(store::mappings(&self.inner.db).await?)
    }

    /// The state of the connection for the Immich page.
    ///
    /// Makes no request unless `check` is set; then one authenticated `GET /server/about` (3 s at
    /// most) refreshes the reachability and the stored server version. Otherwise `reachable` is
    /// what the latest real call (of any part of the application) showed, or what the stored row
    /// remembers from before a restart.
    pub async fn status(&self, check: bool) -> Result<ImmichStatusDto, ImmichError> {
        let db = &self.inner.db;
        let mappings = store::mappings(db).await?;
        let Some(mut stored) = store::load(db).await? else {
            return Ok(ImmichStatusDto {
                connected: false,
                mode: ImmichMode::Disconnected,
                connection: None,
                reachable: None,
                needs_reconnect: false,
                server_version: None,
                version_warning: None,
                mappings,
                last_sync_at: None,
                sync: None,
                outbox_pending: 0,
                needs_attention: 0,
                outbox: None,
                direct_database: direct_db::status(db).await?,
            });
        };

        let mut key_unreadable = false;
        match self.client_for(&stored).await {
            Ok(client) => {
                if check {
                    self.probe(&client, &mut stored).await?;
                }
                self.absorb_health(&client.health(), &mut stored).await?;
            }
            Err(ImmichError::NeedsReconnect) => key_unreadable = true,
            Err(other) => return Err(other),
        }

        let reachable = (!key_unreadable)
            .then(|| server_answered(stored.last_error.as_deref(), stored.key_rejected));
        let sync = assets::status_summary(db, &stored, &mappings).await?;
        let outbox = outbox::summary(db, stored.is_admin).await?;
        Ok(ImmichStatusDto {
            connected: true,
            mode: if stored.is_admin {
                ImmichMode::Full
            } else {
                ImmichMode::Limited
            },
            reachable,
            needs_reconnect: key_unreadable || stored.key_rejected,
            server_version: stored.server_version.clone(),
            version_warning: version_warning(stored.server_version.as_deref()),
            connection: Some(connection_dto(&stored)),
            mappings,
            last_sync_at: sync.as_ref().and_then(|summary| summary.last_sync_at),
            sync,
            outbox_pending: u32::try_from(outbox.pending).unwrap_or(u32::MAX),
            needs_attention: u32::try_from(outbox.attention).unwrap_or(u32::MAX),
            outbox: Some(outbox),
            direct_database: direct_db::status(db).await?,
        })
    }

    async fn stored(&self) -> Result<StoredConnection, ImmichError> {
        store::load(&self.inner.db)
            .await?
            .ok_or(ImmichError::NotConnected)
    }

    /// The shared client for `stored`, rebuilt when the stored connection changed since it was made.
    async fn client_for(
        &self,
        stored: &StoredConnection,
    ) -> Result<Arc<ImmichClient>, ImmichError> {
        let wanted = Fingerprint::of(stored);
        let mut cache = self.inner.cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.built_from == wanted
        {
            return Ok(Arc::clone(&cached.client));
        }
        let client = Arc::new(self.build_client(stored).await?);
        *cache = Some(Cached {
            built_from: wanted,
            client: Arc::clone(&client),
        });
        Ok(client)
    }

    /// A new client for `stored`, outside the cache.
    async fn build_client(&self, stored: &StoredConnection) -> Result<ImmichClient, ImmichError> {
        let key = self.read_key(stored).await?;
        ImmichClient::new(
            &stored.server_url,
            Some(key),
            ClientOptions {
                allow_insecure_tls: stored.allow_insecure_tls,
                server_version: stored
                    .server_version
                    .as_deref()
                    .and_then(ImmichVersion::parse),
                ..ClientOptions::default()
            },
        )
    }

    /// Decrypts the stored API key. Any failure (no key file, another key, a damaged value) means
    /// the same to the user, who has to connect again; the cause is logged once, never the key.
    async fn read_key(&self, stored: &StoredConnection) -> Result<Redacted<String>, ImmichError> {
        let data_dir = self.inner.data_dir.clone();
        let encrypted = stored.api_key_enc.clone();
        let read = tokio::task::spawn_blocking(move || {
            SecretBox::load(&data_dir).and_then(|secrets| secrets.decrypt(&encrypted))
        })
        .await
        .map_err(|err| ImmichError::Internal(format!("reading the API key failed: {err}")))?;
        match read {
            Ok(key) => {
                self.inner.key_warned.store(false, Ordering::Relaxed);
                Ok(key)
            }
            Err(err) => {
                if !self.inner.key_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        error = %err,
                        "the stored Immich API key cannot be read; Immich has to be connected again"
                    );
                }
                Err(ImmichError::NeedsReconnect)
            }
        }
    }

    /// Asks Immich for its version (a failure is recorded in the health of the client) and keeps
    /// the version that came back.
    async fn probe(
        &self,
        client: &ImmichClient,
        stored: &mut StoredConnection,
    ) -> Result<(), ImmichError> {
        if let Ok(about) = client.check().await
            && !about.version.is_empty()
            && stored.server_version.as_deref() != Some(about.version.as_str())
        {
            store::record_version(&self.inner.db, &stored.api_key_id, &about.version).await?;
            stored.server_version = Some(about.version);
        }
        Ok(())
    }

    /// Stores what the client has seen since the row was written, if it differs.
    async fn absorb_health(
        &self,
        health: &Health,
        stored: &mut StoredConnection,
    ) -> Result<(), ImmichError> {
        let merged = merge_health(stored, health);
        let current = HealthRecord {
            last_ok_at: stored.last_ok_at,
            last_error: stored.last_error.clone(),
            key_rejected: stored.key_rejected,
        };
        if merged != current {
            store::record_health(&self.inner.db, &stored.api_key_id, &merged).await?;
            stored.last_ok_at = merged.last_ok_at;
            stored.last_error = merged.last_error;
            stored.key_rejected = merged.key_rejected;
        }
        Ok(())
    }
}

fn connection_dto(stored: &StoredConnection) -> ImmichConnectionDto {
    ImmichConnectionDto {
        server_url: stored.server_url.clone(),
        email: stored.email.clone(),
        user_name: stored.user_name.clone(),
        is_admin: stored.is_admin,
        api_key_name: API_KEY_NAME.to_owned(),
        allow_insecure_tls: stored.allow_insecure_tls,
        server_version: stored.server_version.clone(),
        connected_at: stored.connected_at,
        last_ok_at: stored.last_ok_at,
        last_error: stored.last_error.clone(),
    }
}

/// What to remember of the health of `client` next to what the row already says: the client knows
/// the calls made since it was built, the row the ones before the last restart.
fn merge_health(stored: &StoredConnection, health: &Health) -> HealthRecord {
    let worked_last = health.last_error.is_none() && health.last_ok_at.is_some();
    let failed_last = health.last_error.is_some();
    HealthRecord {
        last_ok_at: stored.last_ok_at.max(health.last_ok_at),
        last_error: if worked_last || failed_last {
            health.last_error.clone()
        } else {
            stored.last_error.clone()
        },
        // A failure that is not a rejection says nothing new about the key.
        key_rejected: if health.key_rejected {
            true
        } else if worked_last {
            false
        } else {
            stored.key_rejected
        },
    }
}

/// Whether the server answered the latest call. A rejected API key is an answer: Immich is there,
/// the key is what is wrong.
fn server_answered(last_error: Option<&str>, key_rejected: bool) -> bool {
    match last_error {
        None => true,
        Some(error) => key_rejected && error == ImmichError::Unauthorized.summary(),
    }
}

/// A warning for a server version outside what the application was written and tested for.
fn version_warning(version: Option<&str>) -> Option<String> {
    let parsed = ImmichVersion::parse(version?)?;
    if parsed.major < MIN_MAJOR_VERSION {
        Some(format!(
            "Immich {parsed} is older than Immich {MIN_MAJOR_VERSION}, the oldest version this \
             application supports; some features may not work."
        ))
    } else if parsed.major > SPEC_VERSION.major {
        Some(format!(
            "Immich {parsed} is newer than Immich {SPEC_VERSION}, the version this application \
             was tested with; some features may not work."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> StoredConnection {
        StoredConnection {
            server_url: "http://immich:2283".to_owned(),
            email: "me@example.org".to_owned(),
            user_id: "user-1".to_owned(),
            user_name: "Me".to_owned(),
            is_admin: true,
            api_key_id: "key-1".to_owned(),
            api_key_enc: "v1.encrypted".to_owned(),
            allow_insecure_tls: false,
            server_version: Some("v3.4.0".to_owned()),
            connected_at: 100,
            last_ok_at: Some(100),
            last_error: None,
            key_rejected: false,
        }
    }

    fn health(last_ok_at: Option<i64>, last_error: Option<&str>, key_rejected: bool) -> Health {
        Health {
            last_ok_at,
            last_error: last_error.map(str::to_owned),
            key_rejected,
        }
    }

    #[test]
    fn a_client_that_has_not_been_called_changes_nothing() {
        let merged = merge_health(&stored(), &Health::default());
        assert_eq!(
            merged,
            HealthRecord {
                last_ok_at: Some(100),
                last_error: None,
                key_rejected: false,
            }
        );
        let mut failing = stored();
        failing.last_error = Some("Immich is unreachable: connection refused".to_owned());
        failing.key_rejected = true;
        let merged = merge_health(&failing, &Health::default());
        assert_eq!(merged.last_error, failing.last_error);
        assert!(merged.key_rejected);
    }

    #[test]
    fn the_latest_call_decides_the_error_and_the_newest_success_the_time() {
        let mut row = stored();
        row.last_error = Some("old failure".to_owned());
        let merged = merge_health(&row, &health(Some(250), None, false));
        assert_eq!(merged.last_ok_at, Some(250));
        assert_eq!(merged.last_error, None);

        let merged = merge_health(
            &stored(),
            &health(Some(250), Some("Immich answered 500"), false),
        );
        assert_eq!(merged.last_ok_at, Some(250));
        assert_eq!(merged.last_error.as_deref(), Some("Immich answered 500"));

        let merged = merge_health(&stored(), &health(None, Some("Immich answered 500"), false));
        assert_eq!(merged.last_ok_at, Some(100), "the row's success is kept");
    }

    #[test]
    fn a_rejected_key_is_only_cleared_by_a_success() {
        let mut row = stored();
        row.key_rejected = true;
        row.last_error = Some(ImmichError::Unauthorized.summary());

        let unreachable = health(None, Some("Immich is unreachable: timed out"), false);
        assert!(
            merge_health(&row, &unreachable).key_rejected,
            "an outage says nothing about the key"
        );
        assert!(!merge_health(&row, &health(Some(300), None, false)).key_rejected);
        assert!(merge_health(&stored(), &health(None, Some("rejected"), true)).key_rejected);
    }

    #[test]
    fn a_rejected_key_still_means_the_server_answered() {
        let rejection = ImmichError::Unauthorized.summary();
        assert!(server_answered(None, false));
        assert!(server_answered(Some(&rejection), true));
        assert!(!server_answered(
            Some("Immich is unreachable: refused"),
            false
        ));
        assert!(
            !server_answered(Some("Immich is unreachable: refused"), true),
            "a later outage is not an answer"
        );
        assert!(!server_answered(Some(&rejection), false));
    }

    #[test]
    fn versions_outside_the_supported_range_get_a_warning() {
        assert_eq!(version_warning(None), None);
        assert_eq!(version_warning(Some("dev build")), None);
        assert_eq!(version_warning(Some("v2.0.0")), None);
        assert_eq!(version_warning(Some("v3.4.0")), None);
        assert_eq!(version_warning(Some("v3.9.1")), None);
        let old = version_warning(Some("v1.130.2")).unwrap();
        assert!(old.contains("older") && old.contains("1.130.2"), "{old}");
        let new = version_warning(Some("v4.0.0")).unwrap();
        assert!(new.contains("newer") && new.contains("4.0.0"), "{new}");
    }

    #[test]
    fn the_connection_view_has_no_secret() {
        let dto = connection_dto(&stored());
        let wire = serde_json::to_string(&dto).unwrap();
        assert!(
            !wire.contains("encrypted") && !wire.contains("key-1"),
            "{wire}"
        );
        assert_eq!(dto.api_key_name, API_KEY_NAME);
        assert_eq!(dto.user_name, "Me");
    }
}
