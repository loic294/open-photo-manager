//! Live formatting samples and the formatter-owned token catalogue.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{CaptureSource, SideParams};
use crate::paths::FileId;
use crate::pattern::{PatternError, PatternExample, TokenGroup, TokenInfo};

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

impl From<PatternError> for PatternIssue {
    fn from(error: PatternError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            suggestion: error.suggestion(),
            start: error.span.start as u32,
            end: error.span.end as u32,
        }
    }
}

impl From<TokenInfo> for FormatToken {
    fn from(token: TokenInfo) -> Self {
        Self {
            name: token.name.to_owned(),
            aliases: token
                .aliases
                .iter()
                .map(|alias| (*alias).to_owned())
                .collect(),
            group: match token.group {
                TokenGroup::Date => "date",
                TokenGroup::File => "file",
                TokenGroup::Other => "other",
            }
            .to_owned(),
            description: token.description.to_owned(),
            example: token.example,
        }
    }
}

impl From<PatternExample> for FormatExample {
    fn from(example: PatternExample) -> Self {
        Self {
            pattern: example.pattern.to_owned(),
            description: example.description.to_owned(),
            result: example.result,
        }
    }
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

    #[test]
    fn shared_pattern_conversions_preserve_the_format_catalogue() {
        use super::*;
        let error = crate::pattern::parse("{yyyy}").expect_err("unknown token");
        let issue = PatternIssue::from(error);
        assert_eq!(issue.code, "unknown_token");
        assert_eq!(issue.suggestion.as_deref(), Some("{YYYY}"));
        assert_eq!((issue.start, issue.end), (0, 6));

        let token = crate::pattern::describe()
            .into_iter()
            .find(|token| token.name == "mm")
            .map(FormatToken::from)
            .expect("month token");
        assert_eq!(token.group, "date");
        assert_eq!(token.aliases, ["MM"]);
        assert_eq!(token.example, "10");

        let example = crate::pattern::examples()
            .into_iter()
            .next()
            .map(FormatExample::from)
            .expect("first pattern example");
        assert_eq!(example.pattern, "{YYYY}/{mm}");
        assert_eq!(example.result, "2026/10/IMG_0042.CR3");
    }
}
