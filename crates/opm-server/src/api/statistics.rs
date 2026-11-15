use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use opm_core::dto::{StatisticsQuery, StatisticsResponse};

use super::extract::ApiQuery;
use crate::error::AppResult;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/statistics", get(history))
}

async fn history(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StatisticsQuery>,
) -> AppResult<Json<StatisticsResponse>> {
    Ok(Json(state.statistics.history(query).await?))
}
