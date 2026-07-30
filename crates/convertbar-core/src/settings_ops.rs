use rusqlite::params;

use crate::ctx::Ctx;
use crate::events::EventSinkExt;
use crate::types::Settings;

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

/// Persists `preset`'s output-filename suffix override (upserting `preset_suffixes`). Moved
/// from the desktop command layer so the server routes can call it too.
pub fn set_preset_suffix(ctx: &Ctx, preset: &str, suffix: &str) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO preset_suffixes (preset_name, suffix) VALUES (?1, ?2) ON CONFLICT(preset_name) DO UPDATE SET suffix = ?2",
        params![preset, suffix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub const ALLOWED_KEYS: &[&str] = &[
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
pub fn normalize_bad_source_action(value: &str) -> &'static str {
    if value == "delete" {
        "delete"
    } else {
        "trash"
    }
}

/// Coerce a stored `cleanup_mode` to a known value. Exactly `"keep"` or `"delete"` pass
/// through; anything else — corrupted, empty, or written by a newer version — reads as
/// `"trash"`, which is what every pre-existing row already means. Sibling of
/// [`normalize_bad_source_action`].
pub fn normalize_cleanup_mode(value: &str) -> &'static str {
    match value {
        "keep" => "keep",
        "delete" => "delete",
        _ => "trash",
    }
}

/// The stored `cleanup_mode`, normalized. The single read path — `converter` and
/// `queue_ops` both go through this so no call site ever string-compares a raw column.
pub fn read_cleanup_mode(conn: &rusqlite::Connection) -> String {
    let raw: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'cleanup_mode'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    normalize_cleanup_mode(&raw).to_string()
}

/// Reads every stored setting into a [`Settings`] snapshot, falling back to defaults for keys
/// that have no row yet. `launch_at_login` is the *stored* value here — on desktop the autostart
/// plugin is the actual source of truth, and the desktop wrapper overlays that on top.
pub fn get_settings(ctx: &Ctx) -> Result<Settings, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;

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
    // Stored raw: the coercion of an unknown value to Automatic is desktop-updater policy and
    // lives in the shell (`commands::settings::get_settings`), like `launch_at_login`.
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
            "cleanup_mode" => cleanup_mode = normalize_cleanup_mode(&value).to_string(),
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
            "update_mode" => update_mode = value,
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
        skip_already_converted,
        skip_by_source_media,
        watch_skip_marker,
        low_disk_min_gb,
        bad_source_action,
        update_mode,
    })
}

/// Validates `key` against [`ALLOWED_KEYS`] and upserts it into the `settings` table. When
/// `key` is `watch_skip_marker`, also refreshes the running watcher's cached marker so it picks
/// up the change without a restart.
pub fn update_setting(ctx: &Ctx, key: &str, value: &str) -> Result<(), String> {
    if !ALLOWED_KEYS.contains(&key) {
        return Err(format!("Invalid setting key: {}", key));
    }
    {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )
        .map_err(|e| e.to_string())?;
    } // conn must be dropped before refresh_skip_marker: it re-locks ctx.db on this same thread,
      // and std::sync::Mutex is not reentrant — holding the guard here self-deadlocks.

    // Let the running watcher pick up a changed skip-marker name without a restart.
    if key == "watch_skip_marker" {
        crate::watcher::refresh_skip_marker(ctx);
    }

    // Switching to keep makes any queued in-place job impossible (its output IS its
    // source). Drop them here, at the moment the user makes the choice, so no such job
    // can reach the converter and no error row is ever written.
    if key == "cleanup_mode" && normalize_cleanup_mode(value) == "keep" {
        let dropped = {
            let conn = ctx.db.lock().map_err(|e| e.to_string())?;
            crate::queue_ops::drop_queued_in_place_jobs(&conn)
        }; // guard released before the emit below — see the comment above.
        if dropped > 0 {
            ctx.events.emit_t("queue-updated", ());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispose::RecordingDisposer;
    use crate::events::TestSink;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    /// Same harness as `queue_ops.rs`/`watcher.rs`'s tests: a `Ctx` backed by an in-memory DB, a
    /// `TestSink` for event assertions, and a `RecordingDisposer` (unused here, but required by
    /// `Ctx::new`).
    fn test_ctx(conn: Connection) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let ctx = Ctx::new(
            conn,
            sink.clone(),
            disposer.clone(),
            Arc::new(crate::handbrake::PanickingLocator),
        );
        (ctx, sink, disposer)
    }

    #[test]
    fn update_setting_hands_the_connection_back_before_the_hooks_run() {
        // 1.0.0 hung here: `update_setting` held the db guard for its whole body while the
        // `watch_skip_marker` hook re-entered the same mutex via `watcher::refresh_skip_marker`
        // -> `read_skip_marker`. std's Mutex is not reentrant, so that is a self-deadlock rather
        // than a wait. The desktop shell adds more hooks after this returns (autostart sync,
        // `updater::on_mode_changed` -> `emit_state`), all of which re-acquire it too.
        //
        // Run on a worker thread with a bounded join so a regression fails loud instead of
        // hanging the suite — the sibling test below would simply never return.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (ctx, _sink, _d) = test_ctx(test_conn());
            let result = update_setting(&ctx, "watch_skip_marker", ".uploading");
            let _ = tx.send((result, ctx.db.try_lock().is_ok()));
        });
        let (result, lock_free) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("update_setting deadlocked against its own post-write hook");
        result.unwrap();
        assert!(
            lock_free,
            "a hook running after the write would deadlock on a still-held connection"
        );

        // A failed write must not strand the connection either, or one bad write would hang every
        // later setting change for the rest of the process's life.
        let (ctx, _sink, _d) = test_ctx(Connection::open_in_memory().unwrap());
        assert!(
            update_setting(&ctx, "preset", "Fast 1080p30").is_err(),
            "no settings table: the write must fail"
        );
        assert!(
            ctx.db.try_lock().is_ok(),
            "a rejected write must still release the connection"
        );
    }

    #[test]
    fn switching_to_keep_drops_queued_in_place_jobs() {
        // Run on a worker thread with a bounded join, exactly like the sibling deadlock test
        // above (`update_setting_hands_the_connection_back_before_the_hooks_run`). This hook
        // re-locks ctx.db, so if the write scope's guard ever stops being dropped first, the
        // failure mode is a SELF-DEADLOCK — an unbounded hang that would freeze the whole
        // suite rather than fail. The timeout turns that into a legible failure.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (ctx, sink, _d) = test_ctx(test_conn());
            {
                let conn = ctx.db.lock().unwrap();
                // One in-place job (source == output) and one normal job.
                conn.execute(
                    "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                     VALUES ('inplace', '/m/a.mp4', '/m/a.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                     VALUES ('normal', '/m/b.mp4', '/m/b-conv.mp4', 'p', 'queued', 1, '2020-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
                // Not byte-identical, but the same file once normalized (`//` and `/.` collapse
                // under Path comparison) — pins that the filter is is_in_place, not a raw
                // source_path = output_path string/SQL comparison.
                conn.execute(
                    "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                     VALUES ('norm', '/m//c.mp4', '/m/./c.mp4', 'p', 'queued', 2, '2020-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
            }

            let result = update_setting(&ctx, "cleanup_mode", "keep");

            let ids: Vec<String> = ctx
                .db
                .lock()
                .unwrap()
                .prepare("SELECT id FROM jobs ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            let emits = sink.payloads("queue-updated").len();
            let lock_free = ctx.db.try_lock().is_ok();
            let _ = tx.send((result, ids, emits, lock_free));
        });

        let (result, ids, emits, lock_free) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("update_setting deadlocked against its own post-write hook");
        result.unwrap();
        assert_eq!(
            ids,
            vec!["normal".to_string()],
            "only the in-place job is dropped"
        );
        // The Queue panel must learn about the removal.
        assert_eq!(emits, 1);
        assert!(
            lock_free,
            "a hook running after the write must not strand the connection"
        );
    }

    #[test]
    fn switching_to_delete_leaves_queued_in_place_jobs_alone() {
        let (ctx, sink, _d) = test_ctx(test_conn());
        {
            let conn = ctx.db.lock().unwrap();
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES ('inplace', '/m/a.mp4', '/m/a.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        update_setting(&ctx, "cleanup_mode", "delete").unwrap();

        let conn = ctx.db.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "in-place jobs are only impossible under keep");
        // NOTE: this does NOT reach (and so cannot pin) the `if dropped > 0` guard at :214 —
        // value is "delete" here, so the outer `key == "cleanup_mode" && ... == "keep"` gate
        // above it is false and the whole drop block, guard included, is never entered. This
        // assertion passes whether the guard exists or not. See
        // `switching_to_keep_with_nothing_to_drop_does_not_emit` below for a scenario that
        // actually enters the keep branch with dropped == 0.
        assert_eq!(sink.payloads("queue-updated").len(), 0);
    }

    #[test]
    fn switching_to_keep_with_nothing_to_drop_does_not_emit() {
        // Pins the `if dropped > 0` guard at :214 for real, unlike the sibling test above:
        // this writes "keep", so the gate at :209 IS true, the drop block runs, and
        // drop_queued_in_place_jobs genuinely returns 0 (the only queued job is NOT in-place).
        // A regression that deletes the guard (always emit) must turn this red.
        let (ctx, sink, _d) = test_ctx(test_conn());
        {
            let conn = ctx.db.lock().unwrap();
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES ('normal', '/m/b.mp4', '/m/b-conv.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        update_setting(&ctx, "cleanup_mode", "keep").unwrap();

        let conn = ctx.db.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "the non-in-place job is untouched");
        assert_eq!(
            sink.payloads("queue-updated").len(),
            0,
            "dropped == 0 must not emit — the case switching_to_delete_... could not reach"
        );
    }

    #[test]
    fn update_mode_is_writable_but_the_internal_updater_keys_are_not() {
        // The Settings UI writes this key via update_setting; the three internal updater keys
        // deliberately are NOT writable this way, so the frontend cannot forge update policy.
        assert!(ALLOWED_KEYS.contains(&"update_mode"));
        assert!(!ALLOWED_KEYS.contains(&"update_skipped_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_notified_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_installed"));
    }

    #[test]
    fn update_setting_refreshes_the_watcher_skip_marker() {
        let (ctx, _sink, _d) = test_ctx(test_conn());
        update_setting(&ctx, "watch_skip_marker", ".uploading").unwrap();
        assert_eq!(
            ctx.watcher.skip_marker.lock().unwrap().as_deref(),
            Some(".uploading")
        );
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
    fn set_preset_suffix_inserts_then_updates_on_conflict() {
        let (ctx, _sink, _d) = test_ctx(test_conn());

        set_preset_suffix(&ctx, "P", ".custom").unwrap();
        assert_eq!(
            read_suffix_template(&ctx.db.lock().unwrap(), "P"),
            ".custom"
        );

        // A second write for the same preset must update the existing row (ON CONFLICT), not
        // fail on the preset_suffixes UNIQUE constraint or insert a duplicate.
        set_preset_suffix(&ctx, "P", ".updated").unwrap();
        assert_eq!(
            read_suffix_template(&ctx.db.lock().unwrap(), "P"),
            ".updated"
        );
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
    fn cleanup_mode_normalizes_to_three_known_values() {
        // Exact matches pass through; everything else reads as "trash". The fallback is
        // deliberately NOT "keep": it preserves the behavior every existing row already has,
        // and mirrors normalize_bad_source_action's convention.
        assert_eq!(normalize_cleanup_mode("keep"), "keep");
        assert_eq!(normalize_cleanup_mode("delete"), "delete");
        assert_eq!(normalize_cleanup_mode("trash"), "trash");
        assert_eq!(normalize_cleanup_mode(""), "trash");
        assert_eq!(normalize_cleanup_mode("KEEP"), "trash");
        assert_eq!(normalize_cleanup_mode("nonsense"), "trash");
    }

    #[test]
    fn read_cleanup_mode_normalizes_what_it_reads() {
        let conn = test_conn();
        // init_db seeds 'trash'.
        assert_eq!(read_cleanup_mode(&conn), "trash");

        conn.execute(
            "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();
        assert_eq!(read_cleanup_mode(&conn), "keep");

        // A corrupted row must never reach a call site as a raw string.
        conn.execute(
            "UPDATE settings SET value = 'garbage' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();
        assert_eq!(read_cleanup_mode(&conn), "trash");
    }

    #[test]
    fn get_settings_normalizes_cleanup_mode() {
        let (ctx, _sink, _d) = test_ctx(test_conn());

        // Default init_db value.
        let settings = get_settings(&ctx).unwrap();
        assert_eq!(settings.cleanup_mode, "trash");

        // Valid value passes through.
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
                [],
            )
            .unwrap();
        let settings = get_settings(&ctx).unwrap();
        assert_eq!(settings.cleanup_mode, "keep");

        // Corrupted row: the Settings struct must never surface a raw garbage value.
        // It must normalize to trash, just like read_cleanup_mode does.
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE settings SET value = 'garbage' WHERE key = 'cleanup_mode'",
                [],
            )
            .unwrap();
        let settings = get_settings(&ctx).unwrap();
        assert_eq!(
            settings.cleanup_mode, "trash",
            "get_settings must normalize corrupted cleanup_mode to trash"
        );
    }
}
