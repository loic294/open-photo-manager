//! Request body limits and timeouts, with one per-route table.
//!
//! Every request gets the defaults of [`SecurityOptions`] (1 MiB body, 30 s to
//! produce the response headers). The routes that need something else are
//! listed in [`default_policies`], the single place to look at and to edit:
//! long-lived routes opt out of the timeout, and a route that legitimately
//! receives more than 1 MiB opts into a larger body.
//!
//! # How the limits are enforced
//!
//! - A declared `Content-Length` above the limit is refused with `413` before
//!   the handler runs. Otherwise the body is wrapped so that reading past the
//!   limit fails; if that happens the response becomes the same `413`, whatever
//!   the handler made of the read error.
//! - The default body limit of axum's extractors (2 MiB) is switched off in
//!   favour of this one, so a larger per-route limit really applies.
//! - The timeout covers the time until the response *headers* are ready, so an
//!   event stream or a large download is never cut once it started. It also
//!   covers reading the request body: a route that receives large uploads must
//!   opt out of the timeout.
//!
//! # Patterns
//!
//! A pattern is a path such as `/api/files/{id}/thumb`: segments are compared
//! literally, `{name}` matches exactly one non-empty segment, and nothing else
//! is a wildcard. The raw request path is matched, exactly as the router matches
//! it, so a policy can never apply to a path the router sends somewhere else.
//! The first matching entry wins.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, LengthLimitError, Limited};

use super::SecurityOptions;
use crate::error::AppError;

/// Request body limit when no policy says otherwise: 1 MiB.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Time allowed to produce the response headers when no policy says otherwise.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Body limit of the routes that receive large id sets.
const LARGE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// How a policy changes the timeout of its routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTimeout {
    /// The server-wide default.
    Default,
    /// No timeout: event streams, downloads, uploads.
    Disabled,
    /// A specific timeout.
    After(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Any,
}

/// The limits of the routes matching one pattern, see the module
/// documentation. Build with [`RoutePolicy::new`] and the `with_*` methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePolicy {
    segments: Vec<Segment>,
    max_body_bytes: Option<usize>,
    timeout: RouteTimeout,
}

impl RoutePolicy {
    /// A policy for `pattern` that changes nothing yet.
    pub fn new(pattern: &str) -> Self {
        let segments = pattern
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') && segment.ends_with('}') {
                    Segment::Any
                } else {
                    Segment::Literal(segment.to_owned())
                }
            })
            .collect();
        Self {
            segments,
            max_body_bytes: None,
            timeout: RouteTimeout::Default,
        }
    }

    /// Never time these routes out.
    #[must_use]
    pub fn without_timeout(mut self) -> Self {
        self.timeout = RouteTimeout::Disabled;
        self
    }

    /// Time these routes out after `timeout` instead of the default.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = RouteTimeout::After(timeout);
        self
    }

    /// Accept request bodies up to `bytes` on these routes.
    #[must_use]
    pub fn with_max_body(mut self, bytes: usize) -> Self {
        self.max_body_bytes = Some(bytes);
        self
    }

    /// Whether the policy applies to the (raw, still percent-encoded) `path`.
    pub fn matches(&self, path: &str) -> bool {
        let mut parts = path.split('/');
        for segment in &self.segments {
            match (segment, parts.next()) {
                (Segment::Literal(expected), Some(actual)) if expected == actual => {}
                (Segment::Any, Some(actual)) if !actual.is_empty() => {}
                _ => return false,
            }
        }
        parts.next().is_none()
    }
}

/// The routes with limits of their own. Add an entry here when a new route
/// streams, downloads or uploads (no timeout) or receives a large body.
pub fn default_policies() -> Vec<RoutePolicy> {
    vec![
        // Server-sent events stay open for as long as the page is.
        RoutePolicy::new("/api/events").without_timeout(),
        // Media is streamed from disk and can be slow on a busy NAS.
        RoutePolicy::new("/api/files/{id}/thumb").without_timeout(),
        RoutePolicy::new("/api/files/{id}/preview").without_timeout(),
        RoutePolicy::new("/api/files/{id}/content").without_timeout(),
        // "Select all" in a large library posts every id.
        RoutePolicy::new("/api/selections").with_max_body(LARGE_BODY_BYTES),
    ]
}

/// The limits that apply to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_body_bytes: usize,
    /// `None`: no timeout.
    pub timeout: Option<Duration>,
}

impl SecurityOptions {
    /// The limits for a request to `path` (raw, without the query string).
    pub fn limits_for(&self, path: &str) -> Limits {
        let policy = self.policies.iter().find(|policy| policy.matches(path));
        let max_body_bytes = policy
            .and_then(|policy| policy.max_body_bytes)
            .unwrap_or(self.max_body_bytes);
        let timeout = match policy.map_or(RouteTimeout::Default, |policy| policy.timeout) {
            RouteTimeout::Default => Some(self.request_timeout),
            RouteTimeout::Disabled => None,
            RouteTimeout::After(timeout) => Some(timeout),
        };
        Limits {
            max_body_bytes,
            timeout,
        }
    }
}

fn payload_too_large(limit: usize) -> Response {
    AppError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        format!("the request body is larger than the limit of {limit} bytes"),
    )
    .into_response()
}

fn timed_out() -> Response {
    AppError::new(
        StatusCode::REQUEST_TIMEOUT,
        "request_timeout",
        "the request took too long and was cancelled",
    )
    .into_response()
}

/// The `Content-Length` when exactly one well-formed header is present.
fn declared_length(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value.to_str().ok()?.parse().ok(),
        _ => None,
    }
}

/// Wraps the body so that reading past `max` fails and raises `tripped`.
fn limit_body(req: Request, max: usize, tripped: Arc<AtomicBool>) -> Request {
    let (parts, body) = req.into_parts();
    let body = Limited::new(body, max).map_err(move |err| {
        if err.is::<LengthLimitError>() {
            tripped.store(true, Ordering::Relaxed);
        }
        err
    });
    Request::from_parts(parts, Body::new(body))
}

/// Middleware applying the limits of the matching [`RoutePolicy`].
pub(super) async fn enforce(
    State(options): State<Arc<SecurityOptions>>,
    req: Request,
    next: Next,
) -> Response {
    let limits = options.limits_for(req.uri().path());
    let max = limits.max_body_bytes;

    let max_declared = u64::try_from(max).unwrap_or(u64::MAX);
    if declared_length(req.headers()).is_some_and(|declared| declared > max_declared) {
        return payload_too_large(max);
    }

    let tripped = Arc::new(AtomicBool::new(false));
    let req = limit_body(req, max, tripped.clone());

    let response = match limits.timeout {
        None => next.run(req).await,
        Some(limit) => match tokio::time::timeout(limit, next.run(req)).await {
            Ok(response) => response,
            Err(_elapsed) => return timed_out(),
        },
    };

    if tripped.load(Ordering::Relaxed) {
        return payload_too_large(max);
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn options() -> SecurityOptions {
        SecurityOptions::default()
    }

    #[test]
    fn patterns_match_whole_paths_segment_by_segment() {
        let policy = RoutePolicy::new("/api/files/{id}/thumb");
        assert!(policy.matches("/api/files/1/thumb"));
        assert!(policy.matches("/api/files/abc-9/thumb"));
        for path in [
            "/api/files/thumb",
            "/api/files//thumb",
            "/api/files/1/thumb/",
            "/api/files/1/thumb/x",
            "/api/files/1/2/thumb",
            "/api/files/1/preview",
            "/api/file/1/thumb",
            "/API/files/1/thumb",
            "api/files/1/thumb",
            "/x/api/files/1/thumb",
            "",
            "/",
        ] {
            assert!(!policy.matches(path), "{path}");
        }
    }

    #[test]
    fn a_literal_pattern_matches_only_that_path() {
        let policy = RoutePolicy::new("/api/events");
        assert!(policy.matches("/api/events"));
        for path in [
            "/api/events/",
            "/api/events/1",
            "/api",
            "/api/event",
            "/api/Events",
        ] {
            assert!(!policy.matches(path), "{path}");
        }
    }

    #[test]
    fn an_encoded_slash_cannot_smuggle_extra_segments_into_a_wildcard() {
        let policy = RoutePolicy::new("/api/files/{id}/thumb");
        // The router matches the raw path too, so this is one segment, `{id}`.
        assert!(policy.matches("/api/files/1%2Fx/thumb"));
        assert!(!policy.matches("/api/files/1/x/thumb"));
    }

    #[test]
    fn defaults_apply_without_a_matching_policy() {
        let limits = options().limits_for("/api/health");
        assert_eq!(limits.max_body_bytes, DEFAULT_MAX_BODY_BYTES);
        assert_eq!(limits.timeout, Some(DEFAULT_REQUEST_TIMEOUT));
        assert_eq!(DEFAULT_MAX_BODY_BYTES, 1024 * 1024);
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn long_lived_routes_have_no_timeout() {
        for path in [
            "/api/events",
            "/api/files/12/thumb",
            "/api/files/12/preview",
            "/api/files/12/content",
        ] {
            let limits = options().limits_for(path);
            assert_eq!(limits.timeout, None, "{path}");
            assert_eq!(limits.max_body_bytes, DEFAULT_MAX_BODY_BYTES, "{path}");
        }
        // The metadata route of the same family keeps its timeout.
        assert!(options().limits_for("/api/files/12").timeout.is_some());
    }

    #[test]
    fn selections_may_post_a_large_id_set() {
        let limits = options().limits_for("/api/selections");
        assert!(limits.max_body_bytes > DEFAULT_MAX_BODY_BYTES);
        assert_eq!(limits.timeout, Some(DEFAULT_REQUEST_TIMEOUT));
        assert_eq!(
            options().limits_for("/api/selections/7").max_body_bytes,
            DEFAULT_MAX_BODY_BYTES
        );
    }

    #[test]
    fn the_first_matching_policy_wins_and_options_are_respected() {
        let mut options = options();
        options.max_body_bytes = 10;
        options.request_timeout = Duration::from_secs(2);
        options.policies = vec![
            RoutePolicy::new("/a/{x}").with_timeout(Duration::from_secs(5)),
            RoutePolicy::new("/a/b").with_max_body(99).without_timeout(),
            RoutePolicy::new("/c").with_max_body(99),
        ];
        let a = options.limits_for("/a/b");
        assert_eq!(a.max_body_bytes, 10);
        assert_eq!(a.timeout, Some(Duration::from_secs(5)));
        let c = options.limits_for("/c");
        assert_eq!(c.max_body_bytes, 99);
        assert_eq!(c.timeout, Some(Duration::from_secs(2)));
        let other = options.limits_for("/other");
        assert_eq!(other.max_body_bytes, 10);
        assert_eq!(other.timeout, Some(Duration::from_secs(2)));
    }

    #[test]
    fn declared_lengths_are_read_only_when_unambiguous() {
        let mut headers = HeaderMap::new();
        assert_eq!(declared_length(&headers), None);
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("42"));
        assert_eq!(declared_length(&headers), Some(42));
        headers.append(CONTENT_LENGTH, HeaderValue::from_static("43"));
        assert_eq!(declared_length(&headers), None, "duplicated");
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("-1"));
        assert_eq!(declared_length(&headers), None);
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("abc"));
        assert_eq!(declared_length(&headers), None);
    }
}
