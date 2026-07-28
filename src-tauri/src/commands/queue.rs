use convertbar_core::ctx::Ctx;
use convertbar_core::queue_ops;
use convertbar_core::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo, PurgeResult,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn add_files(ctx: State<'_, Arc<Ctx>>, paths: Vec<String>) -> Result<AddResult, String> {
    let ctx = ctx.inner().clone();
    // spawn_blocking is load-bearing: add_files probes every file; on the main thread it
    // freezes the UI (see the 4-entry-point probe-hazard fix).
    tauri::async_runtime::spawn_blocking(move || queue_ops::add_files(&ctx, &paths))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    // scan_video_files stats every entry of an unbounded recursive walk; a deep tree
    // or network volume would freeze the UI if this ran on the main thread.
    tauri::async_runtime::spawn_blocking(move || queue_ops::scan_folder(path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn confirm_folder_add(
    ctx: State<'_, Arc<Ctx>>,
    path: String,
) -> Result<AddResult, String> {
    let ctx = ctx.inner().clone();
    // Both the recursive scan and the per-file probe block; run them off the main thread so
    // confirming a large folder doesn't freeze the UI (same hazard as add_files).
    tauri::async_runtime::spawn_blocking(move || queue_ops::confirm_folder_add(&ctx, path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_queue(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<JobInfo>, String> {
    queue_ops::get_queue(&ctx)
}

#[tauri::command]
pub fn remove_job(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<(), String> {
    queue_ops::remove_job(&ctx, &id)
}

#[tauri::command]
pub fn remove_history_entry(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<(), String> {
    queue_ops::remove_history_entry(&ctx, &id)
}

#[tauri::command]
pub fn reorder_queue(ctx: State<'_, Arc<Ctx>>, job_ids: Vec<String>) -> Result<(), String> {
    queue_ops::reorder_queue(&ctx, &job_ids)
}

#[tauri::command]
pub fn clear_completed(ctx: State<'_, Arc<Ctx>>, mode: String) -> Result<(), String> {
    queue_ops::clear_completed(&ctx, &mode)
}

#[tauri::command]
pub fn clear_queue(ctx: State<'_, Arc<Ctx>>) -> Result<(), String> {
    queue_ops::clear_queue(&ctx)
}

#[tauri::command]
pub fn get_bad_sources(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<JobInfo>, String> {
    queue_ops::get_bad_sources(&ctx)
}

#[tauri::command]
pub async fn purge_bad_sources(
    ctx: State<'_, Arc<Ctx>>,
    ids: Vec<String>,
) -> Result<Vec<PurgeResult>, String> {
    let ctx = ctx.inner().clone();
    // Rung 4 can block per id for up to PROBE_TIMEOUT (~30s) scanning a stalled/offline source;
    // offload like this file's other probe-touching commands.
    tauri::async_runtime::spawn_blocking(move || queue_ops::purge_bad_sources(&ctx, ids))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_history(
    ctx: State<'_, Arc<Ctx>>,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    queue_ops::get_history(&ctx, limit, offset, search, sort_by)
}

#[tauri::command]
pub fn get_history_summary(
    ctx: State<'_, Arc<Ctx>>,
    search: Option<String>,
) -> Result<HistorySummary, String> {
    queue_ops::get_history_summary(&ctx, search)
}

#[tauri::command]
pub async fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
    // Dropped folders get the same recursive walk as scan_folder — off the main thread.
    tauri::async_runtime::spawn_blocking(move || queue_ops::classify_paths(paths))
        .await
        .map_err(|e| e.to_string())?
}
