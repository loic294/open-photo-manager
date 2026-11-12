//! Batch-rename request rules and validation issues.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ConflictPolicy, SideParams};

/// How a rename rule selects source file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RenameMatchMode {
    All,
    Glob,
    Regex,
}

/// File-name matcher settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RenameMatcher {
    pub mode: RenameMatchMode,
    pub pattern: String,
    pub ignore_case: bool,
}

/// Regex rewrite applied to the cleaned file stem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RenameRewrite {
    pub find: String,
    pub replace: String,
    pub ignore_case: bool,
}

/// Complete, reusable batch-rename rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(export)]
pub struct RenameRules {
    pub matcher: RenameMatcher,
    pub clean: bool,
    pub rewrite: Option<RenameRewrite>,
    pub pattern: String,
    pub count_digits: u8,
}

impl Default for RenameRules {
    fn default() -> Self {
        Self {
            matcher: RenameMatcher {
                mode: RenameMatchMode::All,
                pattern: String::new(),
                ignore_case: true,
            },
            clean: false,
            rewrite: None,
            pattern: "{name}".to_owned(),
            count_digits: 0,
        }
    }
}

/// Parameters for a batch-rename plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RenameParams {
    pub source: SideParams,
    pub rules: RenameRules,
    pub conflict_policy: Option<ConflictPolicy>,
}

/// The rule field that contains a validation issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RenameIssueField {
    Matcher,
    Rewrite,
    Pattern,
    CountDigits,
}

/// A validation problem with a byte span in its rule field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RenameIssue {
    pub field: RenameIssueField,
    pub code: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub start: u32,
    pub end: u32,
}

/// Request for a bounded preview of the first matching folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RenamePreviewRequest {
    /// Rules to validate and apply to the sample.
    pub rules: RenameRules,
    /// Source folder and filters used by the rename plan.
    pub source: SideParams,
    /// Number of matching files to return, clamped to 1 through 200; defaults to 30.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Whether a sampled file would change, wait for metadata, or fail naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RenameSampleOutcome {
    Rename,
    Unchanged,
    Pending,
    Failed,
}

/// One matched file in a rename preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RenameSample {
    /// Stable index id of the real source file.
    pub file_id: crate::paths::FileId,
    /// Original filename, exactly as indexed.
    pub name: String,
    /// Rendered filename, or `None` when metadata is pending or rendering failed.
    pub new_name: Option<String>,
    /// The sample result.
    pub outcome: RenameSampleOutcome,
    /// Stable failure code when `outcome` is `failed`.
    pub error: Option<String>,
}

/// Preview of the first folder containing a file selected by the matcher.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RenamePreviewResponse {
    /// All rule issues; when non-empty, the response has no samples.
    pub issues: Vec<RenameIssue>,
    /// Volume and directory represented by the sample; the requested source base when no folder matches.
    pub volume_id: Option<crate::paths::VolumeId>,
    /// Volume-relative sample folder, or the requested source directory when no folder matches.
    pub dir: String,
    /// Non-sidecar files in the sample folder, including those not selected by the matcher.
    pub folder_files: u64,
    /// Matched files in the sample folder, including files beyond the in-memory sample cap.
    pub matched: u64,
    /// Whether matching files exceeded the 20,000-file naming cap.
    pub truncated: bool,
    /// At most the requested number of matched samples, in filename order.
    pub samples: Vec<RenameSample>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    #[test]
    fn rename_rules_have_the_expected_default() {
        assert_eq!(
            RenameRules::default(),
            RenameRules {
                matcher: RenameMatcher {
                    mode: RenameMatchMode::All,
                    pattern: String::new(),
                    ignore_case: true,
                },
                clean: false,
                rewrite: None,
                pattern: "{name}".to_owned(),
                count_digits: 0,
            }
        );
        roundtrip(&RenameRules::default());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(
            serde_json::from_value::<RenameMatcher>(json!({
                "mode": "all",
                "pattern": "",
                "ignore_case": true,
                "future": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RenameRewrite>(json!({
                "find": "x",
                "replace": "y",
                "ignore_case": true,
                "future": true
            }))
            .is_err()
        );
    }

    #[test]
    fn rename_params_roundtrip_the_contract_json() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"source":{"scope":{"volume_id":1,"dir":"","recursive":true},"filter":{}},"rules":{"matcher":{"mode":"glob","pattern":"*.jpg","ignore_case":true},"clean":true,"rewrite":{"find":"^IMG_","replace":"Photo_","ignore_case":true},"pattern":"{name}","count_digits":0},"conflict_policy":null}"#,
        )
        .expect("contract JSON");
        let params: RenameParams = serde_json::from_value(value.clone()).expect("valid params");
        assert_eq!(params.source.scope.volume_id, Some(1));
        assert_eq!(params.rules.matcher.pattern, "*.jpg");
        let serialized = roundtrip(&params);
        assert_eq!(
            serde_json::from_value::<RenameParams>(serialized).expect("round trip"),
            params
        );
        let mut unknown = value;
        unknown["future"] = json!(true);
        assert!(serde_json::from_value::<RenameParams>(unknown).is_err());
    }

    #[test]
    fn preview_wire_types_roundtrip() {
        let request = RenamePreviewRequest {
            rules: RenameRules::default(),
            source: SideParams {
                scope: crate::dto::Scope {
                    volume_id: Some(1),
                    dir: "photos".into(),
                    recursive: true,
                },
                filter: Default::default(),
            },
            limit: Some(30),
        };
        roundtrip(&request);

        let sample = RenameSample {
            file_id: 7,
            name: "IMG.jpg".into(),
            new_name: Some("Photo.jpg".into()),
            outcome: RenameSampleOutcome::Rename,
            error: None,
        };
        roundtrip(&sample);
        roundtrip(&RenamePreviewResponse {
            issues: Vec::new(),
            volume_id: Some(1),
            dir: "photos".into(),
            folder_files: 2,
            matched: 1,
            truncated: false,
            samples: vec![sample],
        });
        roundtrip(&RenameSampleOutcome::Pending);
    }
}
