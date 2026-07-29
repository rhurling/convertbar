//! Queue and bad-source routes: `/api/queue*`, `/api/folders/scan`, `/api/paths/classify`,
//! `/api/bad-sources*`.
//!
//! `add_files`/`scan_folder`/`confirm_folder_add`/`classify_paths`/`purge_bad_sources` run
//! inside `spawn_blocking` — they probe files or shell out, same discipline the desktop's
//! async commands follow. The rest are short DB-only calls and run inline.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use convertbar_core::queue_ops;

use super::{core_err, join_err, ServerState};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddFilesBody {
    paths: Vec<String>,
}

pub async fn add_files(State(s): State<ServerState>, Json(b): Json<AddFilesBody>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || queue_ops::add_files(&ctx, &b.paths)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}

/// Shared by `scan_folder` and `confirm_folder_add` — both bodies are just `{"path": "..."}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathBody {
    path: String,
}

pub async fn scan_folder(Json(b): Json<PathBody>) -> Response {
    match tokio::task::spawn_blocking(move || queue_ops::scan_folder(b.path)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}

pub async fn confirm_folder_add(State(s): State<ServerState>, Json(b): Json<PathBody>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || queue_ops::confirm_folder_add(&ctx, b.path)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyPathsBody {
    paths: Vec<String>,
}

pub async fn classify_paths(Json(b): Json<ClassifyPathsBody>) -> Response {
    match tokio::task::spawn_blocking(move || queue_ops::classify_paths(b.paths)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}

pub async fn get_queue(State(s): State<ServerState>) -> Response {
    match queue_ops::get_queue(&s.ctx) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn remove_job(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    match queue_ops::remove_job(&s.ctx, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderQueueBody {
    job_ids: Vec<String>,
}

pub async fn reorder_queue(
    State(s): State<ServerState>,
    Json(b): Json<ReorderQueueBody>,
) -> Response {
    match queue_ops::reorder_queue(&s.ctx, &b.job_ids) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn clear_queue(State(s): State<ServerState>) -> Response {
    match queue_ops::clear_queue(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn get_bad_sources(State(s): State<ServerState>) -> Response {
    match queue_ops::get_bad_sources(&s.ctx) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeBadSourcesBody {
    ids: Vec<String>,
}

pub async fn purge_bad_sources(
    State(s): State<ServerState>,
    Json(b): Json<PurgeBadSourcesBody>,
) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || queue_ops::purge_bad_sources(&ctx, b.ids)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}
