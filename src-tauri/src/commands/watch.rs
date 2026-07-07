use rusqlite::params;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::types::WatchedDirectory;
use crate::watcher;
use crate::AppState;

#[tauri::command]
pub fn get_watched_directories(
    state: State<'_, AppState>,
) -> Result<Vec<WatchedDirectory>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, path, recursive, stability_delay_secs, enabled, created_at
             FROM watched_directories ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(WatchedDirectory {
                id: row.get(0)?,
                path: row.get(1)?,
                recursive: row.get(2)?,
                stability_delay_secs: row.get(3)?,
                enabled: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// Canonical form of a watch path, so aliases of one folder (trailing slash, symlink,
/// case variant) can't register duplicate watchers — the DB UNIQUE constraint only
/// catches byte-identical strings. dunce avoids the `\\?\` prefix std canonicalize
/// yields on Windows.
fn canonical_watch_path(path: &str) -> String {
    dunce::canonicalize(Path::new(path))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

#[tauri::command]
pub fn add_watched_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<WatchedDirectory, String> {
    let path = canonical_watch_path(&path);
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }
    // Floor the delay at one second so a file can never be enqueued mid-write.
    let delay = stability_delay_secs.max(1);
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let record = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO watched_directories
                (id, path, recursive, stability_delay_secs, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![id, path, recursive, delay, now],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                "This folder is already being watched".to_string()
            } else {
                e.to_string()
            }
        })?;
        WatchedDirectory {
            id,
            path: path.clone(),
            recursive,
            stability_delay_secs: delay,
            enabled: true,
            created_at: now,
        }
    }; // db lock released before reconcile re-acquires it

    watcher::reconcile(&app);
    // Scan off-thread: it probes every existing file with a blocking HandBrakeCLI call, which would
    // freeze the UI on the main thread (this command is sync) for a folder full of files.
    watcher::scan_existing_background(&app, dir.to_path_buf(), recursive);
    Ok(record)
}

#[tauri::command]
pub fn update_watched_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<(), String> {
    let delay = stability_delay_secs.max(1);
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let changed = conn
            .execute(
                "UPDATE watched_directories SET recursive = ?1, stability_delay_secs = ?2 WHERE id = ?3",
                params![recursive, delay, id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("Watched directory not found".to_string());
        }
    }
    watcher::reconcile(&app);
    Ok(())
}

#[tauri::command]
pub fn set_watched_directory_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let (path, recursive) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let changed = conn
            .execute(
                "UPDATE watched_directories SET enabled = ?1 WHERE id = ?2",
                params![enabled, id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("Watched directory not found".to_string());
        }
        conn.query_row(
            "SELECT path, recursive FROM watched_directories WHERE id = ?1",
            params![id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .map_err(|e| e.to_string())?
    };

    watcher::reconcile(&app);
    // Re-enabling a folder ingests anything that landed while it was off (or the app was closed).
    // Scan off-thread — see `add_watched_directory`: the blocking HandBrake probe would otherwise
    // freeze the UI (this sync command runs on the main thread) for a folder with many files.
    if enabled {
        watcher::scan_existing_background(&app, PathBuf::from(&path), recursive);
    }
    Ok(())
}

#[tauri::command]
pub fn remove_watched_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM watched_directories WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
    }
    watcher::reconcile(&app);
    Ok(())
}

/// Opens the native folder picker so the UI can add a directory to watch. Invoked from Rust, so
/// no frontend `dialog` ACL permission is required. MUST stay `async`: Tauri runs sync commands
/// on the main thread, and `blocking_pick_folder` dispatches the panel to the main thread and then
/// blocks the calling thread — calling it on the main thread deadlocks the event loop. `async`
/// runs the command on a worker thread, so the main thread stays free to service the panel.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let folder = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|file_path| file_path.into_path().ok())
        .map(|path| path.to_string_lossy().to_string());
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_watch_path_unifies_aliases_of_the_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_str().unwrap().to_string();

        // Trailing separator and a `/./` hop are the everyday duplicate-watch vectors;
        // both must collapse to the same string the UNIQUE constraint compares.
        let with_trailing = format!("{}{}", base, std::path::MAIN_SEPARATOR);
        assert_eq!(
            canonical_watch_path(&with_trailing),
            canonical_watch_path(&base)
        );

        let with_dot = Path::new(&base).join(".").to_string_lossy().to_string();
        assert_eq!(canonical_watch_path(&with_dot), canonical_watch_path(&base));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_watch_path_resolves_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            canonical_watch_path(link.to_str().unwrap()),
            canonical_watch_path(real.to_str().unwrap()),
            "a symlinked alias must not create a second watcher over the same folder"
        );
    }

    #[test]
    fn canonical_watch_path_passes_nonexistent_paths_through() {
        // add_watched_directory still owns the is_dir() rejection; canonicalization
        // must not turn its clear error into a silent transformation.
        assert_eq!(
            canonical_watch_path("definitely-missing"),
            "definitely-missing"
        );
    }
}
