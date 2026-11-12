//! Selection endpoints: `POST /api/selections` (store an id set) and
//! `GET /api/selections/{id}`.
//!
//! Owned by todo `building-plan-engine`; `crate::plans::PlanService` does the work. A selection
//! is a server-side set of files that a `Filter` references through `selection_id`; it is
//! deleted 24 hours after its last use. `POST /api/plans/{id}/selection` creates one from the
//! files of a plan.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use opm_core::dto::{CreateSelectionRequest, SelectionDto};
use opm_core::paths::SelectionId;

use super::extract::{ApiJson, ApiPath};
use crate::error::AppResult;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/selections", post(create))
        .route("/selections/{id}", get(detail))
}

/// Stores the ids and answers 201 with the selection. Duplicates and ids of files that do not
/// exist are dropped; 422 `selection_too_large` above 1,000,000 ids. The body limit is 16 MiB.
async fn create(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateSelectionRequest>,
) -> AppResult<(StatusCode, Json<SelectionDto>)> {
    let selection = state.plans.create_selection(request).await?;
    Ok((StatusCode::CREATED, Json(selection)))
}

/// The selection with the files that are still in it. 404 `selection_not_found` for an unknown or
/// expired one.
async fn detail(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<SelectionId>,
) -> AppResult<Json<SelectionDto>> {
    Ok(Json(state.plans.get_selection(id).await?))
}
