//! Parameters for index-based empty-folder cleanup.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::Scope;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CleanupParams {
    pub scope: Scope,
}

/// Cleanup has no file-selection or execution options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CleanupExecuteOptions {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::testing::roundtrip;

    #[test]
    fn cleanup_contract_roundtrips() {
        roundtrip(&CleanupParams {
            scope: Scope {
                volume_id: Some(7),
                dir: "Photos".to_owned(),
                recursive: false,
            },
        });
        assert_eq!(roundtrip(&CleanupExecuteOptions {}), serde_json::json!({}));
        assert!(serde_json::from_str::<CleanupExecuteOptions>(r#"{"only":[1]}"#).is_err());
    }
}
