//! Capture-date model, tolerant date parsers and the capture-date priority list (plan 5.4).
//!
//! Everything here is pure: the caller supplies the raw strings, the file times and the local time
//! zone, so a result never depends on the clock, the environment or the server's `TZ`.
//!
//! # What is stored
//!
//! A [`CaptureDate`] is the value behind three `files` columns:
//!
//! | Column | Field | Meaning |
//! |---|---|---|
//! | `capture_ts` | [`CaptureDate::naive_local`] | The wall clock where the photo was taken, as epoch seconds *as if it were UTC* ("naive-local"). `2021-01-01 00:00:00` is always `1609459200`, whatever time zone anyone is in. |
//! | `capture_offset_min` | [`CaptureDate::offset_min`] | Minutes east of UTC at the place of capture, when known. `Some(m)` means `naive_local - 60 * m` is the exact UTC instant; `None` means the source was a bare wall-clock time (most EXIF). |
//! | `capture_src` | [`CaptureDate::src`] | Which source won: `exif`, `xmp`, `video` or `fs`. |
//!
//! Because the wall clock is what is stored, `{YYYY}/{mm}` folders and the duplicate finder's "time"
//! mode (equal to the second) use the photographer's local time on any server: a photo shot at
//! 23:30 on 31 December stays a 31 December photo wherever the NAS is.
//!
//! # Priority
//!
//! [`resolve_capture`] takes the first *valid* value in this order. A value that fails to parse is
//! skipped, never fatal, and the function always returns a date.
//!
//! 1. `SubSecDateTimeOriginal`, then `DateTimeOriginal`, plus `OffsetTimeOriginal` when the value
//!    carries no offset of its own: source `exif`.
//! 2. The sidecar date: source `xmp`.
//! 3. In this order: EXIF `CreateDate` (`exif`), `com.apple.quicktime.creationdate` (`video`),
//!    QuickTime `CreateDate` (`video`), `MediaCreateDate` (`video`).
//! 4. File birth time: source `fs`.
//! 5. File modification time (always available): source `fs`.
//!
//! The wall clock of an embedded value is kept verbatim, with no time-zone arithmetic. Only values
//! that are UTC by definition are converted to the local time zone: QuickTime `CreateDate` and
//! `MediaCreateDate` without an explicit offset, and the file times. For those the local UTC offset
//! is recorded, so the exact instant survives. An explicit offset is never second-guessed: a value
//! that carries one keeps its wall clock and that offset.
//!
//! A date is always inside [`MIN_YEAR`]`..=`[`MAX_YEAR`]. Birth times that are zero, negative or out of
//! range mean "unknown"; an out-of-range modification time is clamped.
//!
//! # Accepted text
//!
//! [`parse_datetime`] accepts what exiftool and XMP produce, and is lenient about the rest:
//!
//! - Date `YYYY:MM:DD`, `YYYY-MM-DD` or `YYYY/MM/DD` (one separator used twice); month and day may
//!   have one or two digits.
//! - Optionally followed by a space or `T`, then `HH:MM` or `HH:MM:SS`, and a fraction (`.` or `,`
//!   then digits). Fractions are *truncated*, never rounded, so `23:59:59.999` on 31 December is
//!   still that day.
//! - Optionally followed by a zone: `Z`, `+HH:MM`, `+HHMM` or `+HH`, from `-12:00` to `+14:00`.
//! - Surrounding whitespace and NUL padding are ignored.
//!
//! Zero dates (`0000:00:00 00:00:00`), impossible dates, seconds of 60 or more, years outside
//! [`MIN_YEAR`]`..=`[`MAX_YEAR`], year-only values and trailing garbage are errors.
//!
//! # Daylight saving time
//!
//! UTC to local conversion is never ambiguous. The reverse, [`CaptureDate::utc_secs`] and
//! [`local_to_utc_secs`], is decided like JavaScript's `Temporal` ("compatible"): a recorded offset
//! always wins; otherwise a wall clock that happens twice (clocks going back) means the *earlier*
//! instant, and one that never happens (clocks going forward) is read with the offset in force
//! *before* the change, so it lands the length of the gap later (02:30 becomes 03:30 in Paris).

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use crate::dto::CaptureSource;

/// First year a capture date may have. Earlier years are junk (a camera whose clock was never set).
pub const MIN_YEAR: i32 = 1800;

/// Last year a capture date may have.
pub const MAX_YEAR: i32 = 2200;

/// `1800-01-01T00:00:00Z` as epoch seconds.
const MIN_SECS: i64 = -5_364_662_400;

/// `2200-12-31T23:59:59Z` as epoch seconds.
const MAX_SECS: i64 = 7_289_654_399;

/// UTC-12:00, the westernmost real offset.
const MIN_OFFSET_MIN: i32 = -12 * 60;

/// UTC+14:00, the easternmost real offset.
const MAX_OFFSET_MIN: i32 = 14 * 60;

const SECS_PER_DAY: i64 = 86_400;

/// The resolved capture date of a file. See the module docs for what each field stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureDate {
    /// The wall clock where the photo was taken, as epoch seconds as if it were UTC (`capture_ts`).
    pub naive_local: i64,
    /// Minutes east of UTC at the place of capture, when known (`capture_offset_min`).
    pub offset_min: Option<i32>,
    /// Which source won (`capture_src`).
    pub src: CaptureSource,
}

impl CaptureDate {
    /// Builds a capture date from a wall-clock reading.
    pub fn from_naive(naive: NaiveDateTime, offset_min: Option<i32>, src: CaptureSource) -> Self {
        Self {
            naive_local: naive.and_utc().timestamp(),
            offset_min,
            src,
        }
    }

    /// The wall clock as a [`NaiveDateTime`]. `None` only when `naive_local` is outside chrono's
    /// range, which values produced by this module never are.
    pub fn naive(self) -> Option<NaiveDateTime> {
        DateTime::from_timestamp(self.naive_local, 0).map(|instant| instant.naive_utc())
    }

    /// The exact instant as UTC epoch seconds: through the recorded offset when there is one,
    /// otherwise by reading the wall clock in `fallback_tz` (see [`local_to_utc_secs`]).
    pub fn utc_secs<Z: TimeZone>(self, fallback_tz: &Z) -> i64 {
        match self.offset_min {
            Some(minutes) => self
                .naive_local
                .saturating_sub(i64::from(minutes).saturating_mul(60)),
            None => local_to_utc_secs(self.naive_local, fallback_tz),
        }
    }
}

/// Converts a wall clock (naive-local epoch seconds) in `tz` to UTC epoch seconds, resolving
/// daylight-saving ambiguities as described in the module docs: the earlier instant of a repeated
/// wall clock, the offset from before the change for a skipped one. Values outside chrono's range
/// are returned unchanged.
pub fn local_to_utc_secs<Z: TimeZone>(naive_local: i64, tz: &Z) -> i64 {
    let Some(naive) = DateTime::from_timestamp(naive_local, 0).map(|instant| instant.naive_utc())
    else {
        return naive_local;
    };
    let offset_secs = match tz.from_local_datetime(&naive) {
        LocalResult::Single(local) | LocalResult::Ambiguous(local, _) => {
            local.offset().fix().local_minus_utc()
        }
        LocalResult::None => {
            // A day back is safely before the change whatever the zone, as Temporal does it.
            let day_before = DateTime::from_timestamp(naive_local.saturating_sub(SECS_PER_DAY), 0)
                .map_or(naive, |instant| instant.naive_utc());
            tz.offset_from_utc_datetime(&day_before)
                .fix()
                .local_minus_utc()
        }
    };
    naive_local.saturating_sub(i64::from(offset_secs))
}

/// Why a string is not a usable date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DateParseError {
    /// Nothing but whitespace.
    #[error("the value is empty")]
    Empty,
    /// Not shaped like a date or a date and time.
    #[error("not a recognised date or time format")]
    Syntax,
    /// A `0000:00:00` style placeholder, written by cameras without a clock.
    #[error("zero date")]
    ZeroDate,
    /// The year is outside [`MIN_YEAR`]`..=`[`MAX_YEAR`].
    #[error("year outside {MIN_YEAR}..={MAX_YEAR}")]
    YearOutOfRange,
    /// The month or day does not exist (`2021:02:29`).
    #[error("no such calendar date")]
    InvalidDate,
    /// The hour, minute or second does not exist (`24:00:00`, `12:30:60`).
    #[error("no such time of day")]
    InvalidTime,
    /// The UTC offset is malformed or outside `-12:00..=+14:00`.
    #[error("invalid UTC offset")]
    InvalidOffset,
}

/// A date and optional time parsed from text, with the offset the text carried (if any).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedDateTime {
    /// The wall clock; midnight when the text had no time.
    pub naive: NaiveDateTime,
    /// Minutes east of UTC when the text had a zone (`Z` is `0`).
    pub offset_min: Option<i32>,
    /// Whether the text had a time part.
    pub has_time: bool,
}

/// Parses an exiftool or XMP date and time. See the module docs for the accepted forms.
pub fn parse_datetime(raw: &str) -> Result<ParsedDateTime, DateParseError> {
    let text = trim_value(raw);
    if text.is_empty() {
        return Err(DateParseError::Empty);
    }
    let mut scanner = Scanner::new(text);
    let year = scanner.digits(4, 4).ok_or(DateParseError::Syntax)?;
    let separator = scanner.eat_any(b":-/").ok_or(DateParseError::Syntax)?;
    let month = scanner.digits(1, 2).ok_or(DateParseError::Syntax)?;
    if !scanner.eat(separator) {
        return Err(DateParseError::Syntax);
    }
    let day = scanner.digits(1, 2).ok_or(DateParseError::Syntax)?;

    let has_time = !scanner.at_end();
    let ((hour, minute, second), offset_min) = if has_time {
        scan_time(&mut scanner)?
    } else {
        ((0, 0, 0), None)
    };
    if !scanner.at_end() {
        return Err(DateParseError::Syntax);
    }

    if year == 0 && month == 0 && day == 0 {
        return Err(DateParseError::ZeroDate);
    }
    let year = i32::try_from(year).map_err(|_| DateParseError::Syntax)?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(DateParseError::YearOutOfRange);
    }
    let date = NaiveDate::from_ymd_opt(year, month, day).ok_or(DateParseError::InvalidDate)?;
    let time = NaiveTime::from_hms_opt(hour, minute, second).ok_or(DateParseError::InvalidTime)?;
    Ok(ParsedDateTime {
        naive: date.and_time(time),
        offset_min,
        has_time,
    })
}

/// Parses a UTC offset (`Z`, `+02:00`, `+0200`, `+02`, `-05:30`) into minutes east of UTC.
pub fn parse_offset(raw: &str) -> Result<i32, DateParseError> {
    let text = trim_value(raw);
    if text.is_empty() {
        return Err(DateParseError::Empty);
    }
    let mut scanner = Scanner::new(text);
    let minutes = scan_zone(&mut scanner)?;
    if scanner.at_end() {
        Ok(minutes)
    } else {
        Err(DateParseError::Syntax)
    }
}

fn trim_value(raw: &str) -> &str {
    raw.trim_matches(|c: char| c.is_whitespace() || c == '\0')
}

type TimeParts = (u32, u32, u32);

/// Reads what follows the date: separator, time, fraction and zone.
fn scan_time(scanner: &mut Scanner<'_>) -> Result<(TimeParts, Option<i32>), DateParseError> {
    if scanner.eat_any(b"Tt").is_none() && !scanner.skip_blanks() {
        return Err(DateParseError::Syntax);
    }
    let hour = scanner.digits(1, 2).ok_or(DateParseError::Syntax)?;
    if !scanner.eat(b':') {
        return Err(DateParseError::Syntax);
    }
    let minute = scanner.digits(1, 2).ok_or(DateParseError::Syntax)?;
    let mut second = 0;
    if scanner.eat(b':') {
        second = scanner.digits(1, 2).ok_or(DateParseError::Syntax)?;
        if scanner.eat_any(b".,").is_some() && !scanner.skip_digits() {
            return Err(DateParseError::Syntax);
        }
    }
    scanner.skip_blanks();
    let offset = if scanner.at_end() {
        None
    } else {
        Some(scan_zone(scanner)?)
    };
    Ok(((hour, minute, second), offset))
}

fn scan_zone(scanner: &mut Scanner<'_>) -> Result<i32, DateParseError> {
    if scanner.eat_any(b"Zz").is_some() {
        return Ok(0);
    }
    let sign = match scanner.eat_any(b"+-") {
        Some(b'-') => -1,
        Some(_) => 1,
        None => return Err(DateParseError::Syntax),
    };
    let hours = scanner.digits(2, 2).ok_or(DateParseError::Syntax)?;
    let has_minutes = scanner.eat(b':') || scanner.peek().is_some_and(|b| b.is_ascii_digit());
    let minutes = if has_minutes {
        scanner.digits(2, 2).ok_or(DateParseError::Syntax)?
    } else {
        0
    };
    if minutes >= 60 {
        return Err(DateParseError::InvalidOffset);
    }
    let magnitude =
        i32::try_from(hours * 60 + minutes).map_err(|_| DateParseError::InvalidOffset)?;
    let total = sign * magnitude;
    if (MIN_OFFSET_MIN..=MAX_OFFSET_MIN).contains(&total) {
        Ok(total)
    } else {
        Err(DateParseError::InvalidOffset)
    }
}

/// A cursor over ASCII text. Working on bytes keeps every step valid for any UTF-8 input.
struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn eat_any(&mut self, choices: &[u8]) -> Option<u8> {
        let byte = self.peek().filter(|b| choices.contains(b))?;
        self.pos += 1;
        Some(byte)
    }

    fn eat(&mut self, expected: u8) -> bool {
        self.eat_any(&[expected]).is_some()
    }

    fn skip_blanks(&mut self) -> bool {
        let start = self.pos;
        while self.eat_any(b" \t").is_some() {}
        self.pos > start
    }

    /// Reads between `min` and `max` ASCII digits (`max` is at most 4, so the value cannot overflow).
    fn digits(&mut self, min: usize, max: usize) -> Option<u32> {
        let start = self.pos;
        let mut value = 0u32;
        while self.pos - start < max {
            let Some(digit) = self.peek().filter(u8::is_ascii_digit) else {
                break;
            };
            value = value * 10 + u32::from(digit - b'0');
            self.pos += 1;
        }
        (self.pos - start >= min).then_some(value)
    }

    /// Skips any number of ASCII digits; `false` when there was none.
    fn skip_digits(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        self.pos > start
    }
}

/// Everything the extractor knows about when a file was captured. Strings are passed raw, exactly
/// as exiftool or the XMP parser produced them; [`resolve_capture`] parses them.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureInputs<'a> {
    /// `SubSecDateTimeOriginal` (local wall clock, possibly with a fraction and an offset).
    pub sub_sec_date_time_original: Option<&'a str>,
    /// `DateTimeOriginal` (local wall clock).
    pub date_time_original: Option<&'a str>,
    /// `OffsetTimeOriginal` (`+02:00`), the zone of the two values above.
    pub offset_time_original: Option<&'a str>,
    /// The date of the XMP sidecar (`exif:DateTimeOriginal`, `xmp:CreateDate`, `photoshop:DateCreated`).
    pub sidecar_date: Option<&'a str>,
    /// EXIF or XMP `CreateDate`: a local wall clock. QuickTime's `CreateDate` is UTC and belongs in
    /// `quicktime_create_date` instead.
    pub create_date: Option<&'a str>,
    /// `com.apple.quicktime.creationdate`: a local wall clock that carries its own offset.
    pub quicktime_creation_date: Option<&'a str>,
    /// QuickTime/MP4 `CreateDate`, UTC unless the value carries an offset.
    pub quicktime_create_date: Option<&'a str>,
    /// `MediaCreateDate`, UTC unless the value carries an offset.
    pub media_create_date: Option<&'a str>,
    /// File birth time as UTC epoch seconds, when the filesystem reports one.
    pub birth_time: Option<i64>,
    /// File modification time as UTC epoch seconds; the last resort, so it is not optional.
    pub modified_time: i64,
}

/// Resolves the capture date of a file following the priority list in the module docs.
///
/// `local_tz` converts the UTC-based sources (QuickTime dates without an offset and the file times)
/// to a wall clock; pass a [`chrono_tz::Tz`] (see [`parse_tz`]), a `FixedOffset` or `Utc`. Total:
/// it never fails, and garbage inputs are skipped.
pub fn resolve_capture<Z: TimeZone>(inputs: &CaptureInputs<'_>, local_tz: &Z) -> CaptureDate {
    let original = parse_opt(inputs.sub_sec_date_time_original)
        .or_else(|| parse_opt(inputs.date_time_original));
    if let Some(parsed) = original {
        let tag_offset = inputs
            .offset_time_original
            .and_then(|raw| parse_offset(raw).ok());
        return wall_clock(&parsed, tag_offset, CaptureSource::Exif);
    }
    if let Some(parsed) = parse_opt(inputs.sidecar_date) {
        return wall_clock(&parsed, None, CaptureSource::Xmp);
    }
    if let Some(parsed) = parse_opt(inputs.create_date) {
        return wall_clock(&parsed, None, CaptureSource::Exif);
    }
    if let Some(parsed) = parse_opt(inputs.quicktime_creation_date) {
        return wall_clock(&parsed, None, CaptureSource::Video);
    }
    for raw in [inputs.quicktime_create_date, inputs.media_create_date] {
        if let Some(parsed) = parse_opt(raw) {
            return match parsed.offset_min {
                Some(_) => wall_clock(&parsed, None, CaptureSource::Video),
                None => at_instant(
                    parsed.naive.and_utc().timestamp(),
                    local_tz,
                    CaptureSource::Video,
                ),
            };
        }
    }
    if let Some(birth) = inputs.birth_time
        && (1..=MAX_SECS).contains(&birth)
    {
        return at_instant(birth, local_tz, CaptureSource::Fs);
    }
    at_instant(inputs.modified_time, local_tz, CaptureSource::Fs)
}

fn parse_opt(raw: Option<&str>) -> Option<ParsedDateTime> {
    raw.and_then(|text| parse_datetime(text).ok())
}

/// Keeps the wall clock of `parsed`; `fallback_offset` is used when the text had no zone.
fn wall_clock(
    parsed: &ParsedDateTime,
    fallback_offset: Option<i32>,
    src: CaptureSource,
) -> CaptureDate {
    CaptureDate::from_naive(parsed.naive, parsed.offset_min.or(fallback_offset), src)
}

/// The wall clock of the UTC instant `utc_secs` in `tz`, with the offset in force at that instant.
fn at_instant<Z: TimeZone>(utc_secs: i64, tz: &Z, src: CaptureSource) -> CaptureDate {
    let instant = DateTime::from_timestamp(utc_secs.clamp(MIN_SECS, MAX_SECS), 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let local = instant.with_timezone(tz);
    let offset_secs = local.offset().fix().local_minus_utc();
    CaptureDate {
        naive_local: local
            .naive_local()
            .and_utc()
            .timestamp()
            .clamp(MIN_SECS, MAX_SECS),
        // Whole minutes: only pre-1900 local mean times have seconds, and there it rounds toward zero.
        offset_min: Some(offset_secs / 60),
        src,
    }
}

/// Parses an IANA time-zone name for the server's `TZ` setting. Case-insensitive, and tolerant of
/// a leading `:` and of a `.../zoneinfo/` path prefix. POSIX strings such as `CET-1CEST,M3.5.0` are
/// not understood and give `None`.
pub fn parse_tz(name: &str) -> Option<Tz> {
    let trimmed = name.trim().trim_start_matches(':');
    let name = trimmed
        .rsplit_once("zoneinfo/")
        .map_or(trimmed, |(_, zone)| zone);
    if name.is_empty() {
        return None;
    }
    name.parse::<Tz>().ok().or_else(|| {
        chrono_tz::TZ_VARIANTS
            .iter()
            .copied()
            .find(|tz| tz.name().eq_ignore_ascii_case(name))
    })
}

/// [`parse_tz`] with UTC as the fallback for a missing or unknown name.
pub fn tz_or_utc(name: Option<&str>) -> Tz {
    name.and_then(parse_tz).unwrap_or(Tz::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, FixedOffset};
    use chrono_tz::America::New_York;
    use chrono_tz::Asia::{Kolkata, Tokyo};
    use chrono_tz::Europe::Paris;
    use proptest::prelude::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|date| date.and_hms_opt(h, mi, s))
            .expect("valid test date")
    }

    fn secs(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        at(y, mo, d, h, mi, s).and_utc().timestamp()
    }

    fn cd(naive: NaiveDateTime, offset_min: Option<i32>, src: CaptureSource) -> CaptureDate {
        CaptureDate::from_naive(naive, offset_min, src)
    }

    fn parsed(raw: &str) -> ParsedDateTime {
        parse_datetime(raw).unwrap_or_else(|e| panic!("{raw:?} should parse: {e}"))
    }

    fn fixed(seconds_east: i32) -> FixedOffset {
        FixedOffset::east_opt(seconds_east).expect("valid offset")
    }

    fn inputs() -> CaptureInputs<'static> {
        CaptureInputs {
            modified_time: secs(2019, 1, 2, 3, 4, 5),
            ..CaptureInputs::default()
        }
    }

    fn only_mtime(modified_time: i64) -> CaptureInputs<'static> {
        CaptureInputs {
            modified_time,
            ..CaptureInputs::default()
        }
    }

    #[test]
    fn range_constants_match_chrono() {
        assert_eq!(MIN_SECS, secs(MIN_YEAR, 1, 1, 0, 0, 0));
        assert_eq!(MAX_SECS, secs(MAX_YEAR, 12, 31, 23, 59, 59));
    }

    #[test]
    fn naive_local_is_the_wall_clock_read_as_utc() {
        let date = cd(at(2021, 1, 1, 0, 0, 0), None, CaptureSource::Exif);
        assert_eq!(date.naive_local, 1_609_459_200);
        assert_eq!(date.naive(), Some(at(2021, 1, 1, 0, 0, 0)));
        let before_epoch = cd(at(1969, 12, 31, 23, 59, 59), Some(60), CaptureSource::Fs);
        assert_eq!(before_epoch.naive_local, -1);
        assert_eq!(before_epoch.naive(), Some(at(1969, 12, 31, 23, 59, 59)));
        let absurd = CaptureDate {
            naive_local: i64::MAX,
            offset_min: None,
            src: CaptureSource::Fs,
        };
        assert_eq!(absurd.naive(), None);
        assert_eq!(absurd.utc_secs(&Utc), i64::MAX);
    }

    #[test]
    fn parses_exiftool_xmp_and_iso_forms() {
        let expected = at(2026, 10, 3, 14, 7, 9);
        for raw in [
            "2026:10:03 14:07:09",
            "2026-10-03 14:07:09",
            "2026-10-03T14:07:09",
            "2026/10/03 14:07:09",
            "2026:10:03T14:07:09",
            "2026-10-03t14:07:09",
            "2026:10:03   14:07:09",
            "2026:10:03\t14:07:09",
            "  2026:10:03 14:07:09 ",
            "2026:10:03 14:07:09\0\0",
            "2026:10:03 14:07:09.4",
            "2026:10:03 14:07:09,456789",
            "2026-10-03T14:07:09.123456789",
            "2026:10:3 14:7:9",
        ] {
            let p = parsed(raw);
            assert_eq!(p.naive, expected, "{raw:?}");
            assert_eq!(p.offset_min, None, "{raw:?}");
            assert!(p.has_time, "{raw:?}");
        }
    }

    #[test]
    fn parses_zones_in_every_notation() {
        let expected = at(2026, 10, 3, 14, 7, 9);
        for (raw, offset) in [
            ("2026:10:03 14:07:09Z", 0),
            ("2026:10:03 14:07:09z", 0),
            ("2026-10-03T14:07:09.5Z", 0),
            ("2026:10:03 14:07:09+02:00", 120),
            ("2026:10:03 14:07:09.45+02:00", 120),
            ("2026:10:03 14:07:09+0200", 120),
            ("2026:10:03 14:07:09+02", 120),
            ("2026:10:03 14:07:09 +02:00", 120),
            ("2026:10:03 14:07:09-05:30", -330),
            ("2026:10:03 14:07:09-00:00", 0),
            ("2026:10:03 14:07:09+14:00", 840),
            ("2026:10:03 14:07:09-12:00", -720),
            ("2026:10:03 14:07:09+05:45", 345),
        ] {
            let p = parsed(raw);
            assert_eq!(p.naive, expected, "{raw:?}");
            assert_eq!(p.offset_min, Some(offset), "{raw:?}");
        }
    }

    #[test]
    fn minutes_only_times_and_date_only_values() {
        let p = parsed("2026:10:03 14:07");
        assert_eq!((p.naive, p.has_time), (at(2026, 10, 3, 14, 7, 0), true));
        for raw in ["2026:10:03", "2026-10-03", "2026/10/03", " 2026-10-03\0"] {
            let p = parsed(raw);
            assert_eq!(p.naive, at(2026, 10, 3, 0, 0, 0), "{raw:?}");
            assert!(!p.has_time && p.offset_min.is_none(), "{raw:?}");
        }
    }

    #[test]
    fn fractions_are_truncated_never_rounded() {
        for raw in [
            "2020:12:31 23:59:59.999",
            "2020:12:31 23:59:59.9999999999",
            "2020:12:31 23:59:59,5",
            "2020-12-31T23:59:59.999999+00:00",
        ] {
            assert_eq!(parsed(raw).naive, at(2020, 12, 31, 23, 59, 59), "{raw:?}");
        }
    }

    #[test]
    fn leap_days_follow_the_gregorian_rules() {
        assert!(parse_datetime("2020:02:29 00:00:00").is_ok());
        assert!(parse_datetime("2000:02:29 00:00:00").is_ok());
        for raw in ["2021:02:29 00:00:00", "1900:02:29 00:00:00", "2200:02:29"] {
            assert_eq!(
                parse_datetime(raw),
                Err(DateParseError::InvalidDate),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejects_zero_dates_and_impossible_values() {
        use DateParseError::*;
        for (raw, error) in [
            ("", Empty),
            ("   \0 ", Empty),
            ("0000:00:00 00:00:00", ZeroDate),
            ("0000-00-00", ZeroDate),
            ("0000:00:00", ZeroDate),
            ("0000:01:01 00:00:00", YearOutOfRange),
            ("1799:12:31 23:59:59", YearOutOfRange),
            ("2201:01:01 00:00:00", YearOutOfRange),
            ("9999:12:31 23:59:59", YearOutOfRange),
            ("2020:00:10 00:00:00", InvalidDate),
            ("2020:10:00 00:00:00", InvalidDate),
            ("2020:13:01 00:00:00", InvalidDate),
            ("2020:04:31 00:00:00", InvalidDate),
            ("2020:01:32 00:00:00", InvalidDate),
            ("2020:05:01 24:00:00", InvalidTime),
            ("2020:05:01 12:60:00", InvalidTime),
            ("2020:05:01 12:30:60", InvalidTime),
            ("2020:05:01 99:99:99", InvalidTime),
            ("2020:05:01 12:30:45+15:00", InvalidOffset),
            ("2020:05:01 12:30:45-12:01", InvalidOffset),
            ("2020:05:01 12:30:45+02:60", InvalidOffset),
            ("2020:05:01 12:30:45+99:00", InvalidOffset),
        ] {
            assert_eq!(parse_datetime(raw), Err(error), "{raw:?}");
        }
    }

    #[test]
    fn the_year_range_is_inclusive() {
        assert_eq!(parsed("1800:01:01 00:00:00").naive, at(1800, 1, 1, 0, 0, 0));
        assert_eq!(
            parsed("2200:12:31 23:59:59").naive,
            at(2200, 12, 31, 23, 59, 59)
        );
    }

    #[test]
    fn rejects_text_that_is_not_a_date() {
        for raw in [
            "garbage",
            "2020",
            "2020:05",
            "2020:05:",
            "2020:05-01",
            "2020-05:01",
            "20200501",
            "05/01/2020",
            "2020:05:01 12",
            "2020:05:01 12:",
            "2020:05:01 12:30:",
            "2020:05:01 12:30:45.",
            "2020:05:01 12:30:45 junk",
            "2020:05:01 12:30:45Zjunk",
            "2020:05:01 12:30:45+",
            "2020:05:01 12:30:45+2:00",
            "2020:05:01 12:30:45+020",
            "2020:05:01 12:30:45+02:0",
            "2020:05:01 12:30:45+02:00:00",
            "2020:05:01T",
            "2020:05:01Z",
            "2020:05:01 12:30:45 PM",
            "20200:05:01",
            "2020:005:01",
            "2020:05:001",
            "    :  :     :  :  ",
            "２０２０:05:01",
            "2020:05:01 12:30:45\u{a0}junk",
            "-2020:05:01",
            "+2020:05:01",
            "1e3:05:01",
        ] {
            assert_eq!(
                parse_datetime(raw),
                Err(DateParseError::Syntax),
                "{raw:?} should be a syntax error"
            );
        }
    }

    #[test]
    fn parses_offsets_on_their_own() {
        for (raw, minutes) in [
            ("Z", 0),
            ("z", 0),
            ("+00:00", 0),
            ("+02:00", 120),
            ("+0200", 120),
            ("+02", 120),
            ("-05:30", -330),
            (" +01:00 ", 60),
            ("+14:00", 840),
            ("-12:00", -720),
        ] {
            assert_eq!(parse_offset(raw), Ok(minutes), "{raw:?}");
        }
        for (raw, error) in [
            ("", DateParseError::Empty),
            ("+", DateParseError::Syntax),
            ("+2", DateParseError::Syntax),
            ("02:00", DateParseError::Syntax),
            ("UTC", DateParseError::Syntax),
            ("+02:00 junk", DateParseError::Syntax),
            ("+15:00", DateParseError::InvalidOffset),
            ("-13:00", DateParseError::InvalidOffset),
            ("+02:60", DateParseError::InvalidOffset),
        ] {
            assert_eq!(parse_offset(raw), Err(error), "{raw:?}");
        }
    }

    #[test]
    fn errors_have_readable_messages() {
        assert_eq!(DateParseError::ZeroDate.to_string(), "zero date");
        assert_eq!(
            DateParseError::YearOutOfRange.to_string(),
            "year outside 1800..=2200"
        );
    }

    #[test]
    fn exif_original_wins_over_every_other_source() {
        let all = CaptureInputs {
            sub_sec_date_time_original: Some("2026:10:03 14:07:09.45"),
            date_time_original: Some("2020:01:01 00:00:00"),
            offset_time_original: Some("+02:00"),
            sidecar_date: Some("2021:01:01 00:00:00"),
            create_date: Some("2022:01:01 00:00:00"),
            quicktime_creation_date: Some("2023:01:01 00:00:00+01:00"),
            quicktime_create_date: Some("2024:01:01 00:00:00"),
            media_create_date: Some("2025:01:01 00:00:00"),
            birth_time: Some(secs(2018, 1, 1, 0, 0, 0)),
            modified_time: secs(2019, 1, 1, 0, 0, 0),
        };
        assert_eq!(
            resolve_capture(&all, &Paris),
            cd(at(2026, 10, 3, 14, 7, 9), Some(120), CaptureSource::Exif)
        );
    }

    #[test]
    fn an_unusable_sub_sec_value_falls_back_to_date_time_original() {
        let sources = CaptureInputs {
            sub_sec_date_time_original: Some("0000:00:00 00:00:00"),
            date_time_original: Some("2020:01:02 03:04:05"),
            sidecar_date: Some("2021:01:01 00:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&sources, &Utc),
            cd(at(2020, 1, 2, 3, 4, 5), None, CaptureSource::Exif)
        );
        let only_sub_sec = CaptureInputs {
            sub_sec_date_time_original: Some("2020:01:02 03:04:05.9"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&only_sub_sec, &Utc).naive(),
            Some(at(2020, 1, 2, 3, 4, 5))
        );
    }

    #[test]
    fn offset_time_original_is_attached_unless_the_value_has_its_own() {
        let with_tag = CaptureInputs {
            date_time_original: Some("2020:07:01 12:00:00"),
            offset_time_original: Some("+09:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&with_tag, &Paris),
            cd(at(2020, 7, 1, 12, 0, 0), Some(540), CaptureSource::Exif)
        );
        let embedded_wins = CaptureInputs {
            sub_sec_date_time_original: Some("2020:07:01 12:00:00.5-03:30"),
            offset_time_original: Some("+09:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&embedded_wins, &Paris).offset_min,
            Some(-210)
        );
        let bad_tag = CaptureInputs {
            date_time_original: Some("2020:07:01 12:00:00"),
            offset_time_original: Some("garbage"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&bad_tag, &Paris),
            cd(at(2020, 7, 1, 12, 0, 0), None, CaptureSource::Exif)
        );
        let tag_alone = CaptureInputs {
            offset_time_original: Some("+09:00"),
            ..inputs()
        };
        assert_eq!(resolve_capture(&tag_alone, &Utc).src, CaptureSource::Fs);
    }

    #[test]
    fn exif_wall_clock_never_moves_with_the_local_zone() {
        let new_year_eve = CaptureInputs {
            date_time_original: Some("2020:12:31 23:59:59"),
            ..inputs()
        };
        let expected = cd(at(2020, 12, 31, 23, 59, 59), None, CaptureSource::Exif);
        assert_eq!(resolve_capture(&new_year_eve, &Utc), expected);
        assert_eq!(resolve_capture(&new_year_eve, &Tokyo), expected);
        assert_eq!(resolve_capture(&new_year_eve, &New_York), expected);
        let new_year = CaptureInputs {
            date_time_original: Some("2021:01:01 00:00:00"),
            ..inputs()
        };
        let expected = cd(at(2021, 1, 1, 0, 0, 0), None, CaptureSource::Exif);
        assert_eq!(resolve_capture(&new_year, &Tokyo), expected);
        assert_eq!(resolve_capture(&new_year, &New_York), expected);
        let sub_second = CaptureInputs {
            sub_sec_date_time_original: Some("2020:12:31 23:59:59.999"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&sub_second, &Tokyo).naive(),
            Some(at(2020, 12, 31, 23, 59, 59))
        );
    }

    #[test]
    fn the_sidecar_date_beats_create_date_and_loses_to_the_original() {
        let sources = CaptureInputs {
            sidecar_date: Some("2021-05-06T07:08:09+02:00"),
            create_date: Some("2022:01:01 00:00:00"),
            quicktime_creation_date: Some("2023:01:01 00:00:00+01:00"),
            birth_time: Some(secs(2018, 1, 1, 0, 0, 0)),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&sources, &Paris),
            cd(at(2021, 5, 6, 7, 8, 9), Some(120), CaptureSource::Xmp)
        );
        let with_original = CaptureInputs {
            date_time_original: Some("2000:01:01 00:00:00"),
            ..sources
        };
        assert_eq!(
            resolve_capture(&with_original, &Paris),
            cd(at(2000, 1, 1, 0, 0, 0), None, CaptureSource::Exif)
        );
        let bad_sidecar = CaptureInputs {
            sidecar_date: Some("not a date"),
            ..sources
        };
        assert_eq!(
            resolve_capture(&bad_sidecar, &Paris),
            cd(at(2022, 1, 1, 0, 0, 0), None, CaptureSource::Exif)
        );
    }

    #[test]
    fn create_date_is_a_local_wall_clock_and_beats_the_video_dates() {
        let sources = CaptureInputs {
            create_date: Some("2022:03:04 05:06:07"),
            quicktime_creation_date: Some("2023:01:01 00:00:00+01:00"),
            quicktime_create_date: Some("2024:01:01 00:00:00"),
            media_create_date: Some("2025:01:01 00:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&sources, &New_York),
            cd(at(2022, 3, 4, 5, 6, 7), None, CaptureSource::Exif)
        );
    }

    #[test]
    fn quicktime_creation_date_keeps_its_wall_clock_and_offset() {
        let sources = CaptureInputs {
            quicktime_creation_date: Some("2023-07-14T21:30:00+0200"),
            quicktime_create_date: Some("2023:07:14 19:30:00"),
            media_create_date: Some("2023:07:14 19:30:00"),
            ..inputs()
        };
        let expected = cd(at(2023, 7, 14, 21, 30, 0), Some(120), CaptureSource::Video);
        assert_eq!(resolve_capture(&sources, &Paris), expected);
        assert_eq!(resolve_capture(&sources, &Tokyo), expected);
        assert_eq!(resolve_capture(&sources, &Utc), expected);
    }

    #[test]
    fn quicktime_utc_dates_are_converted_to_the_local_zone() {
        let utc_value = CaptureInputs {
            quicktime_create_date: Some("2020:07:01 10:00:00"),
            ..inputs()
        };
        macro_rules! converts_to {
            ($zone:expr, $local:expr, $offset:expr) => {{
                let got = resolve_capture(&utc_value, &$zone);
                assert_eq!(
                    got,
                    cd($local, Some($offset), CaptureSource::Video),
                    "{}",
                    stringify!($zone)
                );
                assert_eq!(got.utc_secs(&Utc), secs(2020, 7, 1, 10, 0, 0));
            }};
        }
        converts_to!(Utc, at(2020, 7, 1, 10, 0, 0), 0);
        converts_to!(Paris, at(2020, 7, 1, 12, 0, 0), 120);
        converts_to!(New_York, at(2020, 7, 1, 6, 0, 0), -240);
        converts_to!(Kolkata, at(2020, 7, 1, 15, 30, 0), 330);
        converts_to!(Tokyo, at(2020, 7, 1, 19, 0, 0), 540);
        converts_to!(fixed(9 * 3600), at(2020, 7, 1, 19, 0, 0), 540);
        converts_to!(fixed(-(3 * 3600 + 1800)), at(2020, 7, 1, 6, 30, 0), -210);
        let winter = CaptureInputs {
            media_create_date: Some("2020:01:01 10:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&winter, &Paris),
            cd(at(2020, 1, 1, 11, 0, 0), Some(60), CaptureSource::Video)
        );
        assert_eq!(
            resolve_capture(&winter, &New_York),
            cd(at(2020, 1, 1, 5, 0, 0), Some(-300), CaptureSource::Video)
        );
    }

    #[test]
    fn media_create_date_follows_the_quicktime_create_date_and_zero_dates_are_skipped() {
        let zero_create = CaptureInputs {
            quicktime_create_date: Some("0000:00:00 00:00:00"),
            media_create_date: Some("2020:07:01 10:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&zero_create, &Paris),
            cd(at(2020, 7, 1, 12, 0, 0), Some(120), CaptureSource::Video)
        );
        let create_first = CaptureInputs {
            quicktime_create_date: Some("2020:07:01 10:00:00"),
            media_create_date: Some("2021:07:01 10:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&create_first, &Utc).naive(),
            Some(at(2020, 7, 1, 10, 0, 0))
        );
        let both_zero = CaptureInputs {
            quicktime_create_date: Some("0000:00:00 00:00:00"),
            media_create_date: Some("0000:00:00 00:00:00"),
            ..inputs()
        };
        assert_eq!(resolve_capture(&both_zero, &Paris).src, CaptureSource::Fs);
    }

    #[test]
    fn a_quicktime_utc_value_with_its_own_offset_is_not_converted() {
        let sources = CaptureInputs {
            media_create_date: Some("2020:07:01 12:00:00+02:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&sources, &Tokyo),
            cd(at(2020, 7, 1, 12, 0, 0), Some(120), CaptureSource::Video)
        );
    }

    #[test]
    fn year_boundaries_for_utc_values_follow_the_local_zone() {
        let new_year_eve = CaptureInputs {
            quicktime_create_date: Some("2020:12:31 23:30:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&new_year_eve, &Tokyo).naive(),
            Some(at(2021, 1, 1, 8, 30, 0))
        );
        assert_eq!(
            resolve_capture(&new_year_eve, &Paris).naive(),
            Some(at(2021, 1, 1, 0, 30, 0))
        );
        assert_eq!(
            resolve_capture(&new_year_eve, &New_York).naive(),
            Some(at(2020, 12, 31, 18, 30, 0))
        );
        let midnight = CaptureInputs {
            media_create_date: Some("2021:01:01 00:00:00"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&midnight, &New_York).naive(),
            Some(at(2020, 12, 31, 19, 0, 0))
        );
        assert_eq!(
            resolve_capture(&midnight, &Kolkata).naive(),
            Some(at(2021, 1, 1, 5, 30, 0))
        );
        let almost = CaptureInputs {
            media_create_date: Some("2020:12:31 23:59:59.999"),
            ..inputs()
        };
        assert_eq!(
            resolve_capture(&almost, &Paris).naive(),
            Some(at(2021, 1, 1, 0, 59, 59))
        );
        assert_eq!(
            resolve_capture(&almost, &Utc).naive(),
            Some(at(2020, 12, 31, 23, 59, 59))
        );
    }

    #[test]
    fn birth_time_comes_before_the_modification_time() {
        let sources = CaptureInputs {
            birth_time: Some(secs(2018, 6, 1, 10, 0, 0)),
            modified_time: secs(2020, 1, 1, 0, 0, 0),
            ..CaptureInputs::default()
        };
        assert_eq!(
            resolve_capture(&sources, &Paris),
            cd(at(2018, 6, 1, 12, 0, 0), Some(120), CaptureSource::Fs)
        );
        let no_birth = CaptureInputs {
            birth_time: None,
            ..sources
        };
        assert_eq!(
            resolve_capture(&no_birth, &Paris),
            cd(at(2020, 1, 1, 1, 0, 0), Some(60), CaptureSource::Fs)
        );
    }

    #[test]
    fn unknown_birth_times_fall_through_to_the_modification_time() {
        for birth in [0, -1, i64::MIN, i64::MAX, MAX_SECS + 1] {
            let sources = CaptureInputs {
                birth_time: Some(birth),
                modified_time: secs(2020, 1, 1, 0, 0, 0),
                ..CaptureInputs::default()
            };
            assert_eq!(
                resolve_capture(&sources, &Utc),
                cd(at(2020, 1, 1, 0, 0, 0), Some(0), CaptureSource::Fs),
                "{birth}"
            );
        }
    }

    #[test]
    fn the_modification_time_is_clamped_into_the_accepted_years() {
        for (mtime, expected) in [
            (i64::MIN, at(1800, 1, 1, 0, 0, 0)),
            (i64::MAX, at(2200, 12, 31, 23, 59, 59)),
            (secs(1700, 1, 1, 0, 0, 0), at(1800, 1, 1, 0, 0, 0)),
            (0, at(1970, 1, 1, 0, 0, 0)),
            (-1, at(1969, 12, 31, 23, 59, 59)),
        ] {
            let got = resolve_capture(&only_mtime(mtime), &Utc);
            assert_eq!(got.naive(), Some(expected), "{mtime}");
            assert_eq!(got.src, CaptureSource::Fs);
        }
    }

    #[test]
    fn file_times_convert_at_the_daylight_saving_edges() {
        let dst_start = secs(2026, 3, 29, 1, 0, 0);
        assert_eq!(
            resolve_capture(&only_mtime(dst_start - 1), &Paris),
            cd(at(2026, 3, 29, 1, 59, 59), Some(60), CaptureSource::Fs)
        );
        assert_eq!(
            resolve_capture(&only_mtime(dst_start), &Paris),
            cd(at(2026, 3, 29, 3, 0, 0), Some(120), CaptureSource::Fs)
        );

        let dst_end = secs(2026, 10, 25, 1, 0, 0);
        let first_pass = resolve_capture(&only_mtime(dst_end - 1800), &Paris);
        let second_pass = resolve_capture(&only_mtime(dst_end + 1800), &Paris);
        assert_eq!(
            first_pass,
            cd(at(2026, 10, 25, 2, 30, 0), Some(120), CaptureSource::Fs)
        );
        assert_eq!(
            second_pass,
            cd(at(2026, 10, 25, 2, 30, 0), Some(60), CaptureSource::Fs)
        );
        assert_eq!(first_pass.naive_local, second_pass.naive_local);
        assert_eq!(
            second_pass.utc_secs(&Paris) - first_pass.utc_secs(&Paris),
            3600
        );
    }

    #[test]
    fn repeated_wall_clocks_resolve_to_the_earlier_instant() {
        assert_eq!(
            local_to_utc_secs(secs(2026, 10, 25, 2, 30, 0), &Paris),
            secs(2026, 10, 25, 0, 30, 0)
        );
        assert_eq!(
            local_to_utc_secs(secs(2026, 11, 1, 1, 30, 0), &New_York),
            secs(2026, 11, 1, 5, 30, 0)
        );
    }

    #[test]
    fn skipped_wall_clocks_move_forward_by_the_gap() {
        assert_eq!(
            local_to_utc_secs(secs(2026, 3, 29, 2, 30, 0), &Paris),
            secs(2026, 3, 29, 1, 30, 0)
        );
        assert_eq!(
            local_to_utc_secs(secs(2026, 3, 29, 2, 0, 0), &Paris),
            secs(2026, 3, 29, 1, 0, 0)
        );
        assert_eq!(
            local_to_utc_secs(secs(2026, 3, 8, 2, 30, 0), &New_York),
            secs(2026, 3, 8, 7, 30, 0)
        );
    }

    #[test]
    fn ordinary_wall_clocks_use_the_offset_in_force() {
        for (wall, utc, zone) in [
            (
                secs(2026, 1, 15, 12, 0, 0),
                secs(2026, 1, 15, 11, 0, 0),
                Paris,
            ),
            (
                secs(2026, 7, 15, 12, 0, 0),
                secs(2026, 7, 15, 10, 0, 0),
                Paris,
            ),
            (
                secs(2026, 3, 29, 1, 59, 59),
                secs(2026, 3, 29, 0, 59, 59),
                Paris,
            ),
            (
                secs(2026, 3, 29, 3, 0, 0),
                secs(2026, 3, 29, 1, 0, 0),
                Paris,
            ),
            (
                secs(2026, 10, 25, 3, 0, 0),
                secs(2026, 10, 25, 2, 0, 0),
                Paris,
            ),
        ] {
            assert_eq!(local_to_utc_secs(wall, &zone), utc, "{wall}");
        }
        assert_eq!(
            local_to_utc_secs(secs(2026, 7, 15, 12, 0, 0), &fixed(5 * 3600 + 1800)),
            secs(2026, 7, 15, 6, 30, 0)
        );
        assert_eq!(
            local_to_utc_secs(secs(2026, 7, 15, 12, 0, 0), &Utc),
            secs(2026, 7, 15, 12, 0, 0)
        );
    }

    #[test]
    fn a_recorded_offset_beats_the_fallback_zone() {
        let summer = CaptureDate {
            naive_local: secs(2026, 10, 25, 2, 30, 0),
            offset_min: Some(120),
            src: CaptureSource::Exif,
        };
        let winter = CaptureDate {
            offset_min: Some(60),
            ..summer
        };
        let unknown = CaptureDate {
            offset_min: None,
            ..summer
        };
        assert_eq!(summer.utc_secs(&Tokyo), secs(2026, 10, 25, 0, 30, 0));
        assert_eq!(winter.utc_secs(&Tokyo), secs(2026, 10, 25, 1, 30, 0));
        assert_eq!(unknown.utc_secs(&Paris), secs(2026, 10, 25, 0, 30, 0));
        assert_eq!(unknown.utc_secs(&Tokyo), secs(2026, 10, 24, 17, 30, 0));
    }

    #[test]
    fn parses_time_zone_names() {
        for name in [
            "Europe/Paris",
            "europe/paris",
            "EUROPE/PARIS",
            ":Europe/Paris",
            "  Europe/Paris\n",
            "/usr/share/zoneinfo/Europe/Paris",
            "../zoneinfo/Europe/Paris",
        ] {
            assert_eq!(parse_tz(name), Some(Paris), "{name:?}");
        }
        assert_eq!(parse_tz("UTC"), Some(Tz::UTC));
        assert_eq!(parse_tz("utc"), Some(Tz::UTC));
        assert_eq!(parse_tz("America/New_York"), Some(New_York));
        assert_eq!(parse_tz("Etc/GMT-2").map(Tz::name), Some("Etc/GMT-2"));
        for name in [
            "",
            "  ",
            ":",
            "Mars/Olympus",
            "CET-1CEST,M3.5.0,M10.5.0/3",
            "Paris",
        ] {
            assert_eq!(parse_tz(name), None, "{name:?}");
        }
        assert_eq!(tz_or_utc(Some("Asia/Tokyo")), Tokyo);
        assert_eq!(tz_or_utc(Some("nonsense")), Tz::UTC);
        assert_eq!(tz_or_utc(None), Tz::UTC);
    }

    fn arbitrary_text() -> impl Strategy<Value = Option<String>> {
        proptest::option::of(prop_oneof![
            "[0-9:/ TtZz+.,-]{0,32}",
            "\\PC{0,24}",
            (
                1700i32..=2300,
                0u32..=14,
                0u32..=33,
                0u32..=25,
                0u32..=61,
                0u32..=61
            )
                .prop_map(|(y, mo, d, h, mi, s)| format!(
                    "{y:04}:{mo:02}:{d:02} {h:02}:{mi:02}:{s:02}"
                )),
        ])
    }

    proptest! {
        #[test]
        fn parsers_never_panic(raw in "\\PC{0,48}") {
            let _ = parse_datetime(&raw);
            let _ = parse_offset(&raw);
            let _ = parse_tz(&raw);
        }

        #[test]
        fn parsers_never_panic_on_date_like_noise(raw in "[0-9:/ TtZz+.,\\-\0]{0,40}") {
            if let Ok(p) = parse_datetime(&raw) {
                prop_assert!((MIN_YEAR..=MAX_YEAR).contains(&p.naive.year()));
            }
            let _ = parse_offset(&raw);
        }

        #[test]
        fn formatted_dates_parse_back(
            year in MIN_YEAR..=MAX_YEAR,
            month in 1u32..=12,
            day in 1u32..=28,
            hour in 0u32..24,
            minute in 0u32..60,
            second in 0u32..60,
            fraction in proptest::option::of(0u32..1_000_000),
            separator in prop::sample::select(vec![':', '-', '/']),
            joiner in prop::sample::select(vec![" ", "T", "t", "  "]),
            offset in proptest::option::of(-12i32 * 60..=14 * 60),
        ) {
            let mut text = format!(
                "{year:04}{separator}{month:02}{separator}{day:02}{joiner}{hour:02}:{minute:02}:{second:02}"
            );
            if let Some(f) = fraction {
                text.push_str(&format!(".{f:06}"));
            }
            if let Some(minutes) = offset {
                let sign = if minutes < 0 { '-' } else { '+' };
                text.push_str(&format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60));
            }
            let got = parse_datetime(&text)?;
            prop_assert_eq!(got.naive, at(year, month, day, hour, minute, second));
            prop_assert_eq!(got.offset_min, offset);
            prop_assert!(got.has_time);
        }

        #[test]
        fn resolve_is_total_and_stays_in_range(
            texts in prop::collection::vec(arbitrary_text(), 8),
            birth in proptest::option::of(any::<i64>()),
            modified in any::<i64>(),
            zone in 0usize..4,
        ) {
            let get = |i: usize| texts.get(i).and_then(|t| t.as_deref());
            let sources = CaptureInputs {
                sub_sec_date_time_original: get(0),
                date_time_original: get(1),
                offset_time_original: get(2),
                sidecar_date: get(3),
                create_date: get(4),
                quicktime_creation_date: get(5),
                quicktime_create_date: get(6),
                media_create_date: get(7),
                birth_time: birth,
                modified_time: modified,
            };
            let got = match zone {
                0 => resolve_capture(&sources, &Utc),
                1 => resolve_capture(&sources, &Paris),
                2 => resolve_capture(&sources, &Kolkata),
                _ => resolve_capture(&sources, &fixed(-(3 * 3600 + 1800))),
            };
            prop_assert!((MIN_SECS..=MAX_SECS).contains(&got.naive_local));
            prop_assert!(got.naive().is_some());
            let _ = got.utc_secs(&New_York);
            let has_original = [get(0), get(1)]
                .into_iter()
                .flatten()
                .any(|t| parse_datetime(t).is_ok());
            if has_original {
                prop_assert_eq!(got.src, CaptureSource::Exif);
            }
        }

        #[test]
        fn file_times_keep_their_exact_instant(
            instant in 1_000_000_000i64..2_000_000_000,
            zone in 0usize..3,
        ) {
            let sources = only_mtime(instant);
            let got = match zone {
                0 => resolve_capture(&sources, &Utc),
                1 => resolve_capture(&sources, &Paris),
                _ => resolve_capture(&sources, &New_York),
            };
            prop_assert_eq!(got.src, CaptureSource::Fs);
            prop_assert_eq!(got.utc_secs(&Tokyo), instant);
        }

        #[test]
        fn local_to_utc_stays_within_a_zone_offset(wall in 1_000_000_000i64..2_000_000_000) {
            let utc = local_to_utc_secs(wall, &Paris);
            prop_assert!((wall - utc).abs() <= 2 * 3600);
            prop_assert_eq!(local_to_utc_secs(wall, &Utc), wall);
        }
    }
}
