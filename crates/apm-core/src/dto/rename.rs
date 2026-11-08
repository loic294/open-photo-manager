//! Batch-rename request rules and validation issues.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
}
