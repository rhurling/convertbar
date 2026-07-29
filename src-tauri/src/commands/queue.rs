use convertbar_core::ctx::Ctx;
use convertbar_core::queue_ops;
use convertbar_core::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo, PurgeResult,
};
use std::sync::Arc;
use tauri::State;

use super::{blocking, CommandError};

#[tauri::command]
pub async fn add_files(
    ctx: State<'_, Arc<Ctx>>,
    paths: Vec<String>,
) -> Result<AddResult, CommandError> {
    let ctx = ctx.inner().clone();
    // The blocking pool is load-bearing: add_files probes every file; on the main thread it
    // freezes the UI (see the 4-entry-point probe-hazard fix).
    blocking(move || queue_ops::add_files(&ctx, &paths)).await
}

#[tauri::command]
pub async fn scan_folder(path: String) -> Result<FolderScanResult, CommandError> {
    // scan_video_files stats every entry of an unbounded recursive walk; a deep tree
    // or network volume would freeze the UI if this ran on the main thread.
    blocking(move || queue_ops::scan_folder(path)).await
}

#[tauri::command]
pub async fn confirm_folder_add(
    ctx: State<'_, Arc<Ctx>>,
    path: String,
) -> Result<AddResult, CommandError> {
    let ctx = ctx.inner().clone();
    // Both the recursive scan and the per-file probe block; run them off the main thread so
    // confirming a large folder doesn't freeze the UI (same hazard as add_files).
    blocking(move || queue_ops::confirm_folder_add(&ctx, path)).await
}

#[tauri::command]
pub fn get_queue(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<JobInfo>, CommandError> {
    Ok(queue_ops::get_queue(&ctx)?)
}

#[tauri::command]
pub fn remove_job(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<(), CommandError> {
    Ok(queue_ops::remove_job(&ctx, &id)?)
}

#[tauri::command]
pub fn remove_history_entry(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<(), CommandError> {
    Ok(queue_ops::remove_history_entry(&ctx, &id)?)
}

#[tauri::command]
pub fn reorder_queue(ctx: State<'_, Arc<Ctx>>, job_ids: Vec<String>) -> Result<(), CommandError> {
    Ok(queue_ops::reorder_queue(&ctx, &job_ids)?)
}

#[tauri::command]
pub fn clear_completed(ctx: State<'_, Arc<Ctx>>, mode: String) -> Result<(), CommandError> {
    Ok(queue_ops::clear_completed(&ctx, &mode)?)
}

#[tauri::command]
pub fn clear_queue(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(queue_ops::clear_queue(&ctx)?)
}

#[tauri::command]
pub fn get_bad_sources(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<JobInfo>, CommandError> {
    Ok(queue_ops::get_bad_sources(&ctx)?)
}

#[tauri::command]
pub async fn purge_bad_sources(
    ctx: State<'_, Arc<Ctx>>,
    ids: Vec<String>,
) -> Result<Vec<PurgeResult>, CommandError> {
    let ctx = ctx.inner().clone();
    // Rung 4 can block per id for up to PROBE_TIMEOUT (~30s) scanning a stalled/offline source;
    // offload like this file's other probe-touching commands.
    blocking(move || queue_ops::purge_bad_sources(&ctx, ids)).await
}

#[tauri::command]
pub fn get_history(
    ctx: State<'_, Arc<Ctx>>,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, CommandError> {
    Ok(queue_ops::get_history(
        &ctx, limit, offset, search, sort_by,
    )?)
}

#[tauri::command]
pub fn get_history_summary(
    ctx: State<'_, Arc<Ctx>>,
    search: Option<String>,
) -> Result<HistorySummary, CommandError> {
    Ok(queue_ops::get_history_summary(&ctx, search)?)
}

#[tauri::command]
pub async fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, CommandError> {
    // Dropped folders get the same recursive walk as scan_folder — off the main thread.
    blocking(move || queue_ops::classify_paths(paths)).await
}
