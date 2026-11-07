//! The self-contained HTML page for a refused `Host` on a non-API path.
//!
//! It is the one place where untrusted input (the `Host` the client used) is
//! reflected into HTML, so the value is escaped and truncated, and the page is
//! served with its own strict Content-Security-Policy: no scripts, no external
//! resources, only the inline `<style>` below.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;

use super::host::Rejection;

/// What the page itself needs: its inline style and nothing else.
const PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// Longest `Host` value reflected into the page.
const MAX_SHOWN_CHARS: usize = 80;

const STYLE: &str = "\
body{font:16px/1.5 system-ui,sans-serif;max-width:42rem;margin:3rem auto;padding:0 1rem;\
color:#1f2933;background:#fff}\
h1{font-size:1.5rem}\
pre{background:#f0f2f5;padding:.75rem 1rem;border-radius:.4rem;overflow-x:auto}\
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}\
@media (prefers-color-scheme:dark){body{background:#14181d;color:#e4e7eb}\
pre{background:#222a33}}";

/// Escapes the five characters that matter in HTML text and attribute values.
pub(super) fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn shown(raw: &str) -> String {
    let mut text: String = raw.chars().take(MAX_SHOWN_CHARS).collect();
    if raw.chars().count() > MAX_SHOWN_CHARS {
        text.push('…');
    }
    escape_html(&text)
}

/// The 403 page for a refused `Host`.
pub(super) fn host_not_allowed(rejection: &Rejection) -> Response {
    let mut response = Response::new(render(rejection).into());
    *response.status_mut() = StatusCode::FORBIDDEN;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_CSP),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn render(rejection: &Rejection) -> String {
    let used = match &rejection.shown {
        Some(host) => format!("You reached this server as <code>{}</code>", shown(host)),
        None => "This request has no usable <code>Host</code> header".to_owned(),
    };

    let how_to_fix = match &rejection.suggestion {
        Some(host) => format!(
            "<h2>How to fix it</h2>\n\
             <p>Add the name you use to the <code>environment</code> section of this app in \
             your <code>compose.yml</code>:</p>\n\
             <pre><code>environment:\n  - APM_ALLOWED_HOSTS={}</code></pre>\n\
             <p>Several names are separated by commas (<code>nas.local,photos.example.com</code>); \
             <code>*.example.com</code> allows every sub-domain; add <code>:port</code> to \
             restrict a name to one port.</p>\n\
             <p>Then restart the app so it reads the new value: run \
             <code>docker compose up -d</code> next to <code>compose.yml</code>, or on a \
             Synology NAS open <em>Container Manager</em> &rarr; <em>Project</em> and \
             stop, then start the project.</p>",
            escape_html(host)
        ),
        None => "<h2>How to fix it</h2>\n\
             <p>Open the app through its IP address, <code>localhost</code> or a name listed in \
             <code>APM_ALLOWED_HOSTS</code>. If a reverse proxy sits in front of it, it must \
             forward the original <code>Host</code> header (nginx: \
             <code>proxy_set_header Host $http_host;</code>).</p>"
            .to_owned(),
    };

    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Host name not allowed</title>\n\
         <style>{STYLE}</style>\n\
         </head>\n\
         <body>\n\
         <h1>This host name is not allowed</h1>\n\
         <p>{used}. This server only answers requests addressed to <code>localhost</code>, \
         to an IP address or to a name listed in <code>APM_ALLOWED_HOSTS</code> \
         ({reason}).</p>\n\
         <p>This protects your photos from web pages that try to reach this server through \
         your browser (DNS rebinding).</p>\n\
         {how_to_fix}\n\
         </body>\n\
         </html>\n",
        reason = escape_html(rejection.reason.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::super::host::Reason;
    use super::*;

    async fn page(rejection: &Rejection) -> (Response, String) {
        let response = host_not_allowed(rejection);
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        (Response::from_parts(parts, axum::body::Body::empty()), text)
    }

    fn rejection(reason: Reason, shown: Option<&str>, suggestion: Option<&str>) -> Rejection {
        Rejection {
            reason,
            shown: shown.map(str::to_owned),
            suggestion: suggestion.map(str::to_owned),
        }
    }

    #[test]
    fn escaping_covers_the_html_metacharacters() {
        assert_eq!(
            escape_html("<script>alert(\"x\" & 'y')</script>"),
            "&lt;script&gt;alert(&quot;x&quot; &amp; &#39;y&#39;)&lt;/script&gt;"
        );
        assert_eq!(
            escape_html("plain-text.example:80"),
            "plain-text.example:80"
        );
    }

    #[tokio::test]
    async fn the_page_is_a_strict_self_contained_403() {
        let (response, text) = page(&rejection(
            Reason::NotAllowed,
            Some("Photos.Example:8080"),
            Some("photos.example"),
        ))
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let headers = response.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; style-src 'unsafe-inline'"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");

        assert!(text.contains("APM_ALLOWED_HOSTS=photos.example"), "{text}");
        assert!(text.contains("Photos.Example:8080"), "{text}");
        assert!(text.contains("docker compose up -d"), "{text}");
        assert!(text.contains("Container Manager"), "{text}");
        for forbidden in [
            "<script", "http://", "https://", "src=", "href=", "<link", "<img",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in the page");
        }
    }

    #[tokio::test]
    async fn a_reflected_host_is_escaped_and_truncated() {
        let hostile = "<script>alert(1)</script>";
        let (_, text) = page(&rejection(Reason::Malformed, Some(hostile), None)).await;
        assert!(!text.contains("<script>"), "{text}");
        assert!(
            text.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{text}"
        );
        assert!(
            !text.contains("APM_ALLOWED_HOSTS="),
            "no suggestion for junk"
        );

        let long = format!("\"{}", "a".repeat(500));
        let (_, text) = page(&rejection(Reason::Malformed, Some(&long), None)).await;
        assert!(!text.contains(&"a".repeat(100)), "value is truncated");
        assert!(text.contains("&quot;aaaa"), "quote escaped");
        assert!(text.contains('…'));
    }

    #[tokio::test]
    async fn a_missing_host_gets_generic_advice() {
        let (_, text) = page(&rejection(Reason::Missing, None, None)).await;
        assert!(text.contains("no usable"), "{text}");
        assert!(text.contains("reverse proxy"), "{text}");
        assert!(!text.contains("APM_ALLOWED_HOSTS="), "{text}");
    }
}
