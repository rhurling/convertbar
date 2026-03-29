mod db;
mod types;
mod commands;
mod handbrake;
mod converter;

use converter::{ConverterState, MenuBarUpdate};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{Listener, Manager};
use tauri::tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState};

pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub preset_cache: Mutex<HashMap<String, handbrake::PresetMetadata>>,
    pub saved_window_pos: Mutex<Option<(i32, i32)>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = db::get_db_path();
    let conn = Connection::open(&db_path).expect("Failed to open database");
    db::init_db(&conn).expect("Failed to initialize database");

    let converter_state = Arc::new(ConverterState::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            db: Arc::new(Mutex::new(conn)),
            preset_cache: Mutex::new(HashMap::new()),
            saved_window_pos: Mutex::new(None),
        })
        .manage(converter_state)
        .invoke_handler(tauri::generate_handler![
            commands::settings::get_settings,
            commands::settings::update_setting,
            commands::settings::get_preset_suffix,
            commands::settings::set_preset_suffix,
            commands::handbrake::detect_handbrake,
            commands::handbrake::list_handbrake_presets,
            commands::handbrake::generate_preset_suffix,
            commands::queue::add_files,
            commands::queue::scan_folder,
            commands::queue::confirm_folder_add,
            commands::queue::get_queue,
            commands::queue::remove_job,
            commands::queue::reorder_queue,
            commands::queue::clear_completed,
            commands::queue::get_history,
            commands::queue::get_history_summary,
            commands::queue::classify_paths,
            commands::converter::start_queue,
            commands::converter::pause_conversion,
            commands::converter::resume_conversion,
            commands::converter::cancel_conversion,
            commands::converter::pause_after_current,
            commands::converter::cancel_pause_after_current,
            commands::converter::quit_app,
        ])
        .setup(|app| {
            // Task 7: System Tray
            let tray = TrayIconBuilder::new()
                .tooltip("ConvertBar — No active conversions")
                .title("")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png")).unwrap())
                .icon_as_template(true)
                .on_tray_icon_event(|tray_icon, event| {
                    match event {
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } => {
                            let app = tray_icon.app_handle();
                            if let Some(window) = app.get_webview_window("main") {
                                if window.is_visible().unwrap_or(false) {
                                    // Save position before hiding
                                    if let Ok(pos) = window.outer_position() {
                                        let state = app.state::<AppState>();
                                        *state.saved_window_pos.lock().unwrap() = Some((pos.x, pos.y));
                                    }
                                    let _ = window.hide();
                                } else {
                                    // Restore saved position
                                    let state = app.state::<AppState>();
                                    if let Some((x, y)) = *state.saved_window_pos.lock().unwrap() {
                                        let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)));
                                    }

                                    // Confine to screen bounds
                                    if let Ok(Some(monitor)) = window.current_monitor() {
                                        if let (Ok(win_pos), Ok(win_size)) = (window.outer_position(), window.outer_size()) {
                                            let mon_pos = monitor.position();
                                            let mon_size = monitor.size();

                                            let mut x = win_pos.x;
                                            let mut y = win_pos.y;
                                            let w = win_size.width as i32;
                                            let _h = win_size.height as i32;
                                            let sw = mon_size.width as i32;
                                            let sh = mon_size.height as i32;
                                            let sx = mon_pos.x;
                                            let sy = mon_pos.y;

                                            // At least 1/4 of window must be on screen
                                            let min_x = sx - (w * 3 / 4);
                                            let max_x = sx + sw - (w / 4);
                                            // Tab bar (top ~40px) must always be accessible
                                            let min_y = sy;
                                            let max_y = sy + sh - 40;

                                            x = x.clamp(min_x, max_x);
                                            y = y.clamp(min_y, max_y);

                                            if x != win_pos.x || y != win_pos.y {
                                                let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)));
                                            }
                                        }
                                    }

                                    let _ = window.show();
                                    let _ = window.set_focus();
                                }
                            }
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // Listen for menu-bar-update events to update tray title/tooltip
            let tray_id = tray.id().clone();
            let app_handle = app.handle().clone();
            let db_for_tray = app.state::<AppState>().db.clone();
            app.listen("menu-bar-update", move |event| {
                if let Ok(update) = serde_json::from_str::<MenuBarUpdate>(event.payload()) {
                    if let Some(tray) = app_handle.tray_by_id(&tray_id) {
                        match update.status.as_str() {
                            "encoding" => {
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
                                    if let Some(count) = update.queue_count {
                                        if count > 0 {
                                            parts.push(format!("+{}", count));
                                        }
                                    }
                                }
                                if show_filename {
                                    if let Some(ref name) = update.file_name {
                                        let truncated = if name.len() > 20 {
                                            format!("{}…", &name[..19])
                                        } else {
                                            name.clone()
                                        };
                                        parts.push(truncated);
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

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
