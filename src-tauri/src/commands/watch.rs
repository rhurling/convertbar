use convertbar_core::ctx::Ctx;
use convertbar_core::watch_ops;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::types::WatchedDirectory;

#[tauri::command]
pub fn get_watched_directories(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<WatchedDirectory>, String> {
    watch_ops::get_watched_directories(&ctx)
}

#[tauri::command]
pub fn add_watched_directory(
    ctx: State<'_, Arc<Ctx>>,
    path: String,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<WatchedDirectory, String> {
    watch_ops::add_watched_directory(&ctx, &path, recursive, stability_delay_secs)
}

#[tauri::command]
pub fn update_watched_directory(
    ctx: State<'_, Arc<Ctx>>,
    id: String,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<(), String> {
    watch_ops::update_watched_directory(&ctx, &id, recursive, stability_delay_secs)
}

#[tauri::command]
pub fn set_watched_directory_enabled(
    ctx: State<'_, Arc<Ctx>>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    watch_ops::set_watched_directory_enabled(&ctx, &id, enabled)
}

#[tauri::command]
pub fn remove_watched_directory(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<(), String> {
    watch_ops::remove_watched_directory(&ctx, &id)
}

/// Opens the native folder picker so the UI can add a directory to watch. Invoked from Rust, so
/// no frontend `dialog` ACL permission is required. MUST stay `async`: Tauri runs sync commands
/// on the main thread, and `blocking_pick_folder` dispatches the panel to the main thread and then
/// blocks the calling thread — calling it on the main thread deadlocks the event loop. `async`
/// runs the command on a worker thread, so the main thread stays free to service the panel.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let folder = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|file_path| file_path.into_path().ok())
        .map(|path| path.to_string_lossy().to_string());
    Ok(folder)
}
