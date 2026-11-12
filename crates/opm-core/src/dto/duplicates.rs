//! Duplicate finder parameters and the action selected by its split button.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Filter, MatchMode, Scope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SideParams {
    pub scope: Scope,
    #[serde(default)]
    pub filter: Filter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DuplicateParams {
    pub mode: MatchMode,
    #[serde(default)]
    pub strict_prefix: bool,
    pub source: SideParams,
    pub comparison: SideParams,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DuplicateAction {
    DeleteSource,
    #[default]
    DeleteComparison,
    Move,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(export)]
pub struct DuplicateExecuteOptions {
    pub action: DuplicateAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trips() {
        let value = serde_json::json!({
            "mode": "name_prefix", "strict_prefix": false,
            "source": {"scope": {"volume_id": 1, "dir": "", "recursive": true}, "filter": {}},
            "comparison": {"scope": {"volume_id": 2, "dir": "photos", "recursive": false}, "filter": {}}
        });
        let params: DuplicateParams = serde_json::from_value(value).unwrap();
        assert_eq!(
            params,
            serde_json::from_value(serde_json::to_value(&params).unwrap()).unwrap()
        );
        for action in [
            DuplicateAction::DeleteSource,
            DuplicateAction::DeleteComparison,
            DuplicateAction::Move,
        ] {
            let options = DuplicateExecuteOptions { action };
            assert_eq!(
                options,
                serde_json::from_value(serde_json::to_value(&options).unwrap()).unwrap()
            );
        }
        assert_eq!(
            serde_json::from_str::<DuplicateExecuteOptions>("{}")
                .unwrap()
                .action,
            DuplicateAction::DeleteComparison
        );
    }
}
