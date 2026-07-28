//! Watched-directory CRUD: the five operations behind the "Watch Folders" settings UI. Moved
//! from the desktop command layer (`src-tauri/src/commands/watch.rs`) so the server routes can
//! call them too; the desktop commands now delegate here.

use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ctx::Ctx;
use crate::types::WatchedDirectory;
use crate::watcher;

pub fn get_watched_directories(ctx: &Ctx) -> Result<Vec<WatchedDirectory>, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
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

pub fn add_watched_directory(
    ctx: &Arc<Ctx>,
    path: &str,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<WatchedDirectory, String> {
    let path = canonical_watch_path(path);
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }
    // Floor the delay at one second so a file can never be enqueued mid-write.
    let delay = stability_delay_secs.max(1);
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let record = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
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

    watcher::reconcile(ctx);
    // Scan off-thread: it probes every existing file with a blocking HandBrakeCLI call, which would
    // freeze the UI on the main thread (this command is sync) for a folder full of files.
    watcher::scan_existing_background(ctx, dir.to_path_buf(), recursive);
    Ok(record)
}

pub fn update_watched_directory(
    ctx: &Arc<Ctx>,
    id: &str,
    recursive: bool,
    stability_delay_secs: i64,
) -> Result<(), String> {
    let delay = stability_delay_secs.max(1);
    {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
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
    watcher::reconcile(ctx);
    Ok(())
}

pub fn set_watched_directory_enabled(
    ctx: &Arc<Ctx>,
    id: &str,
    enabled: bool,
) -> Result<(), String> {
    let (path, recursive) = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
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

    watcher::reconcile(ctx);
    // Re-enabling a folder ingests anything that landed while it was off (or the app was closed).
    // Scan off-thread — see `add_watched_directory`: the blocking HandBrake probe would otherwise
    // freeze the UI (this sync command runs on the main thread) for a folder with many files.
    if enabled {
        watcher::scan_existing_background(ctx, PathBuf::from(&path), recursive);
    }
    Ok(())
}

pub fn remove_watched_directory(ctx: &Arc<Ctx>, id: &str) -> Result<(), String> {
    {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM watched_directories WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
    }
    watcher::reconcile(ctx);
    Ok(())
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
