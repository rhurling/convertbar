//! History routes: `/api/history*`. All DB-only, short-lock calls — none of these run in
//! `spawn_blocking` (see `routes::queue` for the five handlers that do).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use convertbar_core::queue_ops;

use super::{core_err, ServerState};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
}

pub async fn get_history(State(s): State<ServerState>, Query(q): Query<HistoryQuery>) -> Response {
    match queue_ops::get_history(&s.ctx, q.limit, q.offset, q.search, q.sort_by) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummaryQuery {
    search: Option<String>,
}

pub async fn get_history_summary(
    State(s): State<ServerState>,
    Query(q): Query<HistorySummaryQuery>,
) -> Response {
    match queue_ops::get_history_summary(&s.ctx, q.search) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn remove_history_entry(
    State(s): State<ServerState>,
    Path(id): Path<String>,
) -> Response {
    match queue_ops::remove_history_entry(&s.ctx, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCompletedBody {
    mode: String,
}

pub async fn clear_completed(
    State(s): State<ServerState>,
    Json(b): Json<ClearCompletedBody>,
) -> Response {
    match queue_ops::clear_completed(&s.ctx, &b.mode) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}
