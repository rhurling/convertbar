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

    // Backfill: early error rows were stored without a completion timestamp, which
    // sorted them below every successful job (NULLs last under ORDER BY completed_at DESC)
    // and hid them from History. Use created_at as the best available timestamp.
    conn.execute(
        "UPDATE jobs SET completed_at = created_at WHERE status = 'error' AND completed_at IS NULL",
        [],
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
        ("skip_already_converted", "false"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(conn: &Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .ok()
    }

    fn insert_job(
        conn: &Connection,
        id: &str,
        status: &str,
        created_at: &str,
        completed_at: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at, completed_at)
             VALUES (?1, ?2, ?3, 'preset', ?4, 0, ?5, ?6)",
            rusqlite::params![
                id,
                format!("/src/{id}.mov"),
                format!("/out/{id}.mp4"),
                status,
                created_at,
                completed_at,
            ],
        )
        .unwrap();
    }

    #[test]
    fn init_db_seeds_defaults() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // The exact set of seeded settings keys — count guards against accidental
        // additions/removals drifting out of sync with the app's expectations.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 13);

        // Platform-neutral fixed defaults.
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("trash"));
        assert_eq!(setting(&conn, "launch_at_login").as_deref(), Some("false"));
        assert_eq!(
            setting(&conn, "skip_already_converted").as_deref(),
            Some("false")
        );
        assert_eq!(
            setting(&conn, "notifications_per_file").as_deref(),
            Some("true")
        );

        // The default preset and its suffix template are seeded together.
        assert_eq!(setting(&conn, "preset").as_deref(), Some(default_preset()));
        let suffix: String = conn
            .query_row(
                "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
                rusqlite::params![default_preset()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(suffix, ".{resolution}-{codec}");
    }

    #[test]
    fn init_db_is_idempotent_and_preserves_user_changes() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Simulate a user changing a setting, then an app restart re-running init_db.
        conn.execute(
            "UPDATE settings SET value = 'delete' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        // INSERT OR IGNORE must not clobber the user's value or duplicate rows.
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("delete"));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 13);
    }

    #[test]
    fn init_db_backfills_error_rows_missing_completed_at() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Old error row written before completed_at was tracked.
        insert_job(&conn, "e1", "error", "2020-01-01T00:00:00Z", None);
        // Error row that already has a completion timestamp — must be left alone.
        insert_job(
            &conn,
            "e2",
            "error",
            "2020-02-01T00:00:00Z",
            Some("2020-03-01T00:00:00Z"),
        );
        // Non-error row with a NULL completed_at — the backfill must not touch it.
        insert_job(&conn, "q1", "queued", "2020-04-01T00:00:00Z", None);

        // Re-running init_db runs the backfill migration.
        init_db(&conn).unwrap();

        let completed = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT completed_at FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };

        assert_eq!(completed("e1").as_deref(), Some("2020-01-01T00:00:00Z"));
        assert_eq!(completed("e2").as_deref(), Some("2020-03-01T00:00:00Z"));
        assert_eq!(completed("q1"), None);
    }
}
