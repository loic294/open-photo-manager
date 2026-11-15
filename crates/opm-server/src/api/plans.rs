//! Plan endpoints: `POST /api/plans` (create), `GET /api/plans/{id}`, `DELETE /api/plans/{id}`,
//! `POST /api/plans/{id}/execute` and `POST /api/plans/{id}/selection`.
//!
//! Owned by todo `building-plan-engine`; `crate::plans::PlanService` does the work, and the
//! `Planner` of the plan's kind supplies what is specific to it (see the `crate::plans` docs for
//! the lifecycle and the error codes). Every non-GET request carries the `x-opm-request` header.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use opm_core::dto::{
    CreatePlanRequest, DuplicateFileGroups, ExecutePlanRequest, PlanDto, PlanSelectionRequest,
    SelectionDto, TaskDto,
};
use opm_core::paths::PlanId;

use super::extract::{ApiJson, ApiPath};
use crate::error::AppResult;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/plans", post(create))
        .route("/plans/{id}", get(detail).delete(remove))
        .route("/plans/{id}/duplicate-files", get(duplicate_files))
        .route("/plans/{id}/execute", post(execute))
        .route("/plans/{id}/selection", post(selection))
}

async fn duplicate_files(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<PlanId>,
) -> AppResult<Json<DuplicateFileGroups>> {
    Ok(Json(state.plans.duplicate_files(id).await?))
}

/// Builds the plan and answers 201 with it (`ready`). 501 `not_implemented` for a kind without a
/// planner, 400 `invalid_plan_params`, 422 `plan_too_large`, 503 `plan_build_timeout`.
async fn create(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreatePlanRequest>,
) -> AppResult<(StatusCode, Json<PlanDto>)> {
    let plan = state.plans.create(request).await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

/// The plan with its statistics and warnings and, once executed, its result. 404 `plan_not_found`
/// for an unknown, expired or still-building plan.
async fn detail(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<PlanId>,
) -> AppResult<Json<PlanDto>> {
    Ok(Json(state.plans.get(id).await?))
}

/// Deletes a plan (idempotent: 204 for a plan that is already gone). 409 `plan_executing` while
/// its task runs.
async fn remove(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<PlanId>,
) -> AppResult<StatusCode> {
    state.plans.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues the `plan_execute` task and answers 202 with it. Follow `GET /api/events` for its
/// progress and `GET /api/plans/{id}` for the result. 409 `plan_not_ready` / `plan_executed`,
/// 400 `invalid_plan_options`, 404 `selection_not_found`.
async fn execute(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<PlanId>,
    ApiJson(request): ApiJson<ExecutePlanRequest>,
) -> AppResult<(StatusCode, Json<TaskDto>)> {
    let task = state.plans.execute(id, request).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// Hands the files the plan involves over as a selection (201): "browse the files of this plan",
/// "act on the matched files".
async fn selection(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<PlanId>,
    ApiJson(request): ApiJson<PlanSelectionRequest>,
) -> AppResult<(StatusCode, Json<SelectionDto>)> {
    let selection = state.plans.selection_from_plan(id, request).await?;
    Ok((StatusCode::CREATED, Json(selection)))
}
