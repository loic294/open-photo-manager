//! Index control DTOs.
//!
//! Owned by todo `building-index-controls`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::common::Scope;

/// Body of `POST /api/index/rescan`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct RescanRequest {
    /// Scopes to rescan; empty means every volume.
    pub targets: Vec<Scope>,
    /// Re-extract metadata even for files whose size and mtime are unchanged.
    pub force: bool,
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
        });
        assert_eq!(wire["targets"][0]["dir"], json!("2024"));
        assert_eq!(wire["force"], json!(true));
        let parsed: RescanRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, RescanRequest::default());
    }
}
