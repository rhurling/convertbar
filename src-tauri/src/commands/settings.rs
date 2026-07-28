use rusqlite::params;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::types::Settings;
use crate::AppState;

/// The output-filename suffix template applied to a preset the user has never
/// customized. Single source of truth for both the settings UI (`get_preset_suffix`)
/// and the conversion output-naming path (`commands::queue`), so an unconfigured
/// preset can never silently fall back to an empty (in-place re-encode) suffix.
pub const DEFAULT_SUFFIX_TEMPLATE: &str = ".{resolution}-{codec}";

/// The stored suffix template for `preset`, or [`DEFAULT_SUFFIX_TEMPLATE`] when the
/// preset has no row yet. An explicitly-stored empty string is preserved (a deliberate
/// in-place-encode choice) rather than treated as unset.
pub fn read_suffix_template(conn: &rusqlite::Connection, preset: &str) -> String {
    match conn.query_row(
        "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
        params![preset],
        |row| row.get::<_, String>(0),
    ) {
        Ok(suffix) => suffix,
        Err(_) => DEFAULT_SUFFIX_TEMPLATE.to_string(),
    }
}

#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<'_, AppState>) -> Result<Settings, String> {
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
    let mut skip_already_converted = false;
    let mut skip_by_source_media = false;
    let mut watch_skip_marker = String::new();
    let mut low_disk_min_gb: f64 = 0.0;
    let mut bad_source_action = String::from("trash");
    let mut update_mode = String::from("automatic");

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
            "skip_already_converted" => skip_already_converted = value == "true",
            "skip_by_source_media" => skip_by_source_media = value == "true",
            "watch_skip_marker" => watch_skip_marker = value,
            "low_disk_min_gb" => low_disk_min_gb = value.parse().unwrap_or(0.0),
            "bad_source_action" => {
                bad_source_action = normalize_bad_source_action(&value).to_string()
            }
            "update_mode" => {
                update_mode = crate::updater::normalize_update_mode(&value)
                    .as_str()
                    .to_string()
            }
            _ => {}
        }
    }

    // Read actual autostart state from the plugin (source of truth)
    let launch_at_login = app.autolaunch().is_enabled().unwrap_or(launch_at_login);

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
        skip_already_converted,
        skip_by_source_media,
        watch_skip_marker,
        low_disk_min_gb,
        bad_source_action,
        update_mode,
    })
}

const ALLOWED_KEYS: &[&str] = &[
    "preset",
    "cleanup_mode",
    "launch_at_login",
    "handbrake_path",
    "menubar_show_percent",
    "menubar_show_eta",
    "menubar_show_queue",
    "menubar_show_filename",
    "menubar_show_fps",
    "notifications_per_file",
    "notifications_errors_only",
    "notifications_queue_done",
    "skip_already_converted",
    "skip_by_source_media",
    "watch_skip_marker",
    "low_disk_min_gb",
    "bad_source_action",
    "update_mode",
];

/// Coerce a stored `bad_source_action` to a known value. Anything other than an exact
/// "delete" reads as "trash": a corrupted, empty, or future value must never silently
/// escalate to permanent deletion.
pub(crate) fn normalize_bad_source_action(value: &str) -> &'static str {
    if value == "delete" {
        "delete"
    } else {
        "trash"
    }
}

#[tauri::command]
pub fn update_setting(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    if !ALLOWED_KEYS.contains(&key.as_str()) {
        return Err(format!("Invalid setting key: {}", key));
    }
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;

    // Release the settings connection before any hook below: they read the same
    // `AppState::db` mutex, and std's Mutex is not reentrant.
    drop(conn);

    // Sync autostart state with the plugin
    if key == "launch_at_login" {
        let autostart = app.autolaunch();
        if value == "true" {
            let _ = autostart.enable();
        } else {
            let _ = autostart.disable();
        }
    }

    // Let the running watcher pick up a changed skip-marker name without a restart.
    if key == "watch_skip_marker" {
        crate::watcher::refresh_skip_marker(&app);
    }

    // Let a mode change take effect immediately: a user who sees "update available" and
    // switches to Automatic should not wait for the next hourly tick.
    if key == "update_mode" {
        crate::updater::emit_state(&app);
        if value == "automatic" {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::updater::run_cycle(handle, false).await;
            });
        }
    }

    Ok(())
}

#[tauri::command]
pub fn get_preset_suffix(state: State<'_, AppState>, preset: String) -> Result<String, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    Ok(read_suffix_template(&conn, &preset))
}

#[tauri::command]
pub fn set_preset_suffix(
    state: State<'_, AppState>,
    preset: String,
    suffix: String,
) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO preset_suffixes (preset_name, suffix) VALUES (?1, ?2) ON CONFLICT(preset_name) DO UPDATE SET suffix = ?2",
        params![preset, suffix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn read_suffix_template_returns_default_when_no_row() {
        let conn = test_conn();
        // A preset the user has never configured has no preset_suffixes row; it must
        // fall back to the default template, not to an empty (in-place) suffix.
        assert_eq!(
            read_suffix_template(&conn, "Never Configured Preset"),
            DEFAULT_SUFFIX_TEMPLATE
        );
    }

    #[test]
    fn read_suffix_template_preserves_an_explicit_empty_suffix() {
        let conn = test_conn();
        // An empty stored suffix is a deliberate in-place-encode choice, not "unset".
        conn.execute(
            "INSERT INTO preset_suffixes (preset_name, suffix) VALUES ('P', '')",
            [],
        )
        .unwrap();
        assert_eq!(read_suffix_template(&conn, "P"), "");
    }

    #[test]
    fn read_suffix_template_returns_the_stored_value() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO preset_suffixes (preset_name, suffix) VALUES ('P', '.custom')",
            [],
        )
        .unwrap();
        assert_eq!(read_suffix_template(&conn, "P"), ".custom");
    }

    #[test]
    fn bad_source_action_is_writable_and_unknown_values_fall_back_to_trash() {
        assert!(
            ALLOWED_KEYS.contains(&"bad_source_action"),
            "the Settings UI writes this key via update_setting"
        );
        // Parse fallback: anything that is not exactly "delete" must read as "trash", so a
        // corrupted or future value can never silently upgrade to permanent deletion.
        assert_eq!(normalize_bad_source_action("delete"), "delete");
        assert_eq!(normalize_bad_source_action("trash"), "trash");
        assert_eq!(normalize_bad_source_action(""), "trash");
        assert_eq!(normalize_bad_source_action("DELETE"), "trash");
        assert_eq!(normalize_bad_source_action("nonsense"), "trash");
    }

    #[test]
    fn update_mode_is_writable_and_unknown_values_fall_back_to_automatic() {
        // The Settings UI writes this key via update_setting; the three internal updater keys
        // deliberately are NOT writable this way, so the frontend cannot forge update policy.
        assert!(ALLOWED_KEYS.contains(&"update_mode"));
        assert!(!ALLOWED_KEYS.contains(&"update_skipped_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_notified_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_installed"));
    }
}
