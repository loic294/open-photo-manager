//! Types shared by several endpoints: enums, scope and filter, plan references,
//! overlay flags, volumes, the health probe and the error body.
//!
//! Shared by every area: change it only additively (append fields or types) and
//! keep the diff small. The `Scope` and `Filter` shapes belong to todo
//! `building-filter-query-engine`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::paths::{PlanId, SelectionId, VolumeId};

/// Broad classification of an indexed file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Sidecar,
    #[default]
    Other,
}

/// Where a capture timestamp came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CaptureSource {
    /// Embedded EXIF tag.
    Exif,
    /// XMP metadata, embedded or in a sidecar.
    Xmp,
    /// Video container metadata.
    Video,
    /// Filesystem timestamps (the file carries no embedded date).
    Fs,
}

/// Flag states a filter can include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FlagFilter {
    Selected,
    Rejected,
}

/// A directory scope: the unit every query, plan and re-index operates on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct Scope {
    /// Restrict to one volume; `null` means every volume.
    pub volume_id: Option<VolumeId>,
    /// Directory relative to the volume root, `/`-separated. `""` is the root.
    pub dir: String,
    /// Include subdirectories.
    pub recursive: bool,
}

/// Item filter shared by every browse, plan and selection query. The default
/// (all lists empty, no selection) matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct Filter {
    /// Camera names ("Make Model"); an item matches if its camera is any of them.
    pub cameras: Vec<String>,
    /// Star ratings to include: 0 = unrated, 1-5.
    pub ratings: Vec<u8>,
    /// Flag states to include.
    pub flags: Vec<FlagFilter>,
    /// Tags; an item matches if it has any of them.
    pub tags: Vec<String>,
    /// Restrict to the members of a server-side selection.
    pub selection_id: Option<SelectionId>,
}

/// Sort key for item listings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SortKey {
    #[default]
    Name,
    /// Capture date.
    Date,
    Size,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

/// Which side of a plan a pane shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanSide {
    #[default]
    Source,
    Comparison,
}

/// What to do when a destination name is already taken. Shared by the settings (the default
/// policy) and by plan execution; the pure decision logic lives in `apm_core::conflict`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ConflictPolicy {
    /// Identical content is skipped, anything else gets a numbered name.
    #[default]
    Smart,
    /// Leave the source file where it is.
    Skip,
    /// Always pick a free numbered name.
    Rename,
    /// Send the existing file to the trash and take its place (nothing is overwritten in place).
    Replace,
}

/// How the duplicate finder pairs a source item with comparison items. The rules are the contract
/// of `apm_core::matching`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MatchMode {
    /// Same SHA-1 of the content.
    #[default]
    Hash,
    /// Same file name and same capture second.
    NameAndTime,
    /// Same file name, different capture second.
    NameDifferentTime,
    /// The comparison file name starts with the source file name (extensions ignored).
    NamePrefix,
}

/// Reference to a materialized plan seen from one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanRef {
    pub plan_id: PlanId,
    #[serde(default)]
    pub side: PlanSide,
}

/// Plan overlay flags on a file or folder row. All false when no plan is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Overlay {
    /// The item has a match on the other side of the active plan.
    pub matched: bool,
    /// The item is part of the active plan's actions.
    pub planned: bool,
    /// The item is deleted when the plan runs.
    pub will_delete: bool,
}

/// A mounted volume (a top-level share under the volumes directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VolumeDto {
    pub id: VolumeId,
    pub name: String,
    /// Whether the volume is mounted read-write.
    pub writable: bool,
    /// Whether the volume is currently reachable.
    pub online: bool,
}

/// Body of `GET /api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HealthResponse {
    /// Always `"ok"` when the server is up.
    pub status: String,
    /// Server version.
    pub version: String,
}

/// Machine-readable error code plus a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorDetail {
    /// Stable snake_case code, e.g. `not_found` or `not_implemented`.
    pub code: String,
    pub message: String,
}

/// Body of every non-2xx API response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    #[test]
    fn enums_use_snake_case() {
        assert_eq!(roundtrip(&FileKind::Sidecar), json!("sidecar"));
        assert_eq!(roundtrip(&CaptureSource::Fs), json!("fs"));
        assert_eq!(roundtrip(&FlagFilter::Rejected), json!("rejected"));
        assert_eq!(roundtrip(&SortKey::Date), json!("date"));
        assert_eq!(roundtrip(&SortOrder::Desc), json!("desc"));
        assert_eq!(roundtrip(&PlanSide::Comparison), json!("comparison"));
        assert_eq!(roundtrip(&ConflictPolicy::Replace), json!("replace"));
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Smart);
    }

    #[test]
    fn match_mode_uses_snake_case_and_defaults_to_hash() {
        assert_eq!(roundtrip(&MatchMode::Hash), json!("hash"));
        assert_eq!(roundtrip(&MatchMode::NameAndTime), json!("name_and_time"));
        assert_eq!(
            roundtrip(&MatchMode::NameDifferentTime),
            json!("name_different_time")
        );
        assert_eq!(roundtrip(&MatchMode::NamePrefix), json!("name_prefix"));
        assert_eq!(MatchMode::default(), MatchMode::Hash);
        assert!(serde_json::from_value::<MatchMode>(json!("prefix")).is_err());
    }

    #[test]
    fn scope_roundtrips() {
        let scope = Scope {
            volume_id: Some(2),
            dir: "2024/Trip".to_owned(),
            recursive: true,
        };
        assert_eq!(
            roundtrip(&scope),
            json!({ "volume_id": 2, "dir": "2024/Trip", "recursive": true })
        );
        assert_eq!(
            serde_json::from_str::<Scope>("{}").unwrap(),
            Scope::default()
        );
    }

    #[test]
    fn empty_filter_matches_everything_and_tolerates_missing_fields() {
        assert_eq!(
            roundtrip(&Filter::default()),
            json!({
                "cameras": [],
                "ratings": [],
                "flags": [],
                "tags": [],
                "selection_id": null
            })
        );
        assert_eq!(
            serde_json::from_str::<Filter>("{}").unwrap(),
            Filter::default()
        );
        let partial: Filter = serde_json::from_str(r#"{"ratings":[4,5]}"#).unwrap();
        assert_eq!(partial.ratings, vec![4, 5]);
        assert!(partial.cameras.is_empty());
    }

    #[test]
    fn filter_roundtrips() {
        let filter = Filter {
            cameras: vec!["Canon EOS R5".to_owned()],
            ratings: vec![0, 5],
            flags: vec![FlagFilter::Selected, FlagFilter::Rejected],
            tags: vec!["holiday".to_owned()],
            selection_id: Some(9),
        };
        roundtrip(&filter);
    }

    #[test]
    fn plan_ref_defaults_to_source_side() {
        let plan_ref: PlanRef = serde_json::from_str(r#"{"plan_id":3}"#).unwrap();
        assert_eq!(plan_ref.side, PlanSide::Source);
        let wire = roundtrip(&PlanRef {
            plan_id: 3,
            side: PlanSide::Comparison,
        });
        assert_eq!(wire, json!({ "plan_id": 3, "side": "comparison" }));
    }

    #[test]
    fn overlay_is_all_false_by_default() {
        let overlay = Overlay::default();
        assert!(!overlay.matched && !overlay.planned && !overlay.will_delete);
        roundtrip(&Overlay {
            matched: true,
            planned: true,
            will_delete: false,
        });
    }

    #[test]
    fn volume_health_and_error_bodies_roundtrip() {
        roundtrip(&VolumeDto {
            id: 1,
            name: "photo".to_owned(),
            writable: true,
            online: false,
        });
        roundtrip(&HealthResponse {
            status: "ok".to_owned(),
            version: "0.1.0".to_owned(),
        });
        let wire = roundtrip(&ErrorBody {
            error: ErrorDetail {
                code: "not_found".to_owned(),
                message: "no such file".to_owned(),
            },
        });
        assert_eq!(
            wire,
            json!({ "error": { "code": "not_found", "message": "no such file" } })
        );
    }
}
