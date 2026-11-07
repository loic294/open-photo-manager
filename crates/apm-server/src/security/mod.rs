//! The security baseline: everything that stands between the network and the
//! application code, in one module.
//!
//! # Installing the stack
//!
//! [`layers`] returns the whole middleware stack as one `tower::Layer`, built
//! from a [`SecurityOptions`] ([`SecurityOptions::from_config`] for the running
//! server). `app::router` applies it in one place; nothing else needs to know
//! what is in it. From the outside in:
//!
//! 1. **request id and tracing** (`request_id`): the id (the caller's
//!    `x-request-id` when well formed, else generated), the `request` span, the
//!    one log line per request and the `x-request-id` response header;
//! 2. **security headers** (`headers`): CSP, `nosniff`, `no-referrer`, frame
//!    and cross-origin policies, `no-store` on the API. Outside the layers below
//!    so that *their* refusals carry the headers as well;
//! 3. **panic catcher** (`panic`): a panicking handler becomes `500 internal`;
//! 4. **Host check** ([`host`]): anti DNS-rebinding allow-list for every route;
//! 5. **CSRF** (`csrf`): `x-apm-request: 1` plus same-origin `Origin` /
//!    `Sec-Fetch-Site` on everything that is not a plain read;
//! 6. **body limit and timeout** ([`limits`]): 1 MiB and 30 s unless the route
//!    has a [`RoutePolicy`] in [`limits::default_policies`].
//!
//! The static files and the single-page-app fallback live in [`spa`]; the file
//! system boundary is [`VolumeRoot`], the child-process boundary is [`proc`] and
//! the secret-hiding wrappers are [`Redacted`] and [`redact_url`].
//!
//! There is deliberately no CORS support: the app is same-origin only, so
//! `OPTIONS` requests are answered `405` and no `Access-Control-*` header is
//! ever sent.

mod csrf;
mod error_page;
mod headers;
pub mod host;
pub mod limits;
mod panic;
pub mod proc;
mod redact;
mod request_id;
pub mod spa;
#[cfg(test)]
mod test_support;
mod volume_root;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Request};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::Response;
use axum::routing::Route;
use tower::{Layer, ServiceBuilder};

use crate::config::Config;

pub use csrf::CSRF_HEADER;
pub use headers::CONTENT_SECURITY_POLICY_VALUE;
pub use host::{HostAuthority, HostRule};
pub use limits::{Limits, RoutePolicy, RouteTimeout};
pub use redact::{Redacted, redact_url, redact_urls_in};
pub use request_id::{REQUEST_ID_HEADER, RequestId};
pub use volume_root::{FsError, VolumeRoot};

/// What the middleware stack is configured with.
#[derive(Debug, Clone)]
pub struct SecurityOptions {
    /// The rules of `APM_ALLOWED_HOSTS`; `localhost` and IP literals are always
    /// allowed on top of them.
    pub allowed_hosts: Vec<HostRule>,
    /// Request body limit of the routes without a [`RoutePolicy`] saying
    /// otherwise.
    pub max_body_bytes: usize,
    /// Time allowed to produce the response headers, same caveat.
    pub request_timeout: Duration,
    /// Routes with limits of their own, first match wins.
    pub policies: Vec<RoutePolicy>,
}

impl Default for SecurityOptions {
    /// Only `localhost` and IP literals are allowed hosts; the limits are the
    /// production defaults and the per-route table is [`limits::default_policies`].
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            max_body_bytes: limits::DEFAULT_MAX_BODY_BYTES,
            request_timeout: limits::DEFAULT_REQUEST_TIMEOUT,
            policies: limits::default_policies(),
        }
    }
}

impl SecurityOptions {
    /// The options of a running server. `Config` has validated the host rules
    /// already; an entry that still fails to parse is skipped (and logged),
    /// which can only make the allow-list stricter.
    pub fn from_config(config: &Config) -> Self {
        let allowed_hosts = config
            .allowed_hosts
            .iter()
            .filter_map(|entry| match HostRule::parse(entry) {
                Ok(rule) => Some(rule),
                Err(err) => {
                    tracing::warn!(%entry, error = %err, "ignoring invalid APM_ALLOWED_HOSTS entry");
                    None
                }
            })
            .collect();
        Self {
            allowed_hosts,
            ..Self::default()
        }
    }
}

/// Whether `path` (the raw request path) belongs to the JSON API.
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// The whole middleware stack, see the module documentation for what it holds
/// and in which order.
///
/// ```
/// use apm_server::security::{self, SecurityOptions};
/// use axum::Router;
/// use axum::routing::get;
///
/// let router: Router = Router::new()
///     .route("/hello", get(|| async { "hello" }))
///     .layer(security::layers(SecurityOptions::default()));
/// ```
pub fn layers(
    options: SecurityOptions,
) -> impl Layer<
    Route,
    Service: tower::Service<
        Request,
        Response = Response,
        Error = Infallible,
        Future: Send + 'static,
    > + Clone
                 + Send
                 + Sync
                 + 'static,
> + Clone
+ Send
+ Sync
+ 'static {
    let options = Arc::new(options);
    ServiceBuilder::new()
        .layer(from_fn(request_id::middleware))
        .layer(from_fn(headers::middleware))
        .layer(from_fn(panic::catch))
        .layer(from_fn_with_state(options.clone(), host::guard))
        .layer(from_fn(csrf::guard))
        // The limit is `limits::enforce`'s; axum's own 2 MiB extractor limit
        // would cap the routes that opt into a larger body.
        .layer(DefaultBodyLimit::disable())
        .layer(from_fn_with_state(options, limits::enforce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths_are_the_api_prefix_only() {
        for path in ["/api", "/api/", "/api/health", "/api/files/1/thumb"] {
            assert!(is_api_path(path), "{path}");
        }
        for path in [
            "/",
            "/apix",
            "/api.json",
            "/API/health",
            "/x/api",
            "/ap",
            "",
        ] {
            assert!(!is_api_path(path), "{path}");
        }
    }

    #[test]
    fn default_options_match_the_documented_limits() {
        let options = SecurityOptions::default();
        assert!(options.allowed_hosts.is_empty());
        assert_eq!(options.max_body_bytes, 1024 * 1024);
        assert_eq!(options.request_timeout, Duration::from_secs(30));
        assert!(!options.policies.is_empty());
    }

    #[test]
    fn config_host_entries_become_rules() {
        let dir = test_support::scratch_dir();
        let mut config = Config::for_tests(dir.path());
        config.allowed_hosts = vec![
            "nas.local".into(),
            "*.example.com:8080".into(),
            "bad host".into(),
        ];
        let options = SecurityOptions::from_config(&config);
        assert_eq!(
            options.allowed_hosts.len(),
            2,
            "the invalid entry is skipped"
        );
        let nas: HostAuthority = "nas.local:9".parse().unwrap();
        assert!(options.allowed_hosts.iter().any(|rule| rule.matches(&nas)));
    }
}
