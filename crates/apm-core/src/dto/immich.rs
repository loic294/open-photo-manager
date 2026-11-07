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
        /// Epoch seconds of the last completed asset sync. Filled by the asset sync; `null`
        /// until then.
        pub last_sync_at: Option<i64>,
        /// File operations waiting to be mirrored to Immich. Filled by the Immich outbox; `0`
        /// until then.
        pub outbox_pending: u32,
        /// Mirrored operations that need a decision from the user. Filled by the Immich outbox;
        /// `0` until then.
        pub needs_attention: u32,
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
                api_key_name: "Advanced Photo Manager".to_owned(),
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
                outbox_pending: 0,
                needs_attention: 0,
            };
            let wire = roundtrip(&connected);
            assert_eq!(wire["mode"], "full");
            assert_eq!(wire["mappings"][0]["path_prefix"], "/mnt/photo");

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
                outbox_pending: 0,
                needs_attention: 0,
            };
            assert_eq!(roundtrip(&disconnected)["mode"], "disconnected");
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
                    exclusion_patterns: vec!["**/.apm-trash/**".to_owned()],
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
                pattern: "**/.apm-trash/**".to_owned(),
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
