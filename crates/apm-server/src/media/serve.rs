//! Streaming of original files: `Range`, conditional requests, content type and the headers that
//! make untrusted content safe to hand to a browser.
//!
//! The caller opens the file through `VolumeRoot::open_read` (a regular file, no symlinks); this
//! module only ever reads from that open handle, with a bounded buffer, so a multi-GB video is
//! never held in memory. `ETag` (weak) and `Last-Modified` come from `fstat` of the OPEN file.
//!
//! * `Range`: one range (`a-b`, `a-`, `-n`) answers 206 with `Content-Range`; an unsatisfiable one
//!   416 with `Content-Range: bytes */len`; several ranges, another unit or a malformed value
//!   are ignored (full 200), as RFC 9110 allows. `If-Range` takes a date (an entity tag never
//!   matches: the `ETag` is weak).
//! * `If-None-Match` and `If-Modified-Since` answer 304; `HEAD` has the headers of the GET.
//! * Every answer carries `X-Content-Type-Options: nosniff` and `Content-Security-Policy:
//!   sandbox`; active types (HTML, SVG, XML, scripts, CSS) and `?download=1` are attachments.
//! * The file name is sent RFC 6266 style: `filename="..."` (ASCII fallback) and
//!   `filename*=UTF-8''...`, never a raw byte that could split a header.

use std::fs::File;
use std::io::SeekFrom;
use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE,
    LAST_MODIFIED, RANGE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::Response;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

/// Read buffer of a streamed body.
const CHUNK: usize = 64 * 1024;
/// The longest file name sent in a header (each of the two forms).
const MAX_NAME_BYTES: usize = 180;
const X_CONTENT_TYPE: HeaderName = X_CONTENT_TYPE_OPTIONS;

/// What to serve and how it is named.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions<'a> {
    /// The file name, as stored.
    pub name: &'a str,
    /// `files.mime`, when the extractor found one.
    pub mime: Option<&'a str>,
    /// Force `Content-Disposition: attachment` (`?download=1`).
    pub download: bool,
}

/// The result of reading a `Range` header against a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No usable range: send everything.
    Full,
    /// Bytes `start..=end`.
    Partial(u64, u64),
    /// The range starts past the end (or asks for the last 0 bytes).
    Unsatisfiable,
}

/// Parses a `Range` header value for a file of `len` bytes.
pub fn parse_range(value: &str, len: u64) -> RangeOutcome {
    let value = value.trim();
    let Some(spec) = value
        .split_once('=')
        .filter(|(unit, _)| unit.trim().eq_ignore_ascii_case("bytes"))
        .map(|(_, spec)| spec.trim())
    else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };
    let number = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u64>().ok())
            .flatten()
    };
    if first.is_empty() {
        let Some(count) = number(last) else {
            return RangeOutcome::Full;
        };
        if count == 0 || len == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        return RangeOutcome::Partial(len.saturating_sub(count), len - 1);
    }
    let Some(start) = number(first) else {
        return RangeOutcome::Full;
    };
    let end = if last.is_empty() {
        u64::MAX
    } else if let Some(end) = number(last) {
        end
    } else {
        return RangeOutcome::Full;
    };
    if end < start {
        return RangeOutcome::Full;
    }
    if start >= len {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Partial(start, end.min(len - 1))
}

/// The weak entity tag of an open file.
fn weak_etag(len: u64, seconds: i64, nanos: i64) -> String {
    format!("W/\"{len:x}-{seconds:x}-{nanos:x}\"")
}

/// Does an `If-None-Match` value match `etag` (weak comparison)?
fn none_match(header: &str, etag: &str) -> bool {
    let opaque = |tag: &str| tag.trim().trim_start_matches("W/").to_owned();
    let ours = opaque(etag);
    header
        .split(',')
        .any(|tag| tag.trim() == "*" || opaque(tag) == ours)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn secs_to_time(seconds: i64) -> SystemTime {
    match u64::try_from(seconds) {
        Ok(seconds) => UNIX_EPOCH + Duration::from_secs(seconds),
        Err(_) => UNIX_EPOCH,
    }
}

/// Types a browser would run or render with authority if they were shown inline.
fn is_active(mime: &str) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.ends_with("+xml")
        || matches!(
            essence.as_str(),
            "text/html"
                | "application/xhtml+xml"
                | "image/svg+xml"
                | "text/xml"
                | "application/xml"
                | "text/javascript"
                | "application/javascript"
                | "application/x-javascript"
                | "text/css"
                | "text/ecmascript"
                | "application/ecmascript"
        )
}

/// The content type to send: the indexed one when it is a well-formed media type, else a guess
/// from the name, else `application/octet-stream`.
pub fn content_type(mime: Option<&str>, name: &str) -> String {
    if let Some(parsed) = mime.and_then(|text| text.trim().parse::<mime::Mime>().ok()) {
        return parsed.to_string();
    }
    mime_guess::from_path(name).first().map_or_else(
        || "application/octet-stream".to_owned(),
        |guess| guess.to_string(),
    )
}

/// `inline` or `attachment` with the RFC 6266 file name.
pub fn content_disposition(name: &str, attachment: bool) -> String {
    let kind = if attachment { "attachment" } else { "inline" };
    let mut fallback = String::new();
    for ch in truncate(name) {
        fallback.push(if ch.is_ascii_graphic() || ch == ' ' {
            if matches!(ch, '"' | '\\' | '%') {
                '_'
            } else {
                ch
            }
        } else {
            '_'
        });
    }
    let mut encoded = String::new();
    for ch in truncate(name) {
        let mut buffer = [0_u8; 4];
        for byte in ch.encode_utf8(&mut buffer).bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// The characters of `name` that fit in [`MAX_NAME_BYTES`] bytes.
fn truncate(name: &str) -> impl Iterator<Item = char> + '_ {
    let mut used = 0;
    name.chars().take_while(move |ch| {
        used += ch.len_utf8();
        used <= MAX_NAME_BYTES
    })
}

fn base_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
}

fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    base_headers(&mut response);
    response
}

fn insert(response: &mut Response, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, value);
    }
}

/// Answers a request for the open file `file`.
pub async fn serve(
    file: File,
    method: &Method,
    request: &HeaderMap,
    options: ServeOptions<'_>,
) -> Response {
    let Ok(meta) = file.metadata() else {
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let len = meta.len();
    let etag = weak_etag(len, meta.mtime(), meta.mtime_nsec());
    let last_modified = httpdate::fmt_http_date(secs_to_time(meta.mtime()));

    let common = |response: &mut Response| {
        insert(response, ETAG, &etag);
        insert(response, LAST_MODIFIED, &last_modified);
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    };

    let unchanged = match header_str(request, &IF_NONE_MATCH) {
        Some(tags) => none_match(tags, &etag),
        None => header_str(request, &IF_MODIFIED_SINCE)
            .and_then(|text| httpdate::parse_http_date(text).ok())
            .is_some_and(|since| secs_to_time(meta.mtime()) <= since),
    };
    if unchanged {
        let mut response = empty(StatusCode::NOT_MODIFIED);
        common(&mut response);
        return response;
    }

    let mut outcome =
        header_str(request, &RANGE).map_or(RangeOutcome::Full, |value| parse_range(value, len));
    if let Some(condition) = header_str(request, &IF_RANGE) {
        let matches = httpdate::parse_http_date(condition)
            .is_ok_and(|date| date == secs_to_time(meta.mtime()));
        if !matches {
            outcome = RangeOutcome::Full;
        }
    }

    if outcome == RangeOutcome::Unsatisfiable {
        let mut response = empty(StatusCode::RANGE_NOT_SATISFIABLE);
        insert(&mut response, CONTENT_RANGE, &format!("bytes */{len}"));
        common(&mut response);
        return response;
    }

    let (status, start, count) = match outcome {
        RangeOutcome::Partial(start, end) => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        _ => (StatusCode::OK, 0, len),
    };
    let body = if *method == Method::HEAD || count == 0 {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(file);
        if start > 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
            return empty(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Body::from_stream(ReaderStream::with_capacity(file.take(count), CHUNK))
    };

    let mime = content_type(options.mime, options.name);
    let attachment = options.download || is_active(&mime);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    base_headers(&mut response);
    common(&mut response);
    insert(&mut response, CONTENT_TYPE, &mime);
    insert(&mut response, ACCEPT_RANGES, "bytes");
    insert(&mut response, CONTENT_LENGTH, &count.to_string());
    insert(
        &mut response,
        CONTENT_DISPOSITION,
        &content_disposition(options.name, attachment),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        insert(
            &mut response,
            CONTENT_RANGE,
            &format!("bytes {start}-{}/{len}", start + count - 1),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_rfc_9110() {
        use RangeOutcome::{Full, Partial, Unsatisfiable};
        assert_eq!(parse_range("bytes=0-99", 1000), Partial(0, 99));
        assert_eq!(parse_range("bytes=900-", 1000), Partial(900, 999));
        assert_eq!(parse_range("bytes=-100", 1000), Partial(900, 999));
        assert_eq!(parse_range("bytes=-5000", 1000), Partial(0, 999));
        assert_eq!(parse_range("bytes=990-5000", 1000), Partial(990, 999));
        assert_eq!(parse_range("bytes=1000-", 1000), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 1000), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-1,5-9", 1000), Full);
        assert_eq!(parse_range("bytes=9-5", 1000), Full);
        assert_eq!(parse_range("bytes=+5-9", 1000), Full);
        assert_eq!(parse_range("items=0-9", 1000), Full);
        assert_eq!(parse_range("bytes=abc", 1000), Full);
        assert_eq!(parse_range("BYTES=0-0", 1000), Partial(0, 0));
    }

    #[test]
    fn a_hostile_name_cannot_split_a_header() {
        let value = content_disposition("a\"b\r\nX-Evil: 1\\é.jpg", false);
        assert!(!value.contains('\r') && !value.contains('\n'));
        assert!(value.starts_with("inline; filename=\"a_b__X-Evil: 1__.jpg\""));
        assert!(value.ends_with("filename*=UTF-8''a%22b%0D%0AX-Evil%3A%201%5C%C3%A9.jpg"));
        assert!(HeaderValue::from_str(&value).is_ok());
    }

    #[test]
    fn a_long_name_is_cut() {
        let value = content_disposition(&"é".repeat(5000), true);
        assert!(value.len() < 1500);
        assert!(value.starts_with("attachment;"));
    }

    #[test]
    fn active_types_are_recognised() {
        for mime in [
            "text/html",
            "image/svg+xml",
            "application/xhtml+xml",
            "text/xml; charset=x",
            "text/css",
            "application/javascript",
            "application/atom+xml",
        ] {
            assert!(is_active(mime), "{mime}");
        }
        for mime in ["image/jpeg", "video/mp4", "text/plain", "application/pdf"] {
            assert!(!is_active(mime), "{mime}");
        }
    }

    #[test]
    fn the_content_type_falls_back_to_the_name() {
        assert_eq!(content_type(Some("image/heic"), "a.heic"), "image/heic");
        assert_eq!(content_type(None, "a.png"), "image/png");
        assert_eq!(content_type(Some("not a type"), "a.png"), "image/png");
        assert_eq!(content_type(None, "noext"), "application/octet-stream");
    }

    #[test]
    fn if_none_match_uses_the_weak_comparison() {
        let etag = weak_etag(10, 20, 30);
        assert!(none_match(&etag, &etag));
        assert!(none_match("\"a\", W/\"a\"x, *", &etag));
        assert!(none_match(&etag.replace("W/", ""), &etag));
        assert!(!none_match("W/\"zzz\"", &etag));
    }
}
