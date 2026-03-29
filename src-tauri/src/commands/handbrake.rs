use rusqlite::params;
use tauri::State;

use crate::handbrake as hb;
use crate::AppState;

#[tauri::command]
pub fn detect_handbrake(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    // Check user-configured path first
    let configured: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Ok(Some(path.clone()));
        }
    }

    // Auto-detect
    Ok(hb::detect_handbrake_path())
}

#[tauri::command]
pub fn list_handbrake_presets(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let path = detect_handbrake(state)?;
    match path {
        Some(p) => hb::list_presets(&p),
        None => Err("HandBrakeCLI not found".to_string()),
    }
}
