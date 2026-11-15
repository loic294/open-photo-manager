//! ISM connection and discovery.

use serde::{Deserialize, Serialize};

use crate::paths::VolumeId;
use ts_rs::TS;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmConnectRequest {
    pub base_url: String,
    pub token: String,
}

impl std::fmt::Debug for IsmConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsmConnectRequest")
            .field("base_url", &"[redacted]")
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmRootDto {
    pub root_id: String,
    pub name: String,
    pub immich_import_path: Option<String>,
    pub opm_prefix: Option<String>,
    pub available: bool,
    /// `enabled`, `not_linked` (no Immich library: plain file changes), `disabled` or `blocked`
    /// (Immich-linked changes refused; `direct_writes_reason` says why when ISM tells).
    pub direct_writes: String,
    #[serde(default)]
    pub direct_writes_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmInfoDto {
    pub instance_id: String,
    pub instance_name: String,
    pub version: String,
    pub api_version: u32,
    pub token_kind: String,
    pub actions: Vec<String>,
    pub roots: Vec<IsmRootDto>,
    /// Optional protocol features the server advertises.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmConnectionDto {
    pub base_url: String,
    pub instance_id: String,
    pub instance_name: String,
}

/// A volume whose file changes go through ISM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmBindingDto {
    pub volume_id: VolumeId,
    pub root_id: String,
    pub root_name: String,
    /// The volume root inside the ISM root ('' when they are the same folder).
    pub root_prefix: String,
}

/// A binding change waiting for the next start. `root_id` is `None` for an unbind; `error` says
/// why a bind could not be applied yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmStagedBindingDto {
    pub volume_id: VolumeId,
    pub root_id: Option<String>,
    pub root_name: String,
    pub error: Option<String>,
}

/// Binds a volume to an ISM root, or unbinds it (`root_id: null`), at the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmBindingRequest {
    pub volume_id: VolumeId,
    pub root_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmStatusDto {
    pub connection: Option<IsmConnectionDto>,
    pub bound_volumes: i64,
    pub bindings: Vec<IsmBindingDto>,
    pub staged: Vec<IsmStagedBindingDto>,
}

/// An item in the ISM trash of a bound volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmTrashItemDto {
    pub trash_id: String,
    /// Where it was, relative to the volume.
    pub path: String,
    pub is_folder: bool,
    pub size: u64,
    /// As ISM reports it (RFC 3339).
    pub trashed_at: String,
    /// Immich assets the item belonged to.
    pub asset_count: u32,
}

/// The ISM trash of one bound volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmTrashDto {
    pub volume_id: VolumeId,
    pub root_id: String,
    pub root_name: String,
    /// Whether the token may purge (delete for good).
    pub can_purge: bool,
    pub items: Vec<IsmTrashItemDto>,
}

/// Restore ISM trash items of a bound volume to where they were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmTrashRestoreRequest {
    pub volume_id: VolumeId,
    pub trash_ids: Vec<String>,
}

/// Delete ISM trash items for good. `confirm` must be true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IsmTrashPurgeRequest {
    pub volume_id: VolumeId,
    pub trash_ids: Vec<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_request_round_trips_without_debugging_secrets() {
        let request = IsmConnectRequest {
            base_url: "https://sync.example".into(),
            token: "fixture-token".into(),
        };
        let copy: IsmConnectRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(copy, request);
        let debug = format!("{request:?}");
        assert!(!debug.contains("fixture-token"));
        assert!(!debug.contains("sync.example"));
    }
}
