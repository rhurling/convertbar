use rusqlite::params;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::State;

use crate::converter::IN_PLACE_TEMP_MARKER;
use crate::handbrake;
use crate::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo, SkipCount,
    SkipReason,
};
use crate::AppState;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts",
];

pub(crate) fn is_video_file(path: &Path) -> bool {
    // An in-flight in-place temp must never be treated as a queueable video, or a folder scan
    // or watched folder could enqueue it mid-encode.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.contains(IN_PLACE_TEMP_MARKER) {
            return false;
        }
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

pub(crate) fn scan_video_files(dir: &Path) -> Vec<PathBuf> {
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

/// The cache key for a probe: the file's byte size + last-modified time (epoch millis).
/// `None` when the file can't be stat'd or has no readable mtime — such a file has no
/// stable identity and is probed every scan (handled by `resolve_media`'s forced-miss path).
fn file_identity(path: &str) -> Option<crate::probe_cache::FileIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(crate::probe_cache::FileIdentity {
        size: meta.len() as i64,
        mtime,
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

/// The probe-free skip decision for one path: `Some(reason)` to skip it, `None` to keep it.
/// Shared by `probe_candidates` (which paths are worth the expensive HandBrake probe) and
/// `add_files_to_db` (authoritative accounting + insert) so the two can never disagree about what
/// counts as a skip. Never returns `AlreadyAtTarget` — that decision requires a source probe.
fn cheap_skip_reason(
    path_str: &str,
    suffix: &str,
    queued_paths: &HashSet<String>,
    history_paths: &HashSet<String>,
) -> Option<SkipReason> {
    let path = Path::new(path_str);
    if !is_video_file(path) {
        return Some(SkipReason::NotVideo);
    }
    if queued_paths.contains(path_str) {
        return Some(SkipReason::AlreadyQueued);
    }
    if history_paths.contains(path_str) {
        return Some(SkipReason::AlreadyConverted);
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    if !suffix.is_empty() && stem.ends_with(suffix) {
        return Some(SkipReason::AlreadyConverted);
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let output_path = parent.join(format!("{}{}.mp4", stem, suffix));
    let in_place = output_path.as_path() == path;
    if !in_place && output_path.exists() {
        return Some(SkipReason::OutputExists);
    }
    None
}

/// Reads the active-queue and (when `skip_already_converted`) history source paths the cheap skip
/// checks test against.
fn fetch_skip_sets(
    conn: &rusqlite::Connection,
    skip_already_converted: bool,
) -> Result<(HashSet<String>, HashSet<String>), String> {
    let queued_paths: HashSet<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT source_path FROM jobs WHERE status IN ('queued', 'encoding', 'paused')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let history_paths: HashSet<String> = if skip_already_converted {
        let mut stmt = conn
            .prepare("SELECT source_path FROM jobs WHERE status IN ('done', 'skipped')")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        HashSet::new()
    };
    Ok((queued_paths, history_paths))
}

/// The subset of `paths` worth probing for the source-media skip: those that survive the
/// probe-free skip checks. A re-scan of an already-queued/converted folder yields an empty set, so
/// the source-media probe shells out to HandBrake zero times.
fn probe_candidates(
    conn: &rusqlite::Connection,
    paths: &[String],
    suffix: &str,
    skip_already_converted: bool,
) -> Result<Vec<String>, String> {
    let (queued_paths, history_paths) = fetch_skip_sets(conn, skip_already_converted)?;
    Ok(paths
        .iter()
        .filter(|p| cheap_skip_reason(p, suffix, &queued_paths, &history_paths).is_none())
        .cloned()
        .collect())
}

pub(crate) fn add_files_inner(state: &AppState, paths: &[String]) -> Result<AddResult, String> {
    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
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

        let skip_already_converted: bool = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'skip_already_converted'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(false);

        let skip_by_source_media: bool = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'skip_by_source_media'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(false);

        // Source-media skip also needs the target preset metadata, so fetch HandBrake when
        // either the suffix template or the skip toggle requires it.
        let hb_path = if suffix_template.contains('{') || skip_by_source_media {
            get_handbrake_path(&conn).ok()
        } else {
            None
        };

        (
            preset,
            suffix_template,
            hb_path,
            skip_already_converted,
            skip_by_source_media,
        )
    }; // db lock released

    // Resolve template if needed
    let suffix = if suffix_template.contains('{') {
        let metadata = {
            let mut cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
            if let Some(m) = cache.get(&preset) {
                m.clone()
            } else {
                let hb_path = hb_path.clone().ok_or("HandBrakeCLI not found")?;
                let m = handbrake::get_preset_metadata(&hb_path, &preset)?;
                cache.insert(preset.clone(), m.clone());
                m
            }
        };
        handbrake::resolve_suffix_template(&suffix_template, &metadata)
    } else {
        suffix_template
    };

    // Source-media skip: probe candidate files and drop those already at/below the target. Only
    // files that survive the probe-free skip checks are probed, so a re-scan of an already-handled
    // folder shells out to HandBrake zero times. Probing runs outside the DB lock; on any
    // uncertainty (no HandBrake, probe failure/timeout, unknown codec) the file is kept.
    let candidates_to_probe: Vec<String> = if skip_by_source_media {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        probe_candidates(&conn, paths, &suffix, skip_already_converted)?
    } else {
        Vec::new()
    };

    let media_skipped: HashSet<String> = if !candidates_to_probe.is_empty() {
        if let Some(hb) = hb_path.as_deref() {
            let metadata = {
                let mut cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
                if let Some(m) = cache.get(&preset) {
                    m.clone()
                } else {
                    let m = handbrake::get_preset_metadata(hb, &preset)?;
                    cache.insert(preset.clone(), m.clone());
                    m
                }
            };
            let target_codec = metadata.codec.clone();
            let target_height =
                crate::media_skip::target_height_from_resolution(&metadata.resolution);
            // Stamp each candidate with its filesystem identity (stat outside the DB lock),
            // then reuse cached media for unchanged files and probe only the misses.
            // resolve_media calls lookup (brief lock) -> probe (no lock) -> store (brief
            // lock), so the HandBrake shell-out never runs while the DB mutex is held.
            let with_identity: Vec<(String, Option<crate::probe_cache::FileIdentity>)> =
                candidates_to_probe
                    .iter()
                    .map(|p| (p.clone(), file_identity(p)))
                    .collect();
            let probed = crate::probe_cache::resolve_media(
                &with_identity,
                |ids| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::lookup_batch(&conn, ids)
                },
                |p| crate::probe::probe_source(hb, p),
                |items| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::store_batch(&conn, items);
                },
            );
            crate::media_skip::select_media_skips(&probed, &target_codec, target_height)
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let survivors: Vec<String> = paths
        .iter()
        .filter(|p| !media_skipped.contains(*p))
        .cloned()
        .collect();

    // Re-acquire db lock and hand the resolved suffix to the DB-only core.
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut result = add_files_to_db(&conn, &survivors, &preset, &suffix, skip_already_converted)?;
    if !media_skipped.is_empty() {
        result.skipped.push(SkipCount {
            reason: SkipReason::AlreadyAtTarget,
            count: media_skipped.len() as u32,
        });
    }
    Ok(result)
}

/// DB-only core of `add_files_inner`: applies the skip rules and inserts queued jobs given an
/// already-resolved output suffix. Separated from suffix resolution (which may shell out to
/// HandBrakeCLI) so the skip-rule matrix can be tested against an in-memory database.
fn add_files_to_db(
    conn: &rusqlite::Connection,
    paths: &[String],
    preset: &str,
    suffix: &str,
    skip_already_converted: bool,
) -> Result<AddResult, String> {
    let (queued_paths, history_paths) = fetch_skip_sets(conn, skip_already_converted)?;

    let mut queue_order = get_next_queue_order(conn)?;
    let mut added = Vec::new();
    let (mut n_not_video, mut n_queued, mut n_converted, mut n_output_exists) =
        (0u32, 0u32, 0u32, 0u32);

    for path_str in paths {
        if let Some(reason) = cheap_skip_reason(path_str, suffix, &queued_paths, &history_paths) {
            match reason {
                SkipReason::NotVideo => n_not_video += 1,
                SkipReason::AlreadyQueued => n_queued += 1,
                SkipReason::AlreadyConverted => n_converted += 1,
                SkipReason::OutputExists => n_output_exists += 1,
                // The cheap checks never produce AlreadyAtTarget — that needs a probe.
                SkipReason::AlreadyAtTarget => {}
            }
            continue;
        }

        let path = Path::new(path_str);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let parent = path.parent().unwrap_or(Path::new("."));
        let output_path = parent.join(format!("{}{}.mp4", stem, suffix));

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

        added.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_path.to_string_lossy().to_string(),
            preset: preset.to_string(),
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

    let mut skipped = Vec::new();
    for (reason, count) in [
        (SkipReason::NotVideo, n_not_video),
        (SkipReason::AlreadyQueued, n_queued),
        (SkipReason::AlreadyConverted, n_converted),
        (SkipReason::OutputExists, n_output_exists),
    ] {
        if count > 0 {
            skipped.push(SkipCount { reason, count });
        }
    }

    Ok(AddResult { added, skipped })
}

#[tauri::command]
pub fn add_files(state: State<'_, AppState>, paths: Vec<String>) -> Result<AddResult, String> {
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
pub fn confirm_folder_add(state: State<'_, AppState>, path: String) -> Result<AddResult, String> {
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
    reorder_queue_inner(&conn, &job_ids)
}

fn reorder_queue_inner(conn: &rusqlite::Connection, job_ids: &[String]) -> Result<(), String> {
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
pub fn clear_completed(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    match mode.as_str() {
        "errors" => {
            conn.execute("DELETE FROM jobs WHERE status = 'error'", [])
                .map_err(|e| e.to_string())?;
        }
        _ => {
            // "all" - clear everything in history
            conn.execute(
                "DELETE FROM jobs WHERE status IN ('done', 'skipped', 'error')",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn clear_queue(state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM jobs WHERE status = 'queued'", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_history(
    state: State<'_, AppState>,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    get_history_inner(&conn, limit, offset, search, sort_by)
}

fn get_history_inner(
    conn: &rusqlite::Connection,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    let search_param = search
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));
    let has_search = search_param.is_some();

    let order_clause = match sort_by.as_deref() {
        Some("space_saved") => "space_saved DESC",
        Some("original_size") => "original_size DESC",
        Some("source_path") => "source_path ASC",
        _ => "completed_at DESC",
    };

    // Count query
    let total: i64 = if has_search {
        let count_sql = "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped') AND (source_path LIKE ?1 OR output_path LIKE ?1)";
        conn.query_row(count_sql, params![search_param], |row| row.get(0))
            .map_err(|e| e.to_string())?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?
    };

    // Data query
    let jobs = if has_search {
        let sql = format!(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped') AND (source_path LIKE ?1 OR output_path LIKE ?1)
             ORDER BY {}
             LIMIT ?2 OFFSET ?3",
            order_clause
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let result = stmt
            .query_map(params![search_param, limit, offset], |row| row_to_job(row))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        result
    } else {
        let sql = format!(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped')
             ORDER BY {}
             LIMIT ?1 OFFSET ?2",
            order_clause
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let result = stmt
            .query_map(params![limit, offset], |row| row_to_job(row))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        result
    };

    Ok(HistoryPage { jobs, total })
}

#[tauri::command]
pub fn get_history_summary(
    state: State<'_, AppState>,
    search: Option<String>,
) -> Result<HistorySummary, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    get_history_summary_inner(&conn, search)
}

fn get_history_summary_inner(
    conn: &rusqlite::Connection,
    search: Option<String>,
) -> Result<HistorySummary, String> {
    let search_param = search
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));

    let (total_saved_bytes, total_files): (i64, i64) = if search_param.is_some() {
        conn.query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done' AND (source_path LIKE ?1 OR output_path LIKE ?1)",
            params![search_param],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    } else {
        conn.query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::Path;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    fn insert_queued(conn: &Connection, id: &str, source: &str, status: &str, order: i32) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES (?1, ?2, ?3, 'preset', ?4, ?5, '2020-01-01T00:00:00Z')",
            params![id, source, format!("{source}.out"), status, order],
        )
        .unwrap();
    }

    fn insert_history(
        conn: &Connection,
        id: &str,
        source: &str,
        status: &str,
        space_saved: i64,
        original_size: i64,
        completed_at: &str,
    ) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, space_saved, queue_order, created_at, completed_at)
             VALUES (?1, ?2, ?3, 'preset', ?4, ?5, ?6, 0, ?7, ?7)",
            params![
                id,
                source,
                format!("{source}.out.mp4"),
                status,
                original_size,
                space_saved,
                completed_at
            ],
        )
        .unwrap();
    }

    #[test]
    fn accepts_known_video_extensions_case_insensitively() {
        assert!(is_video_file(Path::new("movie.mp4")));
        assert!(is_video_file(Path::new("movie.MKV")));
        assert!(is_video_file(Path::new("/a/b/c.MoV")));
    }

    #[test]
    fn rejects_non_video_and_extensionless() {
        assert!(!is_video_file(Path::new("notes.txt")));
        assert!(!is_video_file(Path::new("README")));
    }

    #[test]
    fn rejects_in_place_temp_files() {
        // A lingering in-place temp must never be picked up by a folder scan or watched folder,
        // even though it carries a valid .mp4 extension.
        assert!(!is_video_file(Path::new(
            "/movies/.clip.convertbar-tmp.mp4"
        )));
        assert!(!is_video_file(Path::new("clip.convertbar-tmp.mp4")));
    }

    // ---- add_files_to_db skip rules ----

    #[test]
    fn add_files_skips_paths_already_in_queue() {
        let conn = test_conn();
        insert_queued(&conn, "j1", "/movies/a.mp4", "queued", 1);

        let result =
            add_files_to_db(&conn, &["/movies/a.mp4".to_string()], "preset", "", false).unwrap();

        assert!(
            result.added.is_empty(),
            "an already-queued source must be skipped"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyQueued,
                count: 1
            }],
            "the skip is reported as AlreadyQueued"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "no duplicate row inserted");
    }

    #[test]
    fn add_files_skips_when_output_already_exists() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip-conv.mp4"), b"x").unwrap();
        let source = dir.path().join("clip.mov").to_string_lossy().to_string();

        let result = add_files_to_db(&conn, &[source], "preset", "-conv", false).unwrap();

        assert!(
            result.added.is_empty(),
            "must skip when the converted output already exists"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::OutputExists,
                count: 1
            }]
        );
    }

    #[test]
    fn add_files_skips_source_that_already_has_suffix() {
        let conn = test_conn();
        let result = add_files_to_db(
            &conn,
            &["/movies/clip-conv.mov".to_string()],
            "preset",
            "-conv",
            false,
        )
        .unwrap();

        assert!(
            result.added.is_empty(),
            "must skip a source whose stem already carries the suffix"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn add_files_skip_already_converted_union_respects_flag() {
        let conn = test_conn();
        insert_history(
            &conn,
            "h1",
            "/movies/done.mkv",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );

        let with_flag =
            add_files_to_db(&conn, &["/movies/done.mkv".to_string()], "preset", "", true).unwrap();
        assert!(with_flag.added.is_empty());
        assert_eq!(
            with_flag.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );

        let without_flag = add_files_to_db(
            &conn,
            &["/movies/done.mkv".to_string()],
            "preset",
            "",
            false,
        )
        .unwrap();
        assert_eq!(
            without_flag.added.len(),
            1,
            "without the flag, a done source is re-added"
        );
    }

    #[test]
    fn add_files_reencodes_mp4_in_place_instead_of_skipping() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("clip.mp4");
        std::fs::write(&src, b"x").unwrap();
        let src_str = src.to_string_lossy().to_string();

        let result = add_files_to_db(&conn, &[src_str.clone()], "preset", "", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "mp4 + empty suffix must queue an in-place job"
        );
        assert_eq!(
            result.added[0].output_path, src_str,
            "an in-place job stores output_path == source_path"
        );
        assert!(result.skipped.is_empty(), "an in-place job is not a skip");
    }

    // ---- probe_candidates (source-media probe is only worth running on these) ----

    #[test]
    fn probe_candidates_excludes_files_the_cheap_checks_would_skip() {
        // The whole point of probing only candidates: a re-scan of an already-handled folder must
        // not pay for a HandBrake probe on files the cheap checks already reject.
        let conn = test_conn();
        insert_queued(&conn, "j1", "/movies/queued.mp4", "queued", 1);
        insert_history(
            &conn,
            "h1",
            "/movies/done.mp4",
            "done",
            0,
            0,
            "2020-01-01T00:00:00Z",
        );

        let paths = vec![
            "/movies/queued.mp4".to_string(), // already queued -> never probed
            "/movies/done.mp4".to_string(),   // in history -> probed only when the flag is off
            "/movies/fresh.mp4".to_string(),  // new -> always a probe candidate
            "/movies/notes.txt".to_string(),  // not a video -> never probed
        ];

        // skip_already_converted ON: the history file is excluded alongside the queued one.
        let with_flag = probe_candidates(&conn, &paths, "", true).unwrap();
        assert_eq!(with_flag, vec!["/movies/fresh.mp4".to_string()]);

        // Flag OFF: the history file is no longer a cheap skip, so it becomes a probe candidate.
        let without_flag = probe_candidates(&conn, &paths, "", false).unwrap();
        assert_eq!(
            without_flag,
            vec![
                "/movies/done.mp4".to_string(),
                "/movies/fresh.mp4".to_string()
            ]
        );
    }

    // ---- get_next_queue_order ----

    #[test]
    fn next_queue_order_starts_at_one_then_follows_max_active() {
        let conn = test_conn();
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            1,
            "empty queue starts at 1"
        );

        insert_queued(&conn, "a", "/m/a.mp4", "queued", 3);
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            4,
            "next is max active order + 1"
        );

        // A finished job with a high order must not influence the next active order.
        insert_history(&conn, "h", "/m/h.mp4", "done", 0, 0, "2020-01-01T00:00:00Z");
        conn.execute("UPDATE jobs SET queue_order = 100 WHERE id = 'h'", [])
            .unwrap();
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            4,
            "done jobs are excluded from the active max"
        );
    }

    // ---- reorder_queue_inner ----

    #[test]
    fn reorder_queue_reassigns_orders_in_listed_sequence() {
        let conn = test_conn();
        insert_queued(&conn, "A", "/m/a.mp4", "queued", 0);
        insert_queued(&conn, "B", "/m/b.mp4", "queued", 1);
        insert_queued(&conn, "C", "/m/c.mp4", "queued", 2);

        reorder_queue_inner(&conn, &["C".to_string(), "A".to_string(), "B".to_string()]).unwrap();

        let order = |id: &str| -> i32 {
            conn.query_row(
                "SELECT queue_order FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(order("C"), 0);
        assert_eq!(order("A"), 1);
        assert_eq!(order("B"), 2);
    }

    // ---- get_history search / sort / pagination ----

    #[test]
    fn history_search_filters_by_path() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/alpha.mp4",
            "done",
            10,
            100,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/beta.mp4",
            "done",
            20,
            200,
            "2020-01-02T00:00:00Z",
        );

        let page = get_history_inner(&conn, 10, 0, Some("alpha".to_string()), None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.jobs.len(), 1);
        assert_eq!(page.jobs[0].source_path, "/m/alpha.mp4");
    }

    #[test]
    fn history_sorts_by_space_saved_and_source_path() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/c.mp4",
            "done",
            10,
            100,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/a.mp4",
            "done",
            100,
            200,
            "2020-01-02T00:00:00Z",
        );
        insert_history(
            &conn,
            "3",
            "/m/b.mp4",
            "done",
            50,
            300,
            "2020-01-03T00:00:00Z",
        );

        let by_saved =
            get_history_inner(&conn, 10, 0, None, Some("space_saved".to_string())).unwrap();
        assert_eq!(
            by_saved.jobs[0].space_saved,
            Some(100),
            "space_saved sorts descending"
        );

        let by_path =
            get_history_inner(&conn, 10, 0, None, Some("source_path".to_string())).unwrap();
        assert_eq!(
            by_path.jobs[0].source_path, "/m/a.mp4",
            "source_path sorts ascending"
        );
    }

    #[test]
    fn history_default_sort_is_newest_completed_first() {
        let conn = test_conn();
        insert_history(
            &conn,
            "old",
            "/m/old.mp4",
            "done",
            1,
            1,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "new",
            "/m/new.mp4",
            "done",
            1,
            1,
            "2024-12-31T00:00:00Z",
        );

        let page = get_history_inner(&conn, 10, 0, None, None).unwrap();
        assert_eq!(page.jobs[0].id, "new", "default order is completed_at DESC");
    }

    #[test]
    fn history_paginates_with_limit_and_offset() {
        let conn = test_conn();
        for i in 0..3 {
            insert_history(
                &conn,
                &format!("j{i}"),
                &format!("/m/{i}.mp4"),
                "done",
                1,
                1,
                "2020-01-01T00:00:00Z",
            );
        }

        let first = get_history_inner(&conn, 2, 0, None, None).unwrap();
        assert_eq!(first.jobs.len(), 2);
        assert_eq!(first.total, 3, "total reflects all rows, not the page size");

        let second = get_history_inner(&conn, 2, 2, None, None).unwrap();
        assert_eq!(second.jobs.len(), 1, "offset returns the remaining page");
    }

    // ---- get_history_summary ----

    #[test]
    fn history_summary_sums_only_done_jobs() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/a.mp4",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/b.mp4",
            "done",
            200,
            2000,
            "2020-01-02T00:00:00Z",
        );
        // Errors live in history but never count as saved space.
        insert_history(
            &conn,
            "3",
            "/m/c.mp4",
            "error",
            999,
            3000,
            "2020-01-03T00:00:00Z",
        );

        let summary = get_history_summary_inner(&conn, None).unwrap();
        assert_eq!(summary.total_saved_bytes, 300);
        assert_eq!(summary.total_files, 2);
    }

    #[test]
    fn history_summary_respects_search() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/keep.mp4",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/other.mp4",
            "done",
            200,
            2000,
            "2020-01-02T00:00:00Z",
        );

        let summary = get_history_summary_inner(&conn, Some("keep".to_string())).unwrap();
        assert_eq!(summary.total_saved_bytes, 100);
        assert_eq!(summary.total_files, 1);
    }

    // End-to-end (local only): needs ffmpeg to synthesize clips and HandBrakeCLI to probe them.
    // Drives the whole skip-by-source-media path through add_files_inner: an at-target source is
    // dropped and reported as AlreadyAtTarget, a codec-upgrade source is queued. Run with:
    //   cargo test -- --ignored add_files_inner_skips_at_target_source_end_to_end
    #[test]
    #[ignore]
    fn add_files_inner_skips_at_target_source_end_to_end() {
        let conn = test_conn();
        let preset: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "UPDATE settings SET value = 'true' WHERE key = 'skip_by_source_media'",
            [],
        )
        .unwrap();
        let state = crate::AppState {
            db: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            preset_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        // Pin the target to h265/1080p without shelling out to HandBrake for preset metadata.
        state.preset_cache.lock().unwrap().insert(
            preset,
            crate::handbrake::PresetMetadata {
                codec: "h265".into(),
                resolution: "1080p".into(),
                quality: "hq".into(),
                preset: "p".into(),
                device: String::new(),
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, vcodec: &str| {
            let path = dir.path().join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=0.3:size=1920x1080:rate=12",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:v",
                    vcodec,
                ])
                .arg(&path)
                .status()
                .expect("run ffmpeg")
                .success();
            assert!(ok, "ffmpeg failed for {name}");
            path.to_str().unwrap().to_string()
        };
        let at_target = make("a.mp4", "libx265"); // h265 1080p -> skip
        let upgrade = make("b.mp4", "libx264"); // h264 1080p -> queue

        let inputs = vec![at_target, upgrade];
        let result = add_files_inner(&state, &inputs).unwrap();

        assert_eq!(result.added.len(), 1, "only the h264 source is queued");
        assert!(result.added[0].source_path.ends_with("b.mp4"));
        let reported = result
            .skipped
            .iter()
            .find(|c| c.reason == SkipReason::AlreadyAtTarget);
        assert_eq!(
            reported.map(|c| c.count),
            Some(1),
            "the h265 source must be reported as already-at-target, not silently dropped"
        );

        // Second pass over the same inputs: the at-target source's identity is unchanged, so
        // its media is served from probe_cache (zero re-probe), and the codec-upgrade source
        // is now already queued. The at-target source must STILL be reported skipped —
        // proving the cached media drives the same decision as a live probe.
        let again = add_files_inner(&state, &inputs).unwrap();
        assert!(
            again.added.is_empty(),
            "nothing new to queue on a repeat add"
        );
        let at_target_again = again
            .skipped
            .iter()
            .find(|c| c.reason == SkipReason::AlreadyAtTarget);
        assert_eq!(
            at_target_again.map(|c| c.count),
            Some(1),
            "the cached at-target source is still recognized on re-scan"
        );
    }
}
