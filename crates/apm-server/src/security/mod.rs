//! Security baseline layer: `Host` header allow-list, the `x-apm-request: 1`
//! check on non-GET requests (CSRF), security response headers.
//!
//! Owned by todo `securing-api-baseline`. The layer is added in `app::router`
//! at the clearly marked insertion point.
