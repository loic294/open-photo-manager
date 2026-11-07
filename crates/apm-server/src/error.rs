//! The single error type of the HTTP layer.
//!
//! Handlers return [`AppResult`]; every failure becomes the JSON body
//! `{"error":{"code":"...","message":"..."}}` with an appropriate status. Use
//! the constructors below instead of building responses by hand.

use std::fmt;

use apm_core::dto::{ErrorBody, ErrorDetail};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::security::redact_urls_in;

/// Result type of handlers and services that report API errors.
pub type AppResult<T> = Result<T, AppError>;

/// An API error: HTTP status, stable machine-readable code and a client-safe
/// message.
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl AppError {
    /// Arbitrary status, for the rare cases the helpers below do not cover.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// 403.
    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    /// 404.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// 409.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 501 for a scaffolded endpoint; `todo_id` names the todo that implements
    /// it.
    pub fn not_implemented(todo_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "not_implemented",
            format!("not implemented yet (todo {todo_id})"),
        )
    }

    /// 500.
    ///
    /// The cause is logged, with any URL in it scrubbed of credentials and
    /// query values (see [`crate::security::redact_urls_in`]), and never sent to
    /// the client.
    pub fn internal(cause: impl fmt::Display) -> Self {
        let cause = redact_urls_in(&cause.to_string());
        tracing::error!(error = %cause, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal server error",
        )
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_owned(),
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::not_found("not_found", "resource not found"),
            other => Self::internal(other),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::not_found("not_found", "resource not found"),
            std::io::ErrorKind::PermissionDenied => {
                Self::forbidden("forbidden", "permission denied")
            }
            _ => Self::internal(err),
        }
    }
}

impl From<anyhow::Error> for AppError {
    /// An [`AppError`] wrapped in `anyhow` passes through unchanged; anything
    /// else is an internal error.
    fn from(err: anyhow::Error) -> Self {
        match err.downcast::<Self>() {
            Ok(app_error) => app_error,
            Err(other) => Self::internal(format!("{other:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;

    async fn render(err: AppError) -> (StatusCode, Value) {
        let response = err.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn constructors_map_to_status_and_code() {
        let cases = [
            (
                AppError::bad_request("bad_thing", "nope"),
                StatusCode::BAD_REQUEST,
                "bad_thing",
            ),
            (
                AppError::forbidden("denied", "nope"),
                StatusCode::FORBIDDEN,
                "denied",
            ),
            (
                AppError::not_found("missing", "nope"),
                StatusCode::NOT_FOUND,
                "missing",
            ),
            (
                AppError::conflict("clash", "nope"),
                StatusCode::CONFLICT,
                "clash",
            ),
            (
                AppError::not_implemented("some-todo"),
                StatusCode::NOT_IMPLEMENTED,
                "not_implemented",
            ),
            (
                AppError::internal("secret detail"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (err, status, code) in cases {
            let (actual_status, body) = render(err).await;
            assert_eq!(actual_status, status);
            assert_eq!(body["error"]["code"], json!(code));
            assert!(body["error"]["message"].is_string());
        }
    }

    #[tokio::test]
    async fn not_implemented_names_the_owning_todo() {
        let (_, body) = render(AppError::not_implemented("building-browse-api")).await;
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("building-browse-api"), "{message}");
    }

    #[tokio::test]
    async fn internal_errors_never_leak_the_cause() {
        let (_, body) = render(AppError::internal("password=hunter2 in /etc/secret")).await;
        let text = body.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("/etc/secret"), "{text}");
    }

    #[test]
    fn io_errors_map_by_kind() {
        let missing = AppError::from(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let denied = AppError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let other = AppError::from(std::io::Error::other("disk on fire"));
        assert_eq!(other.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!other.message().contains("disk on fire"));
    }

    #[test]
    fn sqlx_errors_map_row_not_found_to_404() {
        let missing = AppError::from(sqlx::Error::RowNotFound);
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let other = AppError::from(sqlx::Error::PoolTimedOut);
        assert_eq!(other.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn anyhow_wrapping_an_app_error_passes_through() {
        let wrapped = anyhow::Error::new(AppError::conflict("clash", "already exists"));
        let unwrapped = AppError::from(wrapped);
        assert_eq!(unwrapped.status(), StatusCode::CONFLICT);
        assert_eq!(unwrapped.code(), "clash");

        let plain = AppError::from(anyhow::anyhow!("boom"));
        assert_eq!(plain.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(plain.code(), "internal");
    }
}
