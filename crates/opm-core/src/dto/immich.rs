//! Immich integration DTOs.
//!
//! Types are added in the marked sections below so that parallel changes merge
//! cleanly.
//!
//! Planned types: the connect request (never carries a stored secret back), the
//! connection and status views, the library list, the library-to-volume mapping
//! request, the sync request and the exclude-trash result.

// ---- Connection, libraries, mappings, status (todo `building-immich-client`) ----
//
// The section lives in its own module so that its imports and tests never collide with the
// other section when both are edited.

pub use connection::*;

mod connection {
    use std::fmt;

    use serde::{Deserialize, Serialize};
    use ts_rs::TS;

    use super::asset_sync::SyncSummary;
    use super::outbox::OutboxSummary;
    use crate::paths::VolumeId;

    /// Body of `POST /api/immich/connect`.
    ///
    /// The password is used once to create an API key and is neither stored nor logged; `Debug`
    /// hides it and the server address.
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichConnectRequest {
        /// Address of the Immich server, for example `http://192.168.1.10:2283`. A trailing `/`
        /// or `/api` is accepted and removed; a sub-path (reverse proxy) is kept.
        pub server_url: String,
        /// Email address of the Immich account. An administrator account allows managing
        /// libraries; any other account connects in limited mode.
        pub email: String,
        pub password: String,
        /// Accept a certificate that is not trusted (self-signed). Only meant for servers on the
        /// local network.
        #[serde(default)]
        pub allow_insecure_tls: bool,
    }

    impl fmt::Debug for ImmichConnectRequest {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ImmichConnectRequest")
                .field("server_url", &"[redacted]")
                .field("email", &self.email)
                .field("password", &"[redacted]")
                .field("allow_insecure_tls", &self.allow_insecure_tls)
                .finish()
        }
    }

    /// How much of Immich the stored connection can use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
    #[serde(rename_all = "snake_case")]
    #[ts(export)]
    pub enum ImmichMode {
        /// Administrator account: libraries can be listed and changed.
        Full,
        /// Regular account: assets can be searched and updated, but libraries cannot be managed.
        Limited,
        /// No connection is stored.
        Disconnected,
    }

    /// The stored connection. It never contains the API key or the password.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichConnectionDto {
        /// Normalised server address, without credentials.
        pub server_url: String,
        /// Email address of the account the API key belongs to.
        pub email: String,
        /// Display name of that account.
        pub user_name: String,
        pub is_admin: bool,
        /// Name of the API key created in Immich; remove it there if disconnecting could not.
        pub api_key_name: String,
        pub allow_insecure_tls: bool,
        /// Immich version reported when connecting or when last checked, for example `v3.4.0`.
        pub server_version: Option<String>,
        /// Epoch seconds.
        pub connected_at: i64,
        /// Epoch seconds of the last successful call to Immich.
        pub last_ok_at: Option<i64>,
        /// Why the last call to Immich failed; `null` after a success.
        pub last_error: Option<String>,
    }

    /// One volume mapped to an Immich library.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichMappingDto {
        pub volume_id: VolumeId,
        pub volume_name: String,
        /// Immich library holding the files of the volume; `null` in limited mode, where
        /// libraries cannot be listed.
        pub library_id: Option<String>,
        /// Library name as last seen in Immich (empty when unknown).
        pub library_name: String,
        /// The path Immich sees as the root of the volume, for example `/mnt/photo`. A file
        /// `<volume root>/a/b.jpg` is `<path_prefix>/a/b.jpg` for Immich.
        pub path_prefix: String,
    }

    /// Connection state for the Immich page.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichStatusDto {
        /// Whether a connection is stored; the same as `mode` not being `disconnected`.
        pub connected: bool,
        pub mode: ImmichMode,
        /// The stored connection; `null` when disconnected.
        pub connection: Option<ImmichConnectionDto>,
        /// Whether Immich answered the last call (or the check when `check=true` was requested).
        /// `null` when disconnected.
        pub reachable: Option<bool>,
        /// The stored API key cannot be used any more (it cannot be decrypted, or Immich
        /// rejected it): connect again.
        pub needs_reconnect: bool,
        /// Same as `connection.server_version`.
        pub server_version: Option<String>,
        /// Set when the Immich version is older or newer than what this application supports.
        pub version_warning: Option<String>,
        pub mappings: Vec<ImmichMappingDto>,
        /// Epoch seconds of the newest completed asset sync (the same as `sync.last_sync_at`);
        /// `null` until a sync completed.
        pub last_sync_at: Option<i64>,
        /// What the asset sync found, from stored data only (no network). `null` when
        /// disconnected and before the first sync attempt.
        pub sync: Option<SyncSummary>,
        /// File operations waiting to be mirrored to Immich (the same as `outbox.pending`; `0`
        /// when disconnected).
        pub outbox_pending: u32,
        /// Mirrored operations that need a decision from the user (the same as
        /// `outbox.attention`; `0` when disconnected).
        pub needs_attention: u32,
        /// How the outbox of file operations stands, from stored data only. `null` when
        /// disconnected.
        pub outbox: Option<OutboxSummary>,
        /// Persisted result of the last executor probe, not a promise of current availability.
        pub direct_database: ImmichDirectDatabaseStatus,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichDirectDatabaseStatus {
        pub ready: bool,
        pub reason: String,
        pub checked_at: Option<i64>,
        pub warning: String,
    }

    /// Result of `DELETE /api/immich/connection`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichDisconnectResult {
        /// Whether the API key was revoked in Immich. When `false` the connection is removed
        /// anyway; the key (see `ImmichConnectionDto.api_key_name`) must then be removed by hand
        /// in Immich under Account settings, API keys.
        pub revoked: bool,
        /// Why the key could not be revoked.
        pub message: Option<String>,
    }

    /// An Immich external library.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichLibraryDto {
        pub id: String,
        pub name: String,
        /// Folders (as Immich sees them) the library scans.
        pub import_paths: Vec<String>,
        /// Glob patterns of paths Immich ignores.
        pub exclusion_patterns: Vec<String>,
        pub asset_count: u64,
    }

    /// Body of `GET /api/immich/libraries`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichLibrariesResponse {
        pub libraries: Vec<ImmichLibraryDto>,
        /// The account is not an administrator, so Immich does not list libraries to it.
        /// `libraries` is then empty.
        pub limited: bool,
    }

    /// One volume of a mapping request.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichMappingEntry {
        pub volume_id: VolumeId,
        /// Required in full mode (it must be an existing library), optional in limited mode.
        pub library_id: Option<String>,
        /// Absolute path without a trailing `/`, without `.` or `..` components and without
        /// `//`. `/` alone is only allowed as the sole mapping. No prefix may equal or contain
        /// another.
        pub path_prefix: String,
    }

    /// Body of `PUT /api/immich/mappings`: the complete set, replacing the stored one.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichMappingsRequest {
        pub mappings: Vec<ImmichMappingEntry>,
    }

    /// Body of the answer to `PUT /api/immich/mappings`: the stored set.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichMappingsResponse {
        pub mappings: Vec<ImmichMappingDto>,
    }

    /// A library named in an exclude-trash result.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichLibraryRef {
        pub id: String,
        pub name: String,
    }

    /// A library that could not be updated.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichLibraryFailure {
        pub id: String,
        pub name: String,
        pub message: String,
    }

    /// Body of the answer to `POST /api/immich/exclude-trash`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichExcludeTrashResult {
        /// The exclusion pattern that keeps the application's trash folder out of the
        /// libraries.
        pub pattern: String,
        /// Libraries that received the pattern.
        pub updated: Vec<ImmichLibraryRef>,
        /// Libraries that already had it.
        pub already_excluded: Vec<ImmichLibraryRef>,
        pub failed: Vec<ImmichLibraryFailure>,
    }

    /// Body of `POST /api/immich/detect-prefix`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichDetectPrefixRequest {
        pub volume_id: VolumeId,
        /// Only look at assets of this library.
        pub library_id: Option<String>,
    }

    /// A path prefix that fits some of the sampled files.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichPrefixSuggestion {
        pub prefix: String,
        /// Sampled files that Immich knows under this prefix.
        pub matched: u32,
        /// Files of the volume that were looked up in Immich.
        pub sampled: u32,
    }

    /// Body of the answer to `POST /api/immich/detect-prefix`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichDetectPrefixResponse {
        /// At most five, best first; empty when Immich knows none of the sampled files.
        pub suggestions: Vec<ImmichPrefixSuggestion>,
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;
        use crate::dto::testing::roundtrip;

        fn connection() -> ImmichConnectionDto {
            ImmichConnectionDto {
                server_url: "http://immich.lan:2283".to_owned(),
                email: "me@example.com".to_owned(),
                user_name: "Me".to_owned(),
                is_admin: true,
                api_key_name: "Open Photo Manager".to_owned(),
                allow_insecure_tls: false,
                server_version: Some("v3.4.0".to_owned()),
                connected_at: 1_700_000_000,
                last_ok_at: Some(1_700_000_100),
                last_error: None,
            }
        }

        fn mapping() -> ImmichMappingDto {
            ImmichMappingDto {
                volume_id: 3,
                volume_name: "photo".to_owned(),
                library_id: Some("lib-1".to_owned()),
                library_name: "Photos".to_owned(),
                path_prefix: "/mnt/photo".to_owned(),
            }
        }

        #[test]
        fn the_connect_request_round_trips_and_the_flag_defaults_to_false() {
            let request = ImmichConnectRequest {
                server_url: "http://immich.lan:2283".to_owned(),
                email: "me@example.com".to_owned(),
                password: "hunter2".to_owned(),
                allow_insecure_tls: true,
            };
            roundtrip(&request);

            let parsed: ImmichConnectRequest = serde_json::from_value(json!({
                "server_url": "http://immich.lan",
                "email": "me@example.com",
                "password": "hunter2",
            }))
            .unwrap();
            assert!(!parsed.allow_insecure_tls);
        }

        #[test]
        fn debug_of_the_connect_request_hides_the_password_and_the_address() {
            let request = ImmichConnectRequest {
                server_url: "http://user:secret-userinfo@immich.lan/?token=secret-query".to_owned(),
                email: "me@example.com".to_owned(),
                password: "super-secret-password".to_owned(),
                allow_insecure_tls: false,
            };
            let shown = format!("{request:?} {request:#?}");
            for secret in ["super-secret-password", "secret-userinfo", "secret-query"] {
                assert!(!shown.contains(secret), "{secret} leaked in {shown}");
            }
            assert!(shown.contains("me@example.com"));
        }

        #[test]
        fn the_status_round_trips_in_every_mode() {
            let connected = ImmichStatusDto {
                connected: true,
                mode: ImmichMode::Full,
                connection: Some(connection()),
                reachable: Some(true),
                needs_reconnect: false,
                server_version: Some("v3.4.0".to_owned()),
                version_warning: None,
                mappings: vec![mapping()],
                last_sync_at: None,
                sync: None,
                outbox_pending: 0,
                needs_attention: 0,
                outbox: Some(OutboxSummary {
                    pending: 0,
                    waiting_for_immich: 0,
                    attention: 0,
                    oldest_pending_at: None,
                    last_run_at: None,
                    last_error: None,
                    scans_on_demand: true,
                }),
                direct_database: ImmichDirectDatabaseStatus {
                    ready: false,
                    reason: "not checked".to_owned(),
                    checked_at: None,
                    warning: "Keep a current backup".to_owned(),
                },
            };
            let wire = roundtrip(&connected);
            assert_eq!(wire["mode"], "full");
            assert_eq!(wire["mappings"][0]["path_prefix"], "/mnt/photo");
            assert_eq!(wire["sync"], json!(null));
            assert_eq!(wire["outbox"]["scans_on_demand"], json!(true));

            let disconnected = ImmichStatusDto {
                connected: false,
                mode: ImmichMode::Disconnected,
                connection: None,
                reachable: None,
                needs_reconnect: false,
                server_version: None,
                version_warning: None,
                mappings: Vec::new(),
                last_sync_at: None,
                sync: None,
                outbox_pending: 0,
                needs_attention: 0,
                outbox: None,
                direct_database: connected.direct_database.clone(),
            };
            assert_eq!(roundtrip(&disconnected)["mode"], "disconnected");
            assert_eq!(roundtrip(&disconnected)["outbox"], json!(null));
            assert_eq!(
                serde_json::to_value(ImmichMode::Limited).unwrap(),
                json!("limited")
            );
        }

        #[test]
        fn the_connection_view_has_no_secret_field() {
            let wire = roundtrip(&connection());
            for key in wire.as_object().unwrap().keys() {
                assert!(
                    !key.contains("secret") && !key.contains("password") && !key.contains("enc"),
                    "unexpected field {key}"
                );
            }
        }

        #[test]
        fn the_other_views_round_trip() {
            roundtrip(&ImmichDisconnectResult {
                revoked: false,
                message: Some("Immich is unreachable".to_owned()),
            });
            roundtrip(&ImmichLibrariesResponse {
                libraries: vec![ImmichLibraryDto {
                    id: "lib-1".to_owned(),
                    name: "Photos".to_owned(),
                    import_paths: vec!["/mnt/photo".to_owned()],
                    exclusion_patterns: vec!["**/.opm-trash/**".to_owned()],
                    asset_count: 12,
                }],
                limited: false,
            });
            roundtrip(&ImmichMappingsRequest {
                mappings: vec![ImmichMappingEntry {
                    volume_id: 1,
                    library_id: None,
                    path_prefix: "/mnt/photo".to_owned(),
                }],
            });
            roundtrip(&ImmichMappingsResponse {
                mappings: vec![mapping()],
            });
            roundtrip(&ImmichExcludeTrashResult {
                pattern: "**/.opm-trash/**".to_owned(),
                updated: vec![ImmichLibraryRef {
                    id: "a".to_owned(),
                    name: "A".to_owned(),
                }],
                already_excluded: Vec::new(),
                failed: vec![ImmichLibraryFailure {
                    id: "b".to_owned(),
                    name: "B".to_owned(),
                    message: "Immich answered 500".to_owned(),
                }],
            });
            roundtrip(&ImmichDetectPrefixRequest {
                volume_id: 2,
                library_id: Some("lib-1".to_owned()),
            });
            roundtrip(&ImmichDetectPrefixResponse {
                suggestions: vec![ImmichPrefixSuggestion {
                    prefix: "/mnt/photo".to_owned(),
                    matched: 28,
                    sampled: 30,
                }],
            });
        }
    }
}

// ---- Asset sync (todo `syncing-immich-assets`) ----
//
// Own module, like the section above: its imports and tests never collide with the other sections.

pub use asset_sync::*;

mod asset_sync {
    use serde::{Deserialize, Serialize};
    use ts_rs::TS;

    /// Body of `POST /api/immich/sync`. The body may be omitted: that is `{"full": false}`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ImmichSyncRequest {
        /// Read every asset again instead of only what changed since the last sync. A full read
        /// also notices assets that disappeared from Immich.
        #[serde(default)]
        pub full: bool,
    }

    /// What the asset sync knows about one scope: the part of Immich it reads in one go. In full
    /// mode a scope is a mapped library, in limited mode (no library can be named) it is a mapped
    /// path prefix.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct SyncScopeSummary {
        /// Opaque key of the scope: the library id, or `path:` followed by the prefix.
        pub scope: String,
        /// The Immich library of the scope; `null` for a path prefix scope.
        pub library_id: Option<String>,
        /// Library name as last seen in Immich; `null` when unknown.
        pub library_name: Option<String>,
        /// The mapped path prefix of a path prefix scope; `null` for a library scope.
        pub path_prefix: Option<String>,
        /// Assets of the scope known to the application: live, offline and trashed ones.
        pub assets_total: i64,
        /// Assets that are neither offline nor trashed in Immich.
        pub assets_live: i64,
        /// Assets whose path maps to a volume AND to a file the index knows. A wrong path
        /// mapping shows up here as `0` next to a large `assets_total`.
        pub assets_matched: i64,
        /// Assets whose path is outside every mapped volume (only counted in full mode, where the
        /// whole library is read).
        pub assets_unmapped: i64,
        /// Epoch seconds of the last completed read of the scope; `null` before the first one.
        pub last_sync_at: Option<i64>,
        /// Epoch seconds of the last completed FULL read of the scope.
        pub last_full_at: Option<i64>,
        /// Why the last attempt failed; `null` after a success.
        pub last_error: Option<String>,
        /// Failed attempts since the last success; the sync waits longer after each one.
        pub consecutive_failures: i64,
    }

    /// What the asset sync found in Immich, summed over every current scope, with one row per
    /// scope so the page can say which library is failing. Also the result of a sync task.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct SyncSummary {
        pub assets_total: i64,
        pub assets_live: i64,
        pub assets_matched: i64,
        pub assets_unmapped: i64,
        /// Epoch seconds of the newest completed read of any scope; `null` before the first one.
        pub last_sync_at: Option<i64>,
        /// Epoch seconds of the newest completed FULL read of any scope.
        pub last_full_at: Option<i64>,
        /// The message of the first scope whose last attempt failed; `null` when none did.
        pub last_error: Option<String>,
        pub scopes: Vec<SyncScopeSummary>,
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;
        use crate::dto::testing::roundtrip;

        fn scope(key: &str) -> SyncScopeSummary {
            SyncScopeSummary {
                scope: key.to_owned(),
                library_id: Some("lib-1".to_owned()),
                library_name: Some("Photos".to_owned()),
                path_prefix: None,
                assets_total: 12,
                assets_live: 9,
                assets_matched: 8,
                assets_unmapped: 2,
                last_sync_at: Some(1_700_000_200),
                last_full_at: Some(1_700_000_100),
                last_error: None,
                consecutive_failures: 0,
            }
        }

        #[test]
        fn the_sync_request_defaults_to_an_incremental_sync() {
            roundtrip(&ImmichSyncRequest { full: true });
            let parsed: ImmichSyncRequest = serde_json::from_value(json!({})).unwrap();
            assert_eq!(parsed, ImmichSyncRequest::default());
            assert!(!parsed.full);
        }

        #[test]
        fn the_summary_round_trips_with_its_scopes() {
            let failing = SyncScopeSummary {
                scope: "path:/mnt/photo".to_owned(),
                library_id: None,
                library_name: None,
                path_prefix: Some("/mnt/photo".to_owned()),
                last_sync_at: None,
                last_full_at: None,
                last_error: Some("Immich is unreachable".to_owned()),
                consecutive_failures: 3,
                ..scope("path:/mnt/photo")
            };
            let summary = SyncSummary {
                assets_total: 24,
                assets_live: 18,
                assets_matched: 16,
                assets_unmapped: 4,
                last_sync_at: Some(1_700_000_200),
                last_full_at: Some(1_700_000_100),
                last_error: failing.last_error.clone(),
                scopes: vec![scope("lib-1"), failing],
            };
            let wire = roundtrip(&summary);
            assert_eq!(wire["scopes"][0]["library_id"], "lib-1");
            assert_eq!(wire["scopes"][1]["library_id"], json!(null));
            assert_eq!(wire["scopes"][1]["consecutive_failures"], 3);
            assert_eq!(wire["last_error"], "Immich is unreachable");
        }
    }
}

// ---- Outbox (todo `reflecting-file-ops-in-immich`) ----
//
// Own module, like the sections above: its imports and tests never collide with the other sections.

pub use outbox::*;

mod outbox {
    use serde::{Deserialize, Serialize};
    use ts_rs::TS;

    /// The most ids one retry or dismiss request may name.
    pub const OUTBOX_MAX_IDS: usize = 10_000;

    /// Where the outbox of file operations stands: the moves, trashes, copies and restores that
    /// still have to show up in Immich. Part of the Immich status as `outbox`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxSummary {
        /// Changes still to be mirrored: every pending one, including those that wait for Immich.
        pub pending: i64,
        /// Of `pending`, the changes whose library scan was requested: they wait for Immich to
        /// notice the new state of the files.
        pub waiting_for_immich: i64,
        /// Changes that need a decision from the user (retry or dismiss).
        pub attention: i64,
        /// Epoch seconds of the oldest pending change (a change of a file that merged into a
        /// waiting one counts from its own moment); `null` when none is pending.
        pub oldest_pending_at: Option<i64>,
        /// Epoch seconds of the end of the latest worker run, successful or not; `null` before
        /// the first one.
        pub last_run_at: Option<i64>,
        /// Why the latest run failed as a whole (Immich unreachable, key rejected, ...); `null`
        /// after a success.
        pub last_error: Option<String>,
        /// Whether the connected account may ask Immich to scan a library (an administrator).
        /// When `false` the application waits for Immich's own scan schedule, which can take a
        /// day.
        pub scans_on_demand: bool,
    }

    /// What a queued change is about.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    #[serde(rename_all = "snake_case")]
    pub enum OutboxKind {
        /// A file moved or was renamed: `from` and `to` are both set.
        Moved,
        /// A file left the library, for instance into the trash: only `from` is set.
        Removed,
        /// A file appeared, for instance a copy or a restore: only `to` is set.
        Added,
    }

    /// Whether a queued change is still being worked on or needs a decision.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    #[serde(rename_all = "snake_case")]
    pub enum OutboxState {
        Pending,
        Attention,
    }

    /// One queued change, for the outbox card of the Immich page.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxItemDto {
        pub id: i64,
        pub kind: OutboxKind,
        /// Where the file was, as `<volume name>/<folder>/<file name>`; `null` for an added file.
        pub from: Option<String>,
        /// Where the file is now, in the same form; `null` for a removed file.
        pub to: Option<String>,
        pub state: OutboxState,
        /// How many times the worker tried this change.
        pub attempts: i64,
        /// Why the last try failed, or why the change needs attention: a short code. While the
        /// change is pending only `unreadable` (the volume of the file could not be read) and
        /// `immich_unavailable` (Immich failed or could not be reached) show. A change that needs
        /// attention holds the reason it gave up: `not_offline` (Immich still shows the old asset
        /// online), `scan_timeout` (Immich has not noticed the change), `carry_failed` (Immich
        /// refused to take over the albums, favorite, rating or tags), `path_mismatch` (the asset
        /// Immich holds is not at the path of the file), `immich_error` (Immich refused a request
        /// for good), or `unreadable` or `immich_unavailable` after too many tries.
        pub last_error: Option<String>,
        /// Epoch seconds of the latest change recorded for this row.
        pub created_at: i64,
        /// Epoch seconds: the change is not tried before this instant.
        pub next_attempt_at: i64,
    }

    /// Answer of `GET /api/immich/outbox`: one page of queued changes, oldest first.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxPage {
        pub items: Vec<OutboxItemDto>,
        /// Every change of the requested state, not only this page.
        pub total: i64,
    }

    /// Body of `POST /api/immich/outbox/retry`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxRetryRequest {
        /// The changes to try again (at most 10,000); `null` or omitted means every change that
        /// needs attention or waits for its next try.
        #[serde(default)]
        pub ids: Option<Vec<i64>>,
    }

    /// Answer of `POST /api/immich/outbox/retry`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxRetryResult {
        /// How many changes the call changed: those that needed attention or waited for their next
        /// try are due now, with a fresh count of tries. A change that is due already is not
        /// counted. Whatever the number, the call also ends the back-off of failed runs and lets
        /// the next run ask Immich to scan again even if it did so a minute ago.
        pub retried: i64,
    }

    /// Body of `POST /api/immich/outbox/dismiss`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxDismissRequest {
        /// The changes to forget (at most 10,000). Immich is not touched.
        pub ids: Vec<i64>,
    }

    /// Answer of `POST /api/immich/outbox/dismiss`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct OutboxDismissResult {
        /// How many changes were dropped.
        pub dismissed: i64,
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;
        use crate::dto::testing::roundtrip;

        fn item(kind: OutboxKind, state: OutboxState) -> OutboxItemDto {
            OutboxItemDto {
                id: 7,
                kind,
                from: (kind != OutboxKind::Added).then(|| "photos/2019/a.jpg".to_owned()),
                to: (kind != OutboxKind::Removed).then(|| "archive/b.jpg".to_owned()),
                state,
                attempts: 2,
                last_error: Some("not_offline".to_owned()),
                created_at: 1_700_000_000,
                next_attempt_at: 1_700_000_900,
            }
        }

        #[test]
        fn the_summary_round_trips() {
            let summary = OutboxSummary {
                pending: 12,
                waiting_for_immich: 5,
                attention: 1,
                oldest_pending_at: Some(1_700_000_000),
                last_run_at: Some(1_700_000_100),
                last_error: Some("Immich is unreachable".to_owned()),
                scans_on_demand: false,
            };
            let wire = roundtrip(&summary);
            assert_eq!(wire["waiting_for_immich"], 5);
            assert_eq!(wire["scans_on_demand"], json!(false));

            let idle = OutboxSummary {
                pending: 0,
                waiting_for_immich: 0,
                attention: 0,
                oldest_pending_at: None,
                last_run_at: None,
                last_error: None,
                scans_on_demand: true,
            };
            assert_eq!(roundtrip(&idle)["oldest_pending_at"], json!(null));
        }

        #[test]
        fn kinds_and_states_are_snake_case_words() {
            assert_eq!(serde_json::to_value(OutboxKind::Moved).unwrap(), "moved");
            assert_eq!(
                serde_json::to_value(OutboxKind::Removed).unwrap(),
                "removed"
            );
            assert_eq!(serde_json::to_value(OutboxKind::Added).unwrap(), "added");
            assert_eq!(
                serde_json::to_value(OutboxState::Attention).unwrap(),
                "attention"
            );
            assert_eq!(
                serde_json::from_value::<OutboxState>(json!("pending")).unwrap(),
                OutboxState::Pending
            );
        }

        #[test]
        fn a_page_round_trips_with_every_kind_of_item() {
            let page = OutboxPage {
                items: vec![
                    item(OutboxKind::Moved, OutboxState::Pending),
                    item(OutboxKind::Removed, OutboxState::Attention),
                    item(OutboxKind::Added, OutboxState::Pending),
                ],
                total: 3,
            };
            let wire = roundtrip(&page);
            assert_eq!(wire["items"][0]["kind"], "moved");
            assert_eq!(wire["items"][1]["to"], json!(null));
            assert_eq!(wire["items"][2]["from"], json!(null));
            assert_eq!(wire["items"][1]["state"], "attention");
        }

        #[test]
        fn retry_and_dismiss_requests_parse_their_bodies() {
            let everything: OutboxRetryRequest = serde_json::from_value(json!({})).unwrap();
            assert_eq!(everything.ids, None);
            let some: OutboxRetryRequest = serde_json::from_value(json!({"ids": [1, 2]})).unwrap();
            assert_eq!(some.ids, Some(vec![1, 2]));
            roundtrip(&some);
            roundtrip(&OutboxDismissRequest { ids: vec![3] });
            assert!(serde_json::from_value::<OutboxDismissRequest>(json!({})).is_err());
            assert_eq!(roundtrip(&OutboxRetryResult { retried: 4 })["retried"], 4);
            assert_eq!(
                roundtrip(&OutboxDismissResult { dismissed: 2 })["dismissed"],
                2
            );
        }
    }
}
