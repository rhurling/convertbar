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
        "H.265 MKV 1080p30"
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
            source_size     INTEGER,
            source_mtime    INTEGER,
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
        CREATE TABLE IF NOT EXISTS watched_directories (
            id                   TEXT PRIMARY KEY,
            path                 TEXT NOT NULL UNIQUE,
            recursive            INTEGER NOT NULL DEFAULT 0,
            stability_delay_secs INTEGER NOT NULL DEFAULT 5,
            enabled              INTEGER NOT NULL DEFAULT 1,
            created_at           TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS probe_cache (
            path      TEXT PRIMARY KEY,
            size      INTEGER NOT NULL,
            mtime     INTEGER NOT NULL,
            codec     TEXT NOT NULL,
            height    INTEGER NOT NULL,
            probed_at TEXT NOT NULL
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

    // Older DBs predate the source-identity fingerprint columns. A fresh DB already has them
    // from CREATE TABLE, so "duplicate column name" is expected and ignored — this keeps the
    // upgrade idempotent. Any other ALTER failure is re-raised so a real error is not masked.
    for col in ["source_size", "source_mtime"] {
        if let Err(e) = conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {col} INTEGER"), []) {
            if !e.to_string().contains("duplicate column name") {
                return Err(e);
            }
        }
    }

    // Backfill: Linux DBs seeded before 0.13.1 stored "H.265 MKV 1080p", which is not a
    // valid preset name in current HandBrake (the built-in is "H.265 MKV 1080p30"), so
    // every default conversion failed. INSERT OR IGNORE below won't touch existing rows,
    // so correct the stored value and its suffix-template row here.
    conn.execute(
        "UPDATE settings SET value = 'H.265 MKV 1080p30' WHERE key = 'preset' AND value = 'H.265 MKV 1080p'",
        [],
    )?;
    conn.execute(
        "UPDATE OR IGNORE preset_suffixes SET preset_name = 'H.265 MKV 1080p30' WHERE preset_name = 'H.265 MKV 1080p'",
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
        ("skip_by_source_media", "false"),
        ("watch_skip_marker", ".downloading"),
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
        assert_eq!(count, 15);

        // Platform-neutral fixed defaults.
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("trash"));
        assert_eq!(setting(&conn, "launch_at_login").as_deref(), Some("false"));
        assert_eq!(
            setting(&conn, "skip_already_converted").as_deref(),
            Some("false")
        );
        assert_eq!(
            setting(&conn, "skip_by_source_media").as_deref(),
            Some("false"),
            "skip-by-source-media defaults OFF — it shells out to HandBrake per file, so it is opt-in"
        );
        assert_eq!(
            setting(&conn, "watch_skip_marker").as_deref(),
            Some(".downloading"),
            "watched folders skip files while this marker exists; empty disables the feature"
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
        assert_eq!(count, 15);
    }

    #[test]
    fn init_db_repairs_the_invalid_pre_0_13_1_linux_default_preset() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Simulate a DB seeded by an older Linux build: "H.265 MKV 1080p" is not a
        // built-in preset in current HandBrake, so every default conversion failed.
        conn.execute(
            "UPDATE settings SET value = 'H.265 MKV 1080p' WHERE key = 'preset'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE preset_suffixes SET preset_name = 'H.265 MKV 1080p'",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        assert_eq!(
            setting(&conn, "preset").as_deref(),
            Some("H.265 MKV 1080p30"),
            "restart must repair the stored preset name or Linux default conversions keep failing"
        );
        let suffix: String = conn
            .query_row(
                "SELECT suffix FROM preset_suffixes WHERE preset_name = 'H.265 MKV 1080p30'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            suffix, ".{resolution}-{codec}",
            "the user's suffix template must follow the renamed preset"
        );
    }

    #[test]
    fn init_db_repair_leaves_a_user_chosen_preset_alone() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "UPDATE settings SET value = 'H.264 MKV 720p30' WHERE key = 'preset'",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        // The repair targets only the known-bad seeded value, never a deliberate choice.
        assert_eq!(
            setting(&conn, "preset").as_deref(),
            Some("H.264 MKV 720p30")
        );
    }

    #[test]
    fn init_db_creates_empty_watched_directories_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // The table exists and starts empty — watching is opt-in, nothing is seeded.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM watched_directories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // The schema accepts the columns the feature relies on, with the documented defaults.
        conn.execute(
            "INSERT INTO watched_directories (id, path, created_at) VALUES ('w1', '/movies', '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let (recursive, delay, enabled): (i64, i64, i64) = conn
            .query_row(
                "SELECT recursive, stability_delay_secs, enabled FROM watched_directories WHERE id = 'w1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(recursive, 0);
        assert_eq!(delay, 5);
        assert_eq!(enabled, 1);

        // `path` is unique so the same folder can't be registered twice.
        let dup = conn.execute(
            "INSERT INTO watched_directories (id, path, created_at) VALUES ('w2', '/movies', '2020-01-02T00:00:00Z')",
            [],
        );
        assert!(
            dup.is_err(),
            "duplicate path should violate UNIQUE constraint"
        );
    }

    #[test]
    fn init_db_creates_probe_cache_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // The table accepts a full cache row with the documented columns.
        conn.execute(
            "INSERT INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES ('/m/a.mp4', 100, 5000, 'h265', 1080, '2026-06-22T00:00:00Z')",
            [],
        )
        .unwrap();

        let (size, mtime, codec, height): (i64, i64, String, i64) = conn
            .query_row(
                "SELECT size, mtime, codec, height FROM probe_cache WHERE path = '/m/a.mp4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (size, mtime, codec.as_str(), height),
            (100, 5000, "h265", 1080)
        );

        // `path` is the primary key, so a second row for the same path is rejected.
        let dup = conn.execute(
            "INSERT INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES ('/m/a.mp4', 1, 1, 'h264', 1, '2026-06-22T00:00:00Z')",
            [],
        );
        assert!(
            dup.is_err(),
            "duplicate path should violate the PRIMARY KEY"
        );
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

    #[test]
    fn init_db_upgrades_an_older_jobs_table_missing_the_fingerprint_columns() {
        // A DB created before the source-identity fingerprint feature has a `jobs` table without
        // source_size/source_mtime. `CREATE TABLE IF NOT EXISTS` leaves the existing table alone,
        // so the idempotent ALTER migration is the only thing that adds the columns to an old DB —
        // an upgrade path no fresh-DB test exercises (a fresh CREATE already includes them).
        let conn = Connection::open_in_memory().unwrap();

        // Hand-build the pre-fingerprint schema and a job row from that era.
        conn.execute_batch(
            "CREATE TABLE jobs (
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
            );",
        )
        .unwrap();
        insert_job(&conn, "old", "done", "2020-01-01T00:00:00Z", None);

        // Upgrade the existing DB in place.
        init_db(&conn).unwrap();

        // The migration added both columns; the pre-existing row survives with NULL fingerprints
        // (the SELECT itself would error if the columns were never added).
        let fingerprint = |id: &str| -> (Option<i64>, Option<i64>) {
            conn.query_row(
                "SELECT source_size, source_mtime FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            fingerprint("old"),
            (None, None),
            "an old row survives the upgrade with empty fingerprint columns"
        );

        // The upgraded table is writable through the new columns: backfill the old row and insert
        // a fresh job that carries a fingerprint.
        conn.execute(
            "UPDATE jobs SET source_size = 123, source_mtime = 456 WHERE id = 'old'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at, source_size, source_mtime)
             VALUES ('new', '/src/new.mov', '/out/new.mp4', 'preset', 'queued', 1, '2026-01-01T00:00:00Z', 789, 1011)",
            [],
        )
        .unwrap();
        assert_eq!(fingerprint("old"), (Some(123), Some(456)));
        assert_eq!(fingerprint("new"), (Some(789), Some(1011)));
    }
}
