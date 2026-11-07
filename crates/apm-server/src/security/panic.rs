//! Turns a panicking handler into a `500 internal` JSON response.
//!
//! Without it a panic aborts the connection task: the client sees a dropped
//! connection and no response. The server itself keeps running either way, and
//! the panic is logged inside the request's span (so with its request id) by
//! [`AppError::internal`]; the client only learns "internal server error".

use std::any::Any;
use std::panic::AssertUnwindSafe;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::FutureExt;

use crate::error::AppError;

const MAX_PANIC_MESSAGE_CHARS: usize = 300;

fn message_of(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    text.chars().take(MAX_PANIC_MESSAGE_CHARS).collect()
}

/// Middleware: runs the rest of the stack and converts a panic into a 500.
pub(super) async fn catch(req: Request, next: Next) -> Response {
    match AssertUnwindSafe(next.run(req)).catch_unwind().await {
        Ok(response) => response,
        Err(payload) => AppError::internal(format!(
            "handler panicked: {}",
            message_of(payload.as_ref())
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_messages_are_extracted_and_bounded() {
        let literal: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(message_of(literal.as_ref()), "boom");

        let formatted: Box<dyn Any + Send> = Box::new(format!("bad id {}", 7));
        assert_eq!(message_of(formatted.as_ref()), "bad id 7");

        let other: Box<dyn Any + Send> = Box::new(42_u8);
        assert_eq!(message_of(other.as_ref()), "non-string panic payload");

        let long: Box<dyn Any + Send> = Box::new("x".repeat(10_000));
        assert_eq!(message_of(long.as_ref()).len(), MAX_PANIC_MESSAGE_CHARS);
    }
}
