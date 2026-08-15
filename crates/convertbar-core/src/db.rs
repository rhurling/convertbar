use rusqlite::{params, Connection, Result};
use std::path::{Path, PathBuf};

/// Canonical form of a watch path. Mirrors `commands::watch::canonical_watch_path`: dunce avoids
/// the Windows `\\?\` prefix, and a path that can't be resolved (e.g. a folder no longer on disk)
/// is returned unchanged so the backfill leaves it alone rather than mangling it.
fn canonical_watch_path(path: &str) -> String {
    dunce::canonicalize(Path::new(path))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// One-time migration for watched_directories rows written before `canonical_watch_path` ran on
/// insert: rewrite each path to its canonical form so re-adding the same folder (which now stores
/// the canonical path) can't slip past the UNIQUE(path) check and create a second watcher over one
/// folder. Idempotent — already-canonical paths are skipped. A row whose canonical form collides
/// with another row is an alias duplicate and is deleted rather than violating UNIQUE(path).
/// Ordered by created_at so that when 3+ rows alias one folder the *earliest* row is the one
/// canonicalized (and kept) and the later aliases are the ones dropped — a deterministic survivor
/// instead of relying on SQLite's unspecified scan order.
fn backfill_canonical_watch_paths(conn: &Connection) -> Result<()> {
    let rows: Vec<(String, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, path FROM watched_directories ORDER BY created_at")?;
        let mapped =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    for (id, path) in rows {
        let canonical = canonical_watch_path(&path);
        if canonical == path {
            continue;
        }
        let collides: i64 = conn.query_row(
            "SELECT COUNT(*) FROM watched_directories WHERE path = ?1 AND id != ?2",
            params![canonical, id],
            |r| r.get(0),
        )?;
        if collides > 0 {
            conn.execute("DELETE FROM watched_directories WHERE id = ?1", params![id])?;
        } else {
            conn.execute(
                "UPDATE watched_directories SET path = ?1 WHERE id = ?2",
                params![canonical, id],
            )?;
        }
    }
    Ok(())
}

/// Resolve the data dir: an explicit base (from CONVERTBAR_DATA_DIR) wins; otherwise the
/// platform data dir + com.convertbar.app. Creates the directory either way.
pub fn get_db_path_from(override_base: Option<PathBuf>) -> PathBuf {
    let base = override_base.unwrap_or_else(|| {
        dirs::data_dir()
            .expect("Could not find Application Support directory")
            .join("com.convertbar.app")
    });
    std::fs::create_dir_all(&base).expect("Could not create app data directory");
    base.join("convertbar.db")
}

pub fn get_db_path() -> PathBuf {
    get_db_path_from(std::env::var_os("CONVERTBAR_DATA_DIR").map(PathBuf::from))
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

/// Whether [`init_db`] found an existing database or created one.
///
/// A head uses this to seed a default that must apply to new installs only. Deliberately
/// **not** `#[must_use]`: 74 call sites legitimately discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbInit {
    Fresh,
    Existing,
}

pub fn init_db(conn: &Connection) -> Result<DbInit> {
    // Probed before any CREATE TABLE below, which is what makes the answer meaningful.
    // Accepted tradeoff: a boot that crashed after CREATE TABLE IF NOT EXISTS settings but
    // before the defaults INSERT loop below leaves a genuinely fresh install reporting
    // Existing on the next init_db, landing it at `normal` instead of `low`. Harmless —
    // read_encode_priority already defaults an absent row to `normal`.
    let state = if conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'settings'",
        [],
        |row| row.get::<_, i64>(0),
    )? == 0
    {
        DbInit::Fresh
    } else {
        DbInit::Existing
    };

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
            failure_class   TEXT,
            queue_order     INTEGER NOT NULL,
            created_at      TEXT NOT NULL,
            started_at      TEXT,
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

    // Backfill: error rows written before the diagnostic headline was promoted lead with
    // HandBrake's build banner ("Compile-time hardening features are enabled"), which is
    // all the single-line history UI shows. Re-promote the real failure reason so old
    // failures read as clearly as new ones. Idempotent — promote_stored_diagnostic returns
    // None once a row is already headlined (or has no diagnostic to surface).
    let legacy_errors: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, error_message FROM jobs WHERE status = 'error' AND error_message IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<Result<Vec<_>>>()?
    };
    for (id, message) in legacy_errors {
        if let Some(promoted) = crate::failure_class::promote_stored_diagnostic(&message) {
            conn.execute(
                "UPDATE jobs SET error_message = ?2 WHERE id = ?1",
                params![id, promoted],
            )?;
        }
    }

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

    // Older DBs predate the failure classification column. Same idempotent pattern as the
    // fingerprint columns above, but TEXT — so it needs its own ALTER rather than a new
    // entry in that INTEGER-typed loop.
    if let Err(e) = conn.execute("ALTER TABLE jobs ADD COLUMN failure_class TEXT", []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }

    // Older DBs predate the encode-start timestamp. Same idempotent pattern as
    // failure_class above. No backfill: a row written before this column existed has no
    // knowable start time, and NULL renders no duration.
    if let Err(e) = conn.execute("ALTER TABLE jobs ADD COLUMN started_at TEXT", []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
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

    backfill_canonical_watch_paths(conn)?;

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
        ("low_disk_min_gb", "0"),
        ("bad_source_action", "trash"),
        ("update_mode", "automatic"),
        ("history_show_duration", "true"),
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

    Ok(state)
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
        assert_eq!(count, 19);

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
            setting(&conn, "low_disk_min_gb").as_deref(),
            Some("0"),
            "low-disk auto-pause is off (0) until the user sets a GB threshold"
        );
        assert_eq!(
            setting(&conn, "notifications_per_file").as_deref(),
            Some("true")
        );
        assert_eq!(setting(&conn, "update_mode").as_deref(), Some("automatic"));

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
    fn init_db_reports_fresh_only_the_first_time() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            init_db(&conn).unwrap(),
            DbInit::Fresh,
            "a database with no settings table has never been initialized"
        );
        assert_eq!(
            init_db(&conn).unwrap(),
            DbInit::Existing,
            "re-running init_db on the same connection must not look like a new install"
        );
    }

    #[test]
    fn history_duration_defaults_on() {
        // The setting's whole rationale is that Docker users get the duration without
        // hunting through settings. A default flip would be invisible in every other test.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(
            setting(&conn, "history_show_duration").as_deref(),
            Some("true")
        );
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
        assert_eq!(count, 19);
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
    fn backfill_rewrites_a_noncanonical_watch_path_to_canonical() {
        // A row written before canonical_watch_path ran on insert kept a verbatim alias; the
        // backfill must rewrite it so re-adding the same folder (now stored canonical) collides
        // on UNIQUE(path) instead of creating a second watcher.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let canonical = canonical_watch_path(dir.path().to_str().unwrap());
        let alias = format!(
            "{}{}",
            dir.path().to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        assert_ne!(
            alias, canonical,
            "test premise: the alias is not already canonical"
        );

        conn.execute(
            "INSERT INTO watched_directories (id, path, created_at) VALUES ('w1', ?1, '2020-01-01T00:00:00Z')",
            params![alias],
        )
        .unwrap();

        backfill_canonical_watch_paths(&conn).unwrap();

        let stored: String = conn
            .query_row(
                "SELECT path FROM watched_directories WHERE id = 'w1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, canonical);
    }

    #[test]
    fn backfill_drops_an_alias_row_that_collides_with_an_existing_canonical_row() {
        // Two pre-fix rows aliasing one folder would violate UNIQUE(path) once canonicalized, so
        // the alias is dropped — exactly one watcher over the folder, not two.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let canonical = canonical_watch_path(dir.path().to_str().unwrap());
        let alias = format!(
            "{}{}",
            dir.path().to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );

        conn.execute(
            "INSERT INTO watched_directories (id, path, created_at) VALUES ('keep', ?1, '2020-01-01T00:00:00Z')",
            params![canonical],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO watched_directories (id, path, created_at) VALUES ('alias', ?1, '2020-01-02T00:00:00Z')",
            params![alias],
        )
        .unwrap();

        backfill_canonical_watch_paths(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM watched_directories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "the aliased duplicate is removed");
        let remaining: String = conn
            .query_row("SELECT id FROM watched_directories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, "keep", "the canonical row is the survivor");
    }

    #[test]
    fn backfill_keeps_the_earliest_of_three_aliasing_rows() {
        // 3+ rows aliasing one folder (all non-canonical) must resolve to exactly one survivor,
        // deterministically the earliest-created one. Rows are inserted latest-first so a scan
        // without ORDER BY created_at (SQLite's rowid order) would keep the latest and fail this.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_string_lossy().to_string();
        let sep = std::path::MAIN_SEPARATOR;
        // Three distinct non-canonical aliases of the same folder (extra trailing separators).
        let aliases = [
            format!("{base}{sep}{sep}{sep}"),
            format!("{base}{sep}{sep}"),
            format!("{base}{sep}"),
        ];
        // Insert latest created_at first (id w0), earliest last (id w2).
        for (i, alias) in aliases.iter().enumerate() {
            conn.execute(
                "INSERT INTO watched_directories (id, path, created_at) VALUES (?1, ?2, ?3)",
                params![format!("w{i}"), alias, format!("2020-01-0{}", 3 - i)],
            )
            .unwrap();
        }

        backfill_canonical_watch_paths(&conn).unwrap();

        let survivors: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM watched_directories").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.collect::<Result<Vec<_>>>().unwrap()
        };
        assert_eq!(
            survivors,
            vec!["w2".to_string()],
            "the earliest-created alias (2020-01-01) survives; later aliases are dropped"
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
    fn init_db_promotes_legacy_banner_first_error_messages() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let insert_error = |id: &str, msg: &str| {
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, error_message, queue_order, created_at, completed_at)
                 VALUES (?1, '/s.mp4', '/o.mp4', 'p', 'error', ?2, 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
                params![id, msg],
            )
            .unwrap();
        };

        // Legacy row: HandBrake's benign banner leads, the real reason is buried below —
        // exactly what the single-line history UI was showing before the fix.
        insert_error(
            "leg",
            "Conversion failed:\n[00:00] Compile-time hardening features are enabled\n[mov] moov atom not found\nNo title found.",
        );
        // A row already carrying a promoted headline must be left byte-for-byte identical.
        let good = "Conversion failed: No title found.\nNo title found.";
        insert_error("good", good);

        // Re-running init_db runs the backfill migration.
        init_db(&conn).unwrap();

        let msg = |id: &str| -> String {
            conn.query_row(
                "SELECT error_message FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };

        assert_eq!(
            msg("leg").lines().next().unwrap(),
            "Conversion failed: [mov] moov atom not found",
            "the buried diagnostic must be promoted to the headline"
        );
        assert!(
            msg("leg").contains("No title found."),
            "detail is preserved"
        );
        assert_eq!(msg("good"), good, "already-headlined rows are untouched");

        // Idempotent: a later startup must not double-promote.
        init_db(&conn).unwrap();
        assert_eq!(
            msg("leg").lines().next().unwrap(),
            "Conversion failed: [mov] moov atom not found"
        );
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

    #[test]
    fn init_db_adds_failure_class_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Writing to the column is the real proof it exists and is TEXT-typed.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at, failure_class)
             VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'error', 0, '2026-01-01T00:00:00Z', 'bad_source')",
            [],
        )
        .unwrap();
        let got: Option<String> = conn
            .query_row("SELECT failure_class FROM jobs WHERE id = 'j1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(got.as_deref(), Some("bad_source"));
    }

    #[test]
    fn init_db_adds_started_at_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Writing through the column is the real proof it exists and is TEXT-typed.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at, started_at)
             VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'encoding', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:05Z')",
            [],
        )
        .unwrap();
        let got: Option<String> = conn
            .query_row("SELECT started_at FROM jobs WHERE id = 'j1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(got.as_deref(), Some("2026-01-01T00:00:05Z"));
    }

    #[test]
    fn init_db_adds_started_at_to_an_existing_database() {
        // An old DB predating the column. init_db must ALTER it in, leave existing rows
        // NULL, and stay idempotent on a second run — users upgrade in place.
        let conn = Connection::open_in_memory().unwrap();
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
        insert_job(
            &conn,
            "old",
            "done",
            "2020-01-01T00:00:00Z",
            Some("2020-01-01T00:10:00Z"),
        );

        init_db(&conn).unwrap();
        // Second run must not error on the duplicate column.
        init_db(&conn).unwrap();

        let started: Option<String> = conn
            .query_row("SELECT started_at FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            started, None,
            "a pre-upgrade row has no start time and must render no duration, not a wrong one"
        );
    }

    #[test]
    fn failure_class_migrates_onto_a_pre_existing_database() {
        // An auto-updating install already has a jobs table without the column. The
        // migration must add it without destroying the row that is already there.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE jobs (
                id TEXT PRIMARY KEY, source_path TEXT NOT NULL, output_path TEXT NOT NULL,
                preset TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued',
                error_message TEXT, queue_order INTEGER NOT NULL, created_at TEXT NOT NULL,
                completed_at TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('old', '/old.mkv', '/old.mp4', 'p', 'done', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        let (id, class): (String, Option<String>) = conn
            .query_row(
                "SELECT id, failure_class FROM jobs WHERE id = 'old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(id, "old", "the pre-existing row must survive the migration");
        assert_eq!(
            class, None,
            "rows predating the feature are NULL — distinct from a classified 'unknown'"
        );

        // Idempotent: a second init on the same DB must not error.
        init_db(&conn).unwrap();
    }

    #[test]
    fn bad_source_action_defaults_to_trash() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(
            setting(&conn, "bad_source_action").as_deref(),
            Some("trash"),
            "the review list's bulk action defaults to the recoverable option; permanent \
             deletion must be chosen deliberately"
        );
    }

    #[test]
    fn get_db_path_from_prefers_the_override_base() {
        let dir = tempfile::tempdir().unwrap();
        let p = get_db_path_from(Some(dir.path().to_path_buf()));
        assert_eq!(p, dir.path().join("convertbar.db"));
        assert!(dir.path().exists(), "base dir is created");
    }

    #[test]
    fn get_db_path_from_falls_back_to_platform_data_dir() {
        let p = get_db_path_from(None);
        let s = p.to_string_lossy();
        assert!(s.contains("com.convertbar.app") && s.ends_with("convertbar.db"));
    }
}
