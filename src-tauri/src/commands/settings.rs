use rusqlite::params;
use tauri::State;

use crate::types::Settings;
use crate::AppState;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT key, value FROM settings")
        .map_err(|e| e.to_string())?;

    let mut preset = String::new();
    let mut cleanup_mode = String::new();
    let mut launch_at_login = false;
    let mut handbrake_path = String::new();
    let mut menubar_show_percent = true;
    let mut menubar_show_eta = true;
    let mut menubar_show_queue = false;
    let mut menubar_show_filename = false;
    let mut menubar_show_fps = false;
    let mut notifications_per_file = true;
    let mut notifications_errors_only = false;
    let mut notifications_queue_done = true;

    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (key, value) = row.map_err(|e| e.to_string())?;
        match key.as_str() {
            "preset" => preset = value,
            "cleanup_mode" => cleanup_mode = value,
            "launch_at_login" => launch_at_login = value == "true",
            "handbrake_path" => handbrake_path = value,
            "menubar_show_percent" => menubar_show_percent = value == "true",
            "menubar_show_eta" => menubar_show_eta = value == "true",
            "menubar_show_queue" => menubar_show_queue = value == "true",
            "menubar_show_filename" => menubar_show_filename = value == "true",
            "menubar_show_fps" => menubar_show_fps = value == "true",
            "notifications_per_file" => notifications_per_file = value == "true",
            "notifications_errors_only" => notifications_errors_only = value == "true",
            "notifications_queue_done" => notifications_queue_done = value == "true",
            _ => {}
        }
    }

    Ok(Settings {
        preset,
        cleanup_mode,
        launch_at_login,
        handbrake_path,
        menubar_show_percent,
        menubar_show_eta,
        menubar_show_queue,
        menubar_show_filename,
        menubar_show_fps,
        notifications_per_file,
        notifications_errors_only,
        notifications_queue_done,
    })
}

const ALLOWED_KEYS: &[&str] = &[
    "preset", "cleanup_mode", "launch_at_login", "handbrake_path",
    "menubar_show_percent", "menubar_show_eta", "menubar_show_queue",
    "menubar_show_filename", "menubar_show_fps",
    "notifications_per_file", "notifications_errors_only", "notifications_queue_done",
];

#[tauri::command]
pub fn update_setting(state: State<'_, AppState>, key: String, value: String) -> Result<(), String> {
    if !ALLOWED_KEYS.contains(&key.as_str()) {
        return Err(format!("Invalid setting key: {}", key));
    }
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_preset_suffix(state: State<'_, AppState>, preset: String) -> Result<Option<String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let result = conn.query_row(
        "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
        params![preset],
        |row| row.get::<_, String>(0),
    );

    match result {
        Ok(suffix) => Ok(Some(suffix)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn set_preset_suffix(state: State<'_, AppState>, preset: String, suffix: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO preset_suffixes (preset_name, suffix) VALUES (?1, ?2) ON CONFLICT(preset_name) DO UPDATE SET suffix = ?2",
        params![preset, suffix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
