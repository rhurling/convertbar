use rusqlite::params;

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
}
