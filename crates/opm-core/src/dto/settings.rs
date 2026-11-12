//! Settings DTOs and their pure validation.
//!
//! Owned by todo `designing-database-schema` (typed settings service).
//!
//! [`SettingsDto`] is the body of `GET /api/settings` and `PUT /api/settings` (always the complete
//! document, there is no partial update) and also the JSON document the server stores under the key
//! `app` of its `settings` table. Validation and normalisation live here, free of IO, so the rules are
//! unit-testable; the server's `SettingsService` only adds persistence and change notification.
//!
//! A rejected document is answered with HTTP 422 and a [`ValidationErrorBody`]: the usual
//! `{"error": {"code", "message"}}` plus `error.fields`, a list of [`FieldError`]. Field paths use dots
//! for nested objects and brackets for list entries (`index.concurrency`, `sidecar_extensions[2]`).
//! The same body is meant for every endpoint that validates per field.

use std::collections::HashSet;
use std::fmt;
use std::ops::RangeInclusive;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;
use unicode_normalization::UnicodeNormalization;

use super::common::ConflictPolicy;
use crate::consts::{DEFAULT_JUNK_NAMES, DEFAULT_JUNK_PREFIXES, DEFAULT_SIDECAR_EXTENSIONS};
use crate::names::normalize_name;

/// Application settings: the complete document, read and written at once.
///
/// Every field is required in a `PUT`; unknown fields are ignored. The server answers with the
/// normalised document (extensions lowercased and de-duplicated, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SettingsDto {
    /// Extensions (lowercase letters and digits, no dot, 1 to 10 characters, at most 50) of the
    /// files that count as sidecars of the media file next to them, such as `xmp`. Applies to the next
    /// scan.
    pub sidecar_extensions: Vec<String>,
    /// Names (at most 200, compared case-insensitively) of files and folders that are never indexed
    /// and that do not count as content when deciding whether a folder is empty. Applies to the next
    /// scan.
    pub junk_names: Vec<String>,
    /// Name prefixes (at most 200, compared case-insensitively) with the same meaning as
    /// `junk_names`, for example `._` for AppleDouble resource forks. Applies to the next scan.
    pub junk_prefixes: Vec<String>,
    /// After moving a file to another volume, keep the original in the trash instead of deleting
    /// it. Uses extra space until the trash is emptied.
    pub keep_cross_volume_source_in_trash: bool,
    /// Longest file or folder name, in UTF-8 bytes (16 to 255), that generated names may have.
    /// Lower it for encrypted shares, which only allow 143.
    pub name_max_len: u16,
    /// What to do when a destination name is already taken, unless a plan says otherwise.
    pub default_conflict_policy: ConflictPolicy,
    /// Background indexing.
    pub index: IndexSettingsDto,
    /// Days after which trashed items are deleted automatically (1 to 3650); `null` keeps them until
    /// "Empty trash".
    pub trash_retention_days: Option<u32>,
    /// Folders (at most 1000) left out of the index and of every file operation, with everything
    /// below them. Missing from a stored document means none. `PUT /api/settings` keeps the stored
    /// list: change it with `POST /api/settings/excluded-folders` and
    /// `POST /api/settings/excluded-folders/remove`.
    #[serde(default)]
    pub excluded_folders: Vec<ExcludedFolder>,
}

/// One folder excluded from the index and from every file operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExcludedFolder {
    pub volume_id: i64,
    /// Path of the folder relative to the volume root; never the root itself.
    pub path: String,
}

/// Background indexing settings. The worker counts are read when a scan or batch starts, so a
/// change applies to the next one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IndexSettingsDto {
    /// Volumes scanned in parallel (1 to 16). Low values are right for a NAS CPU.
    pub concurrency: u8,
    /// Files hashed in parallel (1 to 8).
    pub hash_concurrency: u8,
    /// Thumbnails generated in parallel (1 to 8).
    pub thumbnail_concurrency: u8,
    /// Watch the volumes and re-index files as they change.
    pub watch_enabled: bool,
    /// Minutes between automatic full rescans: `0` never rescans, otherwise 5 to 10080.
    pub rescan_interval_minutes: u32,
}

/// One rejected field of a validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldError {
    /// Path of the field in the request body: dots for nested objects, brackets for list entries
    /// (`index.concurrency`, `sidecar_extensions[2]`).
    pub field: String,
    /// Human-readable reason.
    pub message: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// The `error` object of a 422 `validation_failed` response: an `ErrorDetail` that also lists the
/// rejected fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ValidationErrorDetail {
    /// Always `validation_failed`.
    pub code: String,
    /// All the field messages in one line (`field: message; field: message`), for clients that do not
    /// map errors to fields.
    pub message: String,
    /// One entry per rejected field.
    pub fields: Vec<FieldError>,
}

/// Body of a 422 `validation_failed` response. It is a superset of `ErrorBody`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ValidationErrorBody {
    pub error: ValidationErrorDetail,
}

impl ValidationErrorBody {
    /// Value of `error.code`.
    pub const CODE: &'static str = "validation_failed";

    pub fn new(fields: Vec<FieldError>) -> Self {
        let message = if fields.is_empty() {
            "validation failed".to_owned()
        } else {
            fields
                .iter()
                .map(FieldError::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        };
        Self {
            error: ValidationErrorDetail {
                code: Self::CODE.to_owned(),
                message,
                fields,
            },
        }
    }
}

/// Settings read from storage, plus what had to be repaired on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSettings {
    pub settings: SettingsDto,
    /// Stored values that were ignored (wrong type, no longer valid): one entry per problem. The
    /// affected fields hold their defaults.
    pub issues: Vec<FieldError>,
}

impl SettingsDto {
    pub const NAME_MAX_LEN_RANGE: RangeInclusive<u16> = 16..=255;
    pub const INDEX_CONCURRENCY_RANGE: RangeInclusive<u8> = 1..=16;
    pub const HASH_CONCURRENCY_RANGE: RangeInclusive<u8> = 1..=8;
    pub const THUMBNAIL_CONCURRENCY_RANGE: RangeInclusive<u8> = 1..=8;
    /// Besides `0`, which disables the periodic rescan.
    pub const RESCAN_INTERVAL_RANGE_MINUTES: RangeInclusive<u32> = 5..=10_080;
    pub const TRASH_RETENTION_DAYS_RANGE: RangeInclusive<u32> = 1..=3_650;
    pub const MAX_SIDECAR_EXTENSIONS: usize = 50;
    pub const MAX_SIDECAR_EXTENSION_LEN: usize = 10;
    pub const MAX_JUNK_ENTRIES: usize = 200;
    pub const MAX_JUNK_ENTRY_BYTES: usize = 255;
    pub const MAX_EXCLUDED_FOLDERS: usize = 1_000;

    /// The factory settings. `index_concurrency` is the operator's `OPM_INDEX_CONCURRENCY` and `cpus`
    /// the number of CPU cores; low worker counts are the right default for a NAS.
    pub fn defaults_for(index_concurrency: usize, cpus: usize) -> Self {
        let concurrency = clamp_u8(index_concurrency, &Self::INDEX_CONCURRENCY_RANGE);
        let hash = usize::from(concurrency / 2).clamp(1, 4);
        let thumbnails = usize::from(concurrency).min(cpus);
        Self {
            sidecar_extensions: owned(DEFAULT_SIDECAR_EXTENSIONS),
            junk_names: owned(DEFAULT_JUNK_NAMES),
            junk_prefixes: owned(DEFAULT_JUNK_PREFIXES),
            keep_cross_volume_source_in_trash: false,
            name_max_len: *Self::NAME_MAX_LEN_RANGE.end(),
            default_conflict_policy: ConflictPolicy::default(),
            index: IndexSettingsDto {
                concurrency,
                hash_concurrency: clamp_u8(hash, &Self::HASH_CONCURRENCY_RANGE),
                thumbnail_concurrency: clamp_u8(thumbnails, &Self::THUMBNAIL_CONCURRENCY_RANGE),
                watch_enabled: true,
                rescan_interval_minutes: 360,
            },
            trash_retention_days: None,
            excluded_folders: Vec::new(),
        }
    }

    /// Validates the document and returns its normalised form: extensions trimmed, stripped of one
    /// leading dot, lowercased and de-duplicated; junk entries trimmed, NFC-normalised and
    /// de-duplicated case-insensitively (the first spelling is kept); blank list entries dropped.
    /// Every problem is reported, not just the first. Normalising a normalised document changes
    /// nothing.
    pub fn normalized(self) -> Result<Self, Vec<FieldError>> {
        let mut errors = Vec::new();
        let sidecar_extensions = keep(normalize_extensions(&self.sidecar_extensions), &mut errors);
        let junk_names = keep(normalize_junk_names(&self.junk_names), &mut errors);
        let junk_prefixes = keep(normalize_junk_prefixes(&self.junk_prefixes), &mut errors);
        let name_max_len = keep(check_name_max_len(self.name_max_len), &mut errors);
        let concurrency = keep(check_concurrency(self.index.concurrency), &mut errors);
        let hash_concurrency = keep(
            check_hash_concurrency(self.index.hash_concurrency),
            &mut errors,
        );
        let thumbnail_concurrency = keep(
            check_thumbnail_concurrency(self.index.thumbnail_concurrency),
            &mut errors,
        );
        let rescan = keep(
            check_rescan_interval(self.index.rescan_interval_minutes),
            &mut errors,
        );
        let retention = keep(
            check_trash_retention(self.trash_retention_days),
            &mut errors,
        );
        let excluded = keep(
            normalize_excluded_folders(&self.excluded_folders),
            &mut errors,
        );
        let (
            Some(sidecar_extensions),
            Some(junk_names),
            Some(junk_prefixes),
            Some(name_max_len),
            Some(concurrency),
            Some(hash_concurrency),
            Some(thumbnail_concurrency),
            Some(rescan_interval_minutes),
            Some(trash_retention_days),
            Some(excluded_folders),
        ) = (
            sidecar_extensions,
            junk_names,
            junk_prefixes,
            name_max_len,
            concurrency,
            hash_concurrency,
            thumbnail_concurrency,
            rescan,
            retention,
            excluded,
        )
        else {
            return Err(errors);
        };
        Ok(Self {
            sidecar_extensions,
            junk_names,
            junk_prefixes,
            keep_cross_volume_source_in_trash: self.keep_cross_volume_source_in_trash,
            name_max_len,
            default_conflict_policy: self.default_conflict_policy,
            index: IndexSettingsDto {
                concurrency,
                hash_concurrency,
                thumbnail_concurrency,
                watch_enabled: self.index.watch_enabled,
                rescan_interval_minutes,
            },
            trash_retention_days,
            excluded_folders,
        })
    }

    /// Builds the settings from a stored JSON document, never failing so that an upgrade (or a
    /// downgrade) can not stop the server from starting: the stored values are laid over `defaults`
    /// key by key; a missing key keeps its default, a value of the wrong type or one that no longer
    /// validates is ignored (its field keeps the default, a problem is reported), unknown keys are
    /// ignored.
    pub fn from_stored(stored: &Value, defaults: &Self) -> LoadedSettings {
        let mut settings = defaults.clone();
        let mut issues = Vec::new();
        let Some(object) = stored.as_object() else {
            issues.push(FieldError::new(
                "",
                "the stored document is not a JSON object",
            ));
            return LoadedSettings { settings, issues };
        };

        let top = Loader::new(object, "");
        top.field(
            &mut issues,
            "sidecar_extensions",
            &mut settings.sidecar_extensions,
            |v| normalize_extensions(v),
        );
        top.field(&mut issues, "junk_names", &mut settings.junk_names, |v| {
            normalize_junk_names(v)
        });
        top.field(
            &mut issues,
            "junk_prefixes",
            &mut settings.junk_prefixes,
            |v| normalize_junk_prefixes(v),
        );
        top.field(
            &mut issues,
            "keep_cross_volume_source_in_trash",
            &mut settings.keep_cross_volume_source_in_trash,
            |v| Ok(*v),
        );
        top.field(
            &mut issues,
            "name_max_len",
            &mut settings.name_max_len,
            |v| check_name_max_len(*v),
        );
        top.field(
            &mut issues,
            "default_conflict_policy",
            &mut settings.default_conflict_policy,
            |v| Ok(*v),
        );
        top.field(
            &mut issues,
            "trash_retention_days",
            &mut settings.trash_retention_days,
            |v| check_trash_retention(*v),
        );
        top.field(
            &mut issues,
            "excluded_folders",
            &mut settings.excluded_folders,
            |v| normalize_excluded_folders(v),
        );

        match object.get("index") {
            None => {}
            Some(Value::Object(index)) => {
                let nested = Loader::new(index, "index.");
                let target = &mut settings.index;
                nested.field(&mut issues, "concurrency", &mut target.concurrency, |v| {
                    check_concurrency(*v)
                });
                nested.field(
                    &mut issues,
                    "hash_concurrency",
                    &mut target.hash_concurrency,
                    |v| check_hash_concurrency(*v),
                );
                nested.field(
                    &mut issues,
                    "thumbnail_concurrency",
                    &mut target.thumbnail_concurrency,
                    |v| check_thumbnail_concurrency(*v),
                );
                nested.field(
                    &mut issues,
                    "watch_enabled",
                    &mut target.watch_enabled,
                    |v| Ok(*v),
                );
                nested.field(
                    &mut issues,
                    "rescan_interval_minutes",
                    &mut target.rescan_interval_minutes,
                    |v| check_rescan_interval(*v),
                );
            }
            Some(_) => issues.push(FieldError::new("index", "is not a JSON object")),
        }
        LoadedSettings { settings, issues }
    }
}

/// Reads one key of a stored JSON object into a settings field.
struct Loader<'a> {
    object: &'a Map<String, Value>,
    /// Path of the object inside the document: `""` or `"index."`.
    prefix: &'static str,
}

impl<'a> Loader<'a> {
    fn new(object: &'a Map<String, Value>, prefix: &'static str) -> Self {
        Self { object, prefix }
    }

    fn field<T: DeserializeOwned>(
        &self,
        issues: &mut Vec<FieldError>,
        key: &str,
        slot: &mut T,
        normalize: impl FnOnce(&T) -> Result<T, Vec<FieldError>>,
    ) {
        let Some(raw) = self.object.get(key) else {
            return;
        };
        match serde_json::from_value::<T>(raw.clone()) {
            Ok(value) => match normalize(&value) {
                Ok(valid) => *slot = valid,
                Err(errors) => issues.extend(errors),
            },
            Err(err) => issues.push(FieldError::new(
                format!("{}{key}", self.prefix),
                format!("has the wrong type ({err})"),
            )),
        }
    }
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn clamp_u8(value: usize, range: &RangeInclusive<u8>) -> u8 {
    let clamped = value.clamp(usize::from(*range.start()), usize::from(*range.end()));
    u8::try_from(clamped).unwrap_or(*range.end())
}

/// Moves a validation result into `Option`, collecting its errors.
fn keep<T>(result: Result<T, Vec<FieldError>>, errors: &mut Vec<FieldError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(mut found) => {
            errors.append(&mut found);
            None
        }
    }
}

fn check_range<T>(field: &str, value: T, range: &RangeInclusive<T>) -> Result<T, Vec<FieldError>>
where
    T: PartialOrd + fmt::Display,
{
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(vec![FieldError::new(
            field,
            format!("must be between {} and {}", range.start(), range.end()),
        )])
    }
}

fn check_name_max_len(value: u16) -> Result<u16, Vec<FieldError>> {
    check_range("name_max_len", value, &SettingsDto::NAME_MAX_LEN_RANGE)
}

fn check_concurrency(value: u8) -> Result<u8, Vec<FieldError>> {
    check_range(
        "index.concurrency",
        value,
        &SettingsDto::INDEX_CONCURRENCY_RANGE,
    )
}

fn check_hash_concurrency(value: u8) -> Result<u8, Vec<FieldError>> {
    check_range(
        "index.hash_concurrency",
        value,
        &SettingsDto::HASH_CONCURRENCY_RANGE,
    )
}

fn check_thumbnail_concurrency(value: u8) -> Result<u8, Vec<FieldError>> {
    check_range(
        "index.thumbnail_concurrency",
        value,
        &SettingsDto::THUMBNAIL_CONCURRENCY_RANGE,
    )
}

fn check_rescan_interval(value: u32) -> Result<u32, Vec<FieldError>> {
    let range = &SettingsDto::RESCAN_INTERVAL_RANGE_MINUTES;
    if value == 0 || range.contains(&value) {
        Ok(value)
    } else {
        Err(vec![FieldError::new(
            "index.rescan_interval_minutes",
            format!(
                "must be 0 (never) or between {} and {}",
                range.start(),
                range.end()
            ),
        )])
    }
}

fn check_trash_retention(value: Option<u32>) -> Result<Option<u32>, Vec<FieldError>> {
    match value {
        None => Ok(None),
        Some(days) => check_range(
            "trash_retention_days",
            days,
            &SettingsDto::TRASH_RETENTION_DAYS_RANGE,
        )
        .map(Some),
    }
}

fn normalize_extensions(input: &[String]) -> Result<Vec<String>, Vec<FieldError>> {
    let mut errors = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for (index, raw) in input.iter().enumerate() {
        let trimmed = raw.trim();
        let extension = trimmed
            .strip_prefix('.')
            .unwrap_or(trimmed)
            .to_ascii_lowercase();
        if extension.is_empty() {
            continue;
        }
        let valid = extension.len() <= SettingsDto::MAX_SIDECAR_EXTENSION_LEN
            && extension
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if !valid {
            errors.push(FieldError::new(
                format!("sidecar_extensions[{index}]"),
                format!(
                    "must be 1 to {} lowercase letters or digits, without a dot",
                    SettingsDto::MAX_SIDECAR_EXTENSION_LEN
                ),
            ));
        } else if !out.contains(&extension) {
            out.push(extension);
        }
    }
    if out.len() > SettingsDto::MAX_SIDECAR_EXTENSIONS {
        errors.push(FieldError::new(
            "sidecar_extensions",
            format!(
                "at most {} extensions are allowed",
                SettingsDto::MAX_SIDECAR_EXTENSIONS
            ),
        ));
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Validates the excluded folders and returns them normalised: paths cleaned by `RelPath`, sorted,
/// de-duplicated, and an entry dropped when an ancestor on the same volume is already excluded.
pub fn normalize_excluded_folders(
    input: &[ExcludedFolder],
) -> Result<Vec<ExcludedFolder>, Vec<FieldError>> {
    let mut errors = Vec::new();
    if input.len() > SettingsDto::MAX_EXCLUDED_FOLDERS {
        errors.push(FieldError::new(
            "excluded_folders",
            format!(
                "at most {} folders can be excluded",
                SettingsDto::MAX_EXCLUDED_FOLDERS
            ),
        ));
    }
    let mut parsed = Vec::with_capacity(input.len());
    for (i, entry) in input.iter().enumerate() {
        match crate::paths::RelPath::new(&entry.path) {
            Ok(path) if !path.is_root() => parsed.push((entry.volume_id, path)),
            Ok(_) => errors.push(FieldError::new(
                format!("excluded_folders[{i}].path"),
                "a whole volume cannot be excluded",
            )),
            Err(err) => errors.push(FieldError::new(
                format!("excluded_folders[{i}].path"),
                err.to_string(),
            )),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    parsed.sort_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));
    let mut out: Vec<ExcludedFolder> = Vec::with_capacity(parsed.len());
    let mut kept: Vec<(i64, crate::paths::RelPath)> = Vec::new();
    for (volume_id, path) in parsed {
        if kept
            .iter()
            .any(|(v, base)| *v == volume_id && path.starts_with(base))
        {
            continue;
        }
        out.push(ExcludedFolder {
            volume_id,
            path: path.as_str().to_owned(),
        });
        kept.push((volume_id, path));
    }
    Ok(out)
}

fn normalize_junk_names(input: &[String]) -> Result<Vec<String>, Vec<FieldError>> {
    normalize_junk("junk_names", input, true)
}

fn normalize_junk_prefixes(input: &[String]) -> Result<Vec<String>, Vec<FieldError>> {
    normalize_junk("junk_prefixes", input, false)
}

/// Shared rules of `junk_names` and `junk_prefixes`. A prefix may be `.` (every hidden file); a
/// name may not be `.` or `..`, which no directory entry ever has.
fn normalize_junk(
    field: &str,
    input: &[String],
    reject_dot_names: bool,
) -> Result<Vec<String>, Vec<FieldError>> {
    let mut errors = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (index, raw) in input.iter().enumerate() {
        let entry: String = raw.trim().nfc().collect();
        if entry.is_empty() {
            continue;
        }
        let problem = if entry.contains('/') || entry.contains('\0') {
            Some("must not contain `/` or NUL")
        } else if reject_dot_names && (entry == "." || entry == "..") {
            Some("must not be `.` or `..`")
        } else if entry.len() > SettingsDto::MAX_JUNK_ENTRY_BYTES {
            Some("is too long (at most 255 bytes)")
        } else {
            None
        };
        if let Some(message) = problem {
            errors.push(FieldError::new(format!("{field}[{index}]"), message));
        } else if seen.insert(normalize_name(&entry)) {
            out.push(entry);
        }
    }
    if out.len() > SettingsDto::MAX_JUNK_ENTRIES {
        errors.push(FieldError::new(
            field,
            format!(
                "at most {} entries are allowed",
                SettingsDto::MAX_JUNK_ENTRIES
            ),
        ));
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    fn defaults() -> SettingsDto {
        SettingsDto::defaults_for(2, 4)
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn fields(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|error| error.field.as_str()).collect()
    }

    #[test]
    fn defaults_come_from_the_shared_constants() {
        let settings = defaults();
        assert_eq!(
            settings.sidecar_extensions,
            strings(DEFAULT_SIDECAR_EXTENSIONS)
        );
        assert_eq!(settings.junk_names, strings(DEFAULT_JUNK_NAMES));
        assert_eq!(settings.junk_prefixes, strings(DEFAULT_JUNK_PREFIXES));
        assert!(!settings.keep_cross_volume_source_in_trash);
        assert_eq!(settings.name_max_len, 255);
        assert_eq!(settings.default_conflict_policy, ConflictPolicy::Smart);
        assert!(settings.index.watch_enabled);
        assert_eq!(settings.index.rescan_interval_minutes, 360);
        assert_eq!(settings.trash_retention_days, None);
    }

    #[test]
    fn defaults_are_valid_and_already_normalised() {
        for (concurrency, cpus) in [(0, 0), (1, 1), (2, 4), (8, 2), (64, 128), (usize::MAX, 1)] {
            let settings = SettingsDto::defaults_for(concurrency, cpus);
            assert_eq!(
                settings.clone().normalized(),
                Ok(settings),
                "concurrency {concurrency}, cpus {cpus}"
            );
        }
    }

    #[test]
    fn worker_defaults_follow_the_configured_concurrency_and_the_cpu_count() {
        let low = SettingsDto::defaults_for(1, 8).index;
        assert_eq!(
            (
                low.concurrency,
                low.hash_concurrency,
                low.thumbnail_concurrency
            ),
            (1, 1, 1)
        );
        let nas = SettingsDto::defaults_for(2, 4).index;
        assert_eq!(
            (
                nas.concurrency,
                nas.hash_concurrency,
                nas.thumbnail_concurrency
            ),
            (2, 1, 2)
        );
        let big = SettingsDto::defaults_for(12, 4).index;
        assert_eq!(
            (
                big.concurrency,
                big.hash_concurrency,
                big.thumbnail_concurrency
            ),
            (12, 4, 4)
        );
        let huge = SettingsDto::defaults_for(500, 500).index;
        assert_eq!(
            (
                huge.concurrency,
                huge.hash_concurrency,
                huge.thumbnail_concurrency
            ),
            (16, 4, 8)
        );
    }

    #[test]
    fn the_wire_format_is_a_flat_snake_case_document() {
        let wire = roundtrip(&defaults());
        let mut keys: Vec<_> = wire.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "default_conflict_policy",
                "excluded_folders",
                "index",
                "junk_names",
                "junk_prefixes",
                "keep_cross_volume_source_in_trash",
                "name_max_len",
                "sidecar_extensions",
                "trash_retention_days",
            ]
        );
        assert_eq!(wire["default_conflict_policy"], json!("smart"));
        assert_eq!(wire["trash_retention_days"], json!(null));
        assert_eq!(wire["index"]["rescan_interval_minutes"], json!(360));
    }

    #[test]
    fn a_put_must_carry_the_whole_document_and_may_carry_unknown_fields() {
        let mut wire = serde_json::to_value(defaults()).unwrap();
        wire["from_a_newer_client"] = json!(true);
        let parsed: SettingsDto = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed, defaults());

        wire.as_object_mut().unwrap().remove("name_max_len");
        assert!(serde_json::from_value::<SettingsDto>(wire).is_err());
    }

    #[test]
    fn extensions_are_trimmed_unprefixed_lowercased_and_deduplicated() {
        let mut settings = defaults();
        settings.sidecar_extensions =
            strings(&[" .XMP ", "json", "Xmp", "", "  ", ".", "dop", "JSON"]);
        let normalised = settings.normalized().unwrap();
        assert_eq!(
            normalised.sidecar_extensions,
            strings(&["xmp", "json", "dop"])
        );
    }

    #[test]
    fn an_empty_extension_list_is_allowed() {
        let mut settings = defaults();
        settings.sidecar_extensions = Vec::new();
        assert_eq!(settings.clone().normalized(), Ok(settings));
    }

    #[test]
    fn invalid_extensions_are_reported_at_their_input_position() {
        let mut settings = defaults();
        settings.sidecar_extensions = strings(&["xmp", "a b", "..xmp", "waytoolongext", "é", "ok"]);
        let errors = settings.normalized().unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "sidecar_extensions[1]",
                "sidecar_extensions[2]",
                "sidecar_extensions[3]",
                "sidecar_extensions[4]",
            ]
        );
    }

    #[test]
    fn extension_count_is_capped_after_normalisation() {
        let mut settings = defaults();
        settings.sidecar_extensions = (0..SettingsDto::MAX_SIDECAR_EXTENSIONS)
            .map(|n| format!("e{n}"))
            .collect();
        assert!(settings.clone().normalized().is_ok());
        settings.sidecar_extensions.push("one".to_owned());
        let errors = settings.clone().normalized().unwrap_err();
        assert_eq!(fields(&errors), ["sidecar_extensions"]);
        // Duplicates do not count towards the cap.
        settings.sidecar_extensions.pop();
        settings.sidecar_extensions.push("E1".to_owned());
        assert!(settings.normalized().is_ok());
    }

    #[test]
    fn junk_entries_are_trimmed_nfc_and_deduplicated_case_insensitively() {
        let mut settings = defaults();
        settings.junk_names = vec![
            "  @eaDir ".to_owned(),
            "#recycle".to_owned(),
            "@EADIR".to_owned(),
            "".to_owned(),
            "cafe\u{301}".to_owned(),
            "CAF\u{e9}".to_owned(),
        ];
        settings.junk_prefixes = strings(&["._", "._", " .", ""]);
        let normalised = settings.normalized().unwrap();
        assert_eq!(
            normalised.junk_names,
            ["@eaDir", "#recycle", "caf\u{e9}"].map(str::to_owned)
        );
        assert_eq!(normalised.junk_prefixes, strings(&["._", "."]));
    }

    #[test]
    fn invalid_junk_entries_are_reported() {
        let mut settings = defaults();
        settings.junk_names = vec![
            "ok".to_owned(),
            "a/b".to_owned(),
            ".".to_owned(),
            "..".to_owned(),
            "nul\0".to_owned(),
            "x".repeat(256),
        ];
        settings.junk_prefixes = strings(&["fine", "../"]);
        let errors = settings.normalized().unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                "junk_names[1]",
                "junk_names[2]",
                "junk_names[3]",
                "junk_names[4]",
                "junk_names[5]",
                "junk_prefixes[1]",
            ]
        );
    }

    #[test]
    fn junk_entry_count_is_capped() {
        let mut settings = defaults();
        settings.junk_names = (0..=SettingsDto::MAX_JUNK_ENTRIES)
            .map(|n| format!("n{n}"))
            .collect();
        let errors = settings.normalized().unwrap_err();
        assert_eq!(fields(&errors), ["junk_names"]);
    }

    #[test]
    fn numeric_limits_are_inclusive() {
        let ok = |edit: fn(&mut SettingsDto)| {
            let mut settings = defaults();
            edit(&mut settings);
            settings.normalized().is_ok()
        };
        assert!(ok(|s| s.name_max_len = 16));
        assert!(ok(|s| s.name_max_len = 255));
        assert!(!ok(|s| s.name_max_len = 15));
        assert!(!ok(|s| s.name_max_len = 256));
        assert!(ok(|s| s.index.concurrency = 16));
        assert!(!ok(|s| s.index.concurrency = 0));
        assert!(!ok(|s| s.index.concurrency = 17));
        assert!(ok(|s| s.index.hash_concurrency = 8));
        assert!(!ok(|s| s.index.hash_concurrency = 9));
        assert!(!ok(|s| s.index.thumbnail_concurrency = 0));
        assert!(ok(|s| s.index.rescan_interval_minutes = 0));
        assert!(!ok(|s| s.index.rescan_interval_minutes = 4));
        assert!(ok(|s| s.index.rescan_interval_minutes = 5));
        assert!(ok(|s| s.index.rescan_interval_minutes = 10_080));
        assert!(!ok(|s| s.index.rescan_interval_minutes = 10_081));
        assert!(ok(|s| s.trash_retention_days = None));
        assert!(ok(|s| s.trash_retention_days = Some(1)));
        assert!(ok(|s| s.trash_retention_days = Some(3_650)));
        assert!(!ok(|s| s.trash_retention_days = Some(0)));
        assert!(!ok(|s| s.trash_retention_days = Some(3_651)));
    }

    #[test]
    fn every_problem_is_reported_with_its_path() {
        let mut settings = defaults();
        settings.name_max_len = 1;
        settings.index.concurrency = 0;
        settings.index.rescan_interval_minutes = 3;
        settings.trash_retention_days = Some(0);
        settings.sidecar_extensions = strings(&["bad ext"]);
        let errors = settings.normalized().unwrap_err();
        let mut found = fields(&errors);
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "index.concurrency",
                "index.rescan_interval_minutes",
                "name_max_len",
                "sidecar_extensions[0]",
                "trash_retention_days",
            ]
        );
        assert!(errors.iter().all(|error| !error.message.is_empty()));
    }

    #[test]
    fn the_validation_body_has_the_documented_shape() {
        let body = ValidationErrorBody::new(vec![
            FieldError::new("index.concurrency", "must be between 1 and 16"),
            FieldError::new("name_max_len", "must be between 16 and 255"),
        ]);
        let wire = roundtrip(&body);
        assert_eq!(
            wire,
            json!({ "error": {
                "code": "validation_failed",
                "message": "index.concurrency: must be between 1 and 16; name_max_len: must be between 16 and 255",
                "fields": [
                    { "field": "index.concurrency", "message": "must be between 1 and 16" },
                    { "field": "name_max_len", "message": "must be between 16 and 255" },
                ],
            } })
        );
        assert_eq!(
            ValidationErrorBody::new(Vec::new()).error.message,
            "validation failed"
        );
    }

    #[test]
    fn a_stored_document_that_is_complete_loads_unchanged() {
        let mut settings = defaults();
        settings.name_max_len = 143;
        settings.trash_retention_days = Some(30);
        settings.default_conflict_policy = ConflictPolicy::Rename;
        let stored = serde_json::to_value(&settings).unwrap();
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        assert_eq!(loaded.settings, settings);
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn a_partial_stored_document_keeps_the_defaults_for_missing_keys() {
        let stored = json!({
            "name_max_len": 100,
            "index": { "concurrency": 3 },
        });
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        let mut expected = defaults();
        expected.name_max_len = 100;
        expected.index.concurrency = 3;
        assert_eq!(loaded.settings, expected);
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn an_empty_stored_document_is_the_defaults() {
        let loaded = SettingsDto::from_stored(&json!({}), &defaults());
        assert_eq!(loaded.settings, defaults());
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn stored_null_retention_means_keep_forever() {
        let mut with_default_retention = defaults();
        with_default_retention.trash_retention_days = Some(14);
        let loaded = SettingsDto::from_stored(
            &json!({ "trash_retention_days": null }),
            &with_default_retention,
        );
        assert_eq!(loaded.settings.trash_retention_days, None);
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn unknown_stored_keys_are_ignored() {
        let stored = json!({ "future": 1, "index": { "future": [1, 2] }, "name_max_len": 200 });
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        assert_eq!(loaded.settings.name_max_len, 200);
        assert!(loaded.issues.is_empty());
    }

    #[test]
    fn stored_values_of_the_wrong_type_or_out_of_range_fall_back_one_field_at_a_time() {
        let stored = json!({
            "name_max_len": "wide",
            "sidecar_extensions": ["xmp", "bad ext"],
            "junk_names": ["Keep Me"],
            "default_conflict_policy": "explode",
            "index": { "concurrency": 300, "hash_concurrency": 0, "watch_enabled": false },
            "trash_retention_days": -1,
        });
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        let mut expected = defaults();
        expected.junk_names = strings(&["Keep Me"]);
        expected.index.watch_enabled = false;
        assert_eq!(loaded.settings, expected);
        let mut problems = fields(&loaded.issues);
        problems.sort_unstable();
        assert_eq!(
            problems,
            [
                "default_conflict_policy",
                "index.concurrency",
                "index.hash_concurrency",
                "name_max_len",
                "sidecar_extensions[1]",
                "trash_retention_days",
            ]
        );
    }

    #[test]
    fn stored_values_are_normalised_like_a_put() {
        let stored = json!({ "sidecar_extensions": [".XMP", "xmp", "Json"] });
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        assert_eq!(
            loaded.settings.sidecar_extensions,
            strings(&["xmp", "json"])
        );
    }

    #[test]
    fn a_stored_document_of_the_wrong_shape_is_the_defaults() {
        for stored in [json!(null), json!([1, 2]), json!("text"), json!(7)] {
            let loaded = SettingsDto::from_stored(&stored, &defaults());
            assert_eq!(loaded.settings, defaults());
            assert_eq!(fields(&loaded.issues), [""]);
        }
        let loaded = SettingsDto::from_stored(&json!({ "index": 5 }), &defaults());
        assert_eq!(loaded.settings, defaults());
        assert_eq!(fields(&loaded.issues), ["index"]);
    }

    fn excluded(volume_id: i64, path: &str) -> ExcludedFolder {
        ExcludedFolder {
            volume_id,
            path: path.to_owned(),
        }
    }

    #[test]
    fn excluded_folders_are_cleaned_sorted_and_covered_entries_dropped() {
        let mut settings = defaults();
        settings.excluded_folders = vec![
            excluded(1, "b//c/"),
            excluded(1, "a/x"),
            excluded(1, "a"),
            excluded(2, "a/x"),
            excluded(1, "ab"),
            excluded(1, "a"),
        ];
        let normalized = settings.normalized().unwrap();
        assert_eq!(
            normalized.excluded_folders,
            [
                excluded(1, "a"),
                excluded(1, "ab"),
                excluded(1, "b/c"),
                excluded(2, "a/x")
            ]
        );
    }

    #[test]
    fn excluding_a_root_or_an_escaping_path_is_rejected() {
        let mut settings = defaults();
        settings.excluded_folders = vec![excluded(1, ""), excluded(1, "../x")];
        let errors = settings.normalized().unwrap_err();
        assert_eq!(
            fields(&errors),
            ["excluded_folders[0].path", "excluded_folders[1].path"]
        );
    }

    #[test]
    fn a_document_without_excluded_folders_still_parses() {
        let mut value = serde_json::to_value(defaults()).unwrap();
        value.as_object_mut().unwrap().remove("excluded_folders");
        let parsed: SettingsDto = serde_json::from_value(value).unwrap();
        assert!(parsed.excluded_folders.is_empty());
        let stored = json!({ "excluded_folders": [{ "volume_id": 3, "path": "x/y" }] });
        let loaded = SettingsDto::from_stored(&stored, &defaults());
        assert_eq!(loaded.settings.excluded_folders, [excluded(3, "x/y")]);
    }

    proptest! {
        #[test]
        fn normalising_twice_changes_nothing(
            extensions in proptest::collection::vec("\\PC{0,12}", 0..8),
            names in proptest::collection::vec("\\PC{0,12}", 0..8),
        ) {
            let mut settings = defaults();
            settings.sidecar_extensions = extensions;
            settings.junk_names = names.clone();
            settings.junk_prefixes = names;
            if let Ok(once) = settings.normalized() {
                prop_assert_eq!(once.clone().normalized(), Ok(once));
            }
        }
    }
}
