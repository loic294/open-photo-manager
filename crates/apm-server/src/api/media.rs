//! Media endpoints: `GET /api/files/{id}/thumb`, `/preview` and `/content`.
//!
//! Owned by todo `serving-media`. The status and error codes are tabulated in the `media` module
//! docs. Handlers look the row up, then delegate to `MediaService` and `media::serve`.

use apm_core::dto::FileKind;
use apm_core::paths::{FileId, RelPath};
use axum::Router;
use axum::extract::State;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_NONE_MATCH,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use super::extract::{ApiPath, ApiQuery};
use crate::db::models::{FILE_COLUMNS, FileRow};
use crate::error::{AppError, AppResult};
use crate::media::preview::{PreviewPlan, plan};
use crate::media::serve::{ServeOptions, serve};
use crate::media::{MediaError, MediaService, Variant};
use crate::security::FsError;
use crate::state::AppState;

/// How long a failure answer may be reused by the browser.
const FAILURE_CACHE: &str = "private, max-age=600";
const IMMUTABLE: &str = "private, max-age=31536000, immutable";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/files/{id}/content", get(content))
        .route("/files/{id}/preview", get(preview))
        .route("/files/{id}/thumb", get(thumb))
}

#[derive(Debug, Deserialize)]
struct ThumbQuery {
    size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentQuery {
    download: Option<String>,
}

async fn load(state: &AppState, id: FileId) -> AppResult<FileRow> {
    let mut query =
        sqlx::QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE id = "));
    query.push_bind(id);
    query
        .build_query_as::<FileRow>()
        .fetch_optional(state.db.read())
        .await?
        .ok_or_else(|| AppError::not_found("not_found", "no such file"))
}

fn rel_of(row: &FileRow) -> AppResult<RelPath> {
    RelPath::new(&row.dir_path)
        .and_then(|dir| dir.join(&row.name))
        .map_err(|_| AppError::not_found("file_missing", "the file is gone from the disk"))
}

fn with_cache(mut response: Response, value: &'static str) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(value));
    response
}

fn failure(err: MediaError) -> Response {
    match err {
        MediaError::App(err) => err.into_response(),
        MediaError::Missing | MediaError::Stale => {
            AppError::not_found("file_missing", "the file is gone from the disk").into_response()
        }
        MediaError::Unsupported => with_cache(
            AppError::not_found("no_thumbnail", "this file has no picture").into_response(),
            FAILURE_CACHE,
        ),
        MediaError::Failed(message) => with_cache(
            AppError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "thumbnail_failed",
                format!("the picture could not be rendered: {message}"),
            )
            .into_response(),
            FAILURE_CACHE,
        ),
    }
}

fn picture(bytes: bytes::Bytes, etag: Option<&str>, cache: &'static str) -> Response {
    let len = bytes.len();
    let mut response = Response::new(axum::body::Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    match etag.and_then(|etag| HeaderValue::from_str(etag).ok()) {
        Some(value) => {
            headers.insert(ETAG, value);
            headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
        }
        None => {
            headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
    }
    response
}

fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .any(|tag| tag.trim() == "*" || tag.trim().trim_start_matches("W/") == etag)
        })
}

fn not_modified(etag: &str, cache: &'static str) -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(ETAG, value);
    }
    with_cache(response, cache)
}

async fn thumb(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<FileId>,
    ApiQuery(query): ApiQuery<ThumbQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let variant = match query.size.as_deref() {
        None | Some("sm") => Variant::ThumbSm,
        Some("lg") => Variant::ThumbLg,
        Some(_) => {
            return Err(AppError::bad_request(
                "invalid_query",
                "size must be sm or lg",
            ));
        }
    };
    let row = load(&state, id).await?;
    if !matches!(row.kind, FileKind::Image | FileKind::Video) {
        return Ok(failure(MediaError::Unsupported));
    }
    let etag = MediaService::key(&row, variant).etag();
    if matches_etag(&headers, &etag) {
        return Ok(not_modified(&etag, IMMUTABLE));
    }
    Ok(match state.media.rendition(&row, variant).await {
        Ok(rendition) => picture(rendition.bytes, rendition.etag.as_deref(), IMMUTABLE),
        Err(err) => failure(err),
    })
}

async fn preview(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<FileId>,
    method: Method,
    headers: HeaderMap,
) -> AppResult<Response> {
    let row = load(&state, id).await?;
    match plan(row.kind, &row.ext) {
        PreviewPlan::None => Ok(with_cache(
            AppError::not_found("no_preview", "this file has no preview").into_response(),
            FAILURE_CACHE,
        )),
        PreviewPlan::Original => original(&state, &row, &method, &headers, false).await,
        PreviewPlan::Render => {
            let etag = MediaService::key(&row, Variant::Preview).etag();
            if matches_etag(&headers, &etag) {
                return Ok(not_modified(&etag, "private, no-cache"));
            }
            Ok(match state.media.rendition(&row, Variant::Preview).await {
                Ok(rendition) => picture(
                    rendition.bytes,
                    rendition.etag.as_deref(),
                    "private, no-cache",
                ),
                Err(err) => failure(err),
            })
        }
    }
}

async fn content(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<FileId>,
    ApiQuery(query): ApiQuery<ContentQuery>,
    method: Method,
    headers: HeaderMap,
) -> AppResult<Response> {
    let row = load(&state, id).await?;
    let download = query
        .download
        .as_deref()
        .is_some_and(|value| !matches!(value, "" | "0" | "false"));
    original(&state, &row, &method, &headers, download).await
}

async fn original(
    state: &AppState,
    row: &FileRow,
    method: &Method,
    headers: &HeaderMap,
    download: bool,
) -> AppResult<Response> {
    let root = state.volumes.get(row.volume_id)?;
    let rel = rel_of(row)?;
    let file = tokio::task::spawn_blocking(move || root.open_read(&rel))
        .await
        .map_err(AppError::internal)?;
    let file = match file {
        Ok(file) => file,
        Err(FsError::NotFound(_) | FsError::NotAFile(_)) => {
            return Err(AppError::not_found(
                "file_missing",
                "the file is gone from the disk",
            ));
        }
        Err(err) => return Err(err.into()),
    };
    Ok(serve(
        file,
        method,
        headers,
        ServeOptions {
            name: &row.name,
            mime: row.mime.as_deref(),
            download,
        },
    )
    .await)
}
