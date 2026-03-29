use rusqlite::{Connection, Result};
use std::path::PathBuf;

pub fn get_db_path() -> PathBuf {
    let app_support = dirs::data_dir()
        .expect("Could not find Application Support directory");
    let db_dir = app_support.join("com.convertbar.app");
    std::fs::create_dir_all(&db_dir).expect("Could not create app data directory");
    db_dir.join("convertbar.db")
}

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS jobs (
            id              TEXT PRIMARY KEY,
            source_path     TEXT NOT NULL,
            output_path     TEXT NOT NULL,
            preset          TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'queued',
            original_size   INTEGER,
            converted_size  INTEGER,
            kept_file       TEXT,
            space_saved     INTEGER,
            error_message   TEXT,
            queue_order     INTEGER NOT NULL,
            created_at      TEXT NOT NULL,
            completed_at    TEXT
        );
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS preset_suffixes (
            preset_name TEXT PRIMARY KEY,
            suffix      TEXT NOT NULL
        );
        INSERT OR IGNORE INTO settings (key, value) VALUES ('preset', 'H.265 Apple VideoToolbox 1080p');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('cleanup_mode', 'trash');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('launch_at_login', 'false');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('handbrake_path', '');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('menubar_show_percent', 'true');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('menubar_show_eta', 'true');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('menubar_show_queue', 'false');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('menubar_show_filename', 'false');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('menubar_show_fps', 'false');
        INSERT OR IGNORE INTO preset_suffixes (preset_name, suffix) VALUES ('H.265 Apple VideoToolbox 1080p', '.{resolution}-{codec}');
    ")
}
