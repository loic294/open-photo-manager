//! Keeping secrets out of logs and API responses.
//!
//! - [`Redacted`] wraps a secret (API key, password, token). Its `Debug` and
//!   `Display` print `[redacted]`, and it deliberately has **no** `Serialize`
//!   implementation, so a secret cannot end up in a DTO, a task parameter or a
//!   JSON log field by accident. Read it with [`Redacted::expose`] at the one
//!   place that really needs the value (building an `Authorization` header,
//!   encrypting it for storage).
//! - [`redact_url`] and [`redact_urls_in`] scrub URLs before they are logged
//!   or placed in an error message: userinfo is removed, query values are
//!   replaced and the fragment is dropped. `AppError::internal` applies
//!   [`redact_urls_in`] to every cause it logs.
//!
//! Both are best effort for *accidents*; they do not make it safe to print a
//! value that was never meant to be shown.

use std::fmt;

use serde::de::{Deserialize, Deserializer, Error as _};

const PLACEHOLDER: &str = "[redacted]";
const MAX_QUERY_NAME_LEN: usize = 64;

/// A secret value that never prints itself.
///
/// ```
/// use apm_server::security::Redacted;
///
/// let key = Redacted::new(String::from("hunter2"));
/// assert_eq!(format!("{key:?} {key}"), "[redacted] [redacted]");
/// assert_eq!(key.expose(), "hunter2");
/// ```
///
/// There is no `Serialize` implementation on purpose: if a struct that holds a
/// `Redacted` field derives `Serialize` it fails to compile. `Deserialize` is
/// transparent so incoming secrets (a password in a request body) can be read
/// straight into a `Redacted<String>`; a failed deserialisation never echoes
/// the offending value.
///
/// Equality is constant-time in the content for byte-like secrets. Memory is
/// not zeroed on drop (no `zeroize` dependency in this workspace); keep the
/// lifetime of a secret short.
#[derive(Clone, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The secret itself. Name the call site on purpose: every use of this
    /// method is a place where the value can leak.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwraps the secret, consuming the wrapper.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: AsRef<[u8]>> Redacted<T> {
    /// Compares the secret with `candidate` without short-circuiting on the
    /// first differing byte (the length is not hidden).
    pub fn matches(&self, candidate: impl AsRef<[u8]>) -> bool {
        constant_time_eq(self.0.as_ref(), candidate.as_ref())
    }
}

impl<T> From<T> for Redacted<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PLACEHOLDER)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PLACEHOLDER)
    }
}

impl<T: AsRef<[u8]>> PartialEq for Redacted<T> {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(self.0.as_ref(), other.0.as_ref())
    }
}

impl<T: AsRef<[u8]>> Eq for Redacted<T> {}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Redacted<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The default serde errors quote the rejected value ("invalid type:
        // string \"hunter2\""), which would put the secret into a log line.
        T::deserialize(deserializer)
            .map(Self)
            .map_err(|_| D::Error::custom("invalid secret value"))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let difference = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(difference) == 0
}

/// Returns `url` with everything secret-prone hidden:
///
/// - userinfo (`user:password@`) is removed, up to the last `@` before the
///   first `/` (so an unescaped `?` or `#` inside a password does not leave
///   half of it behind);
/// - every query value becomes `[redacted]` (parameter names are kept, so the
///   log still says *which* parameter was present; a bare token without `=`
///   becomes `[redacted]`);
/// - the fragment is dropped.
///
/// The function is pure string processing: it accepts relative URLs and
/// arbitrary garbage and never panics. A password containing an unescaped `/`
/// is not valid in a URL and is not recognised as userinfo.
///
/// ```
/// use apm_server::security::redact_url;
///
/// assert_eq!(
///     redact_url("https://me:pw@immich.local:2283/api/x?key=abc&page=2#top"),
///     "https://immich.local:2283/api/x?key=[redacted]&page=[redacted]"
/// );
/// ```
pub fn redact_url(url: &str) -> String {
    let stripped = strip_userinfo(url);
    let without_fragment = stripped
        .split_once('#')
        .map_or(stripped.as_str(), |(head, _)| head);
    match without_fragment.split_once('?') {
        None => without_fragment.to_owned(),
        Some((base, query)) => {
            let mut out = String::with_capacity(without_fragment.len());
            out.push_str(base);
            let pairs = redact_query(query);
            if !pairs.is_empty() {
                out.push('?');
                out.push_str(&pairs);
            }
            out
        }
    }
}

/// Applies [`redact_url`] to every `scheme://...` URL found inside free text
/// (an error message, a `Display` of a client error). The rest of the text is
/// returned unchanged.
///
/// ```
/// use apm_server::security::redact_urls_in;
///
/// assert_eq!(
///     redact_urls_in("error sending request for url (https://u:p@h/x?token=t)"),
///     "error sending request for url (https://h/x?token=[redacted])"
/// );
/// ```
pub fn redact_urls_in(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(marker) = rest.find("://") {
        let scheme_start = rest[..marker]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_scheme_char(*c))
            .last()
            .map_or(marker, |(index, _)| index);
        let url_end = rest[marker + 3..]
            .find(is_url_terminator)
            .map_or(rest.len(), |offset| marker + 3 + offset);
        out.push_str(&rest[..scheme_start]);
        out.push_str(&redact_url(&rest[scheme_start..url_end]));
        rest = &rest[url_end..];
    }
    out.push_str(rest);
    out
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}' | '|')
}

/// Removes `userinfo@` from every authority in `text`: after each `scheme://`
/// or, when the text has no `://` at all, after a leading `//` or at the start.
fn strip_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    if !text.contains("://") {
        let lead = if rest.starts_with("//") { 2 } else { 0 };
        let (prefix, tail) = rest.split_at(lead);
        out.push_str(prefix);
        rest = tail;
        push_host_part(&mut out, &mut rest);
        out.push_str(rest);
        return out;
    }
    while let Some(marker) = rest.find("://") {
        let (head, tail) = rest.split_at(marker + 3);
        out.push_str(head);
        rest = tail;
        push_host_part(&mut out, &mut rest);
    }
    out.push_str(rest);
    out
}

/// Moves the authority at the start of `rest` into `out` without its
/// userinfo, leaving `rest` at the first `/` (or empty).
fn push_host_part(out: &mut String, rest: &mut &str) {
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, after) = rest.split_at(end);
    out.push_str(
        authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host),
    );
    *rest = after;
}

fn redact_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        if !out.is_empty() {
            out.push('&');
        }
        match pair.split_once('=') {
            Some((name, _)) if is_plain_name(name) => {
                out.push_str(name);
                out.push('=');
                out.push_str(PLACEHOLDER);
            }
            _ => out.push_str(PLACEHOLDER),
        }
    }
    out
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_QUERY_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '[' | ']'))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde::Serialize;

    use super::*;

    #[derive(Debug, Clone, Default)]
    struct Credentials {
        user: String,
        password: Redacted<String>,
        tokens: Vec<Redacted<String>>,
    }

    #[test]
    fn debug_and_display_never_show_the_secret() {
        let secret = Redacted::new(String::from("hunter2"));
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(format!("{secret:#?}"), "[redacted]");
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(secret.clone().into_inner(), "hunter2");
    }

    #[test]
    fn a_struct_holding_secrets_debug_prints_without_them() {
        let creds = Credentials {
            user: "alice".to_owned(),
            password: Redacted::new("hunter2".to_owned()),
            tokens: vec![Redacted::from("token-one".to_owned())],
        };
        for text in [format!("{creds:?}"), format!("{creds:#?}")] {
            assert!(text.contains("alice"), "{text}");
            assert!(text.contains("[redacted]"), "{text}");
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("token-one"), "{text}");
        }
        let options = Some(Redacted::new("hunter2".to_owned()));
        assert!(!format!("{options:?}").contains("hunter2"));
    }

    #[test]
    fn redacted_is_not_serialize() {
        // Compiles only while `Redacted<String>` does not implement
        // `Serialize` (the call is ambiguous otherwise).
        trait AmbiguousIfSerialize<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
        impl<T: ?Sized + Serialize> AmbiguousIfSerialize<u8> for T {}
        <Redacted<String> as AmbiguousIfSerialize<_>>::check();
        <Credentials as AmbiguousIfSerialize<_>>::check();
    }

    #[test]
    fn deserialize_is_transparent() {
        let secret: Redacted<String> = serde_json::from_str("\"pw\"").unwrap();
        assert_eq!(secret.expose(), "pw");
        let nested: Vec<Redacted<String>> = serde_json::from_str("[\"a\",\"b\"]").unwrap();
        assert_eq!(nested.len(), 2);
    }

    #[test]
    fn a_failed_deserialisation_does_not_echo_the_value() {
        let err = serde_json::from_str::<Redacted<u32>>("\"hunter2\"").unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn equality_compares_content() {
        let a = Redacted::new("secret".to_owned());
        assert_eq!(a, Redacted::new("secret".to_owned()));
        assert_ne!(a, Redacted::new("secreT".to_owned()));
        assert_ne!(a, Redacted::new("secret!".to_owned()));
        assert_eq!(
            Redacted::new(vec![1u8, 2, 3]),
            Redacted::new(vec![1u8, 2, 3])
        );
        assert!(a.matches("secret"));
        assert!(a.matches(b"secret"));
        assert!(!a.matches("secre"));
        assert!(!a.matches(""));
        assert!(Redacted::new(String::new()).matches(""));
    }

    #[test]
    fn default_and_from_work() {
        let empty: Redacted<String> = Redacted::default();
        assert_eq!(empty.expose(), "");
        let from: Redacted<&str> = "x".into();
        assert_eq!(*from.expose(), "x");
    }

    #[test]
    fn redact_url_removes_userinfo_query_values_and_fragment() {
        let cases = [
            (
                "https://me:pw@immich.local:2283/api/x?key=abc&page=2#top",
                "https://immich.local:2283/api/x?key=[redacted]&page=[redacted]",
            ),
            ("http://user@host/path", "http://host/path"),
            ("http://host/path", "http://host/path"),
            ("http://host", "http://host"),
            ("http://host?token=t", "http://host?token=[redacted]"),
            (
                "https://[::1]:8080/x?k=v",
                "https://[::1]:8080/x?k=[redacted]",
            ),
            // `@` inside the password: userinfo ends at the last `@`.
            ("http://user:p@ss@host/", "http://host/"),
            // Password characters that would end the authority too early in a
            // naive parser.
            ("http://user:pa?ss@host/x", "http://host/x"),
            ("http://user:pa#ss@host/x", "http://host/x"),
            // A bare token and an unnamed value.
            ("http://host/x?abc123", "http://host/x?[redacted]"),
            (
                "http://host/x?=v&a=b",
                "http://host/x?[redacted]&a=[redacted]",
            ),
            ("http://host/x?", "http://host/x"),
            ("http://host/x?&&", "http://host/x"),
            // Relative and protocol-relative URLs.
            ("/api/x?key=abc", "/api/x?key=[redacted]"),
            ("//u:p@host/x", "//host/x"),
            ("u:p@host/x", "host/x"),
            ("/files/a@b.jpg", "/files/a@b.jpg"),
            // A URL nested in the path.
            (
                "https://web.example/save/http://u:p@evil.example/",
                "https://web.example/save/http://evil.example/",
            ),
            // A parameter name that is not plain is hidden too.
            (
                "http://host/?sec%72et=1&ok_name=2",
                "http://host/?[redacted]&ok_name=[redacted]",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_url(input), expected, "input: {input}");
        }
    }

    #[test]
    fn redact_url_never_panics_on_garbage() {
        for input in [
            "",
            "://",
            "http://",
            "http://@",
            "@",
            "@@@",
            "?",
            "#",
            "?#",
            "&",
            "&&=&",
            "http://[",
            "ünïcode://üser:pä@höst/pàth?kéy=välue#frägment",
            "\0",
            "a://b://c@d",
            "http://:@:/?=&#",
            "%%%",
            "😀@😀://😀",
        ] {
            let _ = redact_url(input);
        }
    }

    #[test]
    fn redact_urls_in_scrubs_urls_inside_text_only() {
        assert_eq!(
            redact_urls_in("error sending request for url (https://u:p@h/x?token=t)"),
            "error sending request for url (https://h/x?token=[redacted])"
        );
        assert_eq!(
            redact_urls_in("a http://x:y@one/p?k=v and https://two/q?z=1 done"),
            "a http://one/p?k=[redacted] and https://two/q?z=[redacted] done"
        );
        assert_eq!(
            redact_urls_in("no url here, user@example.com"),
            "no url here, user@example.com"
        );
        assert_eq!(
            redact_urls_in("trailing https://u:p@h"),
            "trailing https://h"
        );
        assert_eq!(redact_urls_in("bare :// marker"), "bare :// marker");
        assert_eq!(redact_urls_in(""), "");
    }

    proptest! {
        #[test]
        fn redact_url_never_leaks_credentials_or_query_values(
            user in "[A-Za-z0-9]{1,12}",
            password in "SECRETPW[A-Za-z0-9]{6,16}",
            value in "SECRETVAL[A-Za-z0-9]{6,16}",
            fragment in "SECRETFRAG[A-Za-z0-9]{6,16}",
            path in "[a-z/]{0,20}",
        ) {
            let url = format!("https://{user}:{password}@host.example:8443/{path}?k={value}&x=1#{fragment}");
            let redacted = redact_url(&url);
            prop_assert!(!redacted.contains(&password), "{redacted}");
            prop_assert!(!redacted.contains(&value), "{redacted}");
            prop_assert!(!redacted.contains(&fragment), "{redacted}");
            let embedded = redact_urls_in(&format!("failed: {url} (retrying)"));
            prop_assert!(!embedded.contains(&password), "{embedded}");
            prop_assert!(!embedded.contains(&value), "{embedded}");
            prop_assert!(embedded.starts_with("failed: https://host.example:8443/"), "{embedded}");
        }

        #[test]
        fn redact_url_never_panics(input in "\\PC{0,60}") {
            let _ = redact_url(&input);
            let _ = redact_urls_in(&input);
        }
    }

    #[test]
    fn credentials_fixture_is_used() {
        // Keeps the Debug/Serialize fixture honest: all fields are read.
        let creds = Credentials::default();
        assert!(creds.user.is_empty() && creds.tokens.is_empty());
        assert_eq!(creds.password.expose(), "");
    }
}
