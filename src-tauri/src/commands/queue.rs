use rusqlite::params;
use std::path::{Path, PathBuf};
use tauri::State;

use crate::handbrake;
use crate::types::{ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo};
use crate::AppState;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts",
];

fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn scan_video_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(scan_video_files(&path));
            } else if is_video_file(&path) {
                files.push(path);
            }
        }
    }
    files
}

fn get_next_queue_order(conn: &rusqlite::Connection) -> Result<i32, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(queue_order), 0) + 1 FROM jobs WHERE status IN ('queued', 'encoding', 'paused')",
        [],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

fn row_to_job(row: &rusqlite::Row) -> rusqlite::Result<JobInfo> {
    Ok(JobInfo {
        id: row.get(0)?,
        source_path: row.get(1)?,
        output_path: row.get(2)?,
        preset: row.get(3)?,
        status: row.get(4)?,
        original_size: row.get(5)?,
        converted_size: row.get(6)?,
        kept_file: row.get(7)?,
        space_saved: row.get(8)?,
        error_message: row.get(9)?,
        queue_order: row.get(10)?,
        created_at: row.get(11)?,
        completed_at: row.get(12)?,
    })
}

fn get_handbrake_path(conn: &rusqlite::Connection) -> Result<String, String> {
    let configured: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Ok(path.clone());
        }
    }

    handbrake::detect_handbrake_path().ok_or_else(|| "HandBrakeCLI not found".to_string())
}

fn add_files_inner(
    state: &AppState,
    paths: &[String],
) -> Result<Vec<JobInfo>, String> {
    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;

        let preset: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'preset'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        let suffix_template: String = conn
            .query_row(
                "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
                params![preset],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let hb_path = if suffix_template.contains('{') {
            // May need handbrake path for metadata fetch
            get_handbrake_path(&conn).ok()
        } else {
            None
        };

        (preset, suffix_template, hb_path)
    }; // db lock released

    // Resolve template if needed
    let suffix = if suffix_template.contains('{') {
        let metadata = {
            let mut cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
            if let Some(m) = cache.get(&preset) {
                m.clone()
            } else {
                let hb_path = hb_path.ok_or("HandBrakeCLI not found")?;
                let m = handbrake::get_preset_metadata(&hb_path, &preset)?;
                cache.insert(preset.clone(), m.clone());
                m
            }
        };
        handbrake::resolve_suffix_template(&suffix_template, &metadata)
    } else {
        suffix_template
    };

    // Re-acquire db lock for inserting jobs
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut queue_order = get_next_queue_order(&conn)?;
    let mut jobs = Vec::new();

    for path_str in paths {
        let path = Path::new(path_str);

        // Validate it's a video file
        if !is_video_file(path) {
            continue;
        }

        // Build output path: same directory, add suffix before extension
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let parent = path.parent().unwrap_or(Path::new("."));

        // Skip if source file already has the suffix
        if !suffix.is_empty() && stem.ends_with(&suffix) {
            continue;
        }

        let output_filename = format!("{}{}.mp4", stem, suffix);
        let output_path = parent.join(&output_filename);

        // Skip if output file already exists
        if output_path.exists() {
            continue;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let original_size = std::fs::metadata(path).map(|m| m.len() as i64).ok();

        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, queue_order, created_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7)",
            params![
                id,
                path_str,
                output_path.to_string_lossy().to_string(),
                preset,
                original_size,
                queue_order,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        jobs.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_path.to_string_lossy().to_string(),
            preset: preset.clone(),
            status: "queued".to_string(),
            original_size,
            converted_size: None,
            kept_file: None,
            space_saved: None,
            error_message: None,
            queue_order,
            created_at: now,
            completed_at: None,
        });

        queue_order += 1;
    }

    Ok(jobs)
}

#[tauri::command]
pub fn add_files(state: State<'_, AppState>, paths: Vec<String>) -> Result<Vec<JobInfo>, String> {
    add_files_inner(&state, &paths)
}

#[tauri::command]
pub fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let files = scan_video_files(dir);
    let folder_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string();

    Ok(FolderScanResult {
        file_count: files.len(),
        folder_name,
        folder_path: path,
    })
}

#[tauri::command]
pub fn confirm_folder_add(
    state: State<'_, AppState>,
    path: String,
) -> Result<Vec<JobInfo>, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let files = scan_video_files(dir);
    let paths: Vec<String> = files
        .into_iter()
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .collect();

    add_files_inner(&state, &paths)
}

#[tauri::command]
pub fn get_queue(state: State<'_, AppState>) -> Result<Vec<JobInfo>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('queued', 'encoding', 'paused', 'error')
             ORDER BY queue_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let jobs = stmt
        .query_map([], |row| row_to_job(row))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(jobs)
}

#[tauri::command]
pub fn remove_job(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM jobs WHERE id = ?1 AND status = 'queued'",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn reorder_queue(state: State<'_, AppState>, job_ids: Vec<String>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute("BEGIN", []).map_err(|e| e.to_string())?;
    for (i, id) in job_ids.iter().enumerate() {
        if let Err(e) = conn.execute(
            "UPDATE jobs SET queue_order = ?1 WHERE id = ?2",
            params![i as i32, id],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e.to_string());
        }
    }
    conn.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn clear_completed(state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM jobs WHERE status IN ('done', 'skipped')",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_history(
    state: State<'_, AppState>,
    limit: u32,
    offset: u32,
) -> Result<HistoryPage, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped')
             ORDER BY completed_at DESC
             LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| e.to_string())?;

    let jobs = stmt
        .query_map(params![limit, offset], |row| row_to_job(row))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(HistoryPage { jobs, total })
}

#[tauri::command]
pub fn get_history_summary(state: State<'_, AppState>) -> Result<HistorySummary, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    let (total_saved_bytes, total_files): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(HistorySummary {
        total_saved_bytes,
        total_files,
    })
}

#[tauri::command]
pub fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for path_str in paths {
        let path = Path::new(&path_str);
        if path.is_dir() {
            let video_files = scan_video_files(path);
            let folder_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string();
            folders.push(FolderScanResult {
                file_count: video_files.len(),
                folder_name,
                folder_path: path_str,
            });
        } else if path.is_file() {
            files.push(path_str);
        }
    }
    Ok(ClassifiedPaths { files, folders })
}
