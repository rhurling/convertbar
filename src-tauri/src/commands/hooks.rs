//! Desktop-only access to the command hooks. These settings keys are absent from
//! `settings_ops::ALLOWED_KEYS` on purpose — that absence is what stops the server head's
//! HTTP API from turning ConvertBar into a remote shell. The desktop head is a local,
//! already-trusted context, so it reads and writes them here instead. App-defined
//! `#[tauri::command]`s are ACL-exempt and need no capabilities entry.

use std::sync::Arc;

use convertbar_core::ctx::Ctx;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use super::{blocking, CommandError};

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommandHooks {
    pub post_convert: String,
    pub queue_drained: String,
}

/// Maps a trigger name to its settings key. Anything else is rejected: without this the
/// command would be an unrestricted settings writer that bypasses ALLOWED_KEYS entirely.
fn key_for(trigger: &str) -> Result<&'static str, CommandError> {
    match trigger {
        "post_convert" => Ok("post_convert_command"),
        "queue_drained" => Ok("queue_drained_command"),
        other => Err(CommandError::from(format!("unknown hook trigger: {other}"))),
    }
}

pub fn read_command_hooks(conn: &rusqlite::Connection) -> CommandHooks {
    let get = |key: &str| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
    };
    CommandHooks {
        post_convert: get("post_convert_command"),
        queue_drained: get("queue_drained_command"),
    }
}

pub fn write_command_hook(
    conn: &rusqlite::Connection,
    trigger: &str,
    command: &str,
) -> Result<(), CommandError> {
    let key = key_for(trigger)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, command],
    )
    .map(|_| ())
    .map_err(|e| CommandError::from(e.to_string()))
}

#[tauri::command]
pub fn get_command_hooks(ctx: State<'_, Arc<Ctx>>) -> Result<CommandHooks, CommandError> {
    let conn = ctx
        .db
        .lock()
        .map_err(|e| CommandError::from(e.to_string()))?;
    Ok(read_command_hooks(&conn))
}

#[tauri::command]
pub fn set_command_hook(
    ctx: State<'_, Arc<Ctx>>,
    trigger: String,
    command: String,
) -> Result<(), CommandError> {
    let conn = ctx
        .db
        .lock()
        .map_err(|e| CommandError::from(e.to_string()))?;
    write_command_hook(&conn, &trigger, &command)
}

/// Opens the native file picker so the UI can populate a command-hook field with a script path.
/// Invoked from Rust, so no frontend `dialog` ACL permission is required. Mirrors
/// `watch::pick_folder` exactly — same threading contract, same reasoning, because it is the
/// same class of hazard:
///
/// MUST NOT run on the main thread: `blocking_pick_file` dispatches the panel to the main thread
/// and then blocks the calling thread, so calling it there deadlocks the event loop — which is
/// what a sync command would do, since Tauri runs those on the main thread.
///
/// It goes through `blocking` rather than merely being `async`, because the call blocks for as
/// long as the panel is open — a user who walks away holds the thread for minutes. `async` alone
/// parked that on a core runtime worker; the blocking pool exists for exactly this, is equally
/// not-the-main-thread, and hands the command the same panic taxonomy as every other.
#[tauri::command]
pub async fn pick_file(app: AppHandle) -> Result<Option<String>, CommandError> {
    blocking(move || {
        Ok(app
            .dialog()
            .file()
            .blocking_pick_file()
            .and_then(|file_path| file_path.into_path().ok())
            .map(|path| path.to_string_lossy().to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_round_trip_both_triggers() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        convertbar_core::db::init_db(&conn).unwrap();
        write_command_hook(&conn, "post_convert", "/a.sh").unwrap();
        write_command_hook(&conn, "queue_drained", "/b.sh").unwrap();
        let hooks = read_command_hooks(&conn);
        assert_eq!(hooks.post_convert, "/a.sh");
        assert_eq!(hooks.queue_drained, "/b.sh");
    }

    #[test]
    fn an_unknown_trigger_is_rejected() {
        // The trigger name selects the settings key; an unchecked name would let a caller
        // write ANY settings row through a command that bypasses ALLOWED_KEYS.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        convertbar_core::db::init_db(&conn).unwrap();
        assert!(write_command_hook(&conn, "preset", "Fast 1080p30").is_err());
        assert!(write_command_hook(&conn, "../../etc", "x").is_err());
    }
}
