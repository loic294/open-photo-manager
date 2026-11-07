//! Static files and the single-page-app fallback.
//!
//! Serves the built web app from `APM_WEB_DIR`, a trusted directory baked into
//! the image (symlinks inside it are followed; the symlink policy is about the
//! volumes). A request is answered as follows:
//!
//! 1. Only `GET` and `HEAD` are served; anything else gets `405`.
//! 2. A path that could never name a legitimate asset gets a plain `404`:
//!    invalid percent-escapes or UTF-8, control characters (NUL included),
//!    backslashes, over-long names, and any segment starting with `.` (`..`,
//!    `.env`, `.git/config`, ...). Traversal (`/..%2f..%2fetc/passwd`) therefore
//!    never reaches the file system; `ServeDir` rejects `..` on its own as well.
//! 3. An existing file in the directory is served as is (its own caching
//!    headers; hashed assets keep them).
//! 4. Otherwise, the path is an app route and `index.html` is served, with
//!    `Cache-Control: no-cache` so an updated app is picked up, **only if** it is
//!    not below `/api` and its last non-empty segment has no extension. A
//!    missing `/assets/app.js` or `/favicon.ico` is a plain `404`, never HTML.
//!    A directory is not a file, so it is answered like a missing path.
//!
//! The consequence for the front end: the app's routes must not end in a
//! segment that contains a `.`, and user data (folder names!) must not be put
//! into URL path segments but into the query string.

use std::convert::Infallible;
use std::path::Path;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::Response;
use tower::Service;
use tower::ServiceExt;
use tower::service_fn;
use tower_http::services::{ServeDir, ServeFile};

use super::is_api_path;

const MAX_PATH_BYTES: usize = 2048;
const MAX_SEGMENT_BYTES: usize = 255;

/// The service to install with `Router::fallback_service`.
pub fn service(
    web_dir: &Path,
) -> impl Service<Request, Response = Response, Error = Infallible, Future: Send>
+ Clone
+ Send
+ Sync
+ 'static {
    let index = ServeFile::new(web_dir.join("index.html"));
    let fallback = service_fn(move |req: Request| {
        let index = index.clone();
        async move { Ok::<_, Infallible>(spa_fallback(index, req).await) }
    });
    let files = ServeDir::new(web_dir)
        .append_index_html_on_directories(false)
        .fallback(fallback);

    service_fn(move |req: Request| {
        let files = files.clone();
        async move {
            let head = req.method() == Method::HEAD;
            let mut response = match screen(&req) {
                Ok(path) => {
                    let mut response = match files.oneshot(req).await {
                        Ok(response) => response.map(Body::new),
                        Err(never) => match never {},
                    };
                    if path == "/index.html" {
                        mark_no_cache(&mut response);
                    }
                    response
                }
                Err(refusal) => refusal.into_response(),
            };
            if head {
                *response.body_mut() = Body::empty();
            }
            Ok::<_, Infallible>(response)
        }
    })
}

/// Why a request is refused before it reaches the file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    MethodNotAllowed,
    NotFound,
}

impl Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::MethodNotAllowed => method_not_allowed(),
            Self::NotFound => not_found(),
        }
    }
}

/// The decoded path of a request that may be served, or the refusal.
fn screen(req: &Request) -> Result<String, Refusal> {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Err(Refusal::MethodNotAllowed);
    }
    decode_path(req.uri().path()).ok_or(Refusal::NotFound)
}

/// Index for app routes, plain 404 for everything else that is not a file.
async fn spa_fallback(mut index: ServeFile, req: Request) -> Response {
    let Some(path) = decode_path(req.uri().path()) else {
        return not_found();
    };
    if !is_app_route(&path) || is_api_path(req.uri().path()) {
        return not_found();
    }
    let mut response = match index.call(req).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    };
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        mark_no_cache(&mut response);
    }
    response
}

fn mark_no_cache(response: &mut Response) {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
}

/// A path that is a client-side route: not `/api`, and the last non-empty
/// segment has no extension. Expects a decoded path.
fn is_app_route(decoded: &str) -> bool {
    if is_api_path(decoded) {
        return false;
    }
    decoded
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .is_none_or(|last| !last.contains('.'))
}

/// The percent-decoded path, or `None` when it must not be served at all.
fn decode_path(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw)?;
    if decoded.len() > MAX_PATH_BYTES || decoded.chars().any(|c| c.is_control() || c == '\\') {
        return None;
    }
    let acceptable = decoded
        .split('/')
        .all(|segment| segment.len() <= MAX_SEGMENT_BYTES && !segment.starts_with('.'));
    acceptable.then_some(decoded)
}

/// Strict percent-decoding: malformed escapes and invalid UTF-8 give `None`.
fn percent_decode(raw: &str) -> Option<String> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let mut bytes = raw.bytes();
    let mut out = Vec::with_capacity(raw.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = hex(bytes.next()?)?;
            let low = hex(bytes.next()?)?;
            out.push(high << 4 | low);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
}

fn not_found() -> Response {
    let mut response = Response::new(Body::from("not found\n"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn method_not_allowed() -> Response {
    let mut response = Response::new(Body::from("method not allowed\n"));
    *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    let headers = response.headers_mut();
    headers.insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::header::LAST_MODIFIED;

    use super::*;
    use crate::security::test_support::scratch_dir;

    /// A web directory with `index.html`, `assets/app.js`, `.env`, and a secret
    /// file next to (outside) it.
    struct Site {
        _dir: tempfile::TempDir,
        web: std::path::PathBuf,
    }

    fn site() -> Site {
        let dir = scratch_dir();
        let web = dir.path().join("web");
        std::fs::create_dir_all(web.join("assets")).unwrap();
        std::fs::write(web.join("index.html"), "<!doctype html><title>app</title>").unwrap();
        std::fs::write(web.join("assets").join("app.js"), "console.log(1)").unwrap();
        std::fs::write(web.join("favicon.svg"), "<svg/>").unwrap();
        std::fs::write(web.join(".env"), "TOKEN=hunter2").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "TOP-SECRET").unwrap();
        Site { _dir: dir, web }
    }

    async fn get(site: &Site, method: Method, uri: &str) -> (StatusCode, HeaderMapCopy, String) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = service(&site.web).oneshot(req).await.unwrap();
        let status = response.status();
        let headers = HeaderMapCopy(response.headers().clone());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    struct HeaderMapCopy(axum::http::HeaderMap);

    impl HeaderMapCopy {
        fn get(&self, name: impl axum::http::header::AsHeaderName) -> Option<&str> {
            self.0.get(name).and_then(|value| value.to_str().ok())
        }
    }

    #[test]
    fn decoding_is_strict() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/%E2%82%AC").as_deref(), Some("/€"));
        assert_eq!(percent_decode("/%zz"), None);
        assert_eq!(percent_decode("/%2"), None);
        assert_eq!(percent_decode("/%"), None);
        assert_eq!(percent_decode("/%ff"), None, "invalid UTF-8");
        assert_eq!(decode_path("/%00"), None);
        assert_eq!(decode_path("/a%0Ab"), None);
        assert_eq!(decode_path("/a%5Cb"), None, "backslash");
        assert_eq!(decode_path("/%2e%2e/x"), None);
        assert_eq!(decode_path("/a/.git/config"), None);
        assert_eq!(decode_path("/a%2f..%2fb"), None);
        assert_eq!(decode_path(&format!("/{}", "a".repeat(256))), None);
        assert_eq!(decode_path(&format!("/{}", "a/".repeat(2000))), None);
        assert_eq!(decode_path("/a/b.c").as_deref(), Some("/a/b.c"));
        assert_eq!(decode_path("//").as_deref(), Some("//"));
    }

    #[test]
    fn app_routes_are_extensionless_non_api_paths() {
        for path in [
            "/",
            "//",
            "/browse",
            "/browse/",
            "/browse/2024/Trip",
            "/a/b/c",
        ] {
            assert!(is_app_route(path), "{path}");
        }
        for path in [
            "/app.js",
            "/assets/app.js",
            "/favicon.ico",
            "/a/b.c",
            "/a.b/",
            "/api",
            "/api/",
            "/api/anything",
        ] {
            assert!(!is_app_route(path), "{path}");
        }
    }

    #[tokio::test]
    async fn existing_files_are_served_with_their_own_headers() {
        let site = site();
        let (status, headers, body) = get(&site, Method::GET, "/assets/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "console.log(1)");
        assert!(headers.get(CONTENT_TYPE).unwrap().contains("javascript"));
        assert!(headers.get(LAST_MODIFIED).is_some());
        assert_eq!(
            headers.get(CACHE_CONTROL),
            None,
            "assets keep their headers"
        );

        let (status, _, body) = get(&site, Method::GET, "/favicon.svg").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "<svg/>"));
    }

    #[tokio::test]
    async fn app_routes_get_the_index_that_must_be_revalidated() {
        let site = site();
        for path in [
            "/",
            "/browse",
            "/browse/",
            "/browse/2024/Trip",
            "/assets",
            "/assets/",
        ] {
            let (status, headers, body) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(body.contains("<title>app</title>"), "{path}");
            assert_eq!(headers.get(CACHE_CONTROL), Some("no-cache"), "{path}");
            assert!(
                headers.get(CONTENT_TYPE).unwrap().starts_with("text/html"),
                "{path}"
            );
        }
        let (status, headers, _) = get(&site, Method::GET, "/index.html").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get(CACHE_CONTROL), Some("no-cache"));
    }

    #[tokio::test]
    async fn head_requests_work_and_have_no_body() {
        let site = site();
        let (status, headers, body) = get(&site, Method::HEAD, "/browse").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "");
        assert_eq!(headers.get(CACHE_CONTROL), Some("no-cache"));
        let (status, _, body) = get(&site, Method::HEAD, "/assets/app.js").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, ""));
        for path in ["/missing.js", "/%00", "/.env"] {
            let (status, _, body) = get(&site, Method::HEAD, path).await;
            assert_eq!(
                (status, body.as_str()),
                (StatusCode::NOT_FOUND, ""),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_file_is_a_plain_404_not_html() {
        let site = site();
        for path in [
            "/assets/missing.js",
            "/missing.css",
            "/favicon.ico",
            "/a/b.png",
            "/app.js/",
        ] {
            let (status, headers, body) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                headers.get(CONTENT_TYPE),
                Some("text/plain; charset=utf-8"),
                "{path}"
            );
            assert!(!body.contains("<title>"), "{path}");
        }
    }

    #[tokio::test]
    async fn api_paths_never_get_the_index() {
        let site = site();
        for path in ["/api", "/api/", "/api/unknown", "/%61pi/unknown", "/%61pi"] {
            let (status, _, body) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(!body.contains("<title>"), "{path}");
        }
    }

    #[tokio::test]
    async fn other_methods_are_not_allowed() {
        let site = site();
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::PATCH,
        ] {
            let (status, headers, _) = get(&site, method.clone(), "/").await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(headers.get(ALLOW), Some("GET, HEAD"));
        }
    }

    #[tokio::test]
    async fn traversal_never_reaches_files_outside_the_web_dir() {
        let site = site();
        let attempts = [
            "/..%2fsecret.txt",
            "/..%2f..%2fsecret.txt",
            "/%2e%2e%2fsecret.txt",
            "/%2e%2e/secret.txt",
            "/../secret.txt",
            "/assets/../../secret.txt",
            "/assets/..%2f..%2fsecret.txt",
            "/%252e%252e%252fsecret.txt",
            "/..%5csecret.txt",
            "/..%c0%afsecret.txt",
            "/%2e%2e%2f%2e%2e%2fetc/passwd",
            "/..%2f..%2fetc/passwd",
            "//secret.txt",
            "//../secret.txt",
            "///etc/passwd",
            "/.%2e/secret.txt",
            "/secret.txt%00.js",
            "/%00",
            "/index.html%00",
        ];
        for attempt in attempts {
            let (status, _, body) = get(&site, Method::GET, attempt).await;
            assert!(!body.contains("TOP-SECRET"), "{attempt}: leaked");
            assert!(!body.contains("root:"), "{attempt}: leaked /etc/passwd");
            assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR, "{attempt}");
        }
    }

    #[tokio::test]
    async fn hidden_files_are_never_served() {
        let site = site();
        for path in [
            "/.env",
            "/%2eenv",
            "/assets/.env",
            "/.git/config",
            "/.well-known/x",
        ] {
            let (status, _, body) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(!body.contains("hunter2"), "{path}");
        }
    }

    #[tokio::test]
    async fn nul_bytes_and_junk_are_a_clean_404() {
        let site = site();
        for path in ["/%00", "/a%00b", "/%zz", "/%ff", "/a%0Ab", "/a%5Cb"] {
            let (status, _, _) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        let long = format!("/{}", "a".repeat(5000));
        let (status, _, _) = get(&site, Method::GET, &long).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let long_segment = format!("/{}.js", "a".repeat(300));
        let (status, _, _) = get(&site, Method::GET, &long_segment).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_missing_index_is_a_404() {
        let dir = scratch_dir();
        let site = Site {
            web: dir.path().join("web"),
            _dir: dir,
        };
        for path in ["/", "/browse"] {
            let (status, _, _) = get(&site, Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }
}
