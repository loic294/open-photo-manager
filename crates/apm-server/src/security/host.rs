//! The `Host` header: parsing, the allow-list and the rejection of everything
//! else.
//!
//! There is no authentication, so the browser of anyone on the LAN must not be
//! usable as a bridge to this server. The classic attack is DNS rebinding: a
//! hostile page at `attacker.example` makes its name resolve to the NAS and the
//! browser then happily sends same-origin requests to it, with
//! `Host: attacker.example`. A request is therefore served only when it is
//! addressed to
//!
//! - `localhost` (any port),
//! - an IP literal, v4 or v6 (any port): browsers never resolve literals through
//!   DNS, so a hostile name cannot be rebound onto one, or
//! - a name listed in `APM_ALLOWED_HOSTS`.
//!
//! The check covers every route: the API, server-sent events and the static
//! files.
//!
//! # `APM_ALLOWED_HOSTS`
//!
//! Comma- or whitespace-separated rules, matched case-insensitively:
//!
//! | Rule                  | Allows                                                        |
//! |-----------------------|---------------------------------------------------------------|
//! | `nas.local`           | `nas.local` on any port                                       |
//! | `nas.local:8080`      | `nas.local` on port 8080 only                                 |
//! | `*.example.com`       | any sub-domain of `example.com` (at least one label before the suffix; not `example.com` itself) |
//! | `*.example.com:8080`  | the same, on port 8080 only                                   |
//!
//! A trailing dot (`nas.local.`) is ignored everywhere. IP literals need no
//! rule. Behind a reverse proxy the proxy must forward the original `Host`
//! header (nginx: `proxy_set_header Host $http_host;`), because the same value
//! is compared with `Origin` by the CSRF check.
//!
//! # What a valid `Host` is
//!
//! Exactly one `Host` header (and, when the request line carries an absolute
//! URI, the same authority there), made of ASCII `[A-Za-z0-9._-]` labels of at
//! most 63 characters (253 in total) and an optional port in `1..=65535`, or a
//! bracketed IPv6 literal without zone id. Anything else (missing, duplicated,
//! userinfo, spaces, empty labels, a port such as `:0` or `:`) is rejected
//! before the allow-list is consulted.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{SecurityOptions, error_page, is_api_path};
use crate::error::AppError;

const MAX_NAME_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;
const MAX_AUTHORITY_LEN: usize = 300;

/// Why a string is not a valid `host[:port]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SyntaxError(&'static str);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HostName {
    /// Lowercase, without trailing dot, labels validated.
    Domain(String),
    Ip(IpAddr),
}

/// A validated `host[:port]`: the `Host` header of a request or the authority
/// of an `Origin`. Names are lowercase and have no trailing dot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostAuthority {
    host: HostName,
    port: Option<u16>,
}

impl HostAuthority {
    /// The port, when the authority names one.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// The host without the port (IPv6 literals in brackets).
    pub fn host(&self) -> String {
        match &self.host {
            HostName::Domain(name) => name.clone(),
            HostName::Ip(IpAddr::V4(ip)) => ip.to_string(),
            HostName::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
        }
    }

    /// Both are the same host and the same port, where a missing port stands
    /// for `default_port` (80 or 443 depending on the scheme).
    pub(super) fn same_origin(&self, other: &Self, default_port: u16) -> bool {
        self.host == other.host
            && self.port.unwrap_or(default_port) == other.port.unwrap_or(default_port)
    }

    fn is_localhost(&self) -> bool {
        matches!(&self.host, HostName::Domain(name) if name == "localhost")
    }

    fn is_ip(&self) -> bool {
        matches!(self.host, HostName::Ip(_))
    }
}

impl fmt::Display for HostAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.host())?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        Ok(())
    }
}

impl FromStr for HostAuthority {
    type Err = SyntaxError;

    fn from_str(raw: &str) -> Result<Self, SyntaxError> {
        if raw.is_empty() {
            return Err(SyntaxError("empty host"));
        }
        if raw.len() > MAX_AUTHORITY_LEN {
            return Err(SyntaxError("host too long"));
        }
        if !raw.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(SyntaxError(
                "host contains spaces, control or non-ASCII characters",
            ));
        }

        if let Some(rest) = raw.strip_prefix('[') {
            let (inner, tail) = rest
                .split_once(']')
                .ok_or(SyntaxError("unterminated IPv6 literal"))?;
            let ip = Ipv6Addr::from_str(inner).map_err(|_| SyntaxError("invalid IPv6 literal"))?;
            let port = match tail {
                "" => None,
                _ => {
                    let port = tail
                        .strip_prefix(':')
                        .ok_or(SyntaxError("unexpected text after IPv6 literal"))?;
                    Some(parse_port(port)?)
                }
            };
            return Ok(Self {
                host: HostName::Ip(IpAddr::V6(ip)),
                port,
            });
        }

        let (host, port) = match raw.rsplit_once(':') {
            Some((host, port)) => (host, Some(parse_port(port)?)),
            None => (raw, None),
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        let host = match Ipv4Addr::from_str(host) {
            Ok(ip) => HostName::Ip(IpAddr::V4(ip)),
            Err(_) => HostName::Domain(parse_domain(host)?),
        };
        Ok(Self { host, port })
    }
}

fn parse_domain(host: &str) -> Result<String, SyntaxError> {
    if host.is_empty() {
        return Err(SyntaxError("empty host name"));
    }
    if host.len() > MAX_NAME_LEN {
        return Err(SyntaxError("host name too long"));
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err(SyntaxError("empty label in host name"));
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(SyntaxError("label of the host name too long"));
        }
        let valid = label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !valid {
            return Err(SyntaxError("invalid character in host name"));
        }
    }
    Ok(host.to_ascii_lowercase())
}

fn parse_port(raw: &str) -> Result<u16, SyntaxError> {
    let digits = !raw.is_empty() && raw.len() <= 5 && raw.bytes().all(|b| b.is_ascii_digit());
    match raw.parse::<u16>() {
        Ok(port) if digits && port != 0 => Ok(port),
        _ => Err(SyntaxError("invalid port")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Exact(HostName),
    /// `.example.com`: a name that ends with it and has at least one label
    /// before it.
    Suffix(String),
}

/// One entry of `APM_ALLOWED_HOSTS`, see the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    pattern: Pattern,
    port: Option<u16>,
}

impl HostRule {
    /// Parses `host`, `host:port`, `*.domain` or `*.domain:port`.
    pub fn parse(raw: &str) -> Result<Self, SyntaxError> {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("*.") {
            let authority = rest.parse::<HostAuthority>()?;
            return match authority.host {
                HostName::Domain(suffix) => Ok(Self {
                    pattern: Pattern::Suffix(format!(".{suffix}")),
                    port: authority.port,
                }),
                HostName::Ip(_) => Err(SyntaxError("a wildcard needs a domain name")),
            };
        }
        if raw.starts_with('*') || raw.contains('*') {
            return Err(SyntaxError(
                "`*` is only allowed as a leading `*.` before a domain name",
            ));
        }
        let authority = raw.parse::<HostAuthority>()?;
        Ok(Self {
            pattern: Pattern::Exact(authority.host),
            port: authority.port,
        })
    }

    /// Whether a request addressed to `authority` is covered by this rule.
    pub fn matches(&self, authority: &HostAuthority) -> bool {
        if self.port.is_some_and(|port| authority.port != Some(port)) {
            return false;
        }
        match (&self.pattern, &authority.host) {
            (Pattern::Exact(expected), actual) => expected == actual,
            (Pattern::Suffix(suffix), HostName::Domain(name)) => {
                name.len() > suffix.len() && name.ends_with(suffix.as_str())
            }
            (Pattern::Suffix(_), HostName::Ip(_)) => false,
        }
    }
}

/// Splits an `APM_ALLOWED_HOSTS` value on commas and whitespace, validates every
/// entry and returns them lowercased. The error names the offending entry.
pub fn parse_allowed_hosts(raw: &str) -> Result<Vec<String>, String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            HostRule::parse(entry)
                .map(|_| entry.to_ascii_lowercase())
                .map_err(|err| format!("entry {entry:?}: {err}"))
        })
        .collect()
}

/// Whether the server may answer a request addressed to `authority`.
pub fn is_allowed(authority: &HostAuthority, rules: &[HostRule]) -> bool {
    authority.is_ip()
        || authority.is_localhost()
        || rules.iter().any(|rule| rule.matches(authority))
}

/// Why a request's `Host` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reason {
    Missing,
    Duplicated,
    Malformed,
    Mismatch,
    NotAllowed,
}

impl Reason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "no Host header",
            Self::Duplicated => "more than one Host header",
            Self::Malformed => "malformed Host",
            Self::Mismatch => "Host differs from the request URI authority",
            Self::NotAllowed => "host not in the allow-list",
        }
    }
}

/// A refused `Host`, with what the error page may show about it.
#[derive(Debug, Clone)]
pub(super) struct Rejection {
    pub reason: Reason,
    /// The value the client used, as sent (untrusted: escape before showing).
    pub shown: Option<String>,
    /// The line to add to `APM_ALLOWED_HOSTS`, only for a well-formed host.
    pub suggestion: Option<String>,
}

impl Rejection {
    fn new(reason: Reason, shown: Option<&str>) -> Self {
        Self {
            reason,
            shown: shown.map(str::to_owned),
            suggestion: None,
        }
    }
}

/// The validated authority a request is addressed to, or why it is refused.
pub(super) fn check(
    headers: &HeaderMap,
    uri: &Uri,
    rules: &[HostRule],
) -> Result<HostAuthority, Rejection> {
    let mut values = headers.get_all(HOST).iter();
    let header = values.next();
    if values.next().is_some() {
        return Err(Rejection::new(Reason::Duplicated, None));
    }
    let header_text = header.map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());

    let from_header = match header {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| SyntaxError("non-ASCII Host"))
                .and_then(HostAuthority::from_str)
                .map_err(|_| Rejection::new(Reason::Malformed, header_text.as_deref()))?,
        ),
    };
    let from_uri = match uri.authority() {
        None => None,
        Some(authority) => Some(
            authority
                .as_str()
                .parse::<HostAuthority>()
                .map_err(|_| Rejection::new(Reason::Malformed, Some(authority.as_str())))?,
        ),
    };

    let authority = match (from_header, from_uri) {
        (Some(header), Some(uri)) if header != uri => {
            return Err(Rejection::new(Reason::Mismatch, header_text.as_deref()));
        }
        (Some(authority), _) | (None, Some(authority)) => authority,
        (None, None) => return Err(Rejection::new(Reason::Missing, None)),
    };

    if is_allowed(&authority, rules) {
        Ok(authority)
    } else {
        let mut rejection = Rejection::new(Reason::NotAllowed, header_text.as_deref());
        rejection.suggestion = Some(authority.host());
        if rejection.shown.is_none() {
            rejection.shown = Some(authority.to_string());
        }
        Err(rejection)
    }
}

/// Middleware: refuses requests with a `Host` that is not allowed and stores
/// the validated [`HostAuthority`] in the request extensions for the CSRF check.
pub(super) async fn guard(
    State(options): State<Arc<SecurityOptions>>,
    mut req: Request,
    next: Next,
) -> Response {
    match check(req.headers(), req.uri(), &options.allowed_hosts) {
        Ok(authority) => {
            req.extensions_mut().insert(authority);
            next.run(req).await
        }
        Err(rejection) => {
            tracing::warn!(reason = rejection.reason.as_str(), "request refused");
            if is_api_path(req.uri().path()) {
                AppError::forbidden(
                    "host_not_allowed",
                    "this host name is not allowed: add it to APM_ALLOWED_HOSTS (and make a \
                     reverse proxy forward the original Host header)",
                )
                .into_response()
            } else {
                error_page::host_not_allowed(&rejection)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use proptest::prelude::*;

    use super::*;

    fn authority(raw: &str) -> HostAuthority {
        raw.parse().unwrap_or_else(|err| panic!("{raw:?}: {err}"))
    }

    fn rules(raw: &[&str]) -> Vec<HostRule> {
        raw.iter()
            .map(|rule| HostRule::parse(rule).unwrap())
            .collect()
    }

    fn allowed(host: &str, configured: &[&str]) -> bool {
        is_allowed(&authority(host), &rules(configured))
    }

    #[test]
    fn valid_authorities_are_normalised() {
        let cases = [
            ("localhost", "localhost"),
            ("LOCALHOST:8080", "localhost:8080"),
            ("localhost.", "localhost"),
            ("Nas.Local.:80", "nas.local:80"),
            ("127.0.0.1", "127.0.0.1"),
            ("127.0.0.1:3000", "127.0.0.1:3000"),
            ("192.168.1.20.", "192.168.1.20"),
            ("[::1]", "[::1]"),
            ("[::1]:8080", "[::1]:8080"),
            ("[FE80::1]", "[fe80::1]"),
            ("[::ffff:127.0.0.1]:1", "[::ffff:127.0.0.1]:1"),
            ("my_host-1.example.com", "my_host-1.example.com"),
            ("a", "a"),
            ("localhost:65535", "localhost:65535"),
        ];
        for (raw, expected) in cases {
            assert_eq!(authority(raw).to_string(), expected, "{raw}");
        }
    }

    #[test]
    fn malformed_authorities_are_rejected() {
        let bad = [
            "",
            " ",
            " localhost",
            "localhost ",
            "local host",
            "localhost:",
            "localhost:0",
            "localhost:65536",
            "localhost:+80",
            "localhost:-1",
            "localhost:80:80",
            "localhost:abc",
            ":80",
            "::1",
            "[::1",
            "[::1]x",
            "[::1]:",
            "[::1]:0",
            "[]",
            "[fe80::1%25eth0]",
            "[1.2.3.4]",
            "user@localhost",
            "user:pw@localhost",
            "localhost/path",
            "localhost?x",
            "localhost#x",
            "localhost\\",
            "http://localhost",
            "a..b",
            ".a",
            "a.b..",
            "..",
            ".",
            "a b.com",
            "ex\u{e4}mple.com",
            "<script>alert(1)</script>",
            "a%20b",
            "a\tb",
            "a\u{0}b",
        ];
        for raw in bad {
            assert!(
                raw.parse::<HostAuthority>().is_err(),
                "{raw:?} should be rejected"
            );
        }
        // IDNs arrive as punycode, which is plain ASCII.
        assert!("xn--bcher-kva.example".parse::<HostAuthority>().is_ok());
    }

    #[test]
    fn length_limits_apply() {
        let label = "a".repeat(63);
        assert!(format!("{label}.example").parse::<HostAuthority>().is_ok());
        let too_long_label = "a".repeat(64);
        assert!(
            format!("{too_long_label}.example")
                .parse::<HostAuthority>()
                .is_err()
        );

        let name = [label.as_str(); 3].join(".");
        assert!(name.parse::<HostAuthority>().is_ok());
        let huge = [label.as_str(); 5].join(".");
        assert!(huge.len() > MAX_NAME_LEN);
        assert!(huge.parse::<HostAuthority>().is_err());
    }

    #[test]
    fn built_in_hosts_are_localhost_and_ip_literals_on_any_port() {
        for host in [
            "localhost",
            "localhost:8080",
            "LocalHost:1",
            "localhost.",
            "127.0.0.1",
            "127.0.0.1:1234",
            "10.1.2.3",
            "192.168.68.50:8080",
            "8.8.8.8",
            "[::1]",
            "[::1]:8080",
            "[2001:db8::1]:443",
            "[::ffff:192.168.1.1]",
        ] {
            assert!(allowed(host, &[]), "{host}");
        }
        for host in [
            "evil.example",
            "localhost.evil.example",
            "sub.localhost",
            "127.0.0.1.evil.example",
            "nas.local",
            "0x7f.0.0.1",
            "2130706433",
            "127.1",
        ] {
            assert!(!allowed(host, &[]), "{host}");
        }
    }

    #[test]
    fn exact_rules_match_case_insensitively_on_any_port() {
        let configured = ["NAS.local", "photos.example.com:8080"];
        assert!(allowed("nas.local", &configured));
        assert!(allowed("NAS.LOCAL:9999", &configured));
        assert!(allowed("nas.local.", &configured));
        assert!(allowed("photos.example.com:8080", &configured));
        assert!(!allowed("photos.example.com", &configured), "port required");
        assert!(!allowed("photos.example.com:80", &configured));
        assert!(!allowed("photos.example.com:18080", &configured));
        assert!(!allowed("other.local", &configured));
        assert!(!allowed("xnas.local", &configured));
        assert!(!allowed("nas.local.evil.example", &configured));
        assert!(!allowed("evil.example", &configured));
    }

    #[test]
    fn wildcard_rules_need_a_label_before_the_suffix() {
        let configured = ["*.example.com"];
        assert!(allowed("nas.example.com", &configured));
        assert!(allowed("A.B.Example.COM:8080", &configured));
        assert!(allowed("nas.example.com.", &configured));
        assert!(!allowed("example.com", &configured));
        assert!(!allowed("notexample.com", &configured));
        assert!(!allowed("nas.example.com.evil.example", &configured));
        assert!(!allowed("evilexample.com", &configured));
        assert!(!allowed("nas.example.org", &configured));

        let with_port = ["*.example.com:8443"];
        assert!(allowed("nas.example.com:8443", &with_port));
        assert!(!allowed("nas.example.com", &with_port));
        assert!(!allowed("nas.example.com:443", &with_port));
    }

    #[test]
    fn ip_rules_are_accepted_and_harmless() {
        let configured = ["192.168.1.5", "[fd00::1]:8080"];
        assert!(allowed("192.168.1.5", &configured));
        assert!(allowed("[fd00::1]:8080", &configured));
        assert!(!allowed("nas.local", &configured));
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for rule in [
            "",
            "*",
            "*.",
            "*.*",
            "*foo.example.com",
            "foo.*.com",
            "foo.*",
            "**.example.com",
            "*.1.2.3.4:80x",
            "*.[::1]",
            "http://nas.local",
            "nas.local/path",
            "user@nas.local",
            "nas.local:0",
            "nas local",
            "nas..local",
        ] {
            assert!(HostRule::parse(rule).is_err(), "{rule:?}");
        }
    }

    #[test]
    fn config_values_split_on_commas_and_whitespace() {
        assert_eq!(
            parse_allowed_hosts(" NAS.local, photos.example.com:8080 ,,").unwrap(),
            vec!["nas.local", "photos.example.com:8080"]
        );
        assert_eq!(
            parse_allowed_hosts("a.example b.example\tc.example\nd.example,*.e.example").unwrap(),
            vec![
                "a.example",
                "b.example",
                "c.example",
                "d.example",
                "*.e.example"
            ]
        );
        assert!(parse_allowed_hosts("  ,, ").unwrap().is_empty());

        let err = parse_allowed_hosts("good.example, *, other.example").unwrap_err();
        assert!(err.contains("\"*\""), "{err}");
        let err = parse_allowed_hosts("http://nas.local").unwrap_err();
        assert!(err.contains("http://nas.local"), "{err}");
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(HOST, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn uri(raw: &str) -> Uri {
        raw.parse().unwrap()
    }

    #[test]
    fn check_requires_exactly_one_valid_allowed_host() {
        let rules = rules(&["nas.local"]);
        let ok = check(&headers(&["nas.local:8080"]), &uri("/x"), &rules).unwrap();
        assert_eq!(ok.to_string(), "nas.local:8080");

        let cases: [(&[&str], Reason); 6] = [
            (&[], Reason::Missing),
            (&["nas.local", "nas.local"], Reason::Duplicated),
            (&["nas.local", "evil.example"], Reason::Duplicated),
            (&["nas.local@evil.example"], Reason::Malformed),
            (&["<script>"], Reason::Malformed),
            (&["evil.example"], Reason::NotAllowed),
        ];
        for (values, reason) in cases {
            let rejection = check(&headers(values), &uri("/x"), &rules).unwrap_err();
            assert_eq!(rejection.reason, reason, "{values:?}");
        }
    }

    #[test]
    fn check_rejects_non_ascii_host_values() {
        let mut map = HeaderMap::new();
        map.insert(HOST, HeaderValue::from_bytes(b"caf\xe9.example").unwrap());
        let rejection = check(&map, &uri("/x"), &[]).unwrap_err();
        assert_eq!(rejection.reason, Reason::Malformed);
    }

    #[test]
    fn check_compares_the_uri_authority_with_the_header() {
        let rules = rules(&["nas.local"]);
        // Absolute-form request line with a matching Host header.
        let ok = check(
            &headers(&["NAS.local"]),
            &uri("http://nas.local/api/health"),
            &rules,
        );
        assert!(ok.is_ok());
        // HTTP/2 style: authority in the URI and no Host header.
        let ok = check(&headers(&[]), &uri("http://nas.local/api/health"), &rules);
        assert!(ok.is_ok());
        // A proxy-style request naming another host than the header.
        let rejection = check(
            &headers(&["localhost"]),
            &uri("http://evil.example/api/health"),
            &rules,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, Reason::Mismatch);
        // The URI authority alone is checked against the allow-list too.
        let rejection = check(&headers(&[]), &uri("http://evil.example/"), &rules).unwrap_err();
        assert_eq!(rejection.reason, Reason::NotAllowed);
        // Userinfo in the URI is refused.
        let rejection = check(
            &headers(&["nas.local"]),
            &uri("http://user@nas.local/"),
            &rules,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, Reason::Malformed);
    }

    #[test]
    fn a_rejection_suggests_only_well_formed_hosts_without_the_port() {
        let rejection = check(&headers(&["Photos.Example:8080"]), &uri("/"), &[]).unwrap_err();
        assert_eq!(rejection.reason, Reason::NotAllowed);
        assert_eq!(rejection.suggestion.as_deref(), Some("photos.example"));
        assert_eq!(rejection.shown.as_deref(), Some("Photos.Example:8080"));

        let rejection = check(&headers(&["<b>"]), &uri("/"), &[]).unwrap_err();
        assert_eq!(rejection.suggestion, None);
        assert_eq!(rejection.shown.as_deref(), Some("<b>"));
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_accepted_values_round_trip(raw in "\\PC{0,80}") {
            if let Ok(parsed) = raw.parse::<HostAuthority>() {
                let shown = parsed.to_string();
                prop_assert_eq!(shown.parse::<HostAuthority>().unwrap(), parsed);
            }
        }

        #[test]
        fn rule_parsing_never_panics(raw in "[ -~]{0,60}") {
            let _ = HostRule::parse(&raw);
            let _ = parse_allowed_hosts(&raw);
        }

        #[test]
        fn a_name_is_never_allowed_by_an_unrelated_rule(
            label in "[a-z][a-z0-9]{0,10}",
            other in "[a-z][a-z0-9]{0,10}",
        ) {
            prop_assume!(label != other);
            let rule = HostRule::parse(&format!("{label}.example")).unwrap();
            let host = authority(&format!("{other}.example"));
            prop_assert!(!rule.matches(&host));
        }
    }
}
