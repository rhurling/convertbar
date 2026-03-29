use rusqlite::{Connection, Result};
use std::path::PathBuf;

pub fn get_db_path() -> PathBuf {
    let app_support = dirs::data_dir().expect("Could not find Application Support directory");
    let db_dir = app_support.join("com.convertbar.app");
    std::fs::create_dir_all(&db_dir).expect("Could not create app data directory");
    db_dir.join("convertbar.db")
}

fn default_preset() -> &'static str {
    if cfg!(target_os = "macos") {
        "H.265 Apple VideoToolbox 1080p"
    } else if cfg!(target_os = "windows") {
        "H.265 NVENC 1080p"
    } else {
        "H.265 MKV 1080p"
    }
}

pub fn init_db(conn: &Connection) -> Result<()> {
    let preset = default_preset();

    conn.execute_batch(
        "
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
    ",
    )?;

    let defaults: &[(&str, &str)] = &[
        ("preset", preset),
        ("cleanup_mode", "trash"),
        ("launch_at_login", "false"),
        ("handbrake_path", ""),
        ("menubar_show_percent", "true"),
        ("menubar_show_eta", "true"),
        ("menubar_show_queue", "false"),
        ("menubar_show_filename", "false"),
        ("menubar_show_fps", "false"),
        ("notifications_per_file", "true"),
        ("notifications_errors_only", "false"),
        ("notifications_queue_done", "true"),
    ];

    for (key, value) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
    }

    conn.execute(
        "INSERT OR IGNORE INTO preset_suffixes (preset_name, suffix) VALUES (?1, ?2)",
        rusqlite::params![preset, ".{resolution}-{codec}"],
    )?;

    Ok(())
}
