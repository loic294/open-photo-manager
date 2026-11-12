//! CSRF protection for every request that is not a plain read.
//!
//! There is no authentication, so a hostile web page open in the user's browser
//! must not be able to drive the API. A request with any method other than
//! `GET`, `HEAD` or `OPTIONS` is served only when
//!
//! 1. it carries exactly one `x-opm-request: 1` header (a custom header forces a
//!    CORS preflight, which the server never answers favourably: there is no
//!    CORS support at all), **and**
//! 2. if it has an `Origin` header, that origin is this server (same host and
//!    effective port as the validated `Host`; `Origin: null` is refused), **and**
//! 3. if it has a `Sec-Fetch-Site` header, the value is `same-origin` or `none`.
//!
//! A request without `Origin` (curl, scripts, other servers) only needs (1).
//!
//! # Rules for handlers
//!
//! `GET` and `HEAD` are exempt, as is the server-sent-events stream
//! (`GET /api/events`), so **a `GET` handler must never change state**: not on
//! the server, not in the database, not on disk. Everything that writes, moves,
//! deletes or starts a task is `POST`/`PUT`/`PATCH`/`DELETE`. `OPTIONS` is not
//! handled (the router answers 405) and no `Access-Control-*` header is ever
//! sent.

use axum::extract::Request;
use axum::http::header::{HeaderName, ORIGIN};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::host::HostAuthority;
use crate::error::AppError;

/// The header every state-changing request must carry, with the value `1`.
pub const CSRF_HEADER: HeaderName = HeaderName::from_static("x-opm-request");

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

const MESSAGE: &str = "state-changing requests must come from this app: send \
    `x-opm-request: 1` and a same-origin Origin (behind a reverse proxy, forward the original \
    Host header)";

/// Which check refused a request (logged, never sent to the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    MissingHeader,
    BadOrigin,
    OriginMismatch,
    CrossSite,
    NoHost,
}

impl Failure {
    fn as_str(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing or invalid x-opm-request header",
            Self::BadOrigin => "malformed or opaque Origin",
            Self::OriginMismatch => "Origin differs from Host",
            Self::CrossSite => "Sec-Fetch-Site is not same-origin",
            Self::NoHost => "no validated Host",
        }
    }
}

fn is_read_only(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD || method == Method::OPTIONS
}

fn has_marker_header(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CSRF_HEADER).iter();
    matches!(
        (values.next(), values.next()),
        (Some(value), None) if value.as_bytes() == b"1"
    )
}

/// The scheme's default port and the authority of an `Origin` value.
fn split_origin(origin: &str) -> Option<(u16, &str)> {
    let (scheme, authority) = origin.split_once("://")?;
    if scheme.eq_ignore_ascii_case("http") {
        Some((80, authority))
    } else if scheme.eq_ignore_ascii_case("https") {
        Some((443, authority))
    } else {
        None
    }
}

fn origin_matches_host(headers: &HeaderMap, host: Option<&HostAuthority>) -> Result<(), Failure> {
    let mut origins = headers.get_all(ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return Ok(());
    };
    if origins.next().is_some() {
        return Err(Failure::BadOrigin);
    }
    let origin = origin.to_str().map_err(|_| Failure::BadOrigin)?;
    let (default_port, authority) = split_origin(origin).ok_or(Failure::BadOrigin)?;
    let origin = authority
        .parse::<HostAuthority>()
        .map_err(|_| Failure::BadOrigin)?;
    let host = host.ok_or(Failure::NoHost)?;
    if origin.same_origin(host, default_port) {
        Ok(())
    } else {
        Err(Failure::OriginMismatch)
    }
}

fn fetch_site_is_ours(headers: &HeaderMap) -> Result<(), Failure> {
    let mut values = headers.get_all(SEC_FETCH_SITE).iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(()),
        (Some(value), None) if matches!(value.as_bytes(), b"same-origin" | b"none") => Ok(()),
        _ => Err(Failure::CrossSite),
    }
}

/// The three checks, on their own for testing. `host` is the validated `Host`
/// of the request.
pub(super) fn check(
    method: &Method,
    headers: &HeaderMap,
    host: Option<&HostAuthority>,
) -> Result<(), Failure> {
    if is_read_only(method) {
        return Ok(());
    }
    if !has_marker_header(headers) {
        return Err(Failure::MissingHeader);
    }
    origin_matches_host(headers, host)?;
    fetch_site_is_ours(headers)
}

/// Middleware: runs after the Host check, whose validated [`HostAuthority`] is
/// in the request extensions.
pub(super) async fn guard(req: Request, next: Next) -> Response {
    let host = req.extensions().get::<HostAuthority>();
    match check(req.method(), req.headers(), host) {
        Ok(()) => next.run(req).await,
        Err(failure) => {
            tracing::warn!(reason = failure.as_str(), "request refused");
            AppError::forbidden("csrf_rejected", MESSAGE).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn host(raw: &str) -> HostAuthority {
        raw.parse().unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn post(pairs: &[(&str, &str)], host_header: &str) -> Result<(), Failure> {
        check(&Method::POST, &headers(pairs), Some(&host(host_header)))
    }

    #[test]
    fn reads_are_exempt_whatever_the_headers() {
        let hostile = headers(&[
            ("origin", "http://evil.example"),
            ("sec-fetch-site", "cross-site"),
        ]);
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(check(&method, &hostile, Some(&host("localhost"))), Ok(()));
            assert_eq!(check(&method, &hostile, None), Ok(()));
        }
    }

    #[test]
    fn every_other_method_is_checked() {
        let none = HeaderMap::new();
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::TRACE,
            Method::CONNECT,
            Method::from_bytes(b"PROPFIND").unwrap(),
        ] {
            assert_eq!(
                check(&method, &none, Some(&host("localhost"))),
                Err(Failure::MissingHeader),
                "{method}"
            );
        }
    }

    #[test]
    fn the_marker_header_must_be_exactly_one_with_the_value_1() {
        assert_eq!(post(&[("x-opm-request", "1")], "localhost"), Ok(()));
        for bad in ["0", "true", "", "11", "1 ", " 1", "one"] {
            assert_eq!(
                post(&[("x-opm-request", bad)], "localhost"),
                Err(Failure::MissingHeader),
                "{bad:?}"
            );
        }
        assert_eq!(
            post(
                &[("x-opm-request", "1"), ("x-opm-request", "1")],
                "localhost"
            ),
            Err(Failure::MissingHeader),
            "duplicated"
        );
        assert_eq!(post(&[], "localhost"), Err(Failure::MissingHeader));
    }

    #[test]
    fn a_missing_origin_is_fine_for_scripts() {
        assert_eq!(post(&[("x-opm-request", "1")], "nas.local:8080"), Ok(()));
    }

    #[test]
    fn a_same_origin_is_accepted() {
        let cases = [
            ("http://localhost:3000", "localhost:3000"),
            ("http://LOCALHOST:3000", "localhost:3000"),
            ("http://nas.local", "nas.local"),
            ("http://nas.local:80", "nas.local"),
            ("http://nas.local", "nas.local:80"),
            ("https://photos.example.com", "photos.example.com"),
            ("https://photos.example.com:443", "photos.example.com"),
            ("https://photos.example.com", "photos.example.com:443"),
            ("http://192.168.1.5:8080", "192.168.1.5:8080"),
            ("http://[::1]:8080", "[::1]:8080"),
            ("http://nas.local.", "nas.local"),
        ];
        for (origin, host_header) in cases {
            assert_eq!(
                post(&[("x-opm-request", "1"), ("origin", origin)], host_header),
                Ok(()),
                "{origin} vs {host_header}"
            );
        }
    }

    #[test]
    fn another_origin_is_refused() {
        let cases = [
            ("http://evil.example", "localhost:3000"),
            ("http://localhost:3001", "localhost:3000"),
            ("http://localhost", "localhost:3000"),
            ("http://localhost:3000", "localhost"),
            ("https://nas.local", "nas.local:80"),
            ("http://nas.local", "nas.local:443"),
            ("http://nas.local.evil.example", "nas.local"),
            ("http://evil.example", "evil.example:3000"),
            ("http://127.0.0.1:8080", "localhost:8080"),
        ];
        for (origin, host_header) in cases {
            assert_eq!(
                post(&[("x-opm-request", "1"), ("origin", origin)], host_header),
                Err(Failure::OriginMismatch),
                "{origin} vs {host_header}"
            );
        }
    }

    #[test]
    fn opaque_or_malformed_origins_are_refused() {
        for origin in [
            "null",
            "",
            "localhost",
            "//localhost",
            "ftp://localhost",
            "file://localhost",
            "http://",
            "http://localhost/",
            "http://localhost/path",
            "http://localhost?x=1",
            "http://user@localhost",
            "http://user:pw@localhost",
            "http://local host",
            "http://localhost:0",
            "http://localhost:99999",
            "chrome-extension://abcdef",
            "http://<script>",
        ] {
            assert_eq!(
                post(&[("x-opm-request", "1"), ("origin", origin)], "localhost"),
                Err(Failure::BadOrigin),
                "{origin:?}"
            );
        }
        assert_eq!(
            post(
                &[
                    ("x-opm-request", "1"),
                    ("origin", "http://localhost"),
                    ("origin", "http://localhost"),
                ],
                "localhost"
            ),
            Err(Failure::BadOrigin),
            "two Origin headers"
        );
    }

    #[test]
    fn an_origin_without_a_validated_host_is_refused() {
        let with_origin = headers(&[("x-opm-request", "1"), ("origin", "http://localhost")]);
        assert_eq!(
            check(&Method::POST, &with_origin, None),
            Err(Failure::NoHost)
        );
        // Without Origin the host is not needed.
        let plain = headers(&[("x-opm-request", "1")]);
        assert_eq!(check(&Method::POST, &plain, None), Ok(()));
    }

    #[test]
    fn sec_fetch_site_must_be_same_origin_or_none() {
        for ok in ["same-origin", "none"] {
            assert_eq!(
                post(
                    &[("x-opm-request", "1"), ("sec-fetch-site", ok)],
                    "localhost"
                ),
                Ok(()),
                "{ok}"
            );
        }
        for bad in [
            "cross-site",
            "same-site",
            "",
            "Same-Origin",
            "same-origin, none",
        ] {
            assert_eq!(
                post(
                    &[("x-opm-request", "1"), ("sec-fetch-site", bad)],
                    "localhost"
                ),
                Err(Failure::CrossSite),
                "{bad:?}"
            );
        }
        assert_eq!(
            post(
                &[
                    ("x-opm-request", "1"),
                    ("sec-fetch-site", "same-origin"),
                    ("sec-fetch-site", "cross-site"),
                ],
                "localhost"
            ),
            Err(Failure::CrossSite),
            "duplicated"
        );
    }

    #[test]
    fn a_browser_page_on_the_same_origin_passes_all_three_checks() {
        assert_eq!(
            post(
                &[
                    ("x-opm-request", "1"),
                    ("origin", "http://nas.local:8080"),
                    ("sec-fetch-site", "same-origin"),
                ],
                "nas.local:8080"
            ),
            Ok(())
        );
    }
}
