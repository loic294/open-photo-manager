//! Errors of the Immich integration and how they reach the HTTP client.
//!
//! [`ImmichError`] is what the client, the credential store and the service return. Its messages are
//! written for the user and never contain the API key, the password, or the userinfo or query of a
//! URL: whatever text comes from the network goes through `redact_urls_in` before it is stored here.
//!
//! The mapping to HTTP is deliberate (see `From<ImmichError> for AppError`):
//!
//! | error                                      | status | code                       |
//! |--------------------------------------------|--------|----------------------------|
//! | `NotConnected`                             | 409    | `immich_not_connected`     |
//! | `NeedsReconnect`                           | 409    | `immich_needs_reconnect`   |
//! | `AuthFailed` (wrong email or password)     | 400    | `immich_auth_failed`       |
//! | `InvalidUrl`                               | 400    | `immich_invalid_url`       |
//! | `AdminRequired`                            | 403    | `immich_admin_required`    |
//! | `Validation`                               | 422    | `validation_failed`        |
//! | `Unreachable`                              | 502    | `immich_unreachable`       |
//! | `Unauthorized` (Immich rejected the key)   | 502    | `immich_unauthorized`      |
//! | `Forbidden` (the key lacks a permission)   | 502    | `immich_forbidden`         |
//! | everything else Immich-side                | 502    | `immich_error`             |
//!
//! Upstream failures are 502 rather than 401/403/404 because those statuses describe the request the
//! browser made to *this* server, which was fine.

use axum::http::StatusCode;
use opm_core::dto::FieldError;
use thiserror::Error;

use crate::db::DbError;
use crate::error::AppError;

/// Longest Immich message kept in an error, in characters.
pub const MAX_UPSTREAM_MESSAGE_CHARS: usize = 300;

#[derive(Debug, Error)]
pub enum ImmichError {
    /// No connection is stored.
    #[error("Immich is not connected; connect it first")]
    NotConnected,
    /// A connection is stored but its API key cannot be read (the key file is missing or does not
    /// match): the user has to connect again.
    #[error("the stored Immich API key cannot be used any more; connect to Immich again")]
    NeedsReconnect,
    /// Connecting, DNS, TLS or a timeout. The text says which and never contains a secret.
    #[error("Immich is unreachable: {0}")]
    Unreachable(String),
    /// Immich answered 401 to a call made with the stored API key (it was revoked or expired).
    #[error("Immich rejected the API key; connect to Immich again")]
    Unauthorized,
    /// Immich rejected the email or password while connecting.
    #[error("Immich rejected the email or password")]
    AuthFailed,
    /// Immich answered 403: the API key lacks a permission.
    #[error("Immich refused the request: the API key lacks a permission")]
    Forbidden,
    /// The operation needs an administrator account and the connection is in limited mode.
    #[error(
        "this needs an Immich administrator account; the connected account can only use limited mode"
    )]
    AdminRequired,
    #[error("Immich does not know the requested item")]
    NotFound,
    /// Immich answered with a redirect. Following it could send the API key to another host, so
    /// the address has to be corrected instead.
    #[error("Immich redirected the request to {to}; use that address as the server address")]
    Redirected { to: String },
    /// Any other error status. `message` is the `message` field of Immich's answer, truncated and
    /// scrubbed.
    #[error("Immich answered {status}: {message}")]
    Upstream { status: u16, message: String },
    /// An answer that does not have the expected shape.
    #[error("unexpected answer from Immich: {0}")]
    Protocol(String),
    #[error("invalid Immich address: {0}")]
    InvalidUrl(String),
    /// The request is invalid; the HTTP layer answers 422 with the list of fields.
    #[error("invalid request: {}", describe_fields(.0))]
    Validation(Vec<FieldError>),
    #[error(transparent)]
    Db(#[from] DbError),
    /// A local problem that is not the caller's fault (the key file cannot be written, a worker
    /// panicked). The cause is logged, never sent.
    #[error("internal error: {0}")]
    Internal(String),
}

fn describe_fields(fields: &[FieldError]) -> String {
    fields
        .iter()
        .map(FieldError::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

impl ImmichError {
    /// A validation error about one field.
    pub fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        Self::Validation(vec![FieldError {
            field: field.to_owned(),
            message: message.into(),
        }])
    }

    /// Whether repeating the same request later can succeed: the server was unreachable, failed
    /// (5xx) or asked to slow down (429). Everything else needs a change first. The outbox backoff
    /// uses this.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Unreachable(_) => true,
            Self::Upstream { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// The message to keep as "last error" of the connection: the display text, shortened.
    pub fn summary(&self) -> String {
        truncate_chars(&self.to_string(), MAX_UPSTREAM_MESSAGE_CHARS)
    }

    /// Whether every other call of a batch would fail the same way: Immich cannot be reached, or
    /// it does not accept the connection. A batch (one lookup per sampled file, one update per
    /// library) stops at the first such error instead of repeating it; any other error belongs to
    /// the one item that caused it.
    pub fn affects_every_call(&self) -> bool {
        matches!(
            self,
            Self::NotConnected
                | Self::NeedsReconnect
                | Self::Unreachable(_)
                | Self::Unauthorized
                | Self::Forbidden
                | Self::Redirected { .. }
        )
    }
}

/// Cuts `text` to at most `max` characters (not bytes), adding `...` when something was removed.
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
}

impl From<ImmichError> for AppError {
    fn from(err: ImmichError) -> Self {
        let message = err.to_string();
        match err {
            ImmichError::NotConnected => Self::conflict("immich_not_connected", message),
            ImmichError::NeedsReconnect => Self::conflict("immich_needs_reconnect", message),
            ImmichError::AuthFailed => Self::bad_request("immich_auth_failed", message),
            ImmichError::InvalidUrl(_) => Self::bad_request("immich_invalid_url", message),
            ImmichError::AdminRequired => Self::forbidden("immich_admin_required", message),
            ImmichError::Validation(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
                message,
            ),
            ImmichError::Unreachable(_) => {
                Self::new(StatusCode::BAD_GATEWAY, "immich_unreachable", message)
            }
            ImmichError::Unauthorized => {
                Self::new(StatusCode::BAD_GATEWAY, "immich_unauthorized", message)
            }
            ImmichError::Forbidden => {
                Self::new(StatusCode::BAD_GATEWAY, "immich_forbidden", message)
            }
            ImmichError::NotFound
            | ImmichError::Redirected { .. }
            | ImmichError::Upstream { .. }
            | ImmichError::Protocol(_) => {
                Self::new(StatusCode::BAD_GATEWAY, "immich_error", message)
            }
            ImmichError::Db(err) => err.into(),
            ImmichError::Internal(cause) => Self::internal(cause),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_and_code(err: ImmichError) -> (StatusCode, String) {
        let app: AppError = err.into();
        (app.status(), app.code().to_owned())
    }

    #[test]
    fn only_errors_that_would_repeat_affect_every_call_of_a_batch() {
        for repeating in [
            ImmichError::NotConnected,
            ImmichError::NeedsReconnect,
            ImmichError::Unreachable("down".into()),
            ImmichError::Unauthorized,
            ImmichError::Forbidden,
            ImmichError::Redirected { to: "/x".into() },
        ] {
            assert!(repeating.affects_every_call(), "{repeating:?}");
        }
        for single in [
            ImmichError::NotFound,
            ImmichError::AuthFailed,
            ImmichError::Protocol("odd".into()),
            ImmichError::Upstream {
                status: 500,
                message: "oops".into(),
            },
            ImmichError::invalid_field("library_id", "bad"),
        ] {
            assert!(!single.affects_every_call(), "{single:?}");
        }
    }

    #[test]
    fn every_error_maps_to_its_documented_status_and_code() {
        let cases = [
            (
                ImmichError::NotConnected,
                StatusCode::CONFLICT,
                "immich_not_connected",
            ),
            (
                ImmichError::NeedsReconnect,
                StatusCode::CONFLICT,
                "immich_needs_reconnect",
            ),
            (
                ImmichError::AuthFailed,
                StatusCode::BAD_REQUEST,
                "immich_auth_failed",
            ),
            (
                ImmichError::InvalidUrl("x".into()),
                StatusCode::BAD_REQUEST,
                "immich_invalid_url",
            ),
            (
                ImmichError::AdminRequired,
                StatusCode::FORBIDDEN,
                "immich_admin_required",
            ),
            (
                ImmichError::invalid_field("server_url", "required"),
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
            ),
            (
                ImmichError::Unreachable("timed out".into()),
                StatusCode::BAD_GATEWAY,
                "immich_unreachable",
            ),
            (
                ImmichError::Unauthorized,
                StatusCode::BAD_GATEWAY,
                "immich_unauthorized",
            ),
            (
                ImmichError::Forbidden,
                StatusCode::BAD_GATEWAY,
                "immich_forbidden",
            ),
            (
                ImmichError::NotFound,
                StatusCode::BAD_GATEWAY,
                "immich_error",
            ),
            (
                ImmichError::Redirected {
                    to: "https://h/".into(),
                },
                StatusCode::BAD_GATEWAY,
                "immich_error",
            ),
            (
                ImmichError::Upstream {
                    status: 500,
                    message: "boom".into(),
                },
                StatusCode::BAD_GATEWAY,
                "immich_error",
            ),
            (
                ImmichError::Protocol("not json".into()),
                StatusCode::BAD_GATEWAY,
                "immich_error",
            ),
            (
                ImmichError::Internal("disk".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (err, status, code) in cases {
            let label = format!("{err:?}");
            assert_eq!(status_and_code(err), (status, code.to_owned()), "{label}");
        }
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        let upstream = |status| ImmichError::Upstream {
            status,
            message: String::new(),
        };
        assert!(ImmichError::Unreachable("down".into()).is_retryable());
        assert!(upstream(500).is_retryable());
        assert!(upstream(503).is_retryable());
        assert!(upstream(429).is_retryable());
        assert!(!upstream(400).is_retryable());
        assert!(!upstream(409).is_retryable());
        for err in [
            ImmichError::NotConnected,
            ImmichError::NeedsReconnect,
            ImmichError::Unauthorized,
            ImmichError::AuthFailed,
            ImmichError::Forbidden,
            ImmichError::AdminRequired,
            ImmichError::NotFound,
            ImmichError::Protocol(String::new()),
            ImmichError::InvalidUrl(String::new()),
        ] {
            assert!(!err.is_retryable(), "{err:?}");
        }
    }

    #[test]
    fn the_validation_message_lists_the_fields() {
        let err = ImmichError::Validation(vec![
            FieldError {
                field: "mappings[0].path_prefix".into(),
                message: "must be absolute".into(),
            },
            FieldError {
                field: "mappings[1].volume_id".into(),
                message: "unknown volume".into(),
            },
        ]);
        assert_eq!(
            err.to_string(),
            "invalid request: mappings[0].path_prefix: must be absolute; \
             mappings[1].volume_id: unknown volume"
        );
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let long = "é".repeat(MAX_UPSTREAM_MESSAGE_CHARS + 50);
        let cut = ImmichError::Protocol(long).summary();
        assert_eq!(cut.chars().count(), MAX_UPSTREAM_MESSAGE_CHARS);
        assert!(cut.ends_with("..."));
        assert_eq!(truncate_chars("short", 10), "short");
    }
}
