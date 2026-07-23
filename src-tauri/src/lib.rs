mod add_progress;
mod commands;
mod converter;
mod db;
mod handbrake;
mod media_skip;
mod probe;
mod probe_cache;
mod types;
mod watcher;

use converter::{ConverterState, MenuBarUpdate};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Listener, Manager};
use tauri_plugin_updater::UpdaterExt;

pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub preset_cache: Mutex<HashMap<String, handbrake::PresetMetadata>>,
}

/// Truncate a filename for the tray title on a char boundary — byte slicing panics
/// mid-codepoint on multi-byte names (umlauts, CJK, emoji), crashing the tray updater.
fn truncate_tray_title(name: &str) -> String {
    if name.chars().count() > 20 {
        format!("{}…", name.chars().take(19).collect::<String>())
    } else {
        name.to_string()
    }
}

/// Body for the startup-updater notification. Both outcomes notify (D5/D15): a silent install
/// failure would leave the user invisibly stuck on an old version, so failure is surfaced too —
/// an offline *check* (no update found) stays quiet at the call site and never reaches here.
fn update_install_notification(version: &str, installed: bool) -> String {
    if installed {
        format!("Updated to {version} — restart ConvertBar to apply")
    } else {
        format!("Update to {version} failed to install — still on the current version")
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = db::get_db_path();
    let conn = Connection::open(&db_path).expect("Failed to open database");
    db::init_db(&conn).expect("Failed to initialize database");

    let converter_state = Arc::new(ConverterState::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            db: Arc::new(Mutex::new(conn)),
            preset_cache: Mutex::new(HashMap::new()),
        })
        .manage(converter_state)
        .manage(watcher::WatcherState::new())
        .invoke_handler(tauri::generate_handler![
            commands::settings::get_settings,
            commands::settings::update_setting,
            commands::settings::get_preset_suffix,
            commands::settings::set_preset_suffix,
            commands::handbrake::detect_handbrake,
            commands::handbrake::list_handbrake_presets,
            commands::handbrake::generate_preset_suffix,
            commands::handbrake::resolve_suffix_template,
            commands::queue::add_files,
            commands::queue::scan_folder,
            commands::queue::confirm_folder_add,
            commands::queue::get_queue,
            commands::queue::remove_job,
            commands::queue::remove_history_entry,
            commands::queue::reorder_queue,
            commands::queue::clear_completed,
            commands::queue::get_history,
            commands::queue::get_history_summary,
            commands::queue::classify_paths,
            commands::queue::clear_queue,
            commands::converter::start_queue,
            commands::converter::pause_conversion,
            commands::converter::resume_conversion,
            commands::converter::cancel_conversion,
            commands::converter::pause_after_current,
            commands::converter::cancel_pause_after_current,
            commands::converter::get_pause_after_current,
            commands::converter::get_low_disk_pause,
            commands::converter::quit_app,
            commands::converter::get_platform_capabilities,
            commands::handbrake::validate_handbrake,
            commands::watch::get_watched_directories,
            commands::watch::add_watched_directory,
            commands::watch::update_watched_directory,
            commands::watch::set_watched_directory_enabled,
            commands::watch::remove_watched_directory,
            commands::watch::pick_folder,
            commands::files::check_paths_exist,
            commands::files::open_path,
            commands::files::reveal_in_dir,
        ])
        .setup(|app| {
            // Shared error flag for tray icon state
            let has_error: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

            // Task 7: System Tray
            let show_item = MenuItem::with_id(app, "show", "Show ConvertBar", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit ConvertBar", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &sep, &quit_item])?;

            let tray = TrayIconBuilder::with_id("main")
                .tooltip("ConvertBar — No active conversions")
                .title("")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png")).unwrap())
                .icon_as_template(true)
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event({
                    let has_error = has_error.clone();
                    move |app, event| {
                        match event.id.as_ref() {
                            "show" => {
                                if let Some(window) = app.get_webview_window("main") {
                                    let _ = window.show();
                                    let _ = window.set_focus();
                                }
                                // Clear error indicator when user opens the app
                                let mut err = has_error.lock().unwrap();
                                if *err {
                                    *err = false;
                                    let conv = app.state::<Arc<ConverterState>>();
                                    let is_running = *conv.is_running.lock().unwrap();
                                    if !is_running {
                                        if let Some(tray) = app.tray_by_id("main") {
                                            let _ = tray.set_title(Some(""));
                                            let _ = tray.set_tooltip(Some("ConvertBar — No active conversions"));
                                        }
                                    }
                                }
                            }
                            "quit" => { app.exit(0); }
                            _ => {}
                        }
                    }
                })
                .on_tray_icon_event({
                    let has_error = has_error.clone();
                    move |tray_icon, event| {
                        match event {
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            } => {
                                let app = tray_icon.app_handle();
                                if let Some(window) = app.get_webview_window("main") {
                                    if window.is_visible().unwrap_or(false) {
                                        let _ = window.hide();
                                    } else {
                                        // Confine to screen bounds before showing. After a monitor
                                        // is disconnected the stored position may not map to any
                                        // monitor and current_monitor() yields nothing — exactly the
                                        // case confinement exists for — so fall back to the primary.
                                        let monitor = window
                                            .current_monitor()
                                            .ok()
                                            .flatten()
                                            .or_else(|| window.primary_monitor().ok().flatten());
                                        if let Some(monitor) = monitor {
                                            if let (Ok(win_pos), Ok(win_size)) = (window.outer_position(), window.outer_size()) {
                                                let mon_pos = monitor.position();
                                                let mon_size = monitor.size();

                                                let mut x = win_pos.x;
                                                let mut y = win_pos.y;
                                                let w = win_size.width as i32;
                                                let h = win_size.height as i32;
                                                let sw = mon_size.width as i32;
                                                let sh = mon_size.height as i32;
                                                let sx = mon_pos.x;
                                                let sy = mon_pos.y;

                                                // At least half the window visible on each axis
                                                let min_x = sx - (w / 2);
                                                let max_x = sx + sw - (w / 2);
                                                let min_y = sy;
                                                let max_y = sy + sh - (h / 2);

                                                x = x.clamp(min_x, max_x);
                                                y = y.clamp(min_y, max_y);

                                                if x != win_pos.x || y != win_pos.y {
                                                    let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)));
                                                }
                                            }
                                        }

                                        let _ = window.show();
                                        let _ = window.set_focus();

                                        // Clear error indicator when user opens the app
                                        let mut err = has_error.lock().unwrap();
                                        if *err {
                                            *err = false;
                                            let conv = app.state::<Arc<ConverterState>>();
                                            let is_running = *conv.is_running.lock().unwrap();
                                            if !is_running {
                                                let _ = tray_icon.set_title(Some(""));
                                                let _ = tray_icon.set_tooltip(Some("ConvertBar — No active conversions"));
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                })
                .build(app)?;

            // Listen for menu-bar-update events to update tray title/tooltip
            let tray_id = tray.id().clone();
            let app_handle = app.handle().clone();
            let db_for_tray = app.state::<AppState>().db.clone();
            let error_flag = has_error.clone();
            app.listen("menu-bar-update", move |event| {
                if let Ok(update) = serde_json::from_str::<MenuBarUpdate>(event.payload()) {
                    if let Some(tray) = app_handle.tray_by_id(&tray_id) {
                        match update.status.as_str() {
                            "encoding" => {
                                *error_flag.lock().unwrap() = false;
                                let mut parts: Vec<String> = Vec::new();

                                let (show_percent, show_eta, show_queue, show_filename, show_fps) = {
                                    let db = db_for_tray.lock().unwrap();
                                    let get_bool = |key: &str, default: bool| -> bool {
                                        db.query_row(
                                            "SELECT value FROM settings WHERE key = ?1",
                                            rusqlite::params![key],
                                            |row| row.get::<_, String>(0),
                                        )
                                        .map(|v| v == "true")
                                        .unwrap_or(default)
                                    };
                                    (
                                        get_bool("menubar_show_percent", true),
                                        get_bool("menubar_show_eta", true),
                                        get_bool("menubar_show_queue", false),
                                        get_bool("menubar_show_filename", false),
                                        get_bool("menubar_show_fps", false),
                                    )
                                };

                                if show_percent {
                                    if let Some(percent) = update.percent {
                                        parts.push(format!("{:.0}%", percent));
                                    }
                                }
                                if show_eta {
                                    if let Some(eta) = update.eta_seconds {
                                        if eta > 0 {
                                            let mins = eta / 60;
                                            let secs = eta % 60;
                                            parts.push(format!("ETA {}:{:02}", mins, secs));
                                        }
                                    }
                                }
                                if show_queue {
                                    let count = update.queue_count.unwrap_or_else(|| {
                                        // Progress updates don't include queue_count, so query DB
                                        let db = db_for_tray.lock().unwrap();
                                        db.query_row(
                                            "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
                                            [],
                                            |row| row.get::<_, i64>(0),
                                        ).unwrap_or(0) as usize
                                    });
                                    if count > 0 {
                                        parts.push(format!("+{}", count));
                                    }
                                }
                                if show_filename {
                                    if let Some(ref name) = update.file_name {
                                        parts.push(truncate_tray_title(name));
                                    }
                                }
                                if show_fps {
                                    if let Some(fps) = update.fps {
                                        if fps > 0.0 {
                                            parts.push(format!("{:.0}fps", fps));
                                        }
                                    }
                                }

                                let title = if parts.is_empty() {
                                    String::new()
                                } else {
                                    parts.join(" \u{00b7} ")
                                };
                                let _ = tray.set_title(Some(&title));
                                // Build detailed tooltip
                                let mut tooltip = String::from("ConvertBar");
                                if let Some(ref name) = update.file_name {
                                    tooltip.push_str(&format!(" — Converting {}", name));
                                }
                                if let Some(percent) = update.percent {
                                    tooltip.push_str(&format!(" — {:.0}%", percent));
                                }
                                if let Some(eta) = update.eta_seconds {
                                    let mins = eta / 60;
                                    let secs = eta % 60;
                                    tooltip.push_str(&format!(" — ETA {}:{:02}", mins, secs));
                                }
                                if let Some(count) = update.queue_count {
                                    if count > 0 {
                                        tooltip.push_str(&format!(" — {} queued", count));
                                    }
                                }
                                let _ = tray.set_tooltip(Some(&tooltip));
                            }
                            "paused" => {
                                let _ = tray.set_title(Some("⏸"));
                                let _ = tray.set_tooltip(Some("ConvertBar — Paused"));
                            }
                            "error" => {
                                *error_flag.lock().unwrap() = true;
                                let _ = tray.set_title(Some("!"));
                                let _ = tray.set_tooltip(Some("ConvertBar — Error"));
                            }
                            _ => {
                                let _ = tray.set_title(Some(""));
                                let _ = tray.set_tooltip(Some("ConvertBar — No active conversions"));
                            }
                        }
                    }
                }
            });

            // Task 8: Auto-resume on launch
            let app_state = app.state::<AppState>();
            let conv_state = app.state::<Arc<ConverterState>>();
            let has_queued;
            {
                let db = app_state.db.lock().unwrap();

                // Find interrupted jobs and reset to queued
                let mut stmt = db.prepare(
                    "SELECT id, output_path FROM jobs WHERE status IN ('encoding', 'paused')"
                ).unwrap();
                let interrupted: Vec<(String, String)> = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .flatten()
                    .collect();

                for (id, output_path) in &interrupted {
                    let _ = std::fs::remove_file(output_path);
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'queued' WHERE id = ?1",
                        params![id],
                    );
                }

                has_queued = db.query_row(
                    "SELECT COUNT(*) > 0 FROM jobs WHERE status = 'queued'",
                    [],
                    |row| row.get::<_, bool>(0),
                ).unwrap_or(false);
            }

            if has_queued {
                let db_arc = app_state.db.clone();
                let conv_arc = (*conv_state).clone();
                let app_handle = app.handle().clone();
                converter::run_queue(app_handle, db_arc, conv_arc);
            }

            // Arm directory watchers and ingest any files already present in enabled folders.
            watcher::start(app.handle().clone());

            // Check for updates on startup. Install stays automatic (decision D5),
            // but the user is told an update landed instead of it being invisible;
            // a check/download failure is normal offline behavior and stays quiet.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let Ok(updater) = handle.updater() else {
                    return;
                };
                if let Ok(Some(update)) = updater.check().await {
                    let version = update.version.clone();
                    let result = update.download_and_install(|_, _| {}, || {}).await;
                    if let Err(e) = &result {
                        // An update was found and the install began but failed — unlike an
                        // offline check, this silently strands the user on the old version.
                        eprintln!("updater: install of {version} failed: {e}");
                    }
                    use tauri_plugin_notification::NotificationExt;
                    let _ = handle
                        .notification()
                        .builder()
                        .title("ConvertBar")
                        .body(update_install_notification(&version, result.is_ok()))
                        .show();
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            // Fires for every exit path — quit_app, the tray Quit item, Cmd+Q, logout.
            // Without this, quitting mid-encode orphans HandBrakeCLI, which keeps
            // encoding into the partial output for hours; on the next launch
            // auto-resume would delete that file and start a second encoder against
            // the same path while the orphan still holds it.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let conv = app.state::<Arc<ConverterState>>();
                converter::kill_active_child(&conv);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_title_truncation_is_char_boundary_safe() {
        // Byte slicing (&name[..19]) panicked here: byte 19 lands mid-codepoint.
        let umlauts = "ääääääääääääääääääääää.mp4";
        let truncated = truncate_tray_title(umlauts);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 20);

        let emoji = "🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬.mp4";
        assert!(truncate_tray_title(emoji).ends_with('…'));

        // Short names pass through untouched.
        assert_eq!(truncate_tray_title("clip.mp4"), "clip.mp4");
        // A 20-char name must NOT be truncated (the old code cut at >20 bytes).
        let exactly_20 = "a".repeat(20);
        assert_eq!(truncate_tray_title(&exactly_20), exactly_20);
    }

    #[test]
    fn update_install_notification_surfaces_both_outcomes() {
        let ok = update_install_notification("1.2.3", true);
        assert!(ok.contains("Updated to 1.2.3"));

        // A failed install must NOT be silent (D15): the body names the failure and the
        // version so the user knows they're stranded on the old one.
        let failed = update_install_notification("1.2.3", false);
        assert!(failed.contains("1.2.3"));
        assert!(
            failed.to_lowercase().contains("failed"),
            "install failure must be surfaced, got: {failed}"
        );
        assert_ne!(ok, failed, "success and failure must read differently");
    }
}
