//! Request ids, the per-request tracing span and the one log line per request.
//!
//! An incoming `x-request-id` is kept when it is well formed (1 to 64 characters
//! of `[A-Za-z0-9_-]`, a single header); otherwise a fresh 16-hex-digit id is
//! generated. The id goes into the tracing span (so every log line written while
//! the request is handled carries it), into the request extensions as
//! [`RequestId`], and into the `x-request-id` header of every response,
//! including the JSON error responses (whose body shape does not change).
//!
//! The log line has the method, the path **without the query string**, the
//! status and the latency. Headers, bodies and query strings are never logged.

use std::fmt;
use std::time::Instant;

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// The header carrying the request id, both ways.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

const MAX_ID_LEN: usize = 64;
const MAX_LOGGED_PATH_CHARS: usize = 200;

/// The id of the request being handled; available as a request extension
/// (`Extension<RequestId>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_well_formed(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= MAX_ID_LEN
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn generate() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// The caller's id when it is acceptable, else a new one.
fn id_for(headers: &HeaderMap) -> String {
    let mut values = headers.get_all(&REQUEST_ID_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => match value.to_str() {
            Ok(candidate) if is_well_formed(candidate) => candidate.to_owned(),
            _ => generate(),
        },
        _ => generate(),
    }
}

fn truncated(path: &str) -> String {
    path.chars().take(MAX_LOGGED_PATH_CHARS).collect()
}

/// Middleware: request id, span, response header and the log line.
pub(super) async fn middleware(mut req: Request, next: Next) -> Response {
    let id = id_for(req.headers());
    let method = req.method().clone();
    let path = truncated(req.uri().path());
    let span = tracing::info_span!("request", id = %id, method = %method, path = %path);

    req.extensions_mut().insert(RequestId(id.clone()));
    let started = Instant::now();
    let mut response = next.run(req).instrument(span.clone()).await;

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    span.in_scope(|| log_finished(&method, response.status(), latency_ms));

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Server errors are errors, refusals and writes are worth a line at `info`,
/// and routine successful reads (thumbnails by the hundred) stay at `debug`.
fn log_finished(method: &Method, status: StatusCode, latency_ms: u64) {
    let status = status.as_u16();
    if status >= 500 {
        tracing::error!(status, latency_ms, "request finished");
    } else if status >= 400 || !(method == Method::GET || method == Method::HEAD) {
        tracing::info!(status, latency_ms, "request finished");
    } else {
        tracing::debug!(status, latency_ms, "request finished");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(REQUEST_ID_HEADER, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn well_formed_ids_are_kept() {
        for id in ["abc", "A-b_9", "0123456789abcdef", &"x".repeat(64)] {
            assert_eq!(id_for(&headers(&[id])), id);
        }
    }

    #[test]
    fn malformed_ids_are_replaced() {
        let too_long = "x".repeat(65);
        for id in [
            "",
            "has space",
            "semi;colon",
            "new\u{e9}",
            "a/b",
            "a.b",
            "<x>",
            &too_long,
        ] {
            let replaced = id_for(&headers(&[id]));
            assert_ne!(replaced, id);
            assert_eq!(replaced.len(), 16, "{replaced}");
            assert!(
                replaced.bytes().all(|b| b.is_ascii_hexdigit()),
                "{replaced}"
            );
        }
    }

    #[test]
    fn a_missing_or_repeated_header_yields_a_fresh_id() {
        let fresh = id_for(&HeaderMap::new());
        assert_eq!(fresh.len(), 16);
        let repeated = id_for(&headers(&["abc", "def"]));
        assert!(repeated != "abc" && repeated != "def");
    }

    #[test]
    fn generated_ids_differ() {
        let ids: std::collections::HashSet<String> = (0..64).map(|_| generate()).collect();
        assert_eq!(ids.len(), 64);
    }

    #[test]
    fn logged_paths_are_truncated() {
        assert_eq!(truncated("/api/health"), "/api/health");
        assert_eq!(truncated(&"a".repeat(500)).len(), MAX_LOGGED_PATH_CHARS);
    }
}
