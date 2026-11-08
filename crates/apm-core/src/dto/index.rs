//! Index control DTOs.
//!
//! Owned by todo `building-index-controls`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::common::Scope;
use crate::paths::{TaskId, VolumeId};

/// Body of `POST /api/index/rescan`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct RescanRequest {
    /// Scopes to rescan; empty means every volume. A scope with a null `volume_id` and an empty
    /// `dir` stands for every online volume.
    pub targets: Vec<Scope>,
    /// Re-process every file in scope (metadata, checksums and thumbnails) even when its size and
    /// modification time are unchanged. It does not apply index removals the safety guard held
    /// back: that is `confirm_removals`.
    pub force: bool,
    /// Apply the index removals the mass-disappearance guard held back for the volumes in scope,
    /// after the user confirmed that the files are really gone. No file is touched on disk.
    pub confirm_removals: bool,
}

/// One volume whose index removals the mass-disappearance guard is holding back.
///
/// A scan that finds many indexed files missing at once (an unmounted share looks exactly like
/// that) keeps their index rows instead of deleting them, and reports how many it kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct HeldRemovalsDto {
    pub volume_id: VolumeId,
    pub volume_name: String,
    /// Index entries kept although their files were not found on disk.
    pub files: u64,
}

/// Response of `GET /api/index/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct IndexStatusDto {
    /// Volumes with held removals, ordered by volume name. Empty when nothing is held.
    pub held_removals: Vec<HeldRemovalsDto>,
    /// The index task that is running, else the oldest one that is queued; null when the index is
    /// idle. The task itself comes from `GET /api/tasks`.
    pub active_task_id: Option<TaskId>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    #[test]
    fn rescan_request_roundtrips_and_defaults_to_everything() {
        let wire = roundtrip(&RescanRequest {
            targets: vec![Scope {
                volume_id: Some(1),
                dir: "2024".to_owned(),
                recursive: true,
            }],
            force: true,
            confirm_removals: true,
        });
        assert_eq!(wire["targets"][0]["dir"], json!("2024"));
        assert_eq!(wire["force"], json!(true));
        assert_eq!(wire["confirm_removals"], json!(true));
        let parsed: RescanRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, RescanRequest::default());
        assert!(!parsed.force && !parsed.confirm_removals);
    }

    #[test]
    fn a_rescan_request_without_the_new_flag_still_parses() {
        let parsed: RescanRequest = serde_json::from_str(r#"{"targets":[],"force":true}"#).unwrap();
        assert!(parsed.force);
        assert!(!parsed.confirm_removals);
    }

    #[test]
    fn index_status_roundtrips_and_an_idle_index_is_empty() {
        let wire = roundtrip(&IndexStatusDto {
            held_removals: vec![HeldRemovalsDto {
                volume_id: 2,
                volume_name: "photos".to_owned(),
                files: 12_345,
            }],
            active_task_id: Some(9),
        });
        assert_eq!(wire["held_removals"][0]["volume_id"], json!(2));
        assert_eq!(wire["held_removals"][0]["volume_name"], json!("photos"));
        assert_eq!(wire["held_removals"][0]["files"], json!(12_345));
        assert_eq!(wire["active_task_id"], json!(9));

        let idle = roundtrip(&IndexStatusDto::default());
        assert_eq!(idle["held_removals"], json!([]));
        assert!(idle["active_task_id"].is_null());
    }
}
