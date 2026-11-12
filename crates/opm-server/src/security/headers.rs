//! Security response headers, and the default `Cache-Control` of the API.
//!
//! Every response gets the headers below unless the handler already set that
//! header itself (the media endpoints send their own
//! `Content-Security-Policy: sandbox`). The layer sits outside the Host, CSRF,
//! limit and panic layers, so their error responses carry the headers too.
//!
//! The policy matches the web build, which has no inline script, no inline
//! `<style>` element (only `style=""` attributes, hence `style-src-attr`) and
//! no remote assets. There is no HSTS: on a LAN the app is typically plain
//! HTTP.

use axum::extract::Request;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, HeaderName, HeaderValue, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use super::is_api_path;

/// The Content-Security-Policy of the app's own pages.
pub const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self'; style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; \
    media-src 'self' blob:; connect-src 'self'; font-src 'self'; object-src 'none'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

const PERMISSIONS_POLICY_VALUE: &str = "camera=(), microphone=(), geolocation=(), payment=()";

const CROSS_ORIGIN_OPENER_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-opener-policy");
const CROSS_ORIGIN_RESOURCE_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");

fn defaults() -> [(HeaderName, HeaderValue); 7] {
    [
        (
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
        ),
        (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (REFERRER_POLICY, HeaderValue::from_static("no-referrer")),
        (X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (
            CROSS_ORIGIN_OPENER_POLICY,
            HeaderValue::from_static("same-origin"),
        ),
        (
            CROSS_ORIGIN_RESOURCE_POLICY,
            HeaderValue::from_static("same-origin"),
        ),
        (
            PERMISSIONS_POLICY,
            HeaderValue::from_static(PERMISSIONS_POLICY_VALUE),
        ),
    ]
}

fn apply(headers: &mut HeaderMap, status: StatusCode, api: bool) {
    for (name, value) in defaults() {
        headers.entry(name).or_insert(value);
    }
    // A 304 repeats the caching headers of the 200 it stands for.
    if api && status != StatusCode::NOT_MODIFIED {
        headers
            .entry(CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("no-store"));
    }
}

/// Middleware adding the security headers (and `Cache-Control: no-store` to
/// `/api/*` responses that do not choose their own caching).
pub(super) async fn middleware(req: Request, next: Next) -> Response {
    let api = is_api_path(req.uri().path());
    let mut response = next.run(req).await;
    let status = response.status();
    apply(response.headers_mut(), status, api);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_headers_are_added() {
        let mut headers = HeaderMap::new();
        apply(&mut headers, StatusCode::OK, false);
        assert_eq!(
            headers[CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY_VALUE
        );
        assert_eq!(headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[REFERRER_POLICY], "no-referrer");
        assert_eq!(headers[X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
        assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
        assert_eq!(
            headers["permissions-policy"],
            "camera=(), microphone=(), geolocation=(), payment=()"
        );
        assert!(
            !headers.contains_key(CACHE_CONTROL),
            "only /api gets no-store"
        );
        assert!(
            !headers.contains_key("strict-transport-security"),
            "no HSTS on a plain-HTTP LAN"
        );
    }

    #[test]
    fn the_policy_is_exactly_the_documented_one() {
        assert_eq!(
            CONTENT_SECURITY_POLICY_VALUE,
            "default-src 'self'; script-src 'self'; style-src 'self'; \
             style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; \
             media-src 'self' blob:; connect-src 'self'; font-src 'self'; \
             object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
        );
    }

    #[test]
    fn handler_set_headers_win() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=60"),
        );
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
        apply(&mut headers, StatusCode::OK, true);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], "sandbox");
        assert_eq!(headers[CACHE_CONTROL], "private, max-age=60");
        assert_eq!(headers[X_FRAME_OPTIONS], "SAMEORIGIN");
        assert_eq!(headers.get_all(CONTENT_SECURITY_POLICY).iter().count(), 1);
        assert_eq!(
            headers[X_CONTENT_TYPE_OPTIONS], "nosniff",
            "others still added"
        );
    }

    #[test]
    fn api_responses_default_to_no_store_except_not_modified() {
        let mut headers = HeaderMap::new();
        apply(&mut headers, StatusCode::OK, true);
        assert_eq!(headers[CACHE_CONTROL], "no-store");

        let mut headers = HeaderMap::new();
        apply(&mut headers, StatusCode::NOT_MODIFIED, true);
        assert!(!headers.contains_key(CACHE_CONTROL));

        let mut headers = HeaderMap::new();
        apply(&mut headers, StatusCode::NOT_FOUND, true);
        assert_eq!(headers[CACHE_CONTROL], "no-store");
    }
}
