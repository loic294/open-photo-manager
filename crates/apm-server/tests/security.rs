//! Adversarial tests of the security baseline.
//!
//! Most tests drive the real router (`app::router`) through `oneshot`, exactly
//! as the server does. Behaviours that no real route triggers (a panicking
//! handler, a slow one, one that reads a huge body) use small routers wrapped
//! in the very same `security::layers` stack: an integration test cannot see
//! the `cfg(test)` routes of the library.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use apm_core::paths::RelPath;
use apm_server::error::AppError;
use apm_server::security::{self, FsError, Redacted, RoutePolicy, SecurityOptions, VolumeRoot};
use axum::Extension;
use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use common::TestApp;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

/// The policy of the app's own pages, spelled out so that a change of the
/// constant in the code cannot go unnoticed.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
    style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; \
    connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; \
    form-action 'self'; frame-ancestors 'none'";

/// The policy of the "host not allowed" page, which has an inline `<style>`.
const PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

const INDEX_HTML: &str = "<!doctype html>\n<html><head><title>app</title></head>\
    <body><apm-app></apm-app><script type=\"module\" src=\"/assets/app-abc123.js\"></script>\
    </body></html>\n";

const SECRET: &str = "TOP-SECRET-OUTSIDE-THE-WEB-DIR";

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// A request under construction. Headers are appended, so a test can send the
/// same header twice; nothing is added behind the test's back.
#[derive(Clone)]
struct Req {
    method: Method,
    uri: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
}

fn request(method: &str, uri: &str) -> Req {
    Req {
        method: Method::from_bytes(method.as_bytes()).unwrap(),
        uri: uri.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

/// A request as the web app sends it: `Host: localhost`, and the CSRF marker
/// when it changes something.
fn browser(method: &str, uri: &str) -> Req {
    let req = request(method, uri).host("localhost");
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        req
    } else {
        req.marked()
    }
}

impl Req {
    fn header(self, name: &str, value: &str) -> Self {
        self.raw_header(name, value.as_bytes())
    }

    fn raw_header(mut self, name: &str, value: &[u8]) -> Self {
        self.headers.push((
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_bytes(value).unwrap(),
        ));
        self
    }

    fn host(self, value: &str) -> Self {
        self.header("host", value)
    }

    fn marked(self) -> Self {
        self.header("x-apm-request", "1")
    }

    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    fn json(self, value: &Value) -> Self {
        self.header("content-type", "application/json")
            .body(value.to_string())
    }

    fn build(self) -> Request<Body> {
        let mut builder = Request::builder().method(self.method).uri(self.uri);
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(self.body)).unwrap()
    }
}

/// A fully read response. The body of an event stream is not read (it never
/// ends).
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|err| panic!("the body is not JSON ({err}): {}", self.text()))
    }

    fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn content_type_is(&self, prefix: &str) -> bool {
        self.header("content-type")
            .is_some_and(|value| value.starts_with(prefix))
    }

    fn is_json(&self) -> bool {
        self.content_type_is("application/json")
    }

    fn is_html(&self) -> bool {
        self.content_type_is("text/html")
    }

    fn request_id(&self) -> &str {
        self.header("x-request-id")
            .expect("every response carries x-request-id")
    }
}

async fn send(router: &Router, request: Req) -> Reply {
    send_built(router, request.build()).await
}

async fn send_built(router: &Router, request: Request<Body>) -> Reply {
    let response = router.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let streaming = parts
        .headers
        .get("content-type")
        .is_some_and(|value| value.as_bytes().starts_with(b"text/event-stream"));
    let body = if streaming {
        Vec::new()
    } else {
        body.collect().await.unwrap().to_bytes().to_vec()
    };
    Reply {
        status: parts.status,
        headers: parts.headers,
        body,
    }
}

async fn app_allowing(hosts: &[&str]) -> TestApp {
    let hosts: Vec<String> = hosts.iter().map(|host| (*host).to_owned()).collect();
    TestApp::with_config(move |config| config.allowed_hosts = hosts).await
}

/// Writes a small web app next to a secret that lies outside the web
/// directory, and returns the web directory.
fn write_site(app: &TestApp) -> std::path::PathBuf {
    let web = app.state.config.web_dir.clone();
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), INDEX_HTML).unwrap();
    std::fs::write(web.join("assets/app-abc123.js"), "console.log('app');").unwrap();
    std::fs::write(web.join("favicon.svg"), "<svg/>").unwrap();
    std::fs::write(web.join(".env"), "TOKEN=hunter2-in-dotenv").unwrap();
    std::fs::write(web.join("assets/.hidden"), "hidden-asset").unwrap();
    std::fs::write(app.dir.path().join("secret.txt"), SECRET).unwrap();
    web
}

fn assert_json_error(reply: &Reply, status: u16, code: &str, what: &str) {
    assert_eq!(reply.status.as_u16(), status, "{what}: {}", reply.text());
    assert!(
        reply.is_json(),
        "{what}: not JSON: {:?}",
        reply.header("content-type")
    );
    assert_eq!(reply.code(), code, "{what}");
}

/// No `Access-Control-*` header, ever.
fn assert_no_cors(reply: &Reply, what: &str) {
    for name in reply.headers.keys() {
        assert!(
            !name.as_str().starts_with("access-control-"),
            "{what}: unexpected {name}"
        );
    }
}

/// Everything the headers layer adds except the Content-Security-Policy.
fn assert_other_hardening(reply: &Reply, what: &str) {
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=(), payment=()",
        ),
    ] {
        assert_eq!(reply.header(name), Some(value), "{what}: {name}");
    }
    assert!(
        reply.header("strict-transport-security").is_none(),
        "{what}: no HSTS on a plain-HTTP LAN"
    );
    assert!(
        reply.header("x-request-id").is_some(),
        "{what}: x-request-id"
    );
    assert_no_cors(reply, what);
}

/// The full set, with the policy of the app's own pages.
fn assert_hardened(reply: &Reply, what: &str) {
    assert_eq!(
        reply
            .headers
            .get_all("content-security-policy")
            .iter()
            .count(),
        1,
        "{what}: exactly one CSP header"
    );
    assert_eq!(
        reply.header("content-security-policy"),
        Some(CSP),
        "{what}: CSP"
    );
    assert_other_hardening(reply, what);
}

fn is_generated_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// ----------------------------------------------------------------------------
// Host check
// ----------------------------------------------------------------------------

const ALLOW_LIST: [&str; 3] = ["nas.local", "photos.example.com:8080", "*.example.org"];

#[tokio::test]
async fn allowed_host_names_are_served() {
    let app = app_allowing(&ALLOW_LIST).await;
    for host in [
        // localhost in every spelling, on any port
        "localhost",
        "LOCALHOST",
        "LocalHost",
        "localhost.",
        "localhost:80",
        "localhost:8080",
        "localhost.:8080",
        "localhost:65535",
        // IP literals need no rule
        "127.0.0.1",
        "127.0.0.1:8080",
        "127.1.2.3",
        "0.0.0.0",
        "10.1.2.3",
        "192.168.68.50:8080",
        "8.8.8.8",
        "255.255.255.255",
        "[::1]",
        "[::1]:8080",
        "[0:0:0:0:0:0:0:1]",
        "[FE80::1]",
        "[2001:db8::1]:9000",
        "[::ffff:127.0.0.1]",
        // the allow-list: exact name, exact name and port, wildcard
        "nas.local",
        "NAS.LOCAL",
        "Nas.Local:1234",
        "nas.local.",
        "photos.example.com:8080",
        "PHOTOS.EXAMPLE.COM:8080",
        "photos.example.com.:8080",
        "a.example.org",
        "A.Example.ORG",
        "deep.a.example.org:8443",
        "a.example.org.",
    ] {
        let reply = send(&app.router, request("GET", "/api/health").host(host)).await;
        assert_eq!(reply.status, StatusCode::OK, "Host: {host}");
    }
}

#[tokio::test]
async fn other_host_names_are_refused() {
    let app = app_allowing(&ALLOW_LIST).await;
    let too_long_label = format!("{}.example.org", "a".repeat(64));
    let too_long_host = "a".repeat(400);
    for host in [
        // names that are not on the list
        "evil.example",
        "localhost.evil.example",
        "evil-localhost",
        "notlocalhost",
        "nas.local.evil.example",
        "evilnas.local",
        "nas.localx",
        "xn--caf-dma.example",
        "-",
        // the rule has a port, the request has none or another one
        "photos.example.com",
        "photos.example.com:80",
        "photos.example.com:8081",
        // wildcards cover sub-domains only, and only whole labels
        "example.org",
        "example.org.",
        "evilexample.org",
        "a.example.org.evil.example",
        "a.example.com",
        // numeric spellings of 127.0.0.1 that are not IP literals
        "0x7f.0.0.1",
        "2130706433",
        "127.1",
        "0177.0.0.1",
        // malformed ports
        "localhost:0",
        "localhost:",
        "localhost:65536",
        "localhost:-1",
        "localhost:+80",
        "localhost:abc",
        "[::1]:",
        "[::1]:0",
        // userinfo, paths, whitespace and other junk
        "user@localhost",
        "user:pw@localhost",
        "localhost/path",
        "localhost?x=1",
        "localhost#frag",
        "local host",
        "localhost,evil.example",
        "localhost evil.example",
        // malformed names
        "",
        ".",
        "..",
        "localhost..",
        ".localhost",
        "[::1",
        "[::1]x",
        "[localhost]",
        "[1.2.3.4]",
        "[fe80::1%25eth0]",
        "::1",
        &too_long_label,
        &too_long_host,
    ] {
        let reply = send(&app.router, request("GET", "/api/health").host(host)).await;
        assert_json_error(&reply, 403, "host_not_allowed", &format!("Host: {host:?}"));
    }

    // Header values that are not ASCII text at all.
    for host in [&b"caf\xe9.example"[..], "caf\u{e9}.example".as_bytes()] {
        let reply = send(
            &app.router,
            request("GET", "/api/health").raw_header("host", host),
        )
        .await;
        assert_json_error(&reply, 403, "host_not_allowed", "non-ASCII Host");
    }
    let tab = send(
        &app.router,
        request("GET", "/api/health").raw_header("host", b"localhost\t"),
    )
    .await;
    assert_json_error(&tab, 403, "host_not_allowed", "Host with a tab");
}

#[tokio::test]
async fn a_missing_duplicated_or_mismatched_host_is_refused() {
    let app = app_allowing(&ALLOW_LIST).await;
    let refused = |what: &'static str, req: Req| {
        let router = app.router.clone();
        async move {
            let reply = send(&router, req).await;
            assert_json_error(&reply, 403, "host_not_allowed", what);
        }
    };

    refused("no Host", request("GET", "/api/health")).await;
    refused(
        "the same Host twice",
        request("GET", "/api/health")
            .host("localhost")
            .host("localhost"),
    )
    .await;
    refused(
        "allowed then hostile",
        request("GET", "/api/health")
            .host("localhost")
            .host("evil.example"),
    )
    .await;
    refused(
        "hostile then allowed",
        request("GET", "/api/health")
            .host("evil.example")
            .host("localhost"),
    )
    .await;
    refused(
        "a list in one header",
        request("GET", "/api/health").host("localhost, evil.example"),
    )
    .await;
    refused(
        "hostile absolute URI",
        request("GET", "http://evil.example/api/health"),
    )
    .await;
    refused(
        "absolute URI disagreeing with Host",
        request("GET", "http://evil.example/api/health").host("localhost"),
    )
    .await;
    refused(
        "Host disagreeing with the absolute URI",
        request("GET", "http://localhost/api/health").host("evil.example"),
    )
    .await;
    refused(
        "absolute URI and Host with other ports",
        request("GET", "http://localhost:8080/api/health").host("localhost:9090"),
    )
    .await;

    // The authority of an absolute URI stands in for a missing Host (HTTP/2
    // requests have no Host header), and may repeat it.
    for req in [
        request("GET", "http://localhost/api/health"),
        request("GET", "http://nas.local:8080/api/health"),
        request("GET", "http://nas.local:8080/api/health").host("nas.local:8080"),
        request("GET", "http://LOCALHOST/api/health").host("localhost"),
    ] {
        let reply = send(&app.router, req).await;
        assert_eq!(reply.status, StatusCode::OK);
    }
}

#[tokio::test]
async fn forwarding_headers_do_not_change_the_host() {
    let app = app_allowing(&ALLOW_LIST).await;
    for (name, value) in [
        ("x-forwarded-host", "localhost"),
        ("x-host", "localhost"),
        ("x-original-host", "localhost"),
        ("forwarded", "host=localhost"),
    ] {
        let reply = send(
            &app.router,
            request("GET", "/api/health")
                .host("evil.example")
                .header(name, value),
        )
        .await;
        assert_json_error(&reply, 403, "host_not_allowed", name);
    }
    for (name, value) in [
        ("x-forwarded-host", "evil.example"),
        ("forwarded", "host=evil.example"),
    ] {
        let reply = send(
            &app.router,
            request("GET", "/api/health")
                .host("localhost")
                .header(name, value),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{name} is ignored");
    }
}

#[tokio::test]
async fn the_host_check_covers_every_route_and_method() {
    let app = app_allowing(&ALLOW_LIST).await;
    write_site(&app);

    let api = [
        ("GET", "/api/health"),
        ("GET", "/api/events"),
        ("GET", "/api/files/1/thumb"),
        ("GET", "/api/files/1/content"),
        ("GET", "/api/nope"),
        ("GET", "/api/"),
        ("GET", "/api"),
        ("POST", "/api/browse/items"),
        ("PUT", "/api/settings"),
        ("DELETE", "/api/immich/connection"),
        ("OPTIONS", "/api/health"),
    ];
    for (method, path) in api {
        // A valid CSRF marker does not help, and a missing one does not hide
        // the Host problem: the Host check comes first.
        for marked in [true, false] {
            let mut req = request(method, path).host("evil.example");
            if marked {
                req = req.marked();
            }
            let reply = send(&app.router, req).await;
            assert_json_error(
                &reply,
                403,
                "host_not_allowed",
                &format!("{method} {path} marked={marked}"),
            );
            assert!(
                reply.json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("APM_ALLOWED_HOSTS"),
                "the message names the setting"
            );
        }
    }

    let pages = [
        "/",
        "/index.html",
        "/assets/app-abc123.js",
        "/favicon.svg",
        "/settings",
        "/browse/2024/Trip",
        "/missing.js",
        "/favicon.ico",
        "/apix",
        "/api.json",
        "/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
    ];
    for path in pages {
        let reply = send(&app.router, request("GET", path).host("evil.example")).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
        assert!(
            reply.is_html(),
            "{path}: {:?}",
            reply.header("content-type")
        );
        assert!(!reply.text().contains(SECRET), "{path}");
    }
    for method in ["POST", "PUT", "DELETE", "OPTIONS", "HEAD"] {
        let reply = send(&app.router, request(method, "/").host("evil.example")).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} /");
    }
}

// ----------------------------------------------------------------------------
// The "host not allowed" page
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_host_page_escapes_what_it_reflects() {
    let app = app_allowing(&[]).await;
    for (host, escaped) in [
        (
            "<script>alert(1)</script>",
            "&lt;script&gt;alert(1)&lt;/script&gt;",
        ),
        (
            "\"><img src=x onerror=alert(1)>",
            "&quot;&gt;&lt;img src=x onerror=alert(1)&gt;",
        ),
        ("a'b&c<d", "a&#39;b&amp;c&lt;d"),
        ("</code><script>x</script>", "&lt;/code&gt;&lt;script&gt;x"),
    ] {
        let reply = send(&app.router, request("GET", "/").host(host)).await;
        let page = reply.text();
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{host}");
        assert!(reply.is_html(), "{host}");
        assert!(page.contains(escaped), "{host} is escaped in:\n{page}");
        let lowered = page.to_ascii_lowercase();
        assert!(!lowered.contains("<script"), "{host}: no script element");
        assert!(!lowered.contains("<img"), "{host}: no image element");
        assert!(!page.contains(host), "{host} must not appear verbatim");
    }
}

#[tokio::test]
async fn the_host_page_shows_the_line_to_add_and_how_to_restart() {
    let app = app_allowing(&[]).await;
    let reply = send(
        &app.router,
        request("GET", "/browse").host("nas2.local:8080"),
    )
    .await;
    let page = reply.text();

    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.is_html());
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        page.contains("You reached this server as <code>nas2.local:8080</code>"),
        "{page}"
    );
    assert!(
        page.contains("APM_ALLOWED_HOSTS=nas2.local</code>"),
        "the line to add names the host, without the port:\n{page}"
    );
    assert!(page.contains("compose.yml"), "{page}");
    assert!(page.contains("docker compose up -d"), "{page}");
    assert!(page.contains("Container Manager"), "{page}");
    assert!(page.contains("<title>"), "{page}");
}

#[tokio::test]
async fn the_host_page_is_self_contained_and_has_its_own_policy() {
    let app = app_allowing(&[]).await;
    for host in ["evil.example", "<script>alert(1)</script>", ""] {
        let reply = send(&app.router, request("GET", "/").host(host)).await;
        let page = reply.text().to_ascii_lowercase();
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{host}");
        assert_eq!(
            reply.header("content-security-policy"),
            Some(PAGE_CSP),
            "{host}"
        );
        assert_eq!(reply.header("cache-control"), Some("no-store"), "{host}");
        assert_other_hardening(&reply, &format!("host page for {host:?}"));
        for forbidden in [
            "http://", "https://", "<script", "<link", "<img", "<iframe", "<object", "<embed",
            "<form", "src=", "href=", "@import", "url(",
        ] {
            assert!(
                !page.contains(forbidden),
                "{host}: the page must not contain {forbidden:?}:\n{page}"
            );
        }
        assert!(page.contains("<style>"), "the page styles itself inline");
    }
}

#[tokio::test]
async fn the_host_page_shortens_a_huge_host_and_copes_without_one() {
    let app = app_allowing(&[]).await;
    let huge = "a".repeat(5000);
    let reply = send(&app.router, request("GET", "/").host(&huge)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.len() < 8 * 1024, "{} bytes", reply.body.len());
    assert!(reply.text().contains('…'), "the shown host is cut");
    assert!(!reply.text().contains(&"a".repeat(200)));

    let none = send(&app.router, request("GET", "/")).await;
    assert_eq!(none.status, StatusCode::FORBIDDEN);
    assert!(none.is_html());
    assert!(none.text().contains("no usable"), "{}", none.text());
    assert_eq!(none.header("content-security-policy"), Some(PAGE_CSP));
}

#[tokio::test]
async fn the_host_page_does_not_replace_the_api_error() {
    let app = app_allowing(&[]).await;
    let reply = send(
        &app.router,
        request("GET", "/api/health").host("evil.example"),
    )
    .await;
    assert_json_error(&reply, 403, "host_not_allowed", "API request");
    assert!(!reply.is_html());
    assert_hardened(&reply, "host refusal of an API request");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
}

// ----------------------------------------------------------------------------
// CSRF
// ----------------------------------------------------------------------------

/// Whatever gets past the stack is answered 200, whatever the method.
fn ok_router() -> Router {
    async fn ok() -> &'static str {
        "ok"
    }
    Router::new()
        .route("/ok", any(ok))
        .layer(security::layers(SecurityOptions::default()))
}

/// Whether the stack itself (not a handler) refused the request.
fn refused_by_the_stack(reply: &Reply) -> bool {
    reply.status == StatusCode::FORBIDDEN
        && reply.is_json()
        && matches!(reply.code().as_str(), "csrf_rejected" | "host_not_allowed")
}

#[tokio::test]
async fn a_change_without_the_marker_is_refused_on_every_route() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    for method in [
        "POST", "PUT", "PATCH", "DELETE", "TRACE", "PROPFIND", "PURGE",
    ] {
        for path in [
            "/api/settings",
            "/api/health",
            "/api/nope",
            "/api",
            "/api/",
            "/",
            "/assets/app-abc123.js",
            "/browse/2024",
            "/%2e%2e/secret.txt",
        ] {
            let reply = send(&app.router, request(method, path).host("localhost")).await;
            let what = format!("{method} {path}");
            assert_json_error(&reply, 403, "csrf_rejected", &what);
            assert_hardened(&reply, &what);
        }
    }
}

#[tokio::test]
async fn only_the_exact_marker_is_accepted() {
    let app = app_allowing(&[]).await;
    let attempts = [
        (
            "value 0",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", "0"),
        ),
        (
            "value true",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", "true"),
        ),
        (
            "empty value",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", ""),
        ),
        (
            "value 11",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", "11"),
        ),
        (
            "value 1,1",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", "1,1"),
        ),
        (
            "value with a leading space",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-request", " 1"),
        ),
        (
            "the marker twice",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .marked(),
        ),
        (
            "a good marker and a bad one",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("x-apm-request", "0"),
        ),
        (
            "the marker as a query parameter",
            request("POST", "/api/settings?x-apm-request=1").host("localhost"),
        ),
        (
            "the marker as a cookie",
            request("POST", "/api/settings")
                .host("localhost")
                .header("cookie", "x-apm-request=1"),
        ),
        (
            "a header with a similar name",
            request("POST", "/api/settings")
                .host("localhost")
                .header("x-apm-requests", "1")
                .header("x-apm-request-id", "1"),
        ),
        (
            "an Authorization header",
            request("POST", "/api/settings")
                .host("localhost")
                .header("authorization", "Bearer 1"),
        ),
    ];
    for (what, req) in attempts {
        let reply = send(&app.router, req).await;
        assert_json_error(&reply, 403, "csrf_rejected", what);
    }
}

#[tokio::test]
async fn a_foreign_origin_is_refused_even_with_the_marker() {
    let app = app_allowing(&["nas.local"]).await;
    let long_origin = format!("http://localhost:8080/{}", "a".repeat(300));
    for (host, origin) in [
        // not this server
        ("localhost:8080", "null"),
        ("localhost:8080", "http://evil.example"),
        ("localhost:8080", "http://evil.example:8080"),
        ("localhost:8080", "https://evil.example"),
        ("localhost:8080", "http://localhost.evil.example:8080"),
        ("localhost:8080", "http://evillocalhost:8080"),
        ("localhost:8080", "http://127.0.0.1:8080"),
        ("localhost", "http://127.0.0.1"),
        ("nas.local", "http://localhost"),
        ("localhost", "http://nas.local"),
        // this machine, another port
        ("localhost:8080", "http://localhost:9090"),
        ("localhost:8080", "http://localhost"),
        ("localhost", "http://localhost:8080"),
        ("localhost", "http://localhost:443"),
        ("localhost:443", "http://localhost"),
        ("localhost", "https://localhost:80"),
        ("[::1]:8080", "http://[::1]:9090"),
        // not an origin: path, userinfo, query, fragment, spaces
        ("localhost", "http://localhost/"),
        ("localhost", "http://localhost/x"),
        ("localhost", "http://user@localhost"),
        ("localhost", "http://user:pw@localhost"),
        ("localhost", "http://localhost?x=1"),
        ("localhost", "http://localhost#x"),
        ("localhost", "http://local host"),
        ("localhost", "http://localhost%2f"),
        ("localhost:8080", &long_origin),
        // not an http(s) origin
        ("localhost", "ftp://localhost"),
        ("localhost", "file://localhost"),
        ("localhost", "chrome-extension://localhost"),
        ("localhost", "javascript://localhost"),
        ("localhost", "localhost"),
        ("localhost", "//localhost"),
        ("localhost", "http:localhost"),
        ("localhost", "http:/localhost"),
        ("localhost", ""),
        ("localhost", " "),
        ("localhost", "http://"),
        ("localhost", "http://localhost:"),
        ("localhost", "http://localhost:0"),
        ("localhost", "http://localhost:65536"),
        ("localhost", "http://[::1"),
        ("localhost", "http://,"),
        ("localhost", "http://localhost, http://evil.example"),
    ] {
        let reply = send(
            &app.router,
            request("POST", "/api/settings")
                .host(host)
                .marked()
                .header("origin", origin),
        )
        .await;
        assert_json_error(
            &reply,
            403,
            "csrf_rejected",
            &format!("Host: {host}, Origin: {origin}"),
        );
    }

    // More than one Origin header is never acceptable, even if all are good.
    for origins in [
        ["http://localhost", "http://localhost"],
        ["http://localhost", "http://evil.example"],
        ["http://evil.example", "http://localhost"],
    ] {
        let reply = send(
            &app.router,
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("origin", origins[0])
                .header("origin", origins[1]),
        )
        .await;
        assert_json_error(&reply, 403, "csrf_rejected", &format!("{origins:?}"));
    }

    // Opaque origins arrive as bytes the server cannot read as text.
    let reply = send(
        &app.router,
        request("POST", "/api/settings")
            .host("localhost")
            .marked()
            .raw_header("origin", b"http://localhost\xff"),
    )
    .await;
    assert_json_error(&reply, 403, "csrf_rejected", "Origin that is not UTF-8");
}

#[tokio::test]
async fn a_cross_site_fetch_is_refused_even_with_the_marker() {
    let app = app_allowing(&[]).await;
    for (what, req) in [
        (
            "cross-site",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "cross-site"),
        ),
        (
            "same-site",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "same-site"),
        ),
        (
            "empty",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", ""),
        ),
        (
            "capitalised",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "Same-Origin"),
        ),
        (
            "a list",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "same-origin, cross-site"),
        ),
        (
            "twice",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-site", "same-origin"),
        ),
        (
            "cross-site with a good Origin",
            request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("origin", "http://localhost")
                .header("sec-fetch-site", "cross-site"),
        ),
        (
            "cross-site on DELETE",
            request("DELETE", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", "cross-site"),
        ),
    ] {
        let reply = send(&app.router, req).await;
        assert_json_error(&reply, 403, "csrf_rejected", what);
    }
}

#[tokio::test]
async fn the_apps_own_requests_pass_the_csrf_check() {
    let app = app_allowing(&["nas.local"]).await;
    for (host, origin) in [
        (None, None),
        (Some("localhost"), None),
        (Some("localhost:8080"), Some("http://localhost:8080")),
        (Some("localhost:8080"), Some("https://localhost:8080")),
        (Some("localhost:8080"), Some("HTTP://LOCALHOST:8080")),
        (Some("localhost"), Some("http://localhost")),
        (Some("localhost"), Some("http://localhost:80")),
        (Some("localhost"), Some("https://localhost")),
        (Some("localhost"), Some("https://localhost:443")),
        (Some("localhost:80"), Some("http://localhost")),
        (Some("localhost:443"), Some("https://localhost")),
        (Some("localhost."), Some("http://localhost")),
        (Some("127.0.0.1:8080"), Some("http://127.0.0.1:8080")),
        (Some("[::1]:8080"), Some("http://[::1]:8080")),
        (Some("nas.local"), Some("http://nas.local")),
        (Some("NAS.local:8080"), Some("http://nas.LOCAL:8080")),
    ] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let mut req = request(method, "/api/settings")
                .host(host.unwrap_or("localhost"))
                .marked();
            if let Some(origin) = origin {
                req = req.header("origin", origin);
            }
            let reply = send(&app.router, req).await;
            assert!(
                !refused_by_the_stack(&reply),
                "{method} Host: {host:?} Origin: {origin:?} -> {} {}",
                reply.status,
                reply.text()
            );
        }
    }

    for fetch_site in ["same-origin", "none"] {
        for origin in [None, Some("http://localhost")] {
            let mut req = request("POST", "/api/settings")
                .host("localhost")
                .marked()
                .header("sec-fetch-site", fetch_site)
                .header("sec-fetch-mode", "cors")
                .header("sec-fetch-dest", "empty");
            if let Some(origin) = origin {
                req = req.header("origin", origin);
            }
            let reply = send(&app.router, req).await;
            assert!(
                !refused_by_the_stack(&reply),
                "Sec-Fetch-Site: {fetch_site}, Origin: {origin:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_request_that_passes_the_csrf_check_reaches_the_handler() {
    let router = ok_router();
    for method in ["POST", "PUT", "PATCH", "DELETE", "PROPFIND"] {
        let reply = send(&router, browser(method, "/ok")).await;
        assert_eq!(reply.status, StatusCode::OK, "{method}");
        assert_eq!(reply.text(), "ok");
        assert_hardened(&reply, method);
    }
    let reply = send(
        &router,
        browser("POST", "/ok")
            .header("origin", "http://localhost")
            .header("sec-fetch-site", "same-origin"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);

    let reply = send(
        &router,
        browser("POST", "/ok").header("origin", "http://evil.example"),
    )
    .await;
    assert_json_error(&reply, 403, "csrf_rejected", "foreign Origin");
}

#[tokio::test]
async fn reads_are_exempt_from_the_csrf_check() {
    let app = app_allowing(&[]).await;
    for method in ["GET", "HEAD"] {
        let reply = send(
            &app.router,
            request(method, "/api/health")
                .host("localhost")
                .header("origin", "http://evil.example")
                .header("sec-fetch-site", "cross-site"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{method}");
    }

    // Server-sent events are a GET, so a hostile page can open them; they must
    // not leak anything a GET of the API does not.
    let reply = send(
        &app.router,
        request("GET", "/api/events")
            .host("localhost")
            .header("origin", "http://evil.example")
            .header("sec-fetch-site", "cross-site"),
    )
    .await;
    assert!(!refused_by_the_stack(&reply), "{}", reply.status);

    // The router has nothing for OPTIONS: it must not be a CSRF refusal, and
    // it is not a CORS preflight answer either.
    let reply = send(
        &app.router,
        request("OPTIONS", "/api/health")
            .host("localhost")
            .header("origin", "http://evil.example"),
    )
    .await;
    assert_json_error(&reply, 405, "method_not_allowed", "OPTIONS on the API");
}

// ----------------------------------------------------------------------------
// No CORS
// ----------------------------------------------------------------------------

#[tokio::test]
async fn nothing_ever_answers_a_cors_preflight_or_sends_cors_headers() {
    let app = app_allowing(&[]).await;
    write_site(&app);

    for path in ["/api/settings", "/api/health", "/api/nope", "/", "/browse"] {
        let preflight = request("OPTIONS", path)
            .host("localhost")
            .header("origin", "http://evil.example")
            .header("access-control-request-method", "PUT")
            .header(
                "access-control-request-headers",
                "x-apm-request,content-type",
            )
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "cross-site");
        let reply = send(&app.router, preflight).await;
        let expected = if path == "/api/nope" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::METHOD_NOT_ALLOWED
        };
        assert_eq!(reply.status, expected, "{path}");
        assert_no_cors(&reply, path);
        assert!(
            reply
                .header("vary")
                .is_none_or(|vary| !vary.to_ascii_lowercase().contains("origin")),
            "{path}: no CORS Vary"
        );
    }

    for req in [
        browser("GET", "/").header("origin", "http://evil.example"),
        browser("GET", "/api/health").header("origin", "http://evil.example"),
        browser("GET", "/api/health").header("origin", "http://localhost"),
        browser("GET", "/api/health").header("origin", "null"),
        browser("GET", "/assets/app-abc123.js").header("origin", "http://evil.example"),
        browser("GET", "/api/nope").header("origin", "http://evil.example"),
        browser("POST", "/api/settings").header("origin", "http://evil.example"),
        browser("POST", "/api/settings").header("origin", "http://localhost"),
    ] {
        let reply = send(&app.router, req.clone()).await;
        assert_no_cors(&reply, &format!("{} {}", req.method, req.uri));
    }
}

// ----------------------------------------------------------------------------
// Security headers
// ----------------------------------------------------------------------------

#[tokio::test]
async fn every_kind_of_response_carries_the_hardening_headers() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let cases = [
        ("the SPA index", browser("GET", "/"), 200),
        ("index.html", browser("GET", "/index.html"), 200),
        ("an asset", browser("GET", "/assets/app-abc123.js"), 200),
        ("an SPA route", browser("GET", "/browse/2024"), 200),
        ("a HEAD request", browser("HEAD", "/"), 200),
        ("a missing asset", browser("GET", "/assets/missing.js"), 404),
        ("a refused dotfile", browser("GET", "/.env"), 404),
        ("the health check", browser("GET", "/api/health"), 200),
        ("an unknown API route", browser("GET", "/api/nope"), 404),
        ("a wrong API method", browser("DELETE", "/api/health"), 405),
        ("a wrong SPA method", browser("POST", "/"), 405),
        (
            "a CSRF refusal",
            request("POST", "/api/settings").host("localhost"),
            403,
        ),
    ];
    for (what, req, status) in cases {
        let reply = send(&app.router, req).await;
        assert_eq!(reply.status.as_u16(), status, "{what}: {}", reply.text());
        assert_hardened(&reply, what);
    }
}

#[tokio::test]
async fn a_not_modified_response_is_hardened_too() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let first = send(&app.router, browser("GET", "/assets/app-abc123.js")).await;
    assert_eq!(first.status, StatusCode::OK);
    let modified = first
        .header("last-modified")
        .expect("static files carry Last-Modified")
        .to_owned();

    let second = send(
        &app.router,
        browser("GET", "/assets/app-abc123.js").header("if-modified-since", &modified),
    )
    .await;
    assert_eq!(second.status, StatusCode::NOT_MODIFIED);
    assert!(second.body.is_empty());
    assert_hardened(&second, "304");
}

#[tokio::test]
async fn headers_chosen_by_a_handler_are_not_overridden() {
    async fn custom() -> impl IntoResponse {
        (
            [
                ("content-security-policy", "sandbox"),
                ("x-frame-options", "SAMEORIGIN"),
                ("cache-control", "private, max-age=31536000, immutable"),
            ],
            "custom",
        )
    }
    async fn not_modified() -> impl IntoResponse {
        (StatusCode::NOT_MODIFIED, [("etag", "\"abc\"")])
    }
    async fn plain() -> &'static str {
        "plain"
    }
    let router = Router::new()
        .route("/api/custom", get(custom))
        .route("/api/cached", get(not_modified))
        .route("/api/plain", get(plain))
        .route("/other", get(plain))
        .layer(security::layers(SecurityOptions::default()));

    let custom = send(&router, browser("GET", "/api/custom")).await;
    assert_eq!(
        custom
            .headers
            .get_all("content-security-policy")
            .iter()
            .count(),
        1
    );
    assert_eq!(custom.header("content-security-policy"), Some("sandbox"));
    assert_eq!(custom.header("x-frame-options"), Some("SAMEORIGIN"));
    assert_eq!(
        custom.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
    assert_eq!(custom.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(custom.header("referrer-policy"), Some("no-referrer"));

    // API answers are never cached unless the handler says so...
    let plain = send(&router, browser("GET", "/api/plain")).await;
    assert_eq!(plain.header("cache-control"), Some("no-store"));
    // ...but a 304 must not turn the cached copy into a `no-store` one,
    let cached = send(&router, browser("GET", "/api/cached")).await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    assert!(cached.header("cache-control").is_none());
    assert_hardened(&cached, "304 of the API");
    // and the rule is the API's only.
    let other = send(&router, browser("GET", "/other")).await;
    assert!(other.header("cache-control").is_none());
}

// ----------------------------------------------------------------------------
// JSON error fallbacks of the API
// ----------------------------------------------------------------------------

#[tokio::test]
async fn unknown_api_paths_get_a_json_404_never_the_app() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    for path in [
        "/api/nope",
        "/api/",
        "/api",
        "/api//",
        "/api/files/1/nope",
        "/api/browse/a/b/c/d",
        "/api/index.html",
        "/api/assets/app-abc123.js",
        "/api/%2e%2e/secret.txt",
        "/api/..%2fsecret.txt",
    ] {
        for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] {
            let reply = send(&app.router, browser(method, path)).await;
            let what = format!("{method} {path}");
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{what}");
            assert_hardened(&reply, &what);
            assert!(
                reply.is_json(),
                "{what}: {:?}",
                reply.header("content-type")
            );
            if method != "HEAD" {
                assert_eq!(reply.code(), "route_not_found", "{what}");
                assert!(!reply.text().contains("<html"), "{what}");
            }
        }
    }
}

#[tokio::test]
async fn a_wrong_method_on_an_api_route_gets_a_json_405() {
    let app = app_allowing(&[]).await;
    for (method, path) in [
        ("DELETE", "/api/health"),
        ("POST", "/api/health"),
        ("PUT", "/api/health"),
        ("PATCH", "/api/health"),
        ("OPTIONS", "/api/health"),
    ] {
        let reply = send(&app.router, browser(method, path)).await;
        let what = format!("{method} {path}");
        assert_json_error(&reply, 405, "method_not_allowed", &what);
        assert_hardened(&reply, &what);
    }
    // Without the marker the CSRF check answers first.
    let reply = send(
        &app.router,
        request("POST", "/api/health").host("localhost"),
    )
    .await;
    assert_json_error(&reply, 403, "csrf_rejected", "POST /api/health");
}

#[tokio::test]
async fn the_health_check_is_a_stable_json_200() {
    let app = app_allowing(&[]).await;
    let reply = send(&app.router, browser("GET", "/api/health")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.is_json());
    assert_hardened(&reply, "health");
    assert!(reply.json().is_object());
    // The container health check talks to 127.0.0.1.
    let reply = send(
        &app.router,
        request("GET", "/api/health").host("127.0.0.1:8080"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

// ----------------------------------------------------------------------------
// Static files and the SPA
// ----------------------------------------------------------------------------

#[tokio::test]
async fn client_side_routes_get_the_index() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    for path in [
        "/",
        "/index.html",
        "/browse",
        "/browse/",
        "/browse/2024/Trip",
        "/browse/2024/Trip/",
        "/browse/Trip%20Photos",
        "/browse/caf%C3%A9",
        "/settings",
        "/apix",
        "/assets",
        "/assets/",
        "//",
        "/a/b/c/d/e/f",
    ] {
        let reply = send(&app.router, browser("GET", path)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.text(), INDEX_HTML, "{path}");
        assert!(reply.content_type_is("text/html"), "{path}");
        assert_eq!(reply.header("cache-control"), Some("no-cache"), "{path}");
        assert_hardened(&reply, path);
    }
}

#[tokio::test]
async fn a_missing_file_is_a_plain_404_not_the_index() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    for path in [
        "/browse/2024.07",
        "/assets/missing.js",
        "/assets/app-abc123.js.map",
        "/favicon.ico",
        "/robots.txt",
        "/index.htm",
        "/api.json",
        "/x.html",
    ] {
        let reply = send(&app.router, browser("GET", path)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert!(
            reply.content_type_is("text/plain"),
            "{path}: {:?}",
            reply.header("content-type")
        );
        assert_eq!(reply.text(), "not found\n", "{path}");
        assert_hardened(&reply, path);
    }
}

#[tokio::test]
async fn assets_are_served_as_they_are() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let js = send(&app.router, browser("GET", "/assets/app-abc123.js")).await;
    assert_eq!(js.status, StatusCode::OK);
    assert!(js.header("content-type").unwrap().contains("javascript"));
    assert_eq!(js.text(), "console.log('app');");
    assert!(
        js.header("cache-control")
            .is_none_or(|value| value != "no-store")
    );
    assert_hardened(&js, "asset");

    let svg = send(&app.router, browser("GET", "/favicon.svg")).await;
    assert_eq!(svg.status, StatusCode::OK);
    assert!(svg.header("content-type").unwrap().contains("svg"));
    assert_eq!(svg.text(), "<svg/>");

    let head = send(&app.router, browser("HEAD", "/assets/app-abc123.js")).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(head.header("content-length"), Some("19"));
}

#[tokio::test]
async fn the_static_server_only_reads() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    for path in ["/", "/browse", "/assets/app-abc123.js", "/missing.js"] {
        let head = send(&app.router, browser("HEAD", path)).await;
        assert!(head.body.is_empty(), "HEAD {path}");

        for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            let reply = send(&app.router, browser(method, path)).await;
            let what = format!("{method} {path}");
            assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{what}");
            assert_eq!(reply.header("allow"), Some("GET, HEAD"), "{what}");
            assert_hardened(&reply, &what);

            if method != "OPTIONS" {
                let unmarked = send(&app.router, request(method, path).host("localhost")).await;
                assert_json_error(&unmarked, 403, "csrf_rejected", &what);
            }
        }
    }
}

#[tokio::test]
async fn without_a_built_frontend_the_api_still_works() {
    let app = app_allowing(&[]).await;
    assert!(!app.state.config.web_dir.exists());
    for path in ["/", "/browse", "/assets/app.js"] {
        let reply = send(&app.router, browser("GET", path)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_hardened(&reply, path);
    }
    let reply = send(&app.router, browser("GET", "/api/health")).await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn paths_cannot_leave_the_web_directory_or_reach_dotfiles() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let long_path = format!("/{}", "a".repeat(3000));
    let long_segment = format!("/assets/{}", "b".repeat(300));
    let paths = [
        // the secret lies next to the web directory
        "/..%2f..%2fsecret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/%2e%2e%2fsecret.txt",
        "/%2E%2E%2F..%2Fsecret.txt",
        "/assets/..%2f..%2fsecret.txt",
        "/assets/../../secret.txt",
        "/../secret.txt",
        "/..%252f..%252fsecret.txt",
        "/%252e%252e/secret.txt",
        "/..;/secret.txt",
        "/%c0%ae%c0%ae/secret.txt",
        "/%c0%2e%c0%2e/secret.txt",
        "/%e0%80%ae%e0%80%ae/secret.txt",
        "/%5c..%5csecret.txt",
        "/..%5csecret.txt",
        "/assets/..%5c..%5csecret.txt",
        "/./secret.txt",
        "/%2e/secret.txt",
        "/%2e/",
        "/secret.txt",
        "//secret.txt",
        "///secret.txt",
        "//etc/passwd",
        "/etc/passwd",
        "/%2fetc%2fpasswd",
        "/%2f%2fetc%2fpasswd",
        "/assets/%2fetc%2fpasswd",
        "/C:/Windows/win.ini",
        "/C%3a%5cWindows%5cwin.ini",
        // NUL and other control characters
        "/%00",
        "/%00.html",
        "/index.html%00.png",
        "/assets/app-abc123.js%00",
        "/%0d%0a",
        "/%1b[2J",
        "/assets/%0aapp-abc123.js",
        "/%7f",
        // dotfiles
        "/.env",
        "/%2eenv",
        "/assets/.hidden",
        "/assets/%2ehidden",
        "/.git/config",
        "/.well-known/x",
        "/assets/.",
        "/assets/..",
        // invalid text
        "/%ff",
        "/%",
        "/%zz",
        "/%e9",
        "/assets/%80",
        // over-long input
        &long_path,
        &long_segment,
    ];
    for path in paths {
        let reply = send(&app.router, browser("GET", path)).await;
        let body = reply.text();
        for leaked in [SECRET, "hunter2-in-dotenv", "hidden-asset"] {
            assert!(!body.contains(leaked), "{path} leaks {leaked}");
        }
        assert!(reply.status.as_u16() < 500, "{path}: {}", reply.status);
        assert!(
            reply.status == StatusCode::NOT_FOUND
                || (reply.status == StatusCode::OK && body == INDEX_HTML),
            "{path}: unexpected {} {body:?}",
            reply.status
        );
        assert_hardened(&reply, path);
    }
}

// ----------------------------------------------------------------------------
// Log capture and probes for the synthetic routers
// ----------------------------------------------------------------------------

/// Collects the log lines of one test.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

thread_local! {
    static CAPTURING: RefCell<Option<LogBuffer>> = const { RefCell::new(None) };
}

/// Writes each event to the buffer of the thread that emitted it, if any.
struct ThreadWriter;

impl io::Write for ThreadWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        CAPTURING.with(|slot| {
            if let Some(logs) = slot.borrow().as_ref() {
                logs.0.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stops the capture of the current thread when dropped.
struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURING.with(|slot| *slot.borrow_mut() = None);
    }
}

impl LogBuffer {
    /// Routes the events of the current thread (the whole test, on the
    /// current-thread runtime of `#[tokio::test]`) into this buffer, down to
    /// debug level. Keep the guard alive for the length of the test.
    ///
    /// One subscriber serves the whole test binary: with a subscriber per
    /// test, `tracing` caches whether a callsite is enabled from whichever
    /// thread reaches it first, and a test running next to another one could
    /// lose its spans.
    fn capture(&self) -> CaptureGuard {
        static SUBSCRIBER: Once = Once::new();
        SUBSCRIBER.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(|| ThreadWriter)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other subscriber is installed by these tests");
        });
        CAPTURING.with(|slot| *slot.borrow_mut() = Some(self.clone()));
        CaptureGuard
    }

    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// What a synthetic handler did.
#[derive(Clone, Default)]
struct Probe {
    hits: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
}

impl Probe {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

async fn ok() -> &'static str {
    "ok"
}

// ----------------------------------------------------------------------------
// Panics
// ----------------------------------------------------------------------------

async fn panics_with_a_str() -> &'static str {
    panic!("boom-in-handler")
}

async fn panics_with_a_string() -> &'static str {
    let detail = "boom-in-handler".to_owned();
    panic!("{detail}")
}

async fn panics_with_a_number() -> &'static str {
    std::panic::panic_any(42_u8)
}

#[tokio::test]
async fn a_panicking_handler_gives_a_generic_500_and_the_server_keeps_going() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let router = Router::new()
        .route("/str", get(panics_with_a_str))
        .route("/string", get(panics_with_a_string))
        .route("/number", get(panics_with_a_number))
        .route("/ok", get(ok))
        .layer(security::layers(SecurityOptions::default()));

    for path in ["/str", "/string", "/number"] {
        let reply = send(
            &router,
            browser("GET", path).header("x-request-id", "panic-req-1"),
        )
        .await;
        assert_json_error(&reply, 500, "internal", path);
        assert_eq!(
            reply.json()["error"]["message"],
            "internal server error",
            "{path}"
        );
        assert!(!reply.text().contains("boom"), "{path}: payload leaked");
        assert_hardened(&reply, path);
        assert_eq!(reply.request_id(), "panic-req-1", "{path}");
    }

    let logged = logs.contents();
    let panics: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("handler panicked"))
        .collect();
    assert_eq!(panics.len(), 3, "{logged}");
    for line in &panics {
        assert!(
            line.contains("id=panic-req-1"),
            "no request id in: {line}\nfull log:\n{logged}"
        );
    }
    assert!(panics[0].contains("handler panicked: boom-in-handler"));
    assert!(panics[1].contains("handler panicked: boom-in-handler"));
    assert!(panics[2].contains("non-string panic payload"));
    assert!(
        logged
            .lines()
            .any(|line| line.contains("request finished") && line.contains("status=500")),
        "{logged}"
    );

    // The router, and the stack around it, still serve.
    let after = send(&router, browser("GET", "/ok")).await;
    assert_eq!(after.status, StatusCode::OK);
    assert_eq!(after.text(), "ok");
}

#[tokio::test]
async fn a_panic_in_a_mutating_request_is_a_500_not_a_dropped_connection() {
    let router = Router::new()
        .route("/str", post(panics_with_a_str))
        .layer(security::layers(SecurityOptions::default()));
    let reply = send(&router, browser("POST", "/str")).await;
    assert_json_error(&reply, 500, "internal", "POST");
    assert_hardened(&reply, "panic on POST");
}

// ----------------------------------------------------------------------------
// Request body limit
// ----------------------------------------------------------------------------

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

async fn sink(State(probe): State<Probe>, body: Bytes) -> String {
    probe.hits.fetch_add(1, Ordering::SeqCst);
    body.len().to_string()
}

/// Reads the body and ignores whatever goes wrong.
async fn lenient(State(probe): State<Probe>, body: Body) -> &'static str {
    probe.hits.fetch_add(1, Ordering::SeqCst);
    let _ = body.collect().await;
    "fine"
}

async fn json_len(State(probe): State<Probe>, Json(value): Json<Value>) -> String {
    probe.hits.fetch_add(1, Ordering::SeqCst);
    value["data"].as_str().map_or(0, str::len).to_string()
}

/// 1 KiB by default, 3 MiB on `/big`.
fn body_router(probe: &Probe) -> Router {
    let options = SecurityOptions {
        max_body_bytes: KIB,
        policies: vec![RoutePolicy::new("/big").with_max_body(3 * MIB)],
        ..SecurityOptions::default()
    };
    Router::new()
        .route("/sink", post(sink))
        .route("/lenient", post(lenient))
        .route("/json", post(json_len))
        .route("/big", post(json_len))
        .layer(security::layers(options))
        .with_state(probe.clone())
}

fn upload(path: &str, body: Vec<u8>, declared: Option<usize>) -> Req {
    let mut req = browser("POST", path);
    if let Some(declared) = declared {
        req = req.header("content-length", &declared.to_string());
    }
    req.body(body)
}

fn streamed_upload(path: &str, chunks: usize, chunk_len: usize) -> Request<Body> {
    let chunks = (0..chunks).map(move |_| Ok::<_, io::Error>(Bytes::from(vec![b'x'; chunk_len])));
    Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("x-apm-request", "1")
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap()
}

#[tokio::test]
async fn a_body_of_exactly_the_limit_is_accepted_and_one_byte_more_is_not() {
    let probe = Probe::default();
    let router = body_router(&probe);

    let at_limit = send(&router, upload("/sink", vec![b'x'; KIB], Some(KIB))).await;
    assert_eq!(at_limit.status, StatusCode::OK);
    assert_eq!(at_limit.text(), "1024");
    assert_eq!(probe.hits(), 1);

    let without_header = send(&router, upload("/sink", vec![b'x'; KIB], None)).await;
    assert_eq!(without_header.status, StatusCode::OK);
    assert_eq!(without_header.text(), "1024");

    let over = send(&router, upload("/sink", vec![b'x'; KIB + 1], Some(KIB + 1))).await;
    assert_json_error(&over, 413, "payload_too_large", "one byte over");
    assert_hardened(&over, "413");
    assert_eq!(
        probe.hits(),
        2,
        "the handler did not run for the oversize body"
    );

    let over_without_header = send(&router, upload("/sink", vec![b'x'; KIB + 1], None)).await;
    assert_json_error(&over_without_header, 413, "payload_too_large", "no length");
}

#[tokio::test]
async fn a_huge_declared_length_is_refused_without_reading_anything() {
    let probe = Probe::default();
    let router = body_router(&probe);
    for declared in [KIB + 1, 10 * MIB, usize::MAX / 2] {
        // The body that follows is tiny: the header alone is enough.
        let reply = send(&router, upload("/sink", b"tiny".to_vec(), Some(declared))).await;
        assert_json_error(&reply, 413, "payload_too_large", &format!("{declared}"));
    }
    let reply = send(
        &router,
        upload("/sink", b"tiny".to_vec(), None).header("content-length", "99999999999999999999999"),
    )
    .await;
    // Too large to even parse: the byte counting takes over and the 4 bytes pass.
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(probe.hits(), 1);
}

#[tokio::test]
async fn an_undeclared_or_lying_length_is_counted_as_it_is_read() {
    let probe = Probe::default();
    let router = body_router(&probe);

    // A streamed body has no Content-Length at all.
    let reply = send_built(&router, streamed_upload("/sink", 3, 512)).await;
    assert_json_error(&reply, 413, "payload_too_large", "streamed body");
    assert_hardened(&reply, "streamed 413");
    let within = send_built(&router, streamed_upload("/sink", 2, 512)).await;
    assert_eq!(within.status, StatusCode::OK);
    assert_eq!(within.text(), "1024");

    // A small declared length does not make a big body small.
    let lying = send(&router, upload("/sink", vec![b'x'; 4 * KIB], Some(10))).await;
    assert_json_error(&lying, 413, "payload_too_large", "lying Content-Length");

    // Malformed or repeated lengths are not trusted either.
    for lengths in [
        vec!["abc"],
        vec!["-1"],
        vec![""],
        vec!["2000", "2000"],
        vec!["10", "20"],
    ] {
        let mut req = upload("/sink", vec![b'x'; 2 * KIB], None);
        for length in &lengths {
            req = req.header("content-length", length);
        }
        let reply = send(&router, req).await;
        assert_json_error(&reply, 413, "payload_too_large", &format!("{lengths:?}"));
    }
}

#[tokio::test]
async fn a_handler_that_ignores_the_read_error_still_answers_413() {
    let probe = Probe::default();
    let router = body_router(&probe);
    let reply = send(&router, upload("/lenient", vec![b'x'; 4 * KIB], None)).await;
    assert_json_error(&reply, 413, "payload_too_large", "lenient handler");
    assert_eq!(
        probe.hits(),
        1,
        "the handler did run: only the limit stopped it"
    );

    let reply = send(&router, upload("/lenient", vec![b'x'; 100], None)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "fine");
}

#[tokio::test]
async fn a_route_can_raise_its_limit_above_axums_own_2_mib() {
    let probe = Probe::default();
    let router = body_router(&probe);
    let json_of = |len: usize| format!("{{\"data\":\"{}\"}}", "x".repeat(len)).into_bytes();

    // 2.5 MiB would fail in the `Json` extractor if axum's own limit still
    // applied.
    let body = json_of(5 * MIB / 2);
    let declared = body.len();
    let reply = send(
        &router,
        upload("/big", body, Some(declared)).header("content-type", "application/json"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.text(), (5 * MIB / 2).to_string());

    // Over the route's own limit, declared or not.
    let body = json_of(3 * MIB);
    let declared = body.len();
    let declared_reply = send(
        &router,
        upload("/big", body.clone(), Some(declared)).header("content-type", "application/json"),
    )
    .await;
    assert_json_error(
        &declared_reply,
        413,
        "payload_too_large",
        "declared, over 3 MiB",
    );
    let undeclared_reply = send(
        &router,
        upload("/big", body, None).header("content-type", "application/json"),
    )
    .await;
    assert_json_error(
        &undeclared_reply,
        413,
        "payload_too_large",
        "undeclared, over 3 MiB",
    );

    // The same body on a route with the default limit.
    let body = json_of(5 * MIB / 2);
    let reply = send(
        &router,
        upload("/json", body, None).header("content-type", "application/json"),
    )
    .await;
    assert_json_error(&reply, 413, "payload_too_large", "default route");
}

#[tokio::test]
async fn the_real_routes_have_the_documented_limits() {
    let app = app_allowing(&[]).await;
    let put = |len: usize, declared: usize| {
        browser("PUT", "/api/settings")
            .header("content-type", "application/json")
            .header("content-length", &declared.to_string())
            .body(vec![b' '; len])
    };

    let at_limit = send(&app.router, put(MIB, MIB)).await;
    assert_ne!(
        at_limit.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        at_limit.text()
    );
    let over = send(&app.router, put(MIB + 1, MIB + 1)).await;
    assert_json_error(
        &over,
        413,
        "payload_too_large",
        "PUT /api/settings, 1 MiB + 1",
    );
    assert_hardened(&over, "413 on the real app");

    // "Select all" in a big library posts a lot of ids.
    let select = |len: usize, declared: usize| {
        browser("POST", "/api/selections")
            .header("content-type", "application/json")
            .header("content-length", &declared.to_string())
            .body(vec![b' '; len])
    };
    let big = send(&app.router, select(2 * MIB, 2 * MIB)).await;
    assert_ne!(big.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", big.text());
    let at_limit = send(&app.router, select(16 * MIB, 16 * MIB)).await;
    assert_ne!(at_limit.status, StatusCode::PAYLOAD_TOO_LARGE);
    let lying = send(&app.router, select(10, 16 * MIB + 1)).await;
    assert_json_error(&lying, 413, "payload_too_large", "16 MiB + 1 declared");
}

// ----------------------------------------------------------------------------
// Request timeout
// ----------------------------------------------------------------------------

const SLOW: Duration = Duration::from_millis(300);

async fn slow(State(probe): State<Probe>) -> &'static str {
    tokio::time::sleep(SLOW).await;
    probe.finished.store(true, Ordering::SeqCst);
    "slow"
}

/// A body that starts at once and takes 400 ms to finish.
async fn drip() -> Body {
    let chunks = futures_util::stream::iter(0..5).then(|index| async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        Ok::<_, io::Error>(Bytes::from(index.to_string()))
    });
    Body::from_stream(chunks)
}

fn slow_router(probe: &Probe, options: SecurityOptions) -> Router {
    Router::new()
        .route("/slow", get(slow))
        .route("/drip", get(drip))
        .route("/api/events", get(slow))
        .route("/api/files/{id}", get(slow))
        .route("/api/files/{id}/thumb", get(slow))
        .route("/api/files/{id}/preview", get(slow))
        .route("/api/files/{id}/content", get(slow))
        .route("/longer", get(slow))
        .route("/forever", get(slow))
        .layer(security::layers(options))
        .with_state(probe.clone())
}

fn quick_timeout() -> SecurityOptions {
    SecurityOptions {
        request_timeout: Duration::from_millis(100),
        ..SecurityOptions::default()
    }
}

#[tokio::test]
async fn a_slow_handler_is_cancelled_with_a_408() {
    let probe = Probe::default();
    let router = slow_router(&probe, quick_timeout());

    let started = Instant::now();
    let reply = send(&router, browser("GET", "/slow")).await;
    let waited = started.elapsed();
    assert_json_error(&reply, 408, "request_timeout", "slow handler");
    assert_hardened(&reply, "408");
    assert!(
        waited < Duration::from_millis(250),
        "answered after {waited:?}"
    );

    // The handler was dropped, not left running to completion.
    tokio::time::sleep(SLOW + Duration::from_millis(200)).await;
    assert!(!probe.finished(), "the cancelled handler ran to its end");
}

#[tokio::test]
async fn streams_and_media_have_no_timeout_but_the_rest_does() {
    let probe = Probe::default();
    let router = slow_router(&probe, quick_timeout());
    let exempt = [
        "/api/events",
        "/api/files/1/thumb",
        "/api/files/1/preview",
        "/api/files/1/content",
    ];
    let replies =
        futures_util::future::join_all(exempt.map(|path| send(&router, browser("GET", path))))
            .await;
    for (path, reply) in exempt.iter().zip(replies) {
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.text(), "slow", "{path}");
    }

    let metadata = send(&router, browser("GET", "/api/files/1")).await;
    assert_json_error(
        &metadata,
        408,
        "request_timeout",
        "file metadata keeps its timeout",
    );
    let other = send(&router, browser("GET", "/slow")).await;
    assert_json_error(&other, 408, "request_timeout", "any other route");
}

#[tokio::test]
async fn a_route_policy_can_lengthen_or_remove_the_timeout() {
    let probe = Probe::default();
    let options = SecurityOptions {
        request_timeout: Duration::from_millis(100),
        policies: vec![
            RoutePolicy::new("/longer").with_timeout(Duration::from_millis(1500)),
            RoutePolicy::new("/forever").without_timeout(),
        ],
        ..SecurityOptions::default()
    };
    let router = slow_router(&probe, options);
    for path in ["/longer", "/forever"] {
        let reply = send(&router, browser("GET", path)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
    }
    let default = send(&router, browser("GET", "/slow")).await;
    assert_json_error(&default, 408, "request_timeout", "default timeout");

    // A shorter policy than the default cuts earlier.
    let options = SecurityOptions {
        request_timeout: Duration::from_secs(30),
        policies: vec![RoutePolicy::new("/slow").with_timeout(Duration::from_millis(50))],
        ..SecurityOptions::default()
    };
    let router = slow_router(&Probe::default(), options);
    let reply = send(&router, browser("GET", "/slow")).await;
    assert_json_error(&reply, 408, "request_timeout", "short policy");
}

#[tokio::test]
async fn the_timeout_covers_the_wait_for_headers_not_a_streamed_response() {
    let probe = Probe::default();
    let router = slow_router(&probe, quick_timeout());
    let started = Instant::now();
    let reply = send(&router, browser("GET", "/drip")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "01234", "the whole body arrived");
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "it did stream"
    );
}

// ----------------------------------------------------------------------------
// Request ids
// ----------------------------------------------------------------------------

async fn whoami(Extension(id): Extension<security::RequestId>) -> String {
    id.to_string()
}

/// A handler that tries to choose the id of its own response.
async fn spoof() -> impl IntoResponse {
    ([("x-request-id", "chosen-by-the-handler")], "ok")
}

fn id_router() -> Router {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/spoof", get(spoof))
        .layer(security::layers(SecurityOptions::default()))
}

#[tokio::test]
async fn a_well_formed_request_id_is_echoed_on_every_kind_of_response() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let cases = [
        ("health", browser("GET", "/api/health")),
        ("unknown api path", browser("GET", "/api/nope")),
        ("wrong api method", browser("DELETE", "/api/health")),
        (
            "csrf refusal",
            request("POST", "/api/health").host("localhost"),
        ),
        (
            "host refusal, json",
            request("GET", "/api/health").host("evil.example"),
        ),
        (
            "host refusal, page",
            request("GET", "/").host("evil.example"),
        ),
        ("missing host", request("GET", "/api/health")),
        ("index", browser("GET", "/")),
        ("asset", browser("GET", "/assets/app-abc123.js")),
        ("missing asset", browser("GET", "/assets/missing.js")),
        ("static server, wrong method", browser("POST", "/")),
        (
            "oversized body",
            browser("POST", "/api/health").header("content-length", "99999999"),
        ),
    ];
    for (what, req) in cases {
        let reply = send(&app.router, req.header("x-request-id", "trace-me_01")).await;
        assert_eq!(reply.request_id(), "trace-me_01", "{what}");
        assert_eq!(
            reply.headers.get_all("x-request-id").iter().count(),
            1,
            "{what}: more than one x-request-id"
        );
    }
}

#[tokio::test]
async fn request_ids_of_the_documented_shape_are_kept() {
    let app = app_allowing(&[]).await;
    let longest = "x".repeat(64);
    for id in ["a", "Z", "7", "_", "-", "Mixed-Case_123", longest.as_str()] {
        let reply = send(
            &app.router,
            browser("GET", "/api/health").header("x-request-id", id),
        )
        .await;
        assert_eq!(reply.request_id(), id);
    }
}

#[tokio::test]
async fn a_malformed_request_id_is_replaced_and_never_echoed() {
    let app = app_allowing(&[]).await;
    let too_long = "x".repeat(65);
    let bad: [(&str, &[u8]); 14] = [
        ("empty", b""),
        ("65 characters", too_long.as_bytes()),
        ("space", b"has space"),
        ("leading space", b" lead"),
        ("tab", b"a\tb"),
        ("markup", b"<script>alert(1)</script>"),
        ("quote", b"a\"b"),
        ("semicolon", b"a;b"),
        ("comma", b"a,b"),
        ("slash", b"a/b"),
        ("dot", b"a.b"),
        ("colon", b"a:b"),
        ("non-ascii", "caf\u{e9}".as_bytes()),
        ("raw high byte", &[0x80, b'a']),
    ];
    for (what, value) in bad {
        let reply = send(
            &app.router,
            browser("GET", "/api/health").raw_header("x-request-id", value),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{what}");
        let id = reply.request_id();
        assert!(is_generated_id(id), "{what}: answered with {id:?}");
    }
}

#[tokio::test]
async fn repeated_request_id_headers_are_not_trusted_and_missing_ones_are_generated() {
    let app = app_allowing(&[]).await;
    let twice = send(
        &app.router,
        browser("GET", "/api/health")
            .header("x-request-id", "first-id")
            .header("x-request-id", "second-id"),
    )
    .await;
    assert!(
        is_generated_id(twice.request_id()),
        "{}",
        twice.request_id()
    );

    let mut seen = HashSet::new();
    for _ in 0..25 {
        let reply = send(&app.router, browser("GET", "/api/health")).await;
        let id = reply.request_id().to_owned();
        assert!(is_generated_id(&id), "{id}");
        assert!(seen.insert(id.clone()), "{id} was generated twice");
    }
}

#[tokio::test]
async fn handlers_see_the_request_id_and_cannot_choose_the_header() {
    let router = id_router();
    let given = send(
        &router,
        browser("GET", "/whoami").header("x-request-id", "from-client"),
    )
    .await;
    assert_eq!(given.text(), "from-client");

    let generated = send(&router, browser("GET", "/whoami")).await;
    assert!(is_generated_id(&generated.text()), "{}", generated.text());
    assert_eq!(generated.text(), generated.request_id());

    let spoofed = send(
        &router,
        browser("GET", "/spoof").header("x-request-id", "real-id"),
    )
    .await;
    assert_eq!(spoofed.request_id(), "real-id");
    assert_eq!(spoofed.headers.get_all("x-request-id").iter().count(), 1);
}

// ----------------------------------------------------------------------------
// Logs and secrets
// ----------------------------------------------------------------------------

/// The `request finished` lines that belong to request `id`.
fn finished_lines<'a>(logged: &'a str, id: &str) -> Vec<&'a str> {
    let tag = format!("id={id} ");
    logged
        .lines()
        .filter(|line| line.contains("request finished") && line.contains(&tag))
        .collect()
}

#[tokio::test]
async fn the_log_line_has_the_id_and_the_path_but_not_the_query_headers_or_cookies() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let app = app_allowing(&[]).await;

    let reply = send(
        &app.router,
        browser("GET", "/api/health?token=SECRETQUERY&password=hunter2")
            .header("authorization", "Bearer SECRETBEARER")
            .header("cookie", "session=SECRETCOOKIE")
            .header("x-api-key", "SECRETAPIKEY")
            .header("x-request-id", "log-test-1"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);

    let logged = logs.contents();
    let lines = finished_lines(&logged, "log-test-1");
    assert_eq!(lines.len(), 1, "{logged}");
    for part in [
        "method=GET",
        "path=/api/health}",
        "status=200",
        "latency_ms=",
    ] {
        assert!(lines[0].contains(part), "{part} missing in: {}", lines[0]);
    }
    for secret in [
        "SECRETQUERY",
        "hunter2",
        "SECRETBEARER",
        "SECRETCOOKIE",
        "SECRETAPIKEY",
        "token=",
        "password=",
    ] {
        assert!(
            !logged.contains(secret),
            "{secret} leaked into the log:\n{logged}"
        );
    }
}

#[tokio::test]
async fn every_request_gets_exactly_one_finished_line_at_the_documented_level() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let app = app_allowing(&[]).await;
    write_site(&app);

    // (request id, level, status, request)
    let cases = [
        ("one-ok", "DEBUG", 200, browser("GET", "/api/health")),
        ("one-index", "DEBUG", 200, browser("GET", "/")),
        ("one-404", "INFO", 404, browser("GET", "/api/nope")),
        ("one-405", "INFO", 405, browser("DELETE", "/api/health")),
        (
            "one-host",
            "INFO",
            403,
            request("GET", "/api/health").host("evil.example"),
        ),
        (
            "one-csrf",
            "INFO",
            403,
            request("POST", "/api/health").host("localhost"),
        ),
        ("one-static-404", "INFO", 404, browser("GET", "/missing.js")),
    ];
    let expected: Vec<(&str, &str, u16)> = cases
        .iter()
        .map(|(id, level, status, _)| (*id, *level, *status))
        .collect();
    for (id, _, status, req) in cases {
        let reply = send(&app.router, req.header("x-request-id", id)).await;
        assert_eq!(reply.status.as_u16(), status, "{id}");
    }

    let logged = logs.contents();
    for (id, level, status) in expected {
        let lines = finished_lines(&logged, id);
        assert_eq!(lines.len(), 1, "{id}: {logged}");
        assert!(
            lines[0].trim_start().starts_with(&format!("{level} ")),
            "{id} should be logged at {level}: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(&format!("status={status} ")),
            "{id}: {}",
            lines[0]
        );
    }
}

#[tokio::test]
async fn a_successful_write_is_logged_at_info_and_a_failure_at_error() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let probe = Probe::default();
    let router = Router::new()
        .route("/sink", post(sink))
        .route("/boom", post(panics_with_a_str))
        .layer(security::layers(SecurityOptions::default()))
        .with_state(probe.clone());

    let written = send(
        &router,
        upload("/sink", b"abc".to_vec(), None).header("x-request-id", "level-write"),
    )
    .await;
    assert_eq!(written.status, StatusCode::OK);
    let failed = send(
        &router,
        browser("POST", "/boom").header("x-request-id", "level-boom"),
    )
    .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);

    let logged = logs.contents();
    let write = finished_lines(&logged, "level-write");
    assert_eq!(write.len(), 1, "{logged}");
    assert!(write[0].trim_start().starts_with("INFO "), "{}", write[0]);
    let boom = finished_lines(&logged, "level-boom");
    assert_eq!(boom.len(), 1, "{logged}");
    assert!(boom[0].trim_start().starts_with("ERROR "), "{}", boom[0]);
}

#[tokio::test]
async fn the_values_of_a_refused_request_are_not_logged() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let app = app_allowing(&[]).await;

    let attempts = [
        (
            "foreign host, api",
            request("GET", "/api/health").host("evil-host-canary.example"),
        ),
        (
            "foreign host, page",
            request("GET", "/").host("evil-page-canary.example:8443"),
        ),
        (
            "foreign origin",
            browser("POST", "/api/health").header("origin", "https://evil-origin-canary.example"),
        ),
        (
            "cross-site fetch",
            browser("POST", "/api/health").header("sec-fetch-site", "cross-site-canary"),
        ),
        (
            "wrong marker",
            request("POST", "/api/health")
                .host("localhost")
                .header("x-apm-request", "wrong-marker-canary"),
        ),
    ];
    for (what, req) in attempts {
        let reply = send(&app.router, req).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{what}");
    }

    let logged = logs.contents();
    assert_eq!(
        logged.matches("request refused").count(),
        5,
        "each refusal says why, once:\n{logged}"
    );
    assert!(
        !logged.contains("canary"),
        "a refused value was logged:\n{logged}"
    );
}

#[tokio::test]
async fn request_bodies_are_never_logged() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let probe = Probe::default();
    let router = body_router(&probe);

    let accepted = send(&router, upload("/sink", b"SECRETBODY-small".to_vec(), None)).await;
    assert_eq!(accepted.status, StatusCode::OK);
    let as_json = send(
        &router,
        browser("POST", "/json").json(&json!({ "data": "SECRETBODY-json" })),
    )
    .await;
    assert_eq!(as_json.status, StatusCode::OK);

    let mut big = b"SECRETBODY-big".to_vec();
    big.resize(2 * KIB, b'x');
    let declared = send(&router, upload("/sink", big.clone(), Some(2 * KIB))).await;
    assert_eq!(declared.status, StatusCode::PAYLOAD_TOO_LARGE);
    let counted = send(&router, upload("/sink", big, None)).await;
    assert_eq!(counted.status, StatusCode::PAYLOAD_TOO_LARGE);

    let logged = logs.contents();
    assert_eq!(logged.matches("request finished").count(), 4, "{logged}");
    assert!(
        !logged.contains("SECRETBODY"),
        "a body was logged:\n{logged}"
    );
}

#[tokio::test]
async fn a_hostile_path_cannot_forge_or_flood_log_lines() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let app = app_allowing(&[]).await;

    let forged = send(
        &app.router,
        browser("GET", "/api/x%0a%0d%20INFO%20forged%20line%20status=200")
            .header("x-request-id", "forge-1"),
    )
    .await;
    assert_eq!(forged.status, StatusCode::NOT_FOUND);
    let long = format!("/api/{}", "a".repeat(5000));
    let flooded = send(
        &app.router,
        browser("GET", &long).header("x-request-id", "flood-1"),
    )
    .await;
    assert_eq!(flooded.status, StatusCode::NOT_FOUND);

    let logged = logs.contents();
    assert_eq!(finished_lines(&logged, "forge-1").len(), 1, "{logged}");
    assert!(
        !logged
            .lines()
            .any(|line| line.trim_start().starts_with("INFO forged")),
        "a line was forged:\n{logged}"
    );
    let line = finished_lines(&logged, "flood-1");
    assert_eq!(line.len(), 1, "{logged}");
    assert!(
        line[0].len() < 500,
        "the path was not shortened: {}",
        line[0]
    );
    assert!(!logged.contains(&"a".repeat(201)), "a long path was logged");
}

async fn leaks_a_credential() -> Result<&'static str, AppError> {
    Err(AppError::internal(
        "request to https://user:hunter2@immich.local:2283/api/assets?key=SECRETKEY&page=2 failed",
    ))
}

#[tokio::test]
async fn an_internal_error_logs_its_cause_without_credentials_and_tells_the_client_nothing() {
    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let router = Router::new()
        .route("/leak", get(leaks_a_credential))
        .layer(security::layers(SecurityOptions::default()));

    let reply = send(&router, browser("GET", "/leak")).await;
    assert_json_error(&reply, 500, "internal", "leaky handler");
    assert_eq!(reply.json()["error"]["message"], "internal server error");
    for part in ["hunter2", "SECRETKEY", "immich.local", "user:"] {
        assert!(!reply.text().contains(part), "{part} reached the client");
    }

    let logged = logs.contents();
    assert!(
        logged.contains("immich.local:2283/api/assets"),
        "the cause is worth logging:\n{logged}"
    );
    assert!(logged.contains("[redacted]"), "{logged}");
    for secret in ["hunter2", "SECRETKEY"] {
        assert!(!logged.contains(secret), "{secret} leaked:\n{logged}");
    }
}

#[test]
fn a_redacted_secret_stays_hidden_in_logs_and_debug_output() {
    #[derive(Debug)]
    struct Credentials {
        user: String,
        password: Redacted<String>,
        nested: Option<Redacted<Vec<u8>>>,
    }

    let logs = LogBuffer::default();
    let _capture = logs.capture();
    let credentials = Credentials {
        user: "alice".to_owned(),
        password: Redacted::new("SECRETPASSWORD".to_owned()),
        nested: Some(Redacted::new(b"SECRETBYTES".to_vec())),
    };
    tracing::info!(?credentials, "the whole struct");
    tracing::info!(password = ?credentials.password, "debug field");
    tracing::info!(password = %credentials.password, "display field");
    let printed = format!(
        "{credentials:?} {credentials:#?} {} {:?}",
        credentials.password, credentials.password
    );

    let logged = logs.contents();
    for text in [&logged, &printed] {
        for secret in ["SECRETPASSWORD", "SECRETBYTES", "83, 69, 67"] {
            assert!(!text.contains(secret), "{secret} leaked:\n{text}");
        }
        assert!(text.contains("[redacted]"), "{text}");
        assert!(text.contains("alice"), "plain fields still print: {text}");
    }
    assert_eq!(credentials.user, "alice");
    assert_eq!(credentials.password.expose(), "SECRETPASSWORD");
    assert_eq!(
        credentials.nested.map(Redacted::into_inner),
        Some(b"SECRETBYTES".to_vec())
    );
}

#[test]
fn urls_are_scrubbed_before_they_are_logged_or_put_in_an_error() {
    assert_eq!(
        security::redact_url("https://user:pw@immich.local:2283/api/x?key=abc&page=2#top"),
        "https://immich.local:2283/api/x?key=[redacted]&page=[redacted]"
    );
    for hostile in [
        "http://admin:hunter2@10.0.0.5/a",
        "https://token@host.example/a?apiKey=SECRETKEY",
        "https://host.example/a?x=SECRETKEY#SECRETFRAGMENT",
        "ftp://u:hunter2@host.example:2121/",
        "not a url but has ?secret=SECRETKEY and u:hunter2@host",
    ] {
        let clean = security::redact_url(hostile);
        for secret in ["hunter2", "SECRETKEY", "SECRETFRAGMENT"] {
            assert!(!clean.contains(secret), "{hostile} -> {clean}");
        }
    }
    let sentence =
        "GET https://u:hunter2@a.example/x?k=SECRETKEY failed, then http://b.example/y?t=SECRETKEY";
    let clean = security::redact_urls_in(sentence);
    assert!(
        !clean.contains("hunter2") && !clean.contains("SECRETKEY"),
        "{clean}"
    );
    assert!(
        clean.contains("a.example") && clean.contains("b.example"),
        "{clean}"
    );
    assert!(
        clean.starts_with("GET ") && clean.contains(" failed, then "),
        "{clean}"
    );
}

// ----------------------------------------------------------------------------
// The frontend shell under the policy
// ----------------------------------------------------------------------------

/// The attributes of a start tag (already lower-cased) as (name, value) pairs.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = tag.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < chars.len() && !chars[at].is_whitespace() {
        at += 1;
    }
    while at < chars.len() {
        while at < chars.len() && (chars[at].is_whitespace() || chars[at] == '/') {
            at += 1;
        }
        let name_start = at;
        while at < chars.len() && !matches!(chars[at], '=' | '/') && !chars[at].is_whitespace() {
            at += 1;
        }
        let name: String = chars[name_start..at].iter().collect();
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        let mut value = String::new();
        if at < chars.len() && chars[at] == '=' {
            at += 1;
            while at < chars.len() && chars[at].is_whitespace() {
                at += 1;
            }
            let quote = chars.get(at).copied().filter(|c| matches!(c, '"' | '\''));
            if quote.is_some() {
                at += 1;
            }
            let value_start = at;
            while at < chars.len()
                && match quote {
                    Some(quote) => chars[at] != quote,
                    None => !chars[at].is_whitespace(),
                }
            {
                at += 1;
            }
            value = chars[value_start..at].iter().collect();
            if quote.is_some() {
                at += 1;
            }
        }
        if !name.is_empty() {
            found.push((name, value));
        }
    }
    found
}

/// What the app's policy would block in `html`. A small scan, not an HTML
/// parser: the shell is a handful of lines, and the aim is to notice an inline
/// script or style, an event-handler attribute or a remote URL sneaking in.
fn policy_violations(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut problems = Vec::new();
    let mut rest = lower.as_str();
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        if let Some(comment) = rest.strip_prefix("!--") {
            rest = comment.split_once("-->").map_or("", |(_, after)| after);
            continue;
        }
        let Some(close) = rest.find('>') else {
            problems.push("a tag is never closed".to_owned());
            break;
        };
        let tag = &rest[..close];
        rest = &rest[close + 1..];
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        let attrs = attributes(tag);
        match name {
            "script" if !attrs.iter().any(|(attr, _)| attr == "src") => {
                problems.push("an inline <script> (script-src is 'self')".to_owned());
            }
            "style" => problems.push("a <style> element (style-src is 'self')".to_owned()),
            "base" => problems.push("a <base> element (base-uri is 'none')".to_owned()),
            "object" | "embed" | "applet" => {
                problems.push(format!("an <{name}> element (object-src is 'none')"));
            }
            _ => {}
        }
        for (attr, value) in &attrs {
            if attr.len() > 2 && attr.starts_with("on") {
                problems.push(format!("<{name} {attr}=...>: an inline event handler"));
            }
            let is_url = matches!(
                attr.as_str(),
                "src" | "href" | "action" | "formaction" | "data" | "poster" | "srcset"
            );
            let value = value.trim();
            if is_url && value.starts_with("javascript:") {
                problems.push(format!("<{name} {attr}={value}>: a javascript: URL"));
            }
            let remote = ["http:", "https:", "//", "ws:", "wss:"];
            if is_url && remote.iter().any(|scheme| value.starts_with(scheme)) {
                problems.push(format!("<{name} {attr}={value}>: a remote URL"));
            }
        }
    }
    problems
}

#[test]
fn the_policy_scan_flags_what_the_policy_blocks_and_nothing_else() {
    for (what, html) in [
        ("an inline script", "<script>alert(1)</script>"),
        ("a style element", "<style>body{}</style>"),
        ("a handler", "<img src=\"/a.png\" onerror=\"x()\">"),
        ("a handler in odd case", "<BODY OnLoad=x()>"),
        ("a javascript url", "<a href=\"javascript:void(0)\">x</a>"),
        (
            "a remote script",
            "<script src=\"https://cdn.example/x.js\"></script>",
        ),
        (
            "a protocol-relative url",
            "<link rel=\"stylesheet\" href=\"//cdn.example/x.css\">",
        ),
        ("a base element", "<base href=\"/\">"),
        ("an object", "<object data=\"/x.swf\"></object>"),
        ("an unquoted remote url", "<img src=http://x.example/a.png>"),
    ] {
        assert!(!policy_violations(html).is_empty(), "{what} went unnoticed");
    }

    let fine = concat!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
        "<link rel=\"stylesheet\" crossorigin href=\"/assets/a.css\">",
        "<script type=\"module\" crossorigin src=\"/assets/a.js\"></script></head>",
        "<body style=\"margin:0\"><apm-app></apm-app>",
        "<!-- <script>never runs</script> --></body></html>"
    );
    assert_eq!(policy_violations(fine), Vec::<String>::new());
    assert_eq!(policy_violations(INDEX_HTML), Vec::<String>::new());
}

fn frontend_index() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} must exist: {err}", path.display()))
}

#[test]
fn the_real_frontend_shell_complies_with_the_policy() {
    let html = frontend_index();
    let problems = policy_violations(&html);
    assert!(
        problems.is_empty(),
        "web/index.html does not fit the Content-Security-Policy the server sends:\n{}",
        problems.join("\n")
    );
    assert!(
        html.contains("<script") && html.contains("<apm-app"),
        "the scan looked at something that is not the app's shell"
    );
}

#[tokio::test]
async fn the_real_frontend_shell_is_served_under_the_policy() {
    let html = frontend_index();
    let app = app_allowing(&[]).await;
    let web = app.state.config.web_dir.clone();
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), &html).unwrap();

    for path in ["/", "/albums/42", "/duplicates"] {
        let reply = send(&app.router, browser("GET", path)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert!(reply.is_html(), "{path}");
        assert_eq!(reply.text(), html, "{path}");
        assert_hardened(&reply, path);
    }
}

// ----------------------------------------------------------------------------
// Over a real socket
// ----------------------------------------------------------------------------

/// A server on a free local port, stopped when dropped.
struct LiveServer {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl LiveServer {
    async fn start(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self { addr, task }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Writes `raw`, then reads until the server closes the connection (every
    /// request here asks for `Connection: close`). The client never sends
    /// more than the test wants the server to see.
    async fn exchange(&self, raw: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let _ = stream.write_all(raw.as_bytes()).await;
        let mut answer = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer));
        assert!(
            read.await.is_ok(),
            "no answer within 5 s; so far: {}",
            String::from_utf8_lossy(&answer)
        );
        String::from_utf8_lossy(&answer).into_owned()
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn status_of(answer: &str) -> u16 {
    answer
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// Everything after the blank line that ends the headers.
fn body_of(answer: &str) -> &str {
    answer.split_once("\r\n\r\n").map_or("", |(_, body)| body)
}

/// The header block of an answer, lower-cased.
fn head_of(answer: &str) -> String {
    let head = answer
        .split_once("\r\n\r\n")
        .map_or(answer, |(head, _)| head);
    head.to_ascii_lowercase()
}

#[tokio::test]
async fn the_host_check_holds_on_a_real_connection() {
    let app = app_allowing(&[]).await;
    let server = LiveServer::start(app.router.clone()).await;
    let port = server.port();
    let get = |lines: &str| format!("GET /api/health HTTP/1.1\r\n{lines}Connection: close\r\n\r\n");

    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        "LocalHost".to_owned(),
    ] {
        let answer = server.exchange(&get(&format!("Host: {host}\r\n"))).await;
        assert_eq!(status_of(&answer), 200, "{host}: {answer}");
        let head = head_of(&answer);
        assert!(head.contains("\r\nx-request-id: "), "{host}: {head}");
        assert_eq!(
            head.matches("\r\ncontent-security-policy: ").count(),
            1,
            "{host}: {head}"
        );
    }

    let other_name_with_our_port = format!("Host: evil.example:{port}\r\n");
    for (what, lines) in [
        ("another name", "Host: evil.example\r\n"),
        (
            "another name on our port",
            other_name_with_our_port.as_str(),
        ),
        (
            "two equal Host headers",
            "Host: localhost\r\nHost: localhost\r\n",
        ),
        (
            "an allowed Host followed by a foreign one",
            "Host: localhost\r\nHost: evil.example\r\n",
        ),
        ("an empty Host", "Host: \r\n"),
    ] {
        let answer = server.exchange(&get(lines)).await;
        assert_eq!(status_of(&answer), 403, "{what}: {answer}");
        assert!(body_of(&answer).contains("host_not_allowed"), "{what}");
    }

    // HTTP/1.1 without Host is malformed (hyper may say so itself); HTTP/1.0
    // may omit it, and then it is ours to refuse.
    let no_host = server.exchange(&get("")).await;
    assert!(
        matches!(status_of(&no_host), 400 | 403),
        "no Host: {no_host}"
    );
    let old = server.exchange("GET /api/health HTTP/1.0\r\n\r\n").await;
    assert_eq!(status_of(&old), 403, "{old}");

    // The authority of an absolute-form target must agree with Host.
    let target = format!(
        "GET http://evil.example/api/health HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    );
    let answer = server.exchange(&target).await;
    assert_eq!(status_of(&answer), 403, "{answer}");

    // A page request gets the page, with its own policy.
    let page = server
        .exchange("GET / HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n")
        .await;
    assert_eq!(status_of(&page), 403, "{page}");
    assert!(head_of(&page).contains("text/html"), "{page}");
    assert!(
        head_of(&page)
            .contains("content-security-policy: default-src 'none'; style-src 'unsafe-inline'"),
        "{page}"
    );
}

#[tokio::test]
async fn the_csrf_check_and_the_missing_cors_hold_on_a_real_connection() {
    let app = app_allowing(&[]).await;
    let server = LiveServer::start(app.router.clone()).await;
    let port = server.port();
    let post = |extra: &str| {
        format!(
            "POST /api/health HTTP/1.1\r\nHost: localhost:{port}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
        )
    };

    for (what, extra) in [
        ("no marker", ""),
        ("a wrong marker", "x-apm-request: yes\r\n"),
        (
            "a foreign origin",
            "x-apm-request: 1\r\nOrigin: http://evil.example\r\n",
        ),
        ("an opaque origin", "x-apm-request: 1\r\nOrigin: null\r\n"),
        (
            "a cross-site fetch",
            "x-apm-request: 1\r\nSec-Fetch-Site: cross-site\r\n",
        ),
        (
            "a same-site fetch",
            "x-apm-request: 1\r\nSec-Fetch-Site: same-site\r\n",
        ),
    ] {
        let answer = server.exchange(&post(extra)).await;
        assert_eq!(status_of(&answer), 403, "{what}: {answer}");
        assert!(body_of(&answer).contains("csrf_rejected"), "{what}");
    }

    // What the app itself sends passes the stack; the route then says that
    // /api/health only reads.
    let own = post(&format!(
        "x-apm-request: 1\r\nOrigin: http://localhost:{port}\r\nSec-Fetch-Site: same-origin\r\n"
    ));
    let answer = server.exchange(&own).await;
    assert_eq!(status_of(&answer), 405, "{answer}");
    assert!(body_of(&answer).contains("method_not_allowed"), "{answer}");

    // A browser's preflight gets no permission, whoever asks.
    let preflight = format!(
        "OPTIONS /api/health HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: x-apm-request\r\nConnection: close\r\n\r\n"
    );
    let answer = server.exchange(&preflight).await;
    assert_eq!(status_of(&answer), 405, "{answer}");
    assert!(!head_of(&answer).contains("access-control-"), "{answer}");
}

#[tokio::test]
async fn an_oversized_upload_is_refused_before_its_body_is_sent() {
    let app = app_allowing(&[]).await;
    let server = LiveServer::start(app.router.clone()).await;
    let port = server.port();

    // The headers promise 5 MiB and nothing follows. A stack that waited for
    // the body would sit there until the client gives up.
    let started = Instant::now();
    let answer = server
        .exchange(&format!(
            "POST /api/health HTTP/1.1\r\nHost: localhost:{port}\r\nx-apm-request: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            5 * MIB
        ))
        .await;
    assert_eq!(status_of(&answer), 413, "{answer}");
    assert!(body_of(&answer).contains("payload_too_large"), "{answer}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "answered after {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_chunked_upload_without_a_length_is_cut_off_at_the_limit_on_a_real_connection() {
    let probe = Probe::default();
    let server = LiveServer::start(body_router(&probe)).await;
    let port = server.port();

    // No terminating chunk: the body is still open when the limit trips.
    let chunk = "x".repeat(2 * KIB);
    let answer = server
        .exchange(&format!(
            "POST /sink HTTP/1.1\r\nHost: localhost:{port}\r\nx-apm-request: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n",
            chunk.len()
        ))
        .await;
    assert_eq!(status_of(&answer), 413, "{answer}");
    assert!(body_of(&answer).contains("payload_too_large"), "{answer}");
    assert_eq!(probe.hits(), 0, "the handler never saw the body");
}

#[tokio::test]
async fn a_panicking_handler_does_not_cost_the_connection_or_the_server() {
    let router = Router::new()
        .route("/boom", get(panics_with_a_str))
        .route("/ok", get(ok))
        .layer(security::layers(SecurityOptions::default()));
    let server = LiveServer::start(router).await;
    let port = server.port();
    let get = |path: &str| {
        format!("GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")
    };

    let boom = server.exchange(&get("/boom")).await;
    assert_eq!(status_of(&boom), 500, "{boom}");
    assert!(body_of(&boom).contains("\"internal\""), "{boom}");
    assert!(!boom.contains("boom-in-handler"), "{boom}");
    assert!(head_of(&boom).contains("\r\nx-request-id: "), "{boom}");

    let fine = server.exchange(&get("/ok")).await;
    assert_eq!(status_of(&fine), 200, "{fine}");
    assert_eq!(body_of(&fine), "ok");
}

#[tokio::test]
async fn head_and_get_agree_on_the_length_on_a_real_connection() {
    let app = app_allowing(&[]).await;
    write_site(&app);
    let server = LiveServer::start(app.router.clone()).await;
    let port = server.port();
    let ask = |method: &str| {
        format!(
            "{method} /assets/app-abc123.js HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
        )
    };
    let length = |answer: &str| {
        head_of(answer).lines().find_map(|line| {
            line.strip_prefix("content-length:")
                .map(|v| v.trim().to_owned())
        })
    };

    let get = server.exchange(&ask("GET")).await;
    let head = server.exchange(&ask("HEAD")).await;
    assert_eq!(status_of(&get), 200, "{get}");
    assert_eq!(status_of(&head), 200, "{head}");
    assert_eq!(body_of(&get), "console.log('app');");
    assert_eq!(body_of(&head), "");
    assert_eq!(length(&get).as_deref(), Some("19"), "{get}");
    assert_eq!(length(&head).as_deref(), Some("19"), "{head}");
    assert!(
        head_of(&head).contains("content-security-policy: "),
        "{head}"
    );
}

// ----------------------------------------------------------------------------
// Volume paths, through the public API
// ----------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_volume_never_follows_a_link_and_never_names_the_host_path() {
    let dir = common::scratch_dir();
    let library = dir.path().join("library");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(library.join("trips")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(library.join("trips/a.jpg"), b"jpeg").unwrap();
    std::fs::write(outside.join("secret.txt"), SECRET).unwrap();
    std::os::unix::fs::symlink(&outside, library.join("escape")).unwrap();
    std::os::unix::fs::symlink(library.join("trips/a.jpg"), library.join("same-volume.jpg"))
        .unwrap();
    std::os::unix::fs::symlink(library.join("gone"), library.join("dangling")).unwrap();

    let volume = VolumeRoot::new(7, &library).unwrap();
    assert_eq!(volume.id(), 7);
    let rel = |text: &str| RelPath::new(text).unwrap();

    let file = volume.resolve_existing(&rel("trips/a.jpg")).unwrap();
    assert!(file.is_absolute());
    let mut content = String::new();
    io::Read::read_to_string(
        &mut volume.open_read(&rel("trips/a.jpg")).unwrap(),
        &mut content,
    )
    .unwrap();
    assert_eq!(content, "jpeg");

    for link in ["escape", "escape/secret.txt", "same-volume.jpg", "dangling"] {
        assert!(
            matches!(
                volume.resolve_existing(&rel(link)),
                Err(FsError::SymlinkRejected(_))
            ),
            "{link}"
        );
        assert!(volume.open_read(&rel(link)).is_err(), "{link}");
    }
    assert!(matches!(
        volume.resolve_for_create(&rel("escape/new.txt")),
        Err(FsError::SymlinkRejected(_))
    ));
    assert!(matches!(
        volume.resolve_existing(&rel("missing.jpg")),
        Err(FsError::NotFound(_))
    ));

    // The root itself may be a link (a bind mount); nothing below it may be.
    let linked_root = dir.path().join("linked-root");
    std::os::unix::fs::symlink(&library, &linked_root).unwrap();
    let through_link = VolumeRoot::new(8, &linked_root).unwrap();
    assert!(through_link.resolve_existing(&rel("trips/a.jpg")).is_ok());
    assert!(matches!(
        through_link.resolve_existing(&rel("escape/secret.txt")),
        Err(FsError::SymlinkRejected(_))
    ));

    // What a client is told is about the volume's own paths.
    let host_dir = dir.path().to_string_lossy().into_owned();
    for (err, status, code) in [
        (
            FsError::SymlinkRejected(rel("escape")),
            403,
            "symlink_rejected",
        ),
        (FsError::NotFound(rel("trips/b.jpg")), 404, "not_found"),
        (FsError::OutsideRoot, 403, "outside_volume"),
    ] {
        let shown = err.to_string();
        assert!(!shown.contains(&host_dir), "{shown}");
        let app_error = AppError::from(err);
        assert_eq!(app_error.status().as_u16(), status);
        assert_eq!(app_error.code(), code);
        assert!(!app_error.message().contains(&host_dir));
    }
}
