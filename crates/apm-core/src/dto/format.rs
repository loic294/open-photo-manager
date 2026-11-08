//! Live formatting samples and the formatter-owned token catalogue.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{CaptureSource, SideParams};
use crate::paths::FileId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FormatPreviewRequest {
    pub pattern: String,
    pub source: SideParams,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PatternIssue {
    pub code: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatSample {
    pub file_id: FileId,
    pub source: String,
    pub destination: Option<String>,
    pub skip: Option<String>,
    pub capture_src: Option<CaptureSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatPreviewResponse {
    pub error: Option<PatternIssue>,
    pub has_filename_token: bool,
    pub appends_filename: bool,
    pub samples: Vec<FormatSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatToken {
    pub name: String,
    pub aliases: Vec<String>,
    pub group: String,
    pub description: String,
    pub example: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatExample {
    pub pattern: String,
    pub description: String,
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatSampleFile {
    pub file_name: String,
    /// Naive-local epoch seconds, like the indexed capture time.
    pub captured: i64,
    pub camera: Option<String>,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FormatTokensResponse {
    pub tokens: Vec<FormatToken>,
    pub examples: Vec<FormatExample>,
    pub sample: FormatSampleFile,
}

#[cfg(test)]
mod tests {
    #[test]
    fn wire_round_trips() {
        use super::*;
        let sample = FormatSample {
            file_id: 1,
            source: "photos/a.jpg".into(),
            destination: None,
            skip: Some("metadata_pending".into()),
            capture_src: None,
        };
        let response = FormatPreviewResponse {
            error: Some(PatternIssue {
                code: "unknown_token".into(),
                message: "unknown".into(),
                suggestion: Some("YYYY".into()),
                start: 0,
                end: 6,
            }),
            has_filename_token: false,
            appends_filename: false,
            samples: vec![sample],
        };
        assert_eq!(
            response,
            serde_json::from_value(serde_json::to_value(&response).unwrap()).unwrap()
        );
        let request: FormatPreviewRequest = serde_json::from_value(serde_json::json!({"pattern":"{YYYY}", "source":{"scope":{"volume_id":1,"dir":"","recursive":true},"filter":{}},"limit":null})).unwrap();
        assert_eq!(
            request,
            serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap()
        );
        let response = FormatTokensResponse {
            tokens: vec![FormatToken {
                name: "mm".into(),
                aliases: vec!["MM".into()],
                group: "date".into(),
                description: "month".into(),
                example: "10".into(),
            }],
            examples: vec![FormatExample {
                pattern: "{mm}".into(),
                description: "month".into(),
                result: "10/a.jpg".into(),
            }],
            sample: FormatSampleFile {
                file_name: "a.jpg".into(),
                captured: 0,
                camera: None,
                folder: "".into(),
            },
        };
        assert_eq!(
            response,
            serde_json::from_value(serde_json::to_value(&response).unwrap()).unwrap()
        );
    }
}
