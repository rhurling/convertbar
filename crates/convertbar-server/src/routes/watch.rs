//! Watched-directory routes: `/api/watched*`. Short DB-lock calls (each also triggers a
//! `watcher::reconcile` and, where relevant, a background rescan inside `watch_ops` itself) — none
//! of these run in `spawn_blocking` at the route layer.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use convertbar_core::watch_ops;

use super::{core_err, ServerState};

pub async fn get_watched_directories(State(s): State<ServerState>) -> Response {
    match watch_ops::get_watched_directories(&s.ctx) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddWatchedBody {
    path: String,
    recursive: bool,
    stability_delay_secs: i64,
}

pub async fn add_watched_directory(
    State(s): State<ServerState>,
    Json(b): Json<AddWatchedBody>,
) -> Response {
    match watch_ops::add_watched_directory(&s.ctx, &b.path, b.recursive, b.stability_delay_secs) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWatchedBody {
    recursive: bool,
    stability_delay_secs: i64,
}

pub async fn update_watched_directory(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Json(b): Json<UpdateWatchedBody>,
) -> Response {
    match watch_ops::update_watched_directory(&s.ctx, &id, b.recursive, b.stability_delay_secs) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEnabledBody {
    enabled: bool,
}

pub async fn set_watched_directory_enabled(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Json(b): Json<SetEnabledBody>,
) -> Response {
    match watch_ops::set_watched_directory_enabled(&s.ctx, &id, b.enabled) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn remove_watched_directory(
    State(s): State<ServerState>,
    Path(id): Path<String>,
) -> Response {
    match watch_ops::remove_watched_directory(&s.ctx, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}
