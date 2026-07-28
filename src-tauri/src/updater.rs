use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// How the app behaves when an update exists. Stored in the settings table as a string,
/// like `cleanup_mode` and `bad_source_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Automatic,
    Notify,
    Off,
}

impl UpdateMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateMode::Automatic => "automatic",
            UpdateMode::Notify => "notify",
            UpdateMode::Off => "off",
        }
    }
}

/// Coerce a stored `update_mode` to a known value. Anything other than an exact "notify" or
/// "off" reads as "automatic": a corrupted, empty, or future value must leave the user
/// receiving updates rather than silently stranding them on an old version.
pub fn normalize_update_mode(value: &str) -> UpdateMode {
    match value {
        "notify" => UpdateMode::Notify,
        "off" => UpdateMode::Off,
        _ => UpdateMode::Automatic,
    }
}

/// The update that was installed but whose notes the user has not seen yet. Persisted
/// because on every platform the install is followed by a restart — and on Windows the
/// process is terminated outright — so in-memory notes would be gone at exactly the moment
/// the user is running the new version and would read them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledUpdate {
    pub version: String,
    pub notes: Option<String>,
}

/// Backend-only settings rows. Read-with-default (no seed) so existing databases need no
/// migration and the settings-count guard is untouched. NOT in ALLOWED_KEYS, NOT in the UI —
/// the frontend reaches these only through updater commands. Same discipline as `queue_paused`.
fn read_key(db: &Connection, key: &str) -> Option<String> {
    db.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|v| !v.is_empty())
}

fn write_key(db: &Connection, key: &str, value: &str) {
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    );
}

pub(crate) fn read_skipped_version(db: &Connection) -> Option<String> {
    read_key(db, "update_skipped_version")
}

pub(crate) fn set_skipped_version(db: &Connection, version: &str) {
    write_key(db, "update_skipped_version", version);
}

pub(crate) fn read_notified_version(db: &Connection) -> Option<String> {
    read_key(db, "update_notified_version")
}

pub(crate) fn set_notified_version(db: &Connection, version: &str) {
    write_key(db, "update_notified_version", version);
}

/// JSON-encoded, not delimiter-separated: release notes are arbitrary markdown and would
/// corrupt any `|`- or newline-delimited encoding.
pub(crate) fn read_installed(db: &Connection) -> Option<InstalledUpdate> {
    let raw = read_key(db, "update_installed")?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn set_installed(db: &Connection, version: &str, notes: Option<&str>) {
    let payload = InstalledUpdate {
        version: version.to_string(),
        notes: notes.map(str::to_string),
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        write_key(db, "update_installed", &json);
    }
}

pub(crate) fn clear_installed(db: &Connection) {
    write_key(db, "update_installed", "");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn unknown_update_modes_fall_back_to_automatic() {
        // Unlike bad_source_action (where an unreadable value must not escalate to permanent
        // deletion), an unreadable update_mode must not silently strand the user on an old
        // version — so the fallback here is the DEFAULT, not the most conservative option.
        assert!(matches!(
            normalize_update_mode("notify"),
            UpdateMode::Notify
        ));
        assert!(matches!(normalize_update_mode("off"), UpdateMode::Off));
        assert!(matches!(
            normalize_update_mode("automatic"),
            UpdateMode::Automatic
        ));
        assert!(matches!(normalize_update_mode(""), UpdateMode::Automatic));
        assert!(matches!(
            normalize_update_mode("NOTIFY"),
            UpdateMode::Automatic
        ));
        assert!(matches!(
            normalize_update_mode("nonsense"),
            UpdateMode::Automatic
        ));
    }

    #[test]
    fn skipped_and_notified_versions_round_trip_and_default_to_none() {
        let conn = test_conn();
        // Read-with-default: absent rows, so existing databases need no migration.
        assert_eq!(read_skipped_version(&conn), None);
        assert_eq!(read_notified_version(&conn), None);

        set_skipped_version(&conn, "1.2.0");
        set_notified_version(&conn, "1.3.0");
        assert_eq!(read_skipped_version(&conn).as_deref(), Some("1.2.0"));
        assert_eq!(read_notified_version(&conn).as_deref(), Some("1.3.0"));

        // Overwrite, not append.
        set_skipped_version(&conn, "1.4.0");
        assert_eq!(read_skipped_version(&conn).as_deref(), Some("1.4.0"));
    }

    #[test]
    fn installed_update_survives_notes_containing_delimiters() {
        let conn = test_conn();
        assert!(read_installed(&conn).is_none());

        // Release notes are arbitrary text. A pipe- or newline-delimited encoding would
        // corrupt on exactly the markdown bullets this feature exists to display.
        let notes = "### Fixes\n- fixed a | pipe\n- and a \"quote\"";
        set_installed(&conn, "1.5.0", Some(notes));

        let got = read_installed(&conn).unwrap();
        assert_eq!(got.version, "1.5.0");
        assert_eq!(got.notes.as_deref(), Some(notes));

        clear_installed(&conn);
        assert!(read_installed(&conn).is_none());
    }

    #[test]
    fn installed_update_tolerates_absent_notes() {
        let conn = test_conn();
        set_installed(&conn, "1.6.0", None);
        let got = read_installed(&conn).unwrap();
        assert_eq!(got.version, "1.6.0");
        assert_eq!(got.notes, None);
    }
}
