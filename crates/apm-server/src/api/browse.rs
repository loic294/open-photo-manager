//! Browse endpoints, owned by todo `building-browse-api`:
//!
//! - `POST /api/browse/folders`: `BrowseRequest` -> `FoldersResponse`
//! - `POST /api/browse/items`: `ItemsRequest` -> `ItemsResponse`
//! - `POST /api/browse/ids`: `ItemsRequest` -> `IdsResponse` (offset and limit
//!   are ignored)
//! - `POST /api/facets`: `BrowseRequest` -> `Facets`
//!
//! Contract deviation from plan 5.12: these are `POST` with JSON bodies, not
//! `GET` with query strings, because filters do not fit in a query string.

use apm_core::dto::{
    BrowseRequest, Facets, FoldersResponse, IdsResponse, ItemsRequest, ItemsResponse,
};
use axum::routing::post;
use axum::{Json, Router};

use super::extract::ApiJson;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OWNER: &str = "building-browse-api";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/browse/folders", post(folders))
        .route("/browse/ids", post(ids))
        .route("/browse/items", post(items))
        .route("/facets", post(facets))
}

async fn facets(ApiJson(_request): ApiJson<BrowseRequest>) -> AppResult<Json<Facets>> {
    Err(AppError::not_implemented(OWNER))
}

async fn folders(ApiJson(_request): ApiJson<BrowseRequest>) -> AppResult<Json<FoldersResponse>> {
    Err(AppError::not_implemented(OWNER))
}

async fn ids(ApiJson(_request): ApiJson<ItemsRequest>) -> AppResult<Json<IdsResponse>> {
    Err(AppError::not_implemented(OWNER))
}

async fn items(ApiJson(_request): ApiJson<ItemsRequest>) -> AppResult<Json<ItemsResponse>> {
    Err(AppError::not_implemented(OWNER))
}
