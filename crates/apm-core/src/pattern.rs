//! Folder-pattern language: parser, formatter and name sanitiser (`{YYYY}/{mm}/...`).
//!
//! The move planner (custom structure, folder formatting) and the live-preview endpoint
//! `POST /api/format/preview` both call this single implementation; the UI never reimplements it.
//!
//! ```
//! use apm_core::pattern::{FileContext, Pattern};
//! use chrono::NaiveDate;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let pattern: Pattern = "{YYYY}/{mm}".parse()?;
//! let ctx = FileContext {
//!     file_name: "IMG_0001.jpg".to_owned(),
//!     captured: NaiveDate::from_ymd_opt(2026, 10, 5)
//!         .and_then(|day| day.and_hms_opt(14, 7, 9))
//!         .ok_or("invalid date")?,
//!     camera: Some("Canon EOS R5".to_owned()),
//!     folder: "Holiday".to_owned(),
//! };
//! assert_eq!(pattern.render(&ctx).as_str(), "2026/10/IMG_0001.jpg");
//! # Ok(())
//! # }
//! ```
//!
//! # Syntax
//!
//! * Literal text is copied as written and `/` separates folder levels.
//! * `{token}` is replaced by a value (table below); `{{` and `}}` are literal braces. A lone `}` or
//!   an unclosed `{` is an error.
//! * Empty and `.` components are dropped (`a//b/./c/` means `a/b/c`). A pattern must not be empty,
//!   must not start with `/` (it is relative to the destination folder) and must not contain a `..`
//!   component.
//! * Literal text must not contain control characters or `\` (write `/` to separate folders).
//!   Everything else is kept verbatim: only token values are sanitised.
//! * Tokens are strict: case-sensitive, no whitespace inside the braces. An unknown token is an error
//!   carrying the byte [`Span`] to underline and, when one is close, a "did you mean" suggestion
//!   (`{yyyy}` suggests `{YYYY}`, `{month}` suggests `{mm}`).
//!
//! | Token | Value | Example |
//! |---|---|---|
//! | `{YYYY}` | year, 4 digits | `2026` |
//! | `{YY}` | year, last 2 digits | `26` |
//! | `{mm}`, `{MM}` | month number, 2 digits (both spellings mean month; minutes are `{min}`) | `10` |
//! | `{MMM}` | month, English abbreviation | `Oct` |
//! | `{MMMM}` | month, English name | `October` |
//! | `{dd}` | day of the month, 2 digits | `05` |
//! | `{HH}` | hour on the 24-hour clock, 2 digits | `14` |
//! | `{min}` | minute, 2 digits | `07` |
//! | `{sec}` | second, 2 digits | `09` |
//! | `{filename}` | original file name | `IMG_0042.CR3` |
//! | `{name}` | original file name without its extension | `IMG_0042` |
//! | `{ext}` | extension without the dot, original case; empty when there is none | `CR3` |
//! | `{camera}` | camera name, or the placeholder (`Unknown camera`) when it is unknown | `Canon EOS R5` |
//! | `{folder}` | name of the folder the file is in now (empty at the volume root) | `Holiday` |
//!
//! The date tokens read [`FileContext::captured`]: the resolved capture date as the photographer's
//! local wall clock ([`crate::datetime::CaptureDate::naive`]), never converted to another zone. Years
//! are clamped to `0000..=9999`.
//!
//! # The file name
//!
//! The rendered path ends with the file name. If the LAST component of the pattern contains
//! `{filename}` or `{name}` it names the file (`{YYYY}/{name}_{camera}.{ext}`); otherwise the
//! original file name is appended as one more component, so `{YYYY}/{mm}` gives
//! `2026/10/IMG_0001.jpg`. A trailing `/` also forces the append, which makes `{YYYY}/{name}/` a folder
//! per file stem instead of a file renamed to its stem without extension. `{name}` or `{filename}`
//! in an earlier component is just a folder name.
//!
//! # Sanitising
//!
//! * Token values that come from data (`{camera}`, `{folder}`, the date tokens) have `/`, `\`, NUL,
//!   control characters and the characters SMB and Windows reject (`: * ? " < > |`) replaced by
//!   `_`. See [`sanitize_value`].
//! * The original file name already exists on disk, so `{filename}`, `{name}`, `{ext}` and the
//!   appended name keep every character (only `/` and NUL, which no file name has, become `_`) and a
//!   file component that equals the original name is not trimmed.
//! * Every other component loses its trailing dots and spaces, and a component that ends up empty,
//!   `.` or `..` becomes `_`.
//! * A component is cut to [`RenderOptions::max_component_bytes`] bytes (255 by default; encrypted
//!   Synology shares need 143) on a character boundary; the file component keeps its extension. The
//!   whole path is capped at [`RenderOptions::max_path_bytes`] (4000) by shortening the longest
//!   folder components evenly, never the file name.
//!
//! Rendering is total: it never fails, panics or returns anything but a normalised [`RelPath`] with
//! at least one component. The planner still has to treat a root result as an error, and has to
//! check the destination against conflicts (see [`crate::conflict`]).

use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

use crate::conflict::{DEFAULT_MAX_NAME_BYTES, cut_to_bytes};
use crate::names::split_name;
use crate::paths::RelPath;

/// Longest accepted pattern text, in bytes.
pub const MAX_PATTERN_BYTES: usize = 1024;

/// Most components (folders plus an explicit file name) a pattern may have.
pub const MAX_COMPONENTS: usize = 32;

/// What `{camera}` renders when the camera is unknown, unless [`RenderOptions`] says otherwise.
pub const DEFAULT_CAMERA_PLACEHOLDER: &str = "Unknown camera";

/// Default cap on a whole rendered path, in bytes (Linux's `PATH_MAX` is 4096).
pub const DEFAULT_MAX_PATH_BYTES: usize = 4000;

/// Smallest per-component cap honoured, so a cut name always keeps some stem.
const MIN_COMPONENT_BYTES: usize = 16;

/// Longest extension, dot excluded, that survives shortening a file name.
const MAX_KEPT_EXTENSION_BYTES: usize = 32;

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const MONTH_ABBREVIATIONS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A byte range of the pattern text, braces included, so the editor can underline it. Both ends lie
/// on character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// First byte of the range.
    pub start: usize,
    /// One past the last byte of the range.
    pub end: usize,
}

impl Span {
    /// The range `start..end`.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The part of `text` the span covers; `None` if it does not fit `text`.
    pub fn get(self, text: &str) -> Option<&str> {
        text.get(self.start..self.end)
    }
}

/// A `{token}` of the pattern language. See the module docs for the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    /// `{YYYY}`: year, 4 digits.
    Year,
    /// `{YY}`: year, last 2 digits.
    ShortYear,
    /// `{mm}` or `{MM}`: month number, 2 digits.
    Month,
    /// `{MMM}`: English month abbreviation.
    MonthAbbrev,
    /// `{MMMM}`: English month name.
    MonthName,
    /// `{dd}`: day of the month, 2 digits.
    Day,
    /// `{HH}`: hour on the 24-hour clock, 2 digits.
    Hour,
    /// `{min}`: minute, 2 digits.
    Minute,
    /// `{sec}`: second, 2 digits.
    Second,
    /// `{filename}`: the full original file name.
    FileName,
    /// `{name}`: the original file name without its extension.
    Stem,
    /// `{ext}`: the extension without the dot, as written on disk.
    Extension,
    /// `{camera}`: the camera name, or the placeholder.
    Camera,
    /// `{folder}`: the name of the folder the file is in now.
    Folder,
}

/// Where the pattern editor lists a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenGroup {
    /// The capture date and time.
    Date,
    /// The original file name.
    File,
    /// Everything else.
    Other,
}

impl Token {
    /// Every token, in the order the editor lists them.
    pub const ALL: [Token; 14] = [
        Self::Year,
        Self::ShortYear,
        Self::Month,
        Self::MonthAbbrev,
        Self::MonthName,
        Self::Day,
        Self::Hour,
        Self::Minute,
        Self::Second,
        Self::FileName,
        Self::Stem,
        Self::Extension,
        Self::Camera,
        Self::Folder,
    ];

    /// The canonical spelling, without braces (`YYYY`, `mm`, ...).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Year => "YYYY",
            Self::ShortYear => "YY",
            Self::Month => "mm",
            Self::MonthAbbrev => "MMM",
            Self::MonthName => "MMMM",
            Self::Day => "dd",
            Self::Hour => "HH",
            Self::Minute => "min",
            Self::Second => "sec",
            Self::FileName => "filename",
            Self::Stem => "name",
            Self::Extension => "ext",
            Self::Camera => "camera",
            Self::Folder => "folder",
        }
    }

    /// Other accepted spellings, without braces.
    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Month => &["MM"],
            _ => &[],
        }
    }

    /// The token spelled exactly `name` (case-sensitive, no braces), canonical or alias.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|token| token.name() == name || token.aliases().contains(&name))
    }

    /// The editor group of the token.
    pub const fn group(self) -> TokenGroup {
        match self {
            Self::Year
            | Self::ShortYear
            | Self::Month
            | Self::MonthAbbrev
            | Self::MonthName
            | Self::Day
            | Self::Hour
            | Self::Minute
            | Self::Second => TokenGroup::Date,
            Self::FileName | Self::Stem | Self::Extension => TokenGroup::File,
            Self::Camera | Self::Folder => TokenGroup::Other,
        }
    }

    /// One-line English description for the editor.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Year => "Year, 4 digits",
            Self::ShortYear => "Year, last 2 digits",
            Self::Month => "Month number, 2 digits",
            Self::MonthAbbrev => "Month name, abbreviated (English)",
            Self::MonthName => "Month name, in full (English)",
            Self::Day => "Day of the month, 2 digits",
            Self::Hour => "Hour on the 24-hour clock, 2 digits",
            Self::Minute => "Minute, 2 digits",
            Self::Second => "Second, 2 digits",
            Self::FileName => "Original file name, with its extension",
            Self::Stem => "Original file name, without its extension",
            Self::Extension => "File extension without the dot, in its original case",
            Self::Camera => "Camera make and model, or a placeholder when unknown",
            Self::Folder => "Name of the folder the file is in now",
        }
    }

    /// The value this token takes for `ctx`, sanitised as the module docs describe.
    fn value(self, ctx: &FileContext, options: &RenderOptions) -> String {
        let at = ctx.captured;
        let year = at.year().clamp(0, 9999);
        let month_index = at.month().saturating_sub(1) as usize;
        match self {
            Self::Year => format!("{year:04}"),
            Self::ShortYear => format!("{:02}", year % 100),
            Self::Month => format!("{:02}", at.month()),
            Self::MonthAbbrev => MONTH_ABBREVIATIONS
                .get(month_index)
                .copied()
                .unwrap_or_default()
                .to_owned(),
            Self::MonthName => MONTH_NAMES
                .get(month_index)
                .copied()
                .unwrap_or_default()
                .to_owned(),
            Self::Day => format!("{:02}", at.day()),
            Self::Hour => format!("{:02}", at.hour()),
            Self::Minute => format!("{:02}", at.minute()),
            Self::Second => format!("{:02}", at.second()),
            Self::FileName => file_text(&ctx.file_name),
            Self::Stem => file_text(split_name(&ctx.file_name).0),
            Self::Extension => file_text(split_name(&ctx.file_name).1.unwrap_or_default()),
            Self::Camera => sanitize_value(
                ctx.camera
                    .as_deref()
                    .map(str::trim)
                    .filter(|camera| !camera.is_empty())
                    .unwrap_or(&options.camera_placeholder),
            ),
            Self::Folder => sanitize_value(&ctx.folder),
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.name())
    }
}

/// One entry of the token catalogue, for the pattern editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    /// The token itself.
    pub token: Token,
    /// Canonical spelling without braces, e.g. `YYYY`.
    pub name: &'static str,
    /// Other accepted spellings without braces, e.g. `MM` for `mm`.
    pub aliases: &'static [&'static str],
    /// Where the editor lists it.
    pub group: TokenGroup,
    /// One-line English description.
    pub description: &'static str,
    /// The value for the sample file (`IMG_0042.CR3`, taken 2026-10-05 14:07:09 with a
    /// `Canon EOS R5` in a folder called `Holiday`), computed by the real formatter.
    pub example: String,
}

/// The token catalogue, in editor order. The editor lists tokens from here instead of hard-coding
/// them.
pub fn describe() -> Vec<TokenInfo> {
    let sample = sample_context();
    let options = RenderOptions::default();
    Token::ALL
        .into_iter()
        .map(|token| TokenInfo {
            token,
            name: token.name(),
            aliases: token.aliases(),
            group: token.group(),
            description: token.description(),
            example: token.value(&sample, &options),
        })
        .collect()
}

/// A ready-made pattern the editor can offer, with what it gives for the sample file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternExample {
    /// The pattern text.
    pub pattern: &'static str,
    /// What the pattern is good for.
    pub description: &'static str,
    /// The path the sample file gets (see [`TokenInfo::example`]).
    pub result: String,
}

/// Ready-made example patterns, each rendered for the sample file by the real formatter.
pub fn examples() -> Vec<PatternExample> {
    const PRESETS: [(&str, &str); 6] = [
        ("{YYYY}/{mm}", "One folder per year, then per month"),
        (
            "{YYYY}/{YYYY}-{mm}-{dd}",
            "One folder per year, then per day",
        ),
        ("{YYYY}/{MMMM}", "One folder per year, then per month name"),
        (
            "{camera}/{YYYY}/{mm}",
            "One folder per camera, then year and month",
        ),
        (
            "{YYYY}-{mm}-{dd}_{filename}",
            "Prefix every file name with its capture date",
        ),
        (
            "{YYYY}/{mm}/{dd}_{HH}{min}{sec}_{name}.{ext}",
            "Rename files after their capture time",
        ),
    ];
    let sample = sample_context();
    PRESETS
        .into_iter()
        .filter_map(|(pattern, description)| {
            let parsed = parse(pattern).ok()?;
            Some(PatternExample {
                pattern,
                description,
                result: parsed.render(&sample).to_string(),
            })
        })
        .collect()
}

/// The file behind the catalogue examples.
fn sample_context() -> FileContext {
    FileContext {
        file_name: "IMG_0042.CR3".to_owned(),
        captured: NaiveDate::from_ymd_opt(2026, 10, 5)
            .and_then(|day| day.and_hms_opt(14, 7, 9))
            .unwrap_or_default(),
        camera: Some("Canon EOS R5".to_owned()),
        folder: "Holiday".to_owned(),
    }
}

/// What is wrong with a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternErrorKind {
    /// The pattern has no component.
    Empty,
    /// The pattern text is longer than `max` bytes.
    TooLong {
        /// The limit, [`MAX_PATTERN_BYTES`].
        max: usize,
    },
    /// The pattern has more than `max` components.
    TooDeep {
        /// The limit, [`MAX_COMPONENTS`].
        max: usize,
    },
    /// The pattern starts with `/`.
    LeadingSlash,
    /// A component is `..`.
    ParentComponent,
    /// `{name}` is not a token.
    UnknownToken {
        /// The text between the braces, as written.
        name: String,
        /// The canonical spelling of the closest token, without braces.
        suggestion: Option<&'static str>,
    },
    /// `{}`.
    EmptyToken,
    /// A `{` that is never closed in its component.
    UnclosedBrace {
        /// The canonical spelling of the token the text after the brace looks like.
        suggestion: Option<&'static str>,
    },
    /// A `}` that closes nothing.
    UnmatchedBrace,
    /// A control character (tab, newline, NUL, ...) in the literal text.
    ControlCharacter,
    /// A `\` in the literal text.
    Backslash,
}

fn write_hint(f: &mut fmt::Formatter<'_>, suggestion: Option<&str>) -> fmt::Result {
    match suggestion {
        Some(name) => write!(f, ", did you mean `{{{name}}}`?"),
        None => Ok(()),
    }
}

impl fmt::Display for PatternErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the pattern is empty"),
            Self::TooLong { max } => write!(f, "the pattern is longer than {max} bytes"),
            Self::TooDeep { max } => write!(f, "the pattern has more than {max} levels"),
            Self::LeadingSlash => f.write_str(
                "the pattern must not start with `/`: it is relative to the destination folder",
            ),
            Self::ParentComponent => {
                f.write_str("`..` is not allowed: a pattern cannot leave the destination folder")
            }
            Self::UnknownToken { name, suggestion } => {
                write!(f, "unknown token `{{{name}}}`")?;
                write_hint(f, *suggestion)
            }
            Self::EmptyToken => f.write_str("`{}` is empty: name a token between the braces"),
            Self::UnclosedBrace { suggestion } => {
                f.write_str("`{` is never closed: add a `}`, or write `{{` for a literal brace")?;
                write_hint(f, *suggestion)
            }
            Self::UnmatchedBrace => {
                f.write_str("`}` has no matching `{`: write `}}` for a literal brace")
            }
            Self::ControlCharacter => {
                f.write_str("control characters are not allowed in a pattern")
            }
            Self::Backslash => f.write_str("`\\` is not allowed: use `/` to separate folders"),
        }
    }
}

/// An error in a pattern, with the part of the text to underline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}")]
pub struct PatternError {
    /// The offending part of the pattern text.
    pub span: Span,
    /// What is wrong.
    pub kind: PatternErrorKind,
}

impl PatternError {
    fn new(span: Span, kind: PatternErrorKind) -> Self {
        Self { span, kind }
    }

    /// A stable machine-readable identifier of the kind of error (`unknown_token`, ...).
    pub fn code(&self) -> &'static str {
        match self.kind {
            PatternErrorKind::Empty => "empty",
            PatternErrorKind::TooLong { .. } => "too_long",
            PatternErrorKind::TooDeep { .. } => "too_deep",
            PatternErrorKind::LeadingSlash => "leading_slash",
            PatternErrorKind::ParentComponent => "parent_component",
            PatternErrorKind::UnknownToken { .. } => "unknown_token",
            PatternErrorKind::EmptyToken => "empty_token",
            PatternErrorKind::UnclosedBrace { .. } => "unclosed_brace",
            PatternErrorKind::UnmatchedBrace => "unmatched_brace",
            PatternErrorKind::ControlCharacter => "control_character",
            PatternErrorKind::Backslash => "backslash",
        }
    }

    /// Text that could replace the span to fix the error, if there is an obvious one:
    /// `{YYYY}` for `{yyyy}`, `/` for `\`.
    pub fn suggestion(&self) -> Option<String> {
        match &self.kind {
            PatternErrorKind::UnknownToken {
                suggestion: Some(name),
                ..
            }
            | PatternErrorKind::UnclosedBrace {
                suggestion: Some(name),
            } => Some(format!("{{{name}}}")),
            PatternErrorKind::Backslash => Some("/".to_owned()),
            _ => None,
        }
    }
}

/// Words people type for a token, besides its canonical spelling (lowercase).
const ALIASES: &[(&str, Token)] = &[
    ("year", Token::Year),
    ("years", Token::Year),
    ("yr", Token::Year),
    ("shortyear", Token::ShortYear),
    ("month", Token::Month),
    ("monthnum", Token::Month),
    ("monthnumber", Token::Month),
    ("monthname", Token::MonthName),
    ("month_name", Token::MonthName),
    ("monthlong", Token::MonthName),
    ("monthfull", Token::MonthName),
    ("mon", Token::MonthAbbrev),
    ("monthshort", Token::MonthAbbrev),
    ("month_short", Token::MonthAbbrev),
    ("monthabbr", Token::MonthAbbrev),
    ("day", Token::Day),
    ("days", Token::Day),
    ("hour", Token::Hour),
    ("hours", Token::Hour),
    ("hr", Token::Hour),
    ("minute", Token::Minute),
    ("minutes", Token::Minute),
    ("mins", Token::Minute),
    ("mi", Token::Minute),
    ("second", Token::Second),
    ("seconds", Token::Second),
    ("secs", Token::Second),
    ("ss", Token::Second),
    ("file", Token::FileName),
    ("fname", Token::FileName),
    ("original", Token::FileName),
    ("originalname", Token::FileName),
    ("stem", Token::Stem),
    ("filestem", Token::Stem),
    ("basename", Token::Stem),
    ("extension", Token::Extension),
    ("suffix", Token::Extension),
    ("make", Token::Camera),
    ("model", Token::Camera),
    ("cam", Token::Camera),
    ("device", Token::Camera),
    ("dir", Token::Folder),
    ("parent", Token::Folder),
    ("directory", Token::Folder),
    ("foldername", Token::Folder),
];

/// The token `name` most likely stands for: the same name with stray whitespace or another case, a
/// word people commonly use, or a near miss (one slip, two for names of six or more characters).
fn suggest(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    if let Some(token) = Token::from_name(trimmed) {
        return Some(token.name());
    }
    let lower = trimmed.to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(token) = Token::ALL
        .into_iter()
        .find(|token| token.name().to_lowercase() == lower)
    {
        return Some(token.name());
    }
    if let Some((_, token)) = ALIASES.iter().find(|(alias, _)| *alias == lower) {
        return Some(token.name());
    }
    let typed: Vec<char> = lower.chars().collect();
    let allowed = if typed.len() >= 6 { 2 } else { 1 };
    Token::ALL
        .into_iter()
        .map(|token| (token.name(), token))
        .chain(ALIASES.iter().map(|&(alias, token)| (alias, token)))
        .map(|(word, token)| {
            let word: Vec<char> = word.to_lowercase().chars().collect();
            (distance(&typed, &word), token)
        })
        .filter(|&(distance, _)| distance <= allowed)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, token)| token.name())
}

/// Edit distance where swapping two neighbouring characters counts as one slip.
fn distance(a: &[char], b: &[char]) -> usize {
    let plain = levenshtein(a, b);
    if plain > 1 && is_neighbour_swap(a, b) {
        1
    } else {
        plain
    }
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let deletion = previous.get(j + 1).copied().unwrap_or(0) + 1;
            let insertion = current.get(j).copied().unwrap_or(0) + 1;
            current.push(substitution.min(deletion).min(insertion));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

fn is_neighbour_swap(a: &[char], b: &[char]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let differing: Vec<usize> = a
        .iter()
        .zip(b)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(index, _)| index)
        .collect();
    match differing.as_slice() {
        [first, second] => {
            *second == first + 1
                && a.get(*first) == b.get(*second)
                && a.get(*second) == b.get(*first)
        }
        _ => false,
    }
}

/// One piece of a pattern component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Piece {
    /// Text copied as written, escaped braces already resolved.
    Literal(String),
    /// A value substituted when the pattern is rendered.
    Token(Token),
}

/// A parsed folder pattern. Build one with [`parse`]; the value is canonical, so
/// `parse(&pattern.to_string())` gives back an equal pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pattern {
    components: Vec<Vec<Piece>>,
    folder_only: bool,
}

/// Parses a pattern, reporting the first error (the one nearest to the start of the text).
/// [`validate`] reports all of them.
pub fn parse(text: &str) -> Result<Pattern, PatternError> {
    let Scanned {
        components,
        folder_only,
        errors,
        ..
    } = scan(text);
    match errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(Pattern {
            components,
            folder_only,
        }),
    }
}

/// Every error of `text`, ordered by position; empty when the pattern is valid. For the editor,
/// which underlines them all.
pub fn validate(text: &str) -> Vec<PatternError> {
    scan(text).errors
}

impl FromStr for Pattern {
    type Err = PatternError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse(text)
    }
}

impl Pattern {
    /// The `/`-separated components as runs of literal text and tokens. There is at least one, and
    /// none is empty.
    pub fn components(&self) -> &[Vec<Piece>] {
        &self.components
    }

    /// The tokens in order of appearance, repeats included.
    pub fn tokens(&self) -> impl Iterator<Item = Token> + '_ {
        self.components
            .iter()
            .flatten()
            .filter_map(|piece| match piece {
                Piece::Token(token) => Some(*token),
                Piece::Literal(_) => None,
            })
    }

    /// Whether `{filename}` or `{name}` appears anywhere in the pattern.
    pub fn has_filename_token(&self) -> bool {
        self.tokens().any(names_file)
    }

    /// Whether the original file name is appended as a last component: unless the last component
    /// contains `{filename}` or `{name}`, or the pattern ends with `/`, which says that the last
    /// component is a folder.
    pub fn appends_filename(&self) -> bool {
        self.folder_only
            || !self.components.last().is_some_and(|last| {
                last.iter()
                    .any(|piece| matches!(piece, Piece::Token(token) if names_file(*token)))
            })
    }

    /// Renders the destination (folders and file name) of `ctx` with the default
    /// [`RenderOptions`].
    pub fn render(&self, ctx: &FileContext) -> RelPath {
        self.render_with(ctx, &RenderOptions::default())
    }

    /// Renders the destination (folders and file name) of `ctx`; see the module docs for the rules.
    pub fn render_with(&self, ctx: &FileContext, options: &RenderOptions) -> RelPath {
        let max_component = options.max_component_bytes.max(MIN_COMPONENT_BYTES);
        let names_file = !self.appends_filename();
        let last = self.components.len().saturating_sub(1);
        let mut folders = Vec::with_capacity(self.components.len());
        let mut file = None;
        for (index, pieces) in self.components.iter().enumerate() {
            let raw = render_pieces(pieces, ctx, options);
            if names_file && index == last {
                file = Some(finish_component(&raw, true, &ctx.file_name, max_component));
            } else {
                folders.push(finish_component(&raw, false, &ctx.file_name, max_component));
            }
        }
        let file = file.unwrap_or_else(|| {
            finish_component(
                &file_text(&ctx.file_name),
                true,
                &ctx.file_name,
                max_component,
            )
        });
        fit_path(&mut folders, &file, options.max_path_bytes);
        folders.push(file);
        RelPath::new(&folders.join("/")).unwrap_or_default()
    }
}

fn names_file(token: Token) -> bool {
    matches!(token, Token::FileName | Token::Stem)
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, component) in self.components.iter().enumerate() {
            if index > 0 {
                f.write_str("/")?;
            }
            for piece in component {
                match piece {
                    Piece::Literal(text) => {
                        for c in text.chars() {
                            match c {
                                '{' => f.write_str("{{")?,
                                '}' => f.write_str("}}")?,
                                other => write!(f, "{other}")?,
                            }
                        }
                    }
                    Piece::Token(token) => write!(f, "{token}")?,
                }
            }
        }
        if self.folder_only {
            f.write_str("/")?;
        }
        Ok(())
    }
}

/// A component being scanned.
struct Component {
    start: usize,
    pieces: Vec<Piece>,
    literal: String,
}

impl Component {
    fn new(start: usize) -> Self {
        Self {
            start,
            pieces: Vec::new(),
            literal: String::new(),
        }
    }

    fn flush_literal(&mut self) {
        if !self.literal.is_empty() {
            self.pieces
                .push(Piece::Literal(std::mem::take(&mut self.literal)));
        }
    }

    fn push_token(&mut self, token: Token) {
        self.flush_literal();
        self.pieces.push(Piece::Token(token));
    }

    fn into_pieces(mut self) -> Vec<Piece> {
        self.flush_literal();
        self.pieces
    }
}

/// The outcome of scanning a pattern text: the best-effort components and every error.
struct Scanned {
    components: Vec<Vec<Piece>>,
    starts: Vec<usize>,
    folder_only: bool,
    errors: Vec<PatternError>,
}

impl Scanned {
    fn error(&mut self, start: usize, end: usize, kind: PatternErrorKind) {
        self.errors
            .push(PatternError::new(Span::new(start, end), kind));
    }

    /// Ends `component` (the text up to byte `end`): drops empty and `.` components, rejects `..`.
    fn close(&mut self, component: Component, end: usize) {
        let start = component.start;
        let pieces = component.into_pieces();
        let dropped = match pieces.as_slice() {
            [] => true,
            [Piece::Literal(text)] if text == "." => true,
            [Piece::Literal(text)] if text == ".." => {
                self.error(start, end, PatternErrorKind::ParentComponent);
                true
            }
            _ => false,
        };
        self.folder_only = dropped;
        if !dropped {
            self.starts.push(start);
            self.components.push(pieces);
        }
    }

    /// Whether the pattern is empty: no component is left, or the only one is blank text. The
    /// canonical text of the latter is blank, which `scan` refuses for a whole text, so it has to be
    /// refused here too for `Display` to parse back (`./ ` is as empty as ` `).
    fn is_blank(&self) -> bool {
        match self.components.as_slice() {
            [] => true,
            [only] if !self.folder_only => only
                .iter()
                .all(|piece| matches!(piece, Piece::Literal(text) if text.trim().is_empty())),
            _ => false,
        }
    }
}

fn scan(text: &str) -> Scanned {
    let mut scanned = Scanned {
        components: Vec::new(),
        starts: Vec::new(),
        folder_only: false,
        errors: Vec::new(),
    };
    if text.len() > MAX_PATTERN_BYTES {
        let start = cut_to_bytes(text, MAX_PATTERN_BYTES).len();
        scanned.error(
            start,
            text.len(),
            PatternErrorKind::TooLong {
                max: MAX_PATTERN_BYTES,
            },
        );
        return scanned;
    }
    if text.trim().is_empty() {
        scanned.error(0, text.len(), PatternErrorKind::Empty);
        return scanned;
    }
    if text.starts_with('/') {
        scanned.error(0, 1, PatternErrorKind::LeadingSlash);
    }
    let mut current = Component::new(0);
    let mut index = 0;
    while let Some(c) = text.get(index..).and_then(|rest| rest.chars().next()) {
        let mut next = index + c.len_utf8();
        let after = text.get(next..).unwrap_or_default();
        match c {
            '/' => {
                let finished = std::mem::replace(&mut current, Component::new(next));
                scanned.close(finished, index);
            }
            '{' if after.starts_with('{') => {
                current.literal.push('{');
                next += 1;
            }
            '{' => {
                let name_len = after.find(['}', '{', '/']).unwrap_or(after.len());
                let name = after.get(..name_len).unwrap_or_default();
                let name_end = next + name_len;
                if after
                    .get(name_len..)
                    .is_some_and(|tail| tail.starts_with('}'))
                {
                    next = name_end + 1;
                    if name.is_empty() {
                        scanned.error(index, next, PatternErrorKind::EmptyToken);
                    } else if let Some(token) = Token::from_name(name) {
                        current.push_token(token);
                    } else {
                        let kind = PatternErrorKind::UnknownToken {
                            name: name.to_owned(),
                            suggestion: suggest(name),
                        };
                        scanned.error(index, next, kind);
                    }
                } else {
                    next = name_end;
                    let kind = PatternErrorKind::UnclosedBrace {
                        suggestion: suggest(name),
                    };
                    scanned.error(index, next, kind);
                }
            }
            '}' if after.starts_with('}') => {
                current.literal.push('}');
                next += 1;
            }
            '}' => scanned.error(index, next, PatternErrorKind::UnmatchedBrace),
            '\\' => scanned.error(index, next, PatternErrorKind::Backslash),
            control if control.is_control() => {
                scanned.error(index, next, PatternErrorKind::ControlCharacter);
            }
            literal => current.literal.push(literal),
        }
        index = next;
    }
    scanned.close(current, text.len());
    if scanned.components.len() > MAX_COMPONENTS {
        let start = scanned.starts.get(MAX_COMPONENTS).copied().unwrap_or(0);
        scanned.error(
            start,
            text.len(),
            PatternErrorKind::TooDeep {
                max: MAX_COMPONENTS,
            },
        );
    }
    if scanned.errors.is_empty() && scanned.is_blank() {
        scanned.error(0, text.len(), PatternErrorKind::Empty);
    }
    scanned.errors.sort_by_key(|error| error.span.start);
    scanned
}

/// The facts about one file that a pattern needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContext {
    /// The original file name, without folders, as it is on disk.
    pub file_name: String,
    /// The resolved capture date as the photographer's local wall clock
    /// ([`crate::datetime::CaptureDate::naive`]).
    pub captured: NaiveDateTime,
    /// The normalised camera name, when known.
    pub camera: Option<String>,
    /// The name (not the path) of the folder the file is in now; empty at the volume root.
    pub folder: String,
}

/// Knobs of the formatter; the defaults suit a plain share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    /// What `{camera}` renders when the camera is unknown or blank.
    pub camera_placeholder: String,
    /// Longest component in bytes; 143 for encrypted Synology shares. Values below 16 count as 16.
    pub max_component_bytes: usize,
    /// Longest whole path in bytes.
    pub max_path_bytes: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            camera_placeholder: DEFAULT_CAMERA_PLACEHOLDER.to_owned(),
            max_component_bytes: DEFAULT_MAX_NAME_BYTES,
            max_path_bytes: DEFAULT_MAX_PATH_BYTES,
        }
    }
}

/// Makes a value safe as part of a folder or file name on SMB and Windows clients: `/`, `\`,
/// control characters (NUL included) and `: * ? " < > |` become `_`.
pub fn sanitize_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Text taken from the original file name: kept as it is, except for the two characters no name
/// can contain.
fn file_text(text: &str) -> String {
    text.replace(['/', '\0'], "_")
}

fn render_pieces(pieces: &[Piece], ctx: &FileContext, options: &RenderOptions) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Literal(text) => out.push_str(text),
            Piece::Token(token) => out.push_str(&token.value(ctx, options)),
        }
    }
    out
}

/// Applies the component rules of the module docs to the rendered `raw` text.
fn finish_component(raw: &str, is_file: bool, original: &str, max: usize) -> String {
    let untouched = is_file && raw == original && raw.len() <= max;
    let mut text = if raw.len() <= max {
        raw.to_owned()
    } else if is_file {
        cut_keeping_extension(raw, max)
    } else {
        cut_to_bytes(raw, max).to_owned()
    };
    if !untouched {
        text.truncate(text.trim_end_matches(['.', ' ']).len());
    }
    if matches!(text.as_str(), "" | "." | "..") {
        text = "_".to_owned();
    }
    text
}

/// Cuts `name` to `max` bytes, shortening the stem so that the extension survives.
fn cut_keeping_extension(name: &str, max: usize) -> String {
    let (stem, extension) = split_name(name);
    if let Some(extension) = extension
        && extension.len() <= MAX_KEPT_EXTENSION_BYTES
        && extension.len() + 1 < max
    {
        let head = cut_to_bytes(stem, max - extension.len() - 1);
        if !head.is_empty() {
            return format!("{head}.{extension}");
        }
    }
    cut_to_bytes(name, max).to_owned()
}

/// Shortens the longest folder by one character at a time until the whole path fits `max_path`
/// bytes, so the folders end up about equally long (or until nothing can shrink any more). The file
/// name is never touched.
fn fit_path(folders: &mut [String], file: &str, max_path: usize) {
    let mut total = file.len() + folders.iter().map(|folder| folder.len() + 1).sum::<usize>();
    while total > max_path {
        let Some(folder) = folders
            .iter_mut()
            .filter(|folder| folder.chars().nth(1).is_some())
            .max_by_key(|folder| folder.len())
        else {
            return;
        };
        let before = folder.len();
        folder.pop();
        folder.truncate(folder.trim_end_matches(['.', ' ']).len());
        if folder.is_empty() {
            folder.push('_');
        }
        total -= before - folder.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|date| date.and_hms_opt(hour, minute, second))
            .expect("valid date")
    }

    fn context(name: &str) -> FileContext {
        FileContext {
            file_name: name.to_owned(),
            captured: at(2026, 10, 5, 14, 7, 9),
            camera: Some("Canon EOS R5".to_owned()),
            folder: "Holiday".to_owned(),
        }
    }

    fn pattern(text: &str) -> Pattern {
        parse(text).expect("valid pattern")
    }

    fn error(text: &str) -> PatternError {
        parse(text).expect_err("invalid pattern")
    }

    fn render(text: &str, file_name: &str) -> String {
        pattern(text).render(&context(file_name)).to_string()
    }

    fn lit(text: &str) -> Piece {
        Piece::Literal(text.to_owned())
    }

    fn tok(token: Token) -> Piece {
        Piece::Token(token)
    }

    #[test]
    fn components_hold_literals_and_tokens() {
        let parsed = pattern("Photos/{YYYY}-{mm}/day {dd}");
        assert_eq!(
            parsed.components().to_vec(),
            vec![
                vec![lit("Photos")],
                vec![tok(Token::Year), lit("-"), tok(Token::Month)],
                vec![lit("day "), tok(Token::Day)],
            ]
        );
    }

    #[test]
    fn doubled_braces_are_literal_braces() {
        let parsed = pattern("{{{YYYY}}}/a{{b}}c");
        assert_eq!(
            parsed.components().to_vec(),
            vec![
                vec![lit("{"), tok(Token::Year), lit("}")],
                vec![lit("a{b}c")]
            ]
        );
        assert_eq!(parsed.to_string(), "{{{YYYY}}}/a{{b}}c");
        assert_eq!(render("{{x}}/{YYYY}", "a.jpg"), "{x}/2026/a.jpg");
    }

    #[test]
    fn empty_and_dot_components_are_dropped() {
        assert_eq!(pattern("a//b/./c").to_string(), "a/b/c");
        assert_eq!(pattern("./a/").to_string(), "a/");
        assert_eq!(pattern("a/.").to_string(), "a/");
        assert_eq!(pattern("a/b").components().len(), 2);
    }

    #[test]
    fn a_lone_blank_component_is_as_empty_as_blank_text() {
        for text in [" ", "  ", "./ ", "././  ", ".//\u{3000}"] {
            assert_eq!(error(text).code(), "empty", "{text:?}");
            assert_eq!(validate(text).len(), 1, "{text:?}");
        }
        for text in [" /", "a/ ", " / ", " {name}", "{name} "] {
            let parsed = pattern(text);
            assert_eq!(parsed.to_string(), text, "{text:?}");
            assert_eq!(pattern(&parsed.to_string()), parsed, "{text:?}");
        }
    }

    #[test]
    fn dots_inside_a_component_are_literal_text() {
        assert_eq!(pattern("..a/b..").components().len(), 2);
        assert_eq!(pattern("...").to_string(), "...");
        assert_eq!(render("...", "a.jpg"), "_/a.jpg");
    }

    #[test]
    fn both_spellings_of_the_month_parse_to_the_same_token() {
        assert_eq!(pattern("{MM}"), pattern("{mm}"));
        assert_eq!(pattern("{MM}").to_string(), "{mm}");
        assert_eq!(Token::from_name("MM"), Some(Token::Month));
        assert_eq!(Token::from_name("mm"), Some(Token::Month));
        assert_eq!(Token::from_name("min"), Some(Token::Minute));
        assert_eq!(Token::from_name("yyyy"), None);
        assert_eq!(Token::Year.to_string(), "{YYYY}");
    }

    #[test]
    fn display_is_canonical_and_round_trips() {
        for (text, canonical) in [
            ("{YYYY}/{mm}", "{YYYY}/{mm}"),
            ("{YYYY}/{MM}", "{YYYY}/{mm}"),
            ("a//{{b}}/./c/", "a/{{b}}/c/"),
            ("{YYYY}{mm}{dd}", "{YYYY}{mm}{dd}"),
            ("été/{camera}", "été/{camera}"),
            ("{{}}", "{{}}"),
            ("{name}/", "{name}/"),
        ] {
            let parsed = pattern(text);
            assert_eq!(parsed.to_string(), canonical, "{text}");
            assert_eq!(pattern(canonical), parsed, "{text}");
        }
    }

    #[test]
    fn a_pattern_parses_through_from_str() {
        let parsed: Pattern = "{YYYY}/{mm}".parse().expect("valid pattern");
        assert_eq!(parsed, pattern("{YYYY}/{mm}"));
        assert!("{nope}".parse::<Pattern>().is_err());
    }

    #[test]
    fn tokens_come_in_order_of_appearance() {
        let parsed = pattern("{dd}/x{mm}{dd}/{filename}");
        assert_eq!(
            parsed.tokens().collect::<Vec<_>>(),
            [Token::Day, Token::Month, Token::Day, Token::FileName]
        );
    }

    #[test]
    fn filename_tokens_and_the_append_rule() {
        // (pattern, has a filename token, original name appended)
        for (text, has_token, appends) in [
            ("{YYYY}/{mm}", false, true),
            ("{YYYY}/{filename}", true, false),
            ("{YYYY}/{name}.{ext}", true, false),
            ("{YYYY}/{name}", true, false),
            ("{YYYY}/{name}/", true, true),
            ("{name}/{YYYY}", true, true),
            ("{filename}/{YYYY}", true, true),
            ("{YYYY}/{ext}", false, true),
            ("{YYYY}/{camera}_{folder}", false, true),
            ("{filename}", true, false),
            ("{filename}/", true, true),
            ("{filename}/.", true, true),
        ] {
            let parsed = pattern(text);
            assert_eq!(parsed.has_filename_token(), has_token, "{text}");
            assert_eq!(parsed.appends_filename(), appends, "{text}");
        }
    }

    #[test]
    fn errors_carry_their_span_and_code() {
        // (pattern, code, text under the span)
        for (text, code, underlined) in [
            ("{YYYY}/{foo}", "unknown_token", "{foo}"),
            ("{}", "empty_token", "{}"),
            ("a/{YYYY", "unclosed_brace", "{YYYY"),
            ("{YYYY/{mm}", "unclosed_brace", "{YYYY"),
            ("{", "unclosed_brace", "{"),
            ("a}b", "unmatched_brace", "}"),
            ("{{YYYY}", "unmatched_brace", "}"),
            ("/{YYYY}", "leading_slash", "/"),
            ("//", "leading_slash", "/"),
            ("a/../b", "parent_component", ".."),
            ("..", "parent_component", ".."),
            ("a\\b", "backslash", "\\"),
            ("a\tb", "control_character", "\t"),
            ("a\0b", "control_character", "\0"),
            ("a\u{85}b", "control_character", "\u{85}"),
            ("", "empty", ""),
            ("   ", "empty", "   "),
            ("./", "empty", "./"),
            ("./ ", "empty", "./ "),
        ] {
            let found = error(text);
            assert_eq!(found.code(), code, "{text:?}");
            assert_eq!(found.span.get(text), Some(underlined), "{text:?}");
        }
    }

    #[test]
    fn unknown_tokens_get_a_did_you_mean() {
        for (typed, expected) in [
            ("yyyy", Some("YYYY")),
            ("YYYYY", Some("YYYY")),
            ("yy", Some("YY")),
            ("Y", Some("YY")),
            ("Mm", Some("mm")),
            ("mmm", Some("MMM")),
            ("mmmm", Some("MMMM")),
            ("DD", Some("dd")),
            ("hh", Some("HH")),
            ("MIN", Some("min")),
            ("Min", Some("min")),
            ("SEC", Some("sec")),
            ("year", Some("YYYY")),
            ("month", Some("mm")),
            ("monthname", Some("MMMM")),
            ("mon", Some("MMM")),
            ("day", Some("dd")),
            ("hour", Some("HH")),
            ("minute", Some("min")),
            ("second", Some("sec")),
            ("file", Some("filename")),
            ("filenmae", Some("filename")),
            ("FileName", Some("filename")),
            ("Name", Some("name")),
            ("naem", Some("name")),
            ("stem", Some("name")),
            ("EXT", Some("ext")),
            ("extension", Some("ext")),
            ("Camera", Some("camera")),
            ("camra", Some("camera")),
            ("make", Some("camera")),
            ("fodler", Some("folder")),
            ("parent", Some("folder")),
            (" YYYY ", Some("YYYY")),
            ("xyz", None),
            ("foo", None),
            ("q", None),
        ] {
            let found = error(&format!("{{{typed}}}"));
            let PatternErrorKind::UnknownToken { name, suggestion } = &found.kind else {
                panic!("{typed:?} gave {found:?}");
            };
            assert_eq!(name, typed);
            assert_eq!(*suggestion, expected, "{typed:?}");
        }
    }

    #[test]
    fn an_unclosed_brace_suggests_the_token_it_started() {
        let found = error("a/{yyyy");
        assert_eq!(found.suggestion().as_deref(), Some("{YYYY}"));
        assert_eq!(error("a/{").suggestion(), None);
    }

    #[test]
    fn error_messages_explain_the_fix() {
        assert_eq!(
            error("{yyyy}").to_string(),
            "unknown token `{yyyy}`, did you mean `{YYYY}`?"
        );
        assert_eq!(error("{foo}").to_string(), "unknown token `{foo}`");
        assert_eq!(
            error("a\\b").to_string(),
            "`\\` is not allowed: use `/` to separate folders"
        );
        assert_eq!(error("{yyyy}").suggestion().as_deref(), Some("{YYYY}"));
        assert_eq!(error("a\\b").suggestion().as_deref(), Some("/"));
        assert_eq!(error("{foo}").suggestion(), None);
        assert_eq!(
            error("a/{YYYY").to_string(),
            "`{` is never closed: add a `}`, or write `{{` for a literal brace, did you mean `{YYYY}`?"
        );
    }

    #[test]
    fn validate_reports_every_error_in_order() {
        let text = "{yyyy}/{foo}/}";
        let errors = validate(text);
        let codes: Vec<_> = errors.iter().map(PatternError::code).collect();
        assert_eq!(codes, ["unknown_token", "unknown_token", "unmatched_brace"]);
        let underlined: Vec<_> = errors.iter().filter_map(|e| e.span.get(text)).collect();
        assert_eq!(underlined, ["{yyyy}", "{foo}", "}"]);
        assert_eq!(parse(text).expect_err("invalid"), errors[0]);
        assert!(validate("{YYYY}/{mm}").is_empty());
    }

    #[test]
    fn size_limits_are_enforced() {
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        let found = error(&long);
        assert_eq!(
            found.kind,
            PatternErrorKind::TooLong {
                max: MAX_PATTERN_BYTES
            }
        );
        assert_eq!(
            found.span,
            Span::new(MAX_PATTERN_BYTES, MAX_PATTERN_BYTES + 1)
        );
        assert!(parse(&"a".repeat(MAX_PATTERN_BYTES)).is_ok());

        let deep = "a/".repeat(MAX_COMPONENTS + 1);
        let found = error(&deep);
        assert_eq!(
            found.kind,
            PatternErrorKind::TooDeep {
                max: MAX_COMPONENTS
            }
        );
        assert_eq!(found.span.get(&deep), Some("a/"));
        assert!(parse(&"a/".repeat(MAX_COMPONENTS)).is_ok());
    }

    #[test]
    fn every_token_has_its_value() {
        let ctx = context("IMG_0042.CR3");
        let options = RenderOptions::default();
        for (token, expected) in [
            (Token::Year, "2026"),
            (Token::ShortYear, "26"),
            (Token::Month, "10"),
            (Token::MonthAbbrev, "Oct"),
            (Token::MonthName, "October"),
            (Token::Day, "05"),
            (Token::Hour, "14"),
            (Token::Minute, "07"),
            (Token::Second, "09"),
            (Token::FileName, "IMG_0042.CR3"),
            (Token::Stem, "IMG_0042"),
            (Token::Extension, "CR3"),
            (Token::Camera, "Canon EOS R5"),
            (Token::Folder, "Holiday"),
        ] {
            assert_eq!(token.value(&ctx, &options), expected, "{token}");
        }
    }

    #[test]
    fn dates_are_zero_padded_and_year_boundaries_stay_put() {
        let date_tokens: Vec<_> = Token::ALL
            .into_iter()
            .filter(|token| token.group() == TokenGroup::Date)
            .collect();
        assert_eq!(date_tokens.len(), 9);
        for (when, expected) in [
            (
                at(2025, 12, 31, 23, 59, 59),
                "2025|25|12|Dec|December|31|23|59|59",
            ),
            (
                at(2026, 1, 1, 0, 0, 0),
                "2026|26|01|Jan|January|01|00|00|00",
            ),
            (
                at(2000, 2, 29, 12, 5, 7),
                "2000|00|02|Feb|February|29|12|05|07",
            ),
            (
                at(1999, 9, 9, 9, 9, 9),
                "1999|99|09|Sep|September|09|09|09|09",
            ),
            (at(5, 3, 9, 1, 2, 3), "0005|05|03|Mar|March|09|01|02|03"),
            (at(-44, 3, 15, 0, 0, 0), "0000|00|03|Mar|March|15|00|00|00"),
            (at(12345, 7, 4, 0, 0, 0), "9999|99|07|Jul|July|04|00|00|00"),
        ] {
            let ctx = FileContext {
                captured: when,
                ..context("a.jpg")
            };
            let got: Vec<_> = date_tokens
                .iter()
                .map(|token| token.value(&ctx, &RenderOptions::default()))
                .collect();
            assert_eq!(got.join("|"), expected, "{when}");
        }
    }

    #[test]
    fn month_names_are_english() {
        let full = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        for (index, name) in full.iter().enumerate() {
            let month = u32::try_from(index).expect("small") + 1;
            let ctx = FileContext {
                captured: at(2026, month, 1, 0, 0, 0),
                ..context("a.jpg")
            };
            let options = RenderOptions::default();
            assert_eq!(Token::MonthName.value(&ctx, &options), *name);
            assert_eq!(Token::MonthAbbrev.value(&ctx, &options), name[..3]);
        }
    }

    #[test]
    fn patterns_render_into_folders_and_a_file_name() {
        for (text, expected) in [
            ("{YYYY}/{mm}", "2026/10/IMG_0001.jpg"),
            ("{YYYY}/{MM}", "2026/10/IMG_0001.jpg"),
            ("{YY}/{MMM}", "26/Oct/IMG_0001.jpg"),
            ("{YYYY}/{MMMM}", "2026/October/IMG_0001.jpg"),
            ("{YYYY}-{mm}-{dd}", "2026-10-05/IMG_0001.jpg"),
            ("{HH}{min}{sec}", "140709/IMG_0001.jpg"),
            ("{camera}", "Canon EOS R5/IMG_0001.jpg"),
            ("{folder}", "Holiday/IMG_0001.jpg"),
            ("Photos/{YYYY}", "Photos/2026/IMG_0001.jpg"),
            ("{YYYY}/./{mm}//", "2026/10/IMG_0001.jpg"),
            ("{filename}", "IMG_0001.jpg"),
            ("{YYYY}/{filename}", "2026/IMG_0001.jpg"),
            (
                "{YYYY}/{name}_{camera}.{ext}",
                "2026/IMG_0001_Canon EOS R5.jpg",
            ),
            ("{YYYY}/{name}", "2026/IMG_0001"),
            ("{YYYY}/{name}/", "2026/IMG_0001/IMG_0001.jpg"),
            ("{name}/{YYYY}", "IMG_0001/2026/IMG_0001.jpg"),
            ("{filename}/{YYYY}", "IMG_0001.jpg/2026/IMG_0001.jpg"),
            ("{ext}/{YYYY}", "jpg/2026/IMG_0001.jpg"),
            ("{YYYY}-{mm}-{dd}_{filename}", "2026-10-05_IMG_0001.jpg"),
            ("Trip: Paris/{YYYY}", "Trip: Paris/2026/IMG_0001.jpg"),
        ] {
            assert_eq!(render(text, "IMG_0001.jpg"), expected, "{text}");
        }
    }

    #[test]
    fn file_names_are_split_at_the_last_dot() {
        for (text, name, expected) in [
            ("{YYYY}", "IMG_0001", "2026/IMG_0001"),
            ("{name}.{ext}", "IMG_0001", "IMG_0001"),
            ("{ext}", "IMG_0001", "_/IMG_0001"),
            ("{YYYY}", ".hidden", "2026/.hidden"),
            ("{name}", ".hidden", ".hidden"),
            ("{ext}", ".hidden", "_/.hidden"),
            (
                "{YYYY}/{name}-x.{ext}",
                "archive.tar.gz",
                "2026/archive.tar-x.gz",
            ),
            ("{ext}", "IMG.CR3", "CR3/IMG.CR3"),
            ("{YYYY}", "photo.", "2026/photo."),
            ("{name}", "photo.", "photo."),
            ("{YYYY}/{name}", "été 写真.JPG", "2026/été 写真"),
            (
                "{YYYY}/{name}.{ext}",
                "e\u{301}te\u{301}.jpg",
                "2026/e\u{301}te\u{301}.jpg",
            ),
        ] {
            assert_eq!(render(text, name), expected, "{text} {name:?}");
        }
    }

    #[test]
    fn the_original_name_keeps_every_character_but_the_literal_text_around_it_is_trimmed() {
        for (text, name, expected) in [
            ("{YYYY}", "a:b?.jpg", "2026/a:b?.jpg"),
            ("{filename}", "a:b?.jpg", "a:b?.jpg"),
            ("{name}_{camera}.{ext}", "a:b.jpg", "a:b_Canon EOS R5.jpg"),
            ("{YYYY}", "trailing. ", "2026/trailing. "),
            ("{filename}", "trailing. ", "trailing. "),
            ("{filename}.", "IMG.jpg", "IMG.jpg"),
            ("{YYYY}./{mm} ", "x.jpg", "2026/10/x.jpg"),
        ] {
            assert_eq!(render(text, name), expected, "{text} {name:?}");
        }
    }

    #[test]
    fn nonsense_file_names_still_give_a_valid_path() {
        assert_eq!(render("{YYYY}", ""), "2026/_");
        assert_eq!(render("{YYYY}", "."), "2026/_");
        assert_eq!(render("{YYYY}", ".."), "2026/_");
        assert_eq!(render("{YYYY}", "a/b\0c.jpg"), "2026/a_b_c.jpg");
        assert_eq!(render("{filename}", "a/b.jpg"), "a_b.jpg");
    }

    #[test]
    fn camera_values_are_sanitised() {
        for (camera, expected) in [
            (Some("Canon/EOS"), "Canon_EOS"),
            (Some("a:b*c?d\"e<f>g|h\\i"), "a_b_c_d_e_f_g_h_i"),
            (Some("tab\there\0nul\nline"), "tab_here_nul_line"),
            (Some("Canon. "), "Canon"),
            (Some("  Leica M10  "), "Leica M10"),
            (Some("..."), "_"),
            (Some(".."), "_"),
            (Some("."), "_"),
            (Some("  "), "Unknown camera"),
            (Some(""), "Unknown camera"),
            (None, "Unknown camera"),
        ] {
            let ctx = FileContext {
                camera: camera.map(str::to_owned),
                ..context("a.jpg")
            };
            let got = pattern("{camera}").render(&ctx);
            assert_eq!(got.to_string(), format!("{expected}/a.jpg"), "{camera:?}");
        }
    }

    #[test]
    fn the_camera_placeholder_is_configurable_and_sanitised() {
        let options = RenderOptions {
            camera_placeholder: "No camera/EOS".to_owned(),
            ..RenderOptions::default()
        };
        let ctx = FileContext {
            camera: None,
            ..context("a.jpg")
        };
        let got = pattern("{camera}").render_with(&ctx, &options);
        assert_eq!(got.as_str(), "No camera_EOS/a.jpg");
    }

    #[test]
    fn folder_values_are_sanitised() {
        for (folder, expected) in [
            ("Holiday", "Holiday"),
            ("", "_"),
            ("  ", "_"),
            ("a/b", "a_b"),
            ("a:b", "a_b"),
            ("..", "_"),
        ] {
            let ctx = FileContext {
                folder: folder.to_owned(),
                ..context("a.jpg")
            };
            let got = pattern("{folder}").render(&ctx);
            assert_eq!(got.to_string(), format!("{expected}/a.jpg"), "{folder:?}");
        }
    }

    #[test]
    fn literal_text_is_copied_verbatim() {
        assert_eq!(
            render("Trip: Paris? <1>/{YYYY}", "a.jpg"),
            "Trip: Paris? <1>/2026/a.jpg"
        );
        assert_eq!(render("été/日本", "a.jpg"), "été/日本/a.jpg");
    }

    fn component_lengths(path: &RelPath) -> Vec<usize> {
        path.components().map(str::len).collect()
    }

    #[test]
    fn components_are_cut_on_character_boundaries() {
        let ctx = FileContext {
            camera: Some("x".repeat(300)),
            ..context("a.jpg")
        };
        assert_eq!(
            component_lengths(&pattern("{camera}").render(&ctx)),
            [255, 5]
        );

        let ctx = FileContext {
            camera: Some("é".repeat(200)),
            ..context("a.jpg")
        };
        let got = pattern("{camera}").render(&ctx);
        assert_eq!(got.components().next(), Some("é".repeat(127).as_str()));

        let ctx = FileContext {
            camera: Some(format!("{} bbbb", "a".repeat(254))),
            ..context("a.jpg")
        };
        let got = pattern("{camera}").render(&ctx);
        assert_eq!(got.components().next(), Some("a".repeat(254).as_str()));
    }

    #[test]
    fn a_shortened_file_name_keeps_its_extension() {
        let options = RenderOptions {
            max_component_bytes: 143,
            ..RenderOptions::default()
        };
        let name = format!("{}.jpg", "a".repeat(200));
        let got = pattern("{YYYY}").render_with(&context(&name), &options);
        let file = got.file_name().expect("has a file name");
        assert_eq!(file.len(), 143);
        assert!(file.ends_with("aaa.jpg"), "{file}");

        let name = format!("{}.CR3", "é".repeat(100));
        let got = pattern("{filename}").render_with(&context(&name), &options);
        assert_eq!(got.as_str(), format!("{}.CR3", "é".repeat(69)));

        let name = format!("a.{}", "x".repeat(300));
        let got = pattern("{YYYY}").render(&context(&name));
        assert_eq!(component_lengths(&got), [4, 255]);
    }

    #[test]
    fn the_whole_path_is_capped_by_shortening_the_longest_folders_evenly() {
        let ctx = FileContext {
            camera: Some("c".repeat(255)),
            ..context("IMG_0001.jpg")
        };
        let text = vec!["{camera}"; 20].join("/");
        let got = pattern(&text).render(&ctx);
        assert!(got.as_str().len() <= DEFAULT_MAX_PATH_BYTES);
        assert_eq!(got.file_name(), Some("IMG_0001.jpg"));
        let folders = component_lengths(&got);
        assert_eq!(folders.len(), 21);
        let shortest = folders.iter().take(20).min().expect("20 folders");
        let longest = folders.iter().take(20).max().expect("20 folders");
        assert!(longest - shortest <= 1, "{folders:?}");
        assert!(*shortest >= 190, "{folders:?}");
    }

    #[test]
    fn tiny_limits_are_raised_or_survived() {
        let options = RenderOptions {
            max_component_bytes: 0,
            max_path_bytes: 0,
            ..RenderOptions::default()
        };
        let got = pattern("{camera}/{camera}/{YYYY}").render_with(&context("a.jpg"), &options);
        assert_eq!(got.as_str(), "C/C/2/a.jpg");

        let options = RenderOptions {
            max_component_bytes: 0,
            ..RenderOptions::default()
        };
        let got = pattern("{camera}").render_with(&context("a.jpg"), &options);
        assert_eq!(got.as_str(), "Canon EOS R5/a.jpg");
        let ctx = FileContext {
            camera: Some("y".repeat(100)),
            ..context("a.jpg")
        };
        let got = pattern("{camera}").render_with(&ctx, &options);
        assert_eq!(component_lengths(&got), [16, 5]);
    }

    #[test]
    fn the_catalogue_lists_every_token_with_its_example() {
        let catalogue = describe();
        let listed: Vec<_> = catalogue
            .iter()
            .map(|info| (info.name, info.example.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("YYYY", "2026"),
                ("YY", "26"),
                ("mm", "10"),
                ("MMM", "Oct"),
                ("MMMM", "October"),
                ("dd", "05"),
                ("HH", "14"),
                ("min", "07"),
                ("sec", "09"),
                ("filename", "IMG_0042.CR3"),
                ("name", "IMG_0042"),
                ("ext", "CR3"),
                ("camera", "Canon EOS R5"),
                ("folder", "Holiday"),
            ]
        );
        for info in &catalogue {
            assert_eq!(Token::from_name(info.name), Some(info.token));
            for alias in info.aliases {
                assert_eq!(Token::from_name(alias), Some(info.token));
            }
            assert!(!info.description.is_empty(), "{}", info.name);
        }
        let month = catalogue.iter().find(|info| info.token == Token::Month);
        assert_eq!(month.map(|info| info.aliases), Some(&["MM"][..]));
        let groups: Vec<_> = catalogue.iter().map(|info| info.group).collect();
        assert_eq!(groups.iter().filter(|g| **g == TokenGroup::Date).count(), 9);
        assert_eq!(groups.iter().filter(|g| **g == TokenGroup::File).count(), 3);
        assert_eq!(
            groups.iter().filter(|g| **g == TokenGroup::Other).count(),
            2
        );
    }

    #[test]
    fn the_example_patterns_render_for_the_sample_file() {
        let all = examples();
        let got: Vec<_> = all
            .iter()
            .map(|example| (example.pattern, example.result.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("{YYYY}/{mm}", "2026/10/IMG_0042.CR3"),
                ("{YYYY}/{YYYY}-{mm}-{dd}", "2026/2026-10-05/IMG_0042.CR3"),
                ("{YYYY}/{MMMM}", "2026/October/IMG_0042.CR3"),
                ("{camera}/{YYYY}/{mm}", "Canon EOS R5/2026/10/IMG_0042.CR3"),
                ("{YYYY}-{mm}-{dd}_{filename}", "2026-10-05_IMG_0042.CR3"),
                (
                    "{YYYY}/{mm}/{dd}_{HH}{min}{sec}_{name}.{ext}",
                    "2026/10/05_140709_IMG_0042.CR3"
                ),
            ]
        );
        assert!(all.iter().all(|example| !example.description.is_empty()));
    }

    fn fragment() -> BoxedStrategy<String> {
        prop_oneof![
            4 => proptest::sample::select(Token::ALL.to_vec()).prop_map(|token| token.to_string()),
            1 => Just("{MM}".to_owned()),
            1 => Just("{{".to_owned()),
            1 => Just("}}".to_owned()),
            1 => Just(" ".to_owned()),
            1 => Just(".".to_owned()),
            1 => Just(":*?\"<>|".to_owned()),
            4 => "[A-Za-z0-9_é日 -]{1,6}",
        ]
        .boxed()
    }

    fn pattern_text() -> impl Strategy<Value = String> {
        let component = prop::collection::vec(fragment(), 1..4).prop_map(|parts| parts.concat());
        prop::collection::vec(component, 1..6).prop_map(|components| components.join("/"))
    }

    fn noisy_text() -> impl Strategy<Value = String> {
        let noise = prop_oneof![
            fragment(),
            "[{}/\\\\. a-zA-Z\\t\\x00é]{1,4}".boxed(),
            Just("..".to_owned()),
        ];
        prop::collection::vec(noise, 0..8).prop_map(|parts| parts.concat())
    }

    fn any_text(max_chars: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..max_chars)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    }

    fn any_context() -> impl Strategy<Value = FileContext> {
        (
            any_text(400),
            (
                -262_000i32..262_000,
                1u32..=12,
                1u32..=28,
                0u32..24,
                0u32..60,
                0u32..60,
            ),
            proptest::option::of(any_text(400)),
            any_text(400),
        )
            .prop_map(
                |(file_name, (year, month, day, hour, minute, second), camera, folder)| {
                    FileContext {
                        file_name,
                        captured: at(year, month, day, hour, minute, second),
                        camera,
                        folder,
                    }
                },
            )
    }

    proptest! {
        #[test]
        fn rendering_never_panics_and_yields_a_clean_relative_path(
            text in pattern_text(),
            ctx in any_context(),
            placeholder in any_text(40),
            max_component in 0usize..300,
            max_path in 500usize..4100,
        ) {
            prop_assume!(parse(&text).is_ok());
            let options = RenderOptions {
                camera_placeholder: placeholder,
                max_component_bytes: max_component,
                max_path_bytes: max_path,
            };
            let rendered = pattern(&text).render_with(&ctx, &options);
            let path = rendered.as_str();
            prop_assert!(!rendered.is_root());
            prop_assert!(!path.starts_with('/') && !path.ends_with('/'));
            prop_assert!(!path.contains('\0'));
            prop_assert!(path.len() <= max_path, "{} > {max_path}", path.len());
            for part in path.split('/') {
                prop_assert!(!part.is_empty());
                prop_assert!(part != "." && part != "..");
                prop_assert!(part.len() <= max_component.max(MIN_COMPONENT_BYTES));
            }
            prop_assert_eq!(RelPath::new(path).expect("normalised"), rendered);
        }

        #[test]
        fn rendering_with_default_options_is_within_the_default_limits(
            text in pattern_text(),
            ctx in any_context(),
        ) {
            prop_assume!(parse(&text).is_ok());
            let rendered = pattern(&text).render(&ctx);
            prop_assert!(rendered.as_str().len() <= DEFAULT_MAX_PATH_BYTES);
            prop_assert!(rendered.components().all(|part| part.len() <= DEFAULT_MAX_NAME_BYTES));
        }

        #[test]
        fn the_original_name_survives_when_it_is_appended(
            name in "[^/\\x00]{1,100}",
            text in pattern_text(),
        ) {
            prop_assume!(name != "." && name != ".." && name.len() <= DEFAULT_MAX_NAME_BYTES);
            prop_assume!(parse(&text).is_ok_and(|parsed| parsed.appends_filename()));
            let rendered = pattern(&text).render(&context(&name));
            prop_assert_eq!(rendered.file_name(), Some(name.as_str()));
        }

        #[test]
        fn a_shortened_file_name_still_ends_with_its_extension(
            stem in "[a-z]{1,400}",
            extension in "[a-z0-9]{1,8}",
            cap in 16usize..300,
        ) {
            let name = format!("{stem}.{extension}");
            let suffix = format!(".{extension}");
            let options = RenderOptions { max_component_bytes: cap, ..RenderOptions::default() };
            let rendered = pattern("{YYYY}").render_with(&context(&name), &options);
            let file = rendered.file_name().expect("has a file name");
            prop_assert!(file.len() <= cap);
            prop_assert!(file.ends_with(&suffix));
            prop_assert!(file.starts_with(&stem[..stem.len().min(cap - suffix.len())]));
        }

        #[test]
        fn sanitised_values_contain_no_unsafe_characters(value in any_text(60)) {
            let clean = sanitize_value(&value);
            prop_assert_eq!(clean.chars().count(), value.chars().count());
            prop_assert!(!clean.chars().any(|c| c.is_control() || "/\\:*?\"<>|".contains(c)));
            prop_assert_eq!(sanitize_value(&clean), clean);
        }

        #[test]
        fn display_round_trips(text in pattern_text()) {
            prop_assume!(parse(&text).is_ok());
            let parsed = pattern(&text);
            let shown = parsed.to_string();
            prop_assert_eq!(parse(&shown).expect("canonical text parses"), parsed);
        }

        #[test]
        fn parsing_arbitrary_text_never_panics_and_agrees_with_validate(text in noisy_text()) {
            let errors = validate(&text);
            match parse(&text) {
                Ok(parsed) => {
                    prop_assert!(errors.is_empty());
                    prop_assert_eq!(pattern(&parsed.to_string()), parsed);
                }
                Err(first) => prop_assert_eq!(Some(&first), errors.first()),
            }
            for found in &errors {
                prop_assert!(found.span.start <= found.span.end);
                prop_assert!(found.span.get(&text).is_some());
            }
            prop_assert!(errors.windows(2).all(|pair| pair[0].span.start <= pair[1].span.start));
        }

        #[test]
        fn suggestions_are_always_real_tokens(typed in "[A-Za-z_ ]{0,12}") {
            if let Some(name) = suggest(&typed) {
                prop_assert!(Token::from_name(name).is_some());
            }
        }
    }
}
