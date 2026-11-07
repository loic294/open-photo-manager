//! Request extractors that report failures as the JSON `ErrorBody` instead of
//! axum's plain-text rejections. Handlers use these instead of the raw
//! `axum::Json`, `Path` and `Query` extractors.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts};

use crate::error::AppError;

/// JSON request body.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

/// Path parameters.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);

/// Query-string parameters.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct ApiQuery<T>(pub T);

impl From<JsonRejection> for AppError {
    fn from(rejection: JsonRejection) -> Self {
        Self::new(rejection.status(), "invalid_json", rejection.body_text())
    }
}

impl From<PathRejection> for AppError {
    fn from(rejection: PathRejection) -> Self {
        Self::new(rejection.status(), "invalid_path", rejection.body_text())
    }
}

impl From<QueryRejection> for AppError {
    fn from(rejection: QueryRejection) -> Self {
        Self::new(rejection.status(), "invalid_query", rejection.body_text())
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    #[derive(Deserialize)]
    struct Payload {
        name: String,
    }

    #[derive(Deserialize)]
    struct Paging {
        limit: u32,
    }

    fn app() -> Router {
        Router::new()
            .route(
                "/json",
                post(|ApiJson(payload): ApiJson<Payload>| async move { Json(payload.name) }),
            )
            .route(
                "/path/{id}",
                get(|ApiPath(id): ApiPath<i64>| async move { Json(id) }),
            )
            .route(
                "/query",
                get(|ApiQuery(paging): ApiQuery<Paging>| async move { Json(paging.limit) }),
            )
    }

    async fn call(request: Request<Body>) -> (StatusCode, Value) {
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn post_json(body: &str) -> Request<Body> {
        Request::post("/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn valid_requests_reach_the_handler() {
        let (status, body) = call(post_json(r#"{"name":"alice"}"#)).await;
        assert_eq!((status, body), (StatusCode::OK, json!("alice")));

        let (status, body) = call(Request::get("/path/42").body(Body::empty()).unwrap()).await;
        assert_eq!((status, body), (StatusCode::OK, json!(42)));

        let (status, body) =
            call(Request::get("/query?limit=7").body(Body::empty()).unwrap()).await;
        assert_eq!((status, body), (StatusCode::OK, json!(7)));
    }

    #[tokio::test]
    async fn rejections_are_json_error_bodies() {
        let (status, body) = call(post_json("{not json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("invalid_json"));

        let (status, body) = call(post_json(r#"{"nom":"x"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], json!("invalid_json"));

        let (status, body) = call(Request::get("/path/abc").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("invalid_path"));

        let (status, body) =
            call(Request::get("/query?limit=x").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("invalid_query"));
    }
}
