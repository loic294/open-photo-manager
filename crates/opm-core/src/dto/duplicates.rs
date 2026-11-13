//! Duplicate finder parameters and the action selected by its split button.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{FileItem, Filter, MatchMode, Scope};
use crate::paths::FileId;

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

/// File view searches all volumes; excluded scopes apply to both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DuplicateFileParams {
    pub matching: DuplicateParams,
    pub excluded: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DuplicateFileExecuteOptions {
    pub keep: Vec<FileId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateFile {
    pub item: FileItem,
    pub btime: Option<i64>,
    pub has_description: bool,
    pub tags: Vec<String>,
}

/// Connected matches are not necessarily a clique. Every removed file needs a direct keeper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateFileGroup {
    pub files: Vec<DuplicateFile>,
    pub pairs: Vec<[FileId; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateFileGroups {
    pub groups: Vec<DuplicateFileGroup>,
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
        let file_params = DuplicateFileParams {
            matching: params,
            excluded: vec![Scope {
                volume_id: Some(2),
                dir: "excluded".to_owned(),
                recursive: true,
            }],
        };
        crate::dto::testing::roundtrip(&file_params);
        crate::dto::testing::roundtrip(&DuplicateFileExecuteOptions { keep: vec![1, 2] });
        crate::dto::testing::roundtrip(&DuplicateFileGroups {
            groups: vec![DuplicateFileGroup {
                files: vec![DuplicateFile {
                    item: FileItem::default(),
                    btime: Some(5),
                    has_description: true,
                    tags: vec!["trip".to_owned()],
                }],
                pairs: vec![[1, 2]],
            }],
        });
    }
}
