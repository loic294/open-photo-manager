//! ISM answers as they arrive, read tolerantly: older and newer servers describe some fields
//! differently, and OPM must understand both.

use opm_core::dto::{IsmInfoDto, IsmRootDto};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct Info {
    instance_id: String,
    instance_name: String,
    version: String,
    api_version: u32,
    token_kind: String,
    actions: Vec<String>,
    roots: Vec<Root>,
    #[serde(default)]
    capabilities: Vec<String>,
}

impl Info {
    pub(super) fn into_dto(self) -> IsmInfoDto {
        IsmInfoDto {
            instance_id: self.instance_id,
            instance_name: self.instance_name,
            version: self.version,
            api_version: self.api_version,
            token_kind: self.token_kind,
            actions: self.actions,
            roots: self.roots.into_iter().map(Root::into_dto).collect(),
            capabilities: self.capabilities,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Root {
    root_id: String,
    name: String,
    #[serde(default)]
    immich_import_path: Option<String>,
    #[serde(default)]
    opm_prefix: Option<String>,
    available: bool,
    direct_writes: DirectWrites,
    #[serde(default)]
    direct_writes_reason: Option<String>,
    #[serde(default)]
    blocked_reason: Option<String>,
    #[serde(default)]
    direct_writes_detail: Option<String>,
}

/// `"enabled"`, or `{"state": "blocked", "reason": "…"}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DirectWrites {
    State(String),
    Detailed {
        #[serde(alias = "status")]
        state: String,
        #[serde(default)]
        reason: Option<String>,
    },
}

impl Root {
    fn into_dto(self) -> IsmRootDto {
        let (state, reason) = match self.direct_writes {
            DirectWrites::State(state) => (state, None),
            DirectWrites::Detailed { state, reason } => (state, reason),
        };
        IsmRootDto {
            root_id: self.root_id,
            name: self.name,
            immich_import_path: self.immich_import_path,
            opm_prefix: self.opm_prefix,
            available: self.available,
            direct_writes: state,
            direct_writes_reason: with_detail(
                reason.or(self.direct_writes_reason).or(self.blocked_reason),
                self.direct_writes_detail,
            ),
        }
    }
}

/// The reason code, followed by ISM's detail sentence when it gives one.
fn with_detail(reason: Option<String>, detail: Option<String>) -> Option<String> {
    let reason = reason.filter(|reason| !reason.trim().is_empty());
    let detail = detail.filter(|detail| !detail.trim().is_empty());
    match (reason, detail) {
        (Some(reason), Some(detail)) => Some(format!("{reason}: {detail}")),
        (reason, detail) => reason.or(detail),
    }
}

/// Whether ISM changes files of `root` for OPM now: when its Immich link allows it, or when no
/// Immich library is linked to it at all.
pub(crate) fn writes_allowed(root: &IsmRootDto) -> bool {
    match root.direct_writes.as_str() {
        "enabled" | "not_linked" => true,
        "blocked" => false,
        // `disabled` (or a state this OPM does not know) only gates Immich-linked changes:
        // servers before `not_linked` say `disabled` for a root without a library.
        _ => root.immich_import_path.is_none(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Location {
    pub root_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TrashEntry {
    pub trash_id: String,
    pub location: Location,
    #[serde(default)]
    pub is_folder: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub trashed_at: String,
    #[serde(default)]
    pub asset_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(direct_writes: serde_json::Value, extra: serde_json::Value) -> IsmRootDto {
        let mut value = serde_json::json!({
            "root_id": "photos", "name": "Photos", "immich_import_path": "/import",
            "opm_prefix": "/photos", "available": true, "direct_writes": direct_writes,
        });
        if let (Some(value), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            value.extend(extra.clone());
        }
        serde_json::from_value::<Root>(value).unwrap().into_dto()
    }

    #[test]
    fn direct_writes_are_read_in_every_shape() {
        let plain = root("enabled".into(), serde_json::json!({}));
        assert_eq!(plain.direct_writes, "enabled");
        assert!(writes_allowed(&plain));
        let detailed = root(
            serde_json::json!({"state": "blocked", "reason": "Immich 3.5 is not supported"}),
            serde_json::json!({}),
        );
        assert_eq!(detailed.direct_writes, "blocked");
        assert_eq!(
            detailed.direct_writes_reason.as_deref(),
            Some("Immich 3.5 is not supported")
        );
        assert!(!writes_allowed(&detailed));
        let flat = root(
            "blocked".into(),
            serde_json::json!({"direct_writes_reason": "maintenance"}),
        );
        assert_eq!(flat.direct_writes_reason.as_deref(), Some("maintenance"));
        let explained = root(
            "blocked".into(),
            serde_json::json!({"direct_writes_reason": "drift",
                "direct_writes_detail": "the library watcher is on"}),
        );
        assert_eq!(
            explained.direct_writes_reason.as_deref(),
            Some("drift: the library watcher is on")
        );
        assert!(writes_allowed(&root(
            "not_linked".into(),
            serde_json::json!({})
        )));
        assert!(!writes_allowed(&root(
            "disabled".into(),
            serde_json::json!({})
        )));
        assert!(!writes_allowed(&root(
            "something_new".into(),
            serde_json::json!({})
        )));
        let unlinked = root(
            "disabled".into(),
            serde_json::json!({"immich_import_path": null}),
        );
        assert!(writes_allowed(&unlinked));
    }
}
