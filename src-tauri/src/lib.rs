mod db;
mod types;
mod commands;
mod handbrake;

use rusqlite::Connection;
use std::sync::Mutex;

pub struct AppState {
    pub db: Mutex<Connection>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = db::get_db_path();
    let conn = Connection::open(&db_path).expect("Failed to open database");
    db::init_db(&conn).expect("Failed to initialize database");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState { db: Mutex::new(conn) })
        .invoke_handler(tauri::generate_handler![
            commands::settings::get_settings,
            commands::settings::update_setting,
            commands::settings::get_preset_suffix,
            commands::settings::set_preset_suffix,
            commands::handbrake::detect_handbrake,
            commands::handbrake::list_handbrake_presets,
            commands::queue::add_files,
            commands::queue::scan_folder,
            commands::queue::confirm_folder_add,
            commands::queue::get_queue,
            commands::queue::remove_job,
            commands::queue::reorder_queue,
            commands::queue::clear_completed,
            commands::queue::get_history,
            commands::queue::get_history_summary,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
