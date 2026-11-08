//! Asset move and folder formatting share these parameters.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ConflictPolicy, MatchMode, SideParams};
use crate::paths::VolumeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MoveType {
    KeepSource,
    KeepDestination,
    Flat,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FolderRef {
    pub volume_id: VolumeId,
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MoveParams {
    pub move_type: MoveType,
    pub source: SideParams,
    pub destination: FolderRef,
    pub pattern: Option<String>,
    pub conflict_policy: Option<ConflictPolicy>,
    pub match_mode: Option<MatchMode>,
    #[serde(default)]
    pub strict_prefix: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MoveExecuteOptions {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_no_execute_override() {
        for mode in ["keep_source", "keep_destination", "flat", "custom"] {
            let p: MoveParams = serde_json::from_value(serde_json::json!({
                "move_type": mode, "source": {"scope": {"volume_id": 1, "dir": "", "recursive": true}, "filter": {}},
                "destination": {"volume_id": 2, "dir": "new"}, "pattern": null,
                "conflict_policy": null, "match_mode": null, "strict_prefix": false
            })).unwrap();
            assert_eq!(
                p,
                serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap()
            );
        }
        let p = MoveExecuteOptions::default();
        assert_eq!(
            p,
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap()
        );
        assert!(
            serde_json::from_str::<MoveExecuteOptions>(r#"{"conflict_policy":"replace"}"#).is_err()
        );
    }
}
