//! Pure batch-rename rules shared by plan building, preview and execution re-checks.
//!
//! Resolve capture time in the caller exactly as the move planner does: use
//! `DateTime::<Utc>::from_timestamp(capture_ts, 0)` when the indexed capture second exists;
//! otherwise call `resolve_capture(&CaptureInputs { birth_time, modified_time, .. }, tz)`.
//! Pass that already-resolved local wall clock in [`RenameFile::captured`]. This module performs no
//! metadata lookup or IO.
//!
//! Stable validation codes are `empty_pattern`, `too_long`, `invalid_glob`, `invalid_regex`,
//! `regex_too_complex`, `empty_find`, `unknown_group`, `empty`, `too_deep`, `leading_slash`,
//! `parent_component`, `unknown_token`, `empty_token`, `unclosed_brace`, `unmatched_brace`,
//! `control_character`, `backslash`, `slash_in_name`, and `count_digits_out_of_range`.
//! Per-file error codes are `empty_name`, `extension_changed`, and `invalid_name`.
//!
//! `compile` validates the complete rule set once. For every matched file, `new_name` applies the
//! same pure pipeline. When a pattern uses `{count}`, first call `assign_counts` with all matched
//! files in one folder; counts restart in each folder.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDateTime;
use globset::{GlobBuilder, GlobMatcher};
use regex::{Regex, RegexBuilder};

use crate::dto::{
    FormatExample, FormatSampleFile, FormatToken, FormatTokensResponse, PatternIssue, RenameIssue,
    RenameIssueField, RenameMatchMode, RenameRules,
};
use crate::names::{self, natural_cmp};
use crate::paths::FileId;
use crate::pattern::{
    self, FileContext, Pattern, Piece, RenderOptions, Token, TokenGroup, finish_name_component,
};

const MAX_MATCHER_BYTES: usize = 512;
const MAX_REGEX_SIZE: usize = 1 << 20;
const MAX_REGEX_NESTING: u32 = 64;
const MAX_CLEAN_ITERATIONS: usize = 16;

#[derive(Debug)]
enum Matcher {
    All,
    Glob(GlobMatcher),
    Regex(Regex),
}

/// Validated rules, cheap to reuse and safe to share between threads.
#[derive(Debug)]
pub struct CompiledRules {
    matcher: Matcher,
    clean: bool,
    rewrite: Option<Rewrite>,
    pattern: Pattern,
    count_digits: u8,
    uses_count: bool,
    needs_metadata: bool,
}

#[derive(Debug)]
struct Rewrite {
    regex: Regex,
    replacement: String,
}

/// Per-file facts required by the pure rename engine.
#[derive(Debug, Clone, Copy)]
pub struct RenameFile<'a> {
    /// Stable index id used as the final counter-order tie-breaker.
    pub id: FileId,
    /// Original file name, exactly as indexed.
    pub name: &'a str,
    /// Resolved local capture date and time.
    pub captured: NaiveDateTime,
    /// Indexed epoch seconds, used only to order counters.
    pub capture_ts: Option<i64>,
    /// Camera make and model, when known.
    pub camera: Option<&'a str>,
    /// Current parent folder's name; empty at the volume root.
    pub folder: &'a str,
    /// Configured maximum file-name component length in bytes.
    pub max_component_bytes: usize,
}

/// A per-file rendering failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rename produced an unsafe or extension-changing file name ({code})")]
pub struct RenameError {
    code: &'static str,
}

impl RenameError {
    /// Stable machine-readable error code.
    pub const fn code(self) -> &'static str {
        self.code
    }
}

/// Compiles rules and reports every validation issue in rule-field order.
pub fn compile(rules: &RenameRules) -> Result<CompiledRules, Vec<RenameIssue>> {
    let mut issues = Vec::new();
    let matcher = compile_matcher(&rules.matcher, &mut issues);
    let rewrite = compile_rewrite(rules.rewrite.as_ref(), &mut issues);

    for error in pattern::validate_name(&rules.pattern) {
        let error = PatternIssue::from(error);
        issues.push(RenameIssue {
            field: RenameIssueField::Pattern,
            code: error.code,
            message: error.message,
            suggestion: error.suggestion,
            start: error.start,
            end: error.end,
        });
    }

    if rules.count_digits > 9 {
        issues.push(issue(
            RenameIssueField::CountDigits,
            "count_digits_out_of_range",
            "count_digits must be between 0 and 9",
            None,
            0,
            0,
        ));
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    let pattern = match pattern::parse_name(&rules.pattern) {
        Ok(pattern) => pattern,
        Err(error) => {
            let error = PatternIssue::from(error);
            return Err(vec![RenameIssue {
                field: RenameIssueField::Pattern,
                code: error.code,
                message: error.message,
                suggestion: error.suggestion,
                start: error.start,
                end: error.end,
            }]);
        }
    };
    let uses_count = pattern.uses_count();
    let needs_metadata = uses_count
        || pattern
            .tokens()
            .any(|token| token.group() == TokenGroup::Date || token == Token::Camera);
    Ok(CompiledRules {
        matcher: matcher.unwrap_or(Matcher::All),
        clean: rules.clean,
        rewrite,
        pattern,
        count_digits: rules.count_digits,
        uses_count,
        needs_metadata,
    })
}

impl CompiledRules {
    /// Whether this rule set needs resolved capture/camera metadata.
    pub const fn needs_metadata(&self) -> bool {
        self.needs_metadata
    }

    /// Whether the name pattern contains `{count}`.
    pub const fn uses_count(&self) -> bool {
        self.uses_count
    }

    /// Whether the complete original file name matches the configured selector.
    pub fn matches(&self, name: &str) -> bool {
        match &self.matcher {
            Matcher::All => true,
            Matcher::Glob(glob) => glob.is_match(name),
            Matcher::Regex(regex) => regex.is_match(name),
        }
    }
}

/// Renders the destination file name, or returns `Ok(None)` when the matcher does not select it.
/// A missing count means one, which is the right value for a single-file group.
pub fn new_name(
    rules: &CompiledRules,
    file: &RenameFile<'_>,
    count: Option<u64>,
) -> Result<Option<String>, RenameError> {
    if !rules.matches(file.name) {
        return Ok(None);
    }
    let prepared = prepare_stem(rules, file.name)?;
    let count_text = format_count(count.unwrap_or(1), rules.count_digits);
    let raw = render_name(rules, file, &prepared.stem, &count_text, true);
    let rendered = finish_name_component(&raw, file.name, file.max_component_bytes);
    validate_result(file.name, &rendered)?;
    Ok(Some(rendered))
}

/// Assigns deterministic 1-based counts for the matched files in one folder.
///
/// The returned map is keyed by file id. If `{count}` is unused, the map is empty.
pub fn assign_counts(
    rules: &CompiledRules,
    files: &[RenameFile<'_>],
) -> Result<HashMap<FileId, u64>, RenameError> {
    let mut assignments = HashMap::new();
    if !rules.uses_count {
        return Ok(assignments);
    }
    let mut groups: BTreeMap<String, Vec<CountMember<'_>>> = BTreeMap::new();
    for file in files.iter().filter(|file| rules.matches(file.name)) {
        let prepared = prepare_stem(rules, file.name)?;
        let group_name = render_name(rules, file, &prepared.stem, "", true);
        let group_name = finish_name_component(&group_name, file.name, file.max_component_bytes);
        let extension = names::split_name(file.name).1;
        let group_stem = extension
            .map(|extension| format!(".{extension}"))
            .and_then(|suffix| group_name.strip_suffix(&suffix).map(str::to_owned))
            .unwrap_or(group_name);
        groups
            .entry(names::normalize_name(&group_stem))
            .or_default()
            .push(CountMember {
                file: *file,
                stem_norm: names::stem_norm(file.name),
                clean_changed: prepared.clean_changed,
            });
    }
    for members in groups.values_mut() {
        let mut sets: BTreeMap<String, Vec<CountMember<'_>>> = BTreeMap::new();
        for member in std::mem::take(members) {
            sets.entry(member.stem_norm.clone())
                .or_default()
                .push(member);
        }
        let mut parts: Vec<Vec<CountMember<'_>>> = sets.into_values().collect();
        for part in &mut parts {
            part.sort_by(compare_members);
        }
        parts.sort_by(|left, right| match (left.first(), right.first()) {
            (Some(left), Some(right)) => compare_members(left, right),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        });
        for (index, part) in parts.into_iter().enumerate() {
            let count = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            for member in part {
                assignments.insert(member.file.id, count);
            }
        }
    }
    Ok(assignments)
}

struct CountMember<'a> {
    file: RenameFile<'a>,
    stem_norm: String,
    clean_changed: bool,
}

fn compare_members(left: &CountMember<'_>, right: &CountMember<'_>) -> Ordering {
    match (left.file.capture_ts, right.file.capture_ts) {
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (Some(left), Some(right)) => left.cmp(&right),
        (None, None) => Ordering::Equal,
    }
    .then_with(|| left.clean_changed.cmp(&right.clean_changed))
    .then_with(|| {
        natural_cmp(
            &names::normalize_name(left.file.name),
            &names::normalize_name(right.file.name),
        )
    })
    .then_with(|| left.file.id.cmp(&right.file.id))
}

struct PreparedStem {
    stem: String,
    clean_changed: bool,
}

fn prepare_stem(rules: &CompiledRules, name: &str) -> Result<PreparedStem, RenameError> {
    let (original_stem, _) = names::split_name(name);
    let (mut stem, clean_changed) = if rules.clean {
        clean_stem(original_stem)
    } else {
        (original_stem.to_owned(), false)
    };
    if let Some(rewrite) = &rules.rewrite {
        let rewritten = rewrite
            .regex
            .replace_all(&stem, rewrite.replacement.as_str())
            .into_owned();
        if rewritten != stem {
            stem = pattern::sanitize_value(&rewritten);
        }
    }
    if matches!(stem.as_str(), "" | "." | "..") {
        return Err(RenameError { code: "empty_name" });
    }
    Ok(PreparedStem {
        stem,
        clean_changed,
    })
}

fn render_name(
    rules: &CompiledRules,
    file: &RenameFile<'_>,
    stem: &str,
    count: &str,
    append_extension: bool,
) -> String {
    let (_, extension) = names::split_name(file.name);
    let extension = extension.unwrap_or_default();
    let context = FileContext {
        file_name: file.name.to_owned(),
        captured: file.captured,
        camera: file.camera.map(str::to_owned),
        folder: file.folder.to_owned(),
    };
    let options = RenderOptions::default();
    let mut result = String::new();
    for piece in rules.pattern.components().iter().flatten() {
        match piece {
            Piece::Literal(text) => result.push_str(text),
            Piece::Count => result.push_str(count),
            Piece::Token(token) => match token {
                Token::FileName => {
                    result.push_str(&file_name_text(&joined_name(stem, extension)));
                }
                Token::Stem => result.push_str(&file_name_text(stem)),
                Token::Extension => result.push_str(&file_name_text(extension)),
                _ => result.push_str(&token.value(&context, &options)),
            },
        }
    }
    if append_extension
        && !rules
            .pattern
            .tokens()
            .any(|token| matches!(token, Token::Extension | Token::FileName))
        && !extension.is_empty()
    {
        result.push('.');
        result.push_str(extension);
    }
    result
}

fn joined_name(stem: &str, extension: &str) -> String {
    if extension.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}.{extension}")
    }
}

fn file_name_text(text: &str) -> String {
    text.replace(['/', '\0'], "_")
}

fn validate_result(original: &str, rendered: &str) -> Result<(), RenameError> {
    if matches!(rendered, "" | "." | "..") {
        return Err(RenameError { code: "empty_name" });
    }
    if rendered.contains('/') || rendered.contains('\0') {
        return Err(RenameError {
            code: "invalid_name",
        });
    }
    let original_extension = names::split_name(original).1;
    let rendered_extension = names::split_name(rendered).1;
    if original_extension != rendered_extension {
        return Err(RenameError {
            code: "extension_changed",
        });
    }
    Ok(())
}

fn format_count(count: u64, digits: u8) -> String {
    if digits == 0 {
        count.to_string()
    } else {
        format!("{count:0width$}", width = usize::from(digits))
    }
}

fn clean_stem(original: &str) -> (String, bool) {
    let mut stem = original.to_owned();
    let mut changed = false;
    for _ in 0..MAX_CLEAN_ITERATIONS {
        let Some(next) = clean_suffix(&stem) else {
            break;
        };
        if next.trim().is_empty() || next == stem {
            break;
        }
        stem = next;
        changed = true;
    }
    (stem, changed)
}

fn clean_suffix(stem: &str) -> Option<String> {
    if let Some(open) = stem.rfind(" (")
        && stem.ends_with(')')
        && is_one_to_three_digits(stem.get(open + 2..stem.len().saturating_sub(1))?)
    {
        return Some(stem.get(..open)?.trim_end().to_owned());
    }
    if let Some(open) = stem.rfind(" - Copy (")
        && stem.ends_with(')')
        && is_one_to_three_digits(stem.get(open + 9..stem.len().saturating_sub(1))?)
    {
        return Some(stem.get(..open)?.trim_end().to_owned());
    }
    for suffix in [" - Copy", " copy"] {
        if let Some(prefix) = strip_ascii_case_insensitive_suffix(stem, suffix) {
            return Some(prefix.0.trim_end().to_owned());
        }
    }
    if let Some(index) = stem.to_ascii_lowercase().rfind(" copy ")
        && is_one_to_three_digits(stem.get(index + 6..)?)
    {
        return Some(stem.get(..index)?.trim_end().to_owned());
    }
    if let Some((prefix, digit)) = stem.split_at_checked(stem.len().saturating_sub(2))
        && digit.starts_with('-')
        && digit
            .as_bytes()
            .get(1)
            .is_some_and(|byte| (b'1'..=b'9').contains(byte))
    {
        return Some(prefix.trim_end().to_owned());
    }
    None
}

fn is_one_to_three_digits(text: &str) -> bool {
    (1..=3).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn strip_ascii_case_insensitive_suffix<'a>(
    text: &'a str,
    suffix: &str,
) -> Option<(&'a str, &'a str)> {
    let start = text.len().checked_sub(suffix.len())?;
    let found = text.get(start..)?;
    found
        .eq_ignore_ascii_case(suffix)
        .then_some((text.get(..start)?, found))
}

fn compile_matcher(
    matcher: &crate::dto::RenameMatcher,
    issues: &mut Vec<RenameIssue>,
) -> Option<Matcher> {
    match matcher.mode {
        RenameMatchMode::All => Some(Matcher::All),
        RenameMatchMode::Glob | RenameMatchMode::Regex => {
            if matcher.pattern.is_empty() {
                issues.push(issue(
                    RenameIssueField::Matcher,
                    "empty_pattern",
                    "matcher pattern must not be empty",
                    None,
                    0,
                    0,
                ));
                return None;
            }
            if matcher.pattern.len() > MAX_MATCHER_BYTES {
                issues.push(issue(
                    RenameIssueField::Matcher,
                    "too_long",
                    "matcher pattern is longer than 512 bytes",
                    None,
                    MAX_MATCHER_BYTES,
                    matcher.pattern.len(),
                ));
                return None;
            }
            match matcher.mode {
                RenameMatchMode::Glob => GlobBuilder::new(&matcher.pattern)
                    .case_insensitive(matcher.ignore_case)
                    .build()
                    .map(|glob| Matcher::Glob(glob.compile_matcher()))
                    .map_err(|error| {
                        issues.push(issue(
                            RenameIssueField::Matcher,
                            "invalid_glob",
                            &error.to_string(),
                            None,
                            0,
                            matcher.pattern.len(),
                        ));
                    })
                    .ok(),
                RenameMatchMode::Regex => build_regex(
                    &matcher.pattern,
                    matcher.ignore_case,
                    RenameIssueField::Matcher,
                    issues,
                )
                .map(Matcher::Regex),
                RenameMatchMode::All => Some(Matcher::All),
            }
        }
    }
}

fn compile_rewrite(
    rewrite: Option<&crate::dto::RenameRewrite>,
    issues: &mut Vec<RenameIssue>,
) -> Option<Rewrite> {
    let rewrite = rewrite?;
    if rewrite.find.is_empty() {
        issues.push(issue(
            RenameIssueField::Rewrite,
            "empty_find",
            "rewrite find pattern must not be empty",
            None,
            0,
            0,
        ));
        return None;
    }
    if rewrite.find.len() > MAX_MATCHER_BYTES {
        issues.push(issue(
            RenameIssueField::Rewrite,
            "too_long",
            "rewrite find pattern is longer than 512 bytes",
            None,
            MAX_MATCHER_BYTES,
            rewrite.find.len(),
        ));
        return None;
    }
    let regex = build_regex(
        &rewrite.find,
        rewrite.ignore_case,
        RenameIssueField::Rewrite,
        issues,
    )?;
    validate_replacement(&regex, &rewrite.replace, issues);
    Some(Rewrite {
        regex,
        replacement: rewrite.replace.clone(),
    })
}

fn build_regex(
    text: &str,
    ignore_case: bool,
    field: RenameIssueField,
    issues: &mut Vec<RenameIssue>,
) -> Option<Regex> {
    let mut builder = RegexBuilder::new(text);
    builder
        .case_insensitive(ignore_case)
        .size_limit(MAX_REGEX_SIZE)
        .nest_limit(MAX_REGEX_NESTING);
    match builder.build() {
        Ok(regex) => Some(regex),
        Err(error) => {
            let detail = error.to_string();
            let complex = (detail.contains("size") && detail.contains("limit"))
                || (detail.contains("nest") && detail.contains("limit"))
                || detail.contains("maximum number of nested");
            issues.push(issue(
                field,
                if complex {
                    "regex_too_complex"
                } else {
                    "invalid_regex"
                },
                &detail,
                None,
                0,
                text.len(),
            ));
            None
        }
    }
}

fn validate_replacement(regex: &Regex, replacement: &str, issues: &mut Vec<RenameIssue>) {
    let named: HashSet<&str> = regex.capture_names().flatten().collect();
    let max_index = regex.captures_len().saturating_sub(1);
    let mut index = 0;
    while let Some(rest) = replacement.get(index..) {
        let Some(relative) = rest.find('$') else {
            break;
        };
        let dollar = index + relative;
        let after_dollar = dollar + 1;
        let Some(next) = replacement.get(after_dollar..) else {
            break;
        };
        if next.starts_with('$') {
            index = after_dollar + 1;
            continue;
        }
        let (group, end) = if next.starts_with('{') {
            let content_start = after_dollar + 1;
            let Some(relative_end) = replacement
                .get(content_start..)
                .and_then(|remaining| remaining.find('}'))
            else {
                index = after_dollar;
                continue;
            };
            let end = content_start + relative_end + 1;
            (
                replacement
                    .get(content_start..content_start + relative_end)
                    .unwrap_or_default(),
                end,
            )
        } else {
            let end = after_dollar
                + next
                    .char_indices()
                    .take_while(|(_, character)| {
                        character.is_ascii_alphanumeric() || *character == '_'
                    })
                    .map(|(offset, character)| offset + character.len_utf8())
                    .last()
                    .unwrap_or(0);
            if end == after_dollar {
                index = after_dollar;
                continue;
            }
            (replacement.get(after_dollar..end).unwrap_or_default(), end)
        };
        let exists = group
            .parse::<usize>()
            .map(|number| number <= max_index)
            .unwrap_or_else(|_| named.contains(group));
        if !exists {
            issues.push(issue(
                RenameIssueField::Rewrite,
                "unknown_group",
                &format!("rewrite references unknown capture group `${group}`"),
                None,
                dollar,
                end,
            ));
        }
        index = end;
    }
}

fn issue(
    field: RenameIssueField,
    code: &str,
    message: &str,
    suggestion: Option<String>,
    start: usize,
    end: usize,
) -> RenameIssue {
    RenameIssue {
        field,
        code: code.to_owned(),
        message: message.to_owned(),
        suggestion,
        start: start as u32,
        end: end as u32,
    }
}

/// The rename token catalogue and examples, computed by the same formatter used for plans.
pub fn tokens() -> FormatTokensResponse {
    let sample = pattern::sample_context();
    let mut tokens: Vec<FormatToken> = pattern::describe()
        .into_iter()
        .map(FormatToken::from)
        .collect();
    tokens.insert(
        12,
        FormatToken {
            name: "count".to_owned(),
            aliases: Vec::new(),
            group: "file".to_owned(),
            description: "Folder-local sequence number for files with the same rendered stem"
                .to_owned(),
            example: example_name("{name}_{count}", &sample, Some(1)),
        },
    );
    let examples = [
        (
            "{name}",
            "Keep the cleaned stem and extension; use Clean or a rewrite to remove copy suffixes",
        ),
        (
            "{name}_{count}",
            "Add a folder-local counter to duplicate names",
        ),
        (
            "{YYYY}-{mm}-{dd}_{name}",
            "Prefix the name with its capture date",
        ),
        (
            "{YYYY}{mm}{dd}_{HH}{min}{sec}",
            "Name the file after its capture timestamp",
        ),
    ]
    .into_iter()
    .map(|(pattern, description)| FormatExample {
        pattern: pattern.to_owned(),
        description: description.to_owned(),
        result: example_name(pattern, &sample, (pattern.contains("{count}")).then_some(1)),
    })
    .collect();
    FormatTokensResponse {
        tokens,
        examples,
        sample: FormatSampleFile {
            file_name: sample.file_name,
            captured: sample.captured.and_utc().timestamp(),
            camera: sample.camera,
            folder: sample.folder,
        },
    }
}

fn example_name(pattern_text: &str, sample: &FileContext, count: Option<u64>) -> String {
    let rules = RenameRules {
        pattern: pattern_text.to_owned(),
        ..RenameRules::default()
    };
    match compile(&rules) {
        Ok(compiled) => {
            let file = RenameFile {
                id: 1,
                name: &sample.file_name,
                captured: sample.captured,
                capture_ts: Some(sample.captured.and_utc().timestamp()),
                camera: sample.camera.as_deref(),
                folder: &sample.folder,
                max_component_bytes: crate::conflict::DEFAULT_MAX_NAME_BYTES,
            };
            match new_name(&compiled, &file, count) {
                Ok(Some(name)) => name,
                Ok(None) => sample.file_name.clone(),
                Err(error) => format!("Invalid example ({})", error.code()),
            }
        }
        Err(issues) => format!(
            "Invalid example ({})",
            issues
                .first()
                .map_or("invalid_rules", |issue| issue.code.as_str())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime};
    use proptest::prelude::*;

    fn file(id: FileId, name: &str, capture_ts: Option<i64>) -> RenameFile<'_> {
        RenameFile {
            id,
            name,
            captured: NaiveDate::from_ymd_opt(2026, 10, 5)
                .and_then(|date| date.and_hms_opt(14, 7, 9))
                .unwrap_or(NaiveDateTime::MIN),
            capture_ts,
            camera: Some("Canon EOS R5"),
            folder: "Holiday",
            max_component_bytes: crate::conflict::DEFAULT_MAX_NAME_BYTES,
        }
    }

    fn rules(pattern_text: &str) -> RenameRules {
        RenameRules {
            pattern: pattern_text.to_owned(),
            ..RenameRules::default()
        }
    }

    fn compiled(rules: &RenameRules) -> CompiledRules {
        compile(rules).expect("valid rules")
    }

    #[test]
    fn matches_all_glob_and_unanchored_regex_with_case_flags() {
        assert!(compiled(&rules("{name}")).matches("Any.JPG"));
        let mut glob = rules("{name}");
        glob.matcher = crate::dto::RenameMatcher {
            mode: RenameMatchMode::Glob,
            pattern: "*.jpg".to_owned(),
            ignore_case: true,
        };
        assert!(compiled(&glob).matches("Photo.JPG"));
        glob.matcher.ignore_case = false;
        assert!(!compiled(&glob).matches("Photo.JPG"));
        glob.matcher.pattern = "photo[0-9]?.jpg".to_owned();
        assert!(compiled(&glob).matches("photo1x.jpg"));
        assert!(!compiled(&glob).matches("photo1.jpg"));
        let mut regex = rules("{name}");
        regex.matcher = crate::dto::RenameMatcher {
            mode: RenameMatchMode::Regex,
            pattern: "IMG_[0-9]+".to_owned(),
            ignore_case: true,
        };
        assert!(compiled(&regex).matches("prefix-img_123.jpg"));
    }

    #[test]
    fn matcher_rejects_empty_invalid_and_oversize_patterns() {
        for (pattern, code) in [("", "empty_pattern"), ("[", "invalid_glob")] {
            let mut input = rules("{name}");
            input.matcher.mode = RenameMatchMode::Glob;
            input.matcher.pattern = pattern.to_owned();
            assert_eq!(compile(&input).expect_err("invalid matcher")[0].code, code);
        }
        let mut regex = rules("{name}");
        regex.matcher.mode = RenameMatchMode::Regex;
        regex.matcher.pattern = "(".to_owned();
        assert_eq!(
            compile(&regex).expect_err("invalid regex")[0].code,
            "invalid_regex"
        );
        regex.matcher.pattern.clear();
        assert_eq!(
            compile(&regex).expect_err("empty regex")[0].code,
            "empty_pattern"
        );
        regex.matcher.pattern = "x".repeat(MAX_MATCHER_BYTES + 1);
        assert_eq!(compile(&regex).expect_err("oversize")[0].code, "too_long");
        regex.matcher.pattern = format!("{}a{}", "(".repeat(70), ")".repeat(70));
        let issues = compile(&regex).expect_err("regex nesting limit");
        assert_eq!(issues[0].code, "regex_too_complex", "{}", issues[0].message);
    }

    #[test]
    fn clean_removes_copy_suffixes_repeatedly_without_emptying_stems() {
        for (input, expected) in [
            ("IMG_0001-1-1.jpg", "IMG_0001.jpg"),
            ("A (1) (2).jpg", "A.jpg"),
            ("A copy 2.jpg", "A.jpg"),
            ("A COPY.jpg", "A.jpg"),
            ("A - Copy (2).jpg", "A.jpg"),
            ("A_1.jpg", "A_1.jpg"),
            ("2024-12-25.jpg", "2024-12-25.jpg"),
            ("photo-0.jpg", "photo-0.jpg"),
            ("A (1234).jpg", "A (1234).jpg"),
            (" (1).jpg", " (1).jpg"),
        ] {
            let mut input_rules = rules("{name}");
            input_rules.clean = true;
            let result = new_name(&compiled(&input_rules), &file(1, input, None), None)
                .expect("safe name")
                .expect("matched");
            assert_eq!(result, expected, "{input}");
        }
        let repeated = format!("A{}", " (1)".repeat(MAX_CLEAN_ITERATIONS + 2));
        let (result, changed) = clean_stem(&repeated);
        assert!(changed);
        assert!(!result.is_empty());
        assert!(result.ends_with(" (1)"));
    }

    #[test]
    fn rewrite_uses_groups_replaces_all_and_does_not_sanitize_inert_rules() {
        let mut input = rules("{name}");
        input.rewrite = Some(crate::dto::RenameRewrite {
            find: r"(?P<base>IMG)_(\d+)".to_owned(),
            replace: "$$${base}-$2".to_owned(),
            ignore_case: true,
        });
        assert_eq!(
            new_name(&compiled(&input), &file(1, "img_1_IMG_2.jpg", None), None)
                .expect("safe")
                .expect("matched"),
            "$img-1_$IMG-2.jpg"
        );
        input.rewrite = Some(crate::dto::RenameRewrite {
            find: "not_found".to_owned(),
            replace: "bad/name".to_owned(),
            ignore_case: true,
        });
        assert_eq!(
            new_name(&compiled(&input), &file(1, "a.jpg", None), None)
                .expect("inert rewrite")
                .expect("matched"),
            "a.jpg"
        );
        input.rewrite = Some(crate::dto::RenameRewrite {
            find: "a".to_owned(),
            replace: "$3".to_owned(),
            ignore_case: true,
        });
        assert_eq!(
            compile(&input).expect_err("unknown group")[0].code,
            "unknown_group"
        );
        input.rewrite = Some(crate::dto::RenameRewrite {
            find: "(a)".to_owned(),
            replace: "$1a".to_owned(),
            ignore_case: true,
        });
        assert_eq!(
            compile(&input).expect_err("crate parses this as one group name")[0].code,
            "unknown_group"
        );
    }

    #[test]
    fn name_pattern_preserves_extension_and_rejects_invalid_results() {
        for (pattern_text, source, expected) in [
            ("{name}", "IMG_1.JPG", "IMG_1.JPG"),
            ("literal", "IMG_1.JPG", "literal.JPG"),
            ("{filename}", "IMG_1.JPG", "IMG_1.JPG"),
            ("{name}.{ext}", "IMG_1.JpG", "IMG_1.JpG"),
            ("{ext}", "IMG_1.JPG", "extension_changed"),
            ("{name}", "without-extension", "without-extension"),
        ] {
            let input = compiled(&rules(pattern_text));
            let actual = new_name(&input, &file(1, source, None), None);
            if expected == "extension_changed" {
                assert_eq!(actual.expect_err("changed extension").code(), expected);
            } else {
                assert_eq!(actual.expect("valid name").expect("matched"), expected);
            }
        }
        assert_eq!(
            compile(&rules("a/b")).expect_err("slash forbidden")[0].code,
            "slash_in_name"
        );
        assert!(
            pattern::parse_name("{count}")
                .expect("count token")
                .uses_count()
        );
        let mut rewrite = rules("{name}");
        rewrite.rewrite = Some(crate::dto::RenameRewrite {
            find: "^a$".to_owned(),
            replace: "new.name".to_owned(),
            ignore_case: false,
        });
        assert_eq!(
            new_name(&compiled(&rewrite), &file(1, "a", None), None)
                .expect_err("extension changed")
                .code(),
            "extension_changed"
        );
        let long_name = format!("{}.cr3", "界".repeat(100));
        let result = new_name(
            &compiled(&rules("{name}")),
            &file(1, &long_name, None),
            None,
        )
        .expect("long name")
        .expect("matched");
        assert!(result.ends_with(".cr3"));
        assert!(result.len() <= crate::conflict::DEFAULT_MAX_NAME_BYTES);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn counters_group_sets_order_naturally_and_restart_per_folder() {
        let mut input = rules("{name}_{count}");
        input.clean = true;
        let compiled_rules = compiled(&input);
        let candidates = [
            file(1, "IMG_0001.jpg", Some(10)),
            file(2, "IMG_0001 (1).jpg", Some(10)),
            file(3, "IMG_0001 (2).jpg", Some(10)),
            file(4, "IMG_0001 (10).jpg", Some(10)),
        ];
        let counts = assign_counts(&compiled_rules, &candidates).expect("counts");
        assert_eq!(
            (1..=4).map(|id| counts[&id]).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        let paired = [
            file(1, "A.jpg", Some(1)),
            file(2, "A.cr3", Some(1)),
            file(3, "A (1).jpg", Some(1)),
        ];
        let counts = assign_counts(&compiled_rules, &paired).expect("counts");
        assert_eq!(counts[&1], 1);
        assert_eq!(counts[&2], 1);
        assert_eq!(counts[&3], 2);
        assert_eq!(
            new_name(&compiled_rules, &paired[0], Some(counts[&1])).expect("name"),
            Some("A_1.jpg".to_owned())
        );
        assert_eq!(
            new_name(&compiled_rules, &paired[1], Some(counts[&2])).expect("name"),
            Some("A_1.cr3".to_owned())
        );
        let other_folder = [file(9, "Z.jpg", Some(5))];
        assert_eq!(
            assign_counts(&compiled_rules, &other_folder).expect("counts")[&9],
            1
        );
        assert_eq!(
            assign_counts(&compiled_rules, &other_folder).expect("singleton")[&9],
            1
        );
        assert!(
            assign_counts(&compiled(&rules("{name}")), &other_folder)
                .expect("without count")
                .is_empty()
        );
    }

    #[test]
    fn counts_ignore_input_order_and_pad_without_truncating() {
        let mut input = rules("{name}_{count}");
        input.clean = true;
        input.count_digits = 2;
        let compiled = compiled(&input);
        let files = [file(1, "A.jpg", None), file(2, "A (1).jpg", None)];
        let forward = assign_counts(&compiled, &files).expect("counts");
        let reverse = assign_counts(&compiled, &files.iter().rev().copied().collect::<Vec<_>>())
            .expect("reverse counts");
        assert_eq!(forward, reverse);
        assert_eq!(format_count(1, 2), "01");
        assert_eq!(format_count(1234, 2), "1234");
    }

    proptest! {
        #[test]
        fn arbitrary_names_never_panic_or_violate_the_output_invariants(
            name in any::<String>(),
            pattern_text in prop::sample::select(vec![
                "{name}",
                "{filename}",
                "{YYYY}_{name}",
                "{name}_{count}",
                "renamed",
            ]),
            clean in any::<bool>(),
            rewrite in any::<bool>(),
        ) {
            let mut input = rules(pattern_text);
            input.clean = clean;
            if rewrite {
                input.rewrite = Some(crate::dto::RenameRewrite {
                    find: "x".to_owned(),
                    replace: "/".to_owned(),
                    ignore_case: true,
                });
            }
            let compiled = compile(&input).expect("generated rules are valid");
            let candidate = RenameFile {
                id: 1,
                name: &name,
                captured: NaiveDateTime::MIN,
                capture_ts: None,
                camera: None,
                folder: "",
                max_component_bytes: crate::conflict::DEFAULT_MAX_NAME_BYTES,
            };
            if let Ok(Some(result)) = new_name(&compiled, &candidate, Some(1)) {
                prop_assert!(!result.is_empty() && result != "." && result != "..");
                prop_assert!(!result.contains('/') && !result.contains('\0'));
                prop_assert_eq!(
                    names::split_name(&result).1,
                    names::split_name(&name).1
                );
            }
        }

        #[test]
        fn counts_are_positive_set_consistent_and_permutation_independent(
            variants in prop::collection::vec(0_u8..5, 1..16),
        ) {
            let compiled = compiled(&RenameRules {
                clean: true,
                pattern: "{name}_{count}".to_owned(),
                ..RenameRules::default()
            });
            let names = ["A.jpg", "A (1).jpg", "A (2).jpg", "A.cr3", "B.jpg"];
            let files: Vec<_> = variants
                .iter()
                .enumerate()
                .map(|(index, variant)| file(index as FileId + 1, names[usize::from(*variant)], None))
                .collect();
            let counts = assign_counts(&compiled, &files).expect("valid counts");
            let reversed = files.iter().rev().copied().collect::<Vec<_>>();
            prop_assert_eq!(assign_counts(&compiled, &reversed).expect("permuted counts"), counts.clone());
            for left in &files {
                prop_assert!(counts[&left.id] >= 1);
                for right in &files {
                    if names::stem_norm(left.name) == names::stem_norm(right.name) {
                        prop_assert_eq!(counts[&left.id], counts[&right.id]);
                    }
                    let left_stem = clean_stem(names::split_name(left.name).0).0;
                    let right_stem = clean_stem(names::split_name(right.name).0).0;
                    if names::normalize_name(&left_stem) == names::normalize_name(&right_stem)
                        && names::stem_norm(left.name) != names::stem_norm(right.name)
                    {
                        prop_assert_ne!(counts[&left.id], counts[&right.id]);
                    }
                }
            }
        }
    }

    #[test]
    fn compile_collects_issues_in_field_order_and_metadata_need_is_accurate() {
        let mut invalid = rules("{bad}");
        invalid.matcher.mode = RenameMatchMode::Regex;
        invalid.matcher.pattern = "(".to_owned();
        invalid.rewrite = Some(crate::dto::RenameRewrite {
            find: String::new(),
            replace: String::new(),
            ignore_case: true,
        });
        invalid.count_digits = 10;
        let issues = compile(&invalid).expect_err("several invalid fields");
        assert_eq!(
            issues.iter().map(|issue| issue.field).collect::<Vec<_>>(),
            [
                RenameIssueField::Matcher,
                RenameIssueField::Rewrite,
                RenameIssueField::Pattern,
                RenameIssueField::CountDigits,
            ]
        );
        assert!(!compiled(&rules("{name}")).needs_metadata());
        assert!(compiled(&rules("{YYYY}-{camera}")).needs_metadata());
        assert!(compiled(&rules("{count}")).needs_metadata());
    }

    #[test]
    fn compiled_rules_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompiledRules>();
    }

    #[test]
    fn rename_token_catalogue_adds_count_and_computed_examples() {
        let catalogue = tokens();
        assert_eq!(catalogue.tokens.len(), Token::ALL.len() + 1);
        assert_eq!(catalogue.tokens[12].name, "count");
        assert_eq!(catalogue.tokens[12].group, "file");
        assert_eq!(catalogue.examples.len(), 4);
        assert_eq!(catalogue.examples[0].result, "IMG_0042.CR3");
        assert_eq!(catalogue.examples[1].result, "IMG_0042_1.CR3");
        assert_eq!(catalogue.sample.file_name, "IMG_0042.CR3");
    }
}
