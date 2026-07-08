use rusqlite::params;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

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
pub(crate) fn file_identity(path: &str) -> Option<crate::probe_cache::FileIdentity> {
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
    identity: Option<(i64, i64)>,
    queued_paths: &HashSet<String>,
    legacy_history_paths: &HashSet<String>,
    converted_identities: &HashSet<(i64, i64)>,
) -> Option<SkipReason> {
    let path = Path::new(path_str);
    if !is_video_file(path) {
        return Some(SkipReason::NotVideo);
    }
    if queued_paths.contains(path_str) {
        return Some(SkipReason::AlreadyQueued);
    }
    // A file whose (size, mtime) matches a completed conversion is genuinely already done,
    // whatever it is named. This replaces the old output-filename-exists heuristic, which
    // wrongly skipped a different video that recycled a converted file's name.
    if let Some(id) = identity {
        if converted_identities.contains(&id) {
            return Some(SkipReason::AlreadyConverted);
        }
    }
    // Pre-migration completed rows carry no fingerprint; fall back to the old source_path
    // match (see `fetch_skip_sets` for how this set is scoped).
    if legacy_history_paths.contains(path_str) {
        return Some(SkipReason::AlreadyConverted);
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    if !suffix.is_empty() && stem.ends_with(suffix) {
        return Some(SkipReason::AlreadyConverted);
    }
    None
}

/// Output path for a source, renumbering the BASE name to avoid clobbering an existing file:
/// `movie.mp4` -> `movie.h265.mp4`, then `movie (1).h265.mp4`, `movie (2).h265.mp4`, ...
/// In-place jobs (default output == source) are returned verbatim and never renumbered, so the
/// converter's in-place path stays intact. `is_taken` reports whether a candidate name is already
/// claimed (on disk, by another job row, or earlier in the same batch).
fn choose_output_path(source_path: &str, suffix: &str, is_taken: &dyn Fn(&str) -> bool) -> String {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = path.parent().unwrap_or(Path::new("."));
    let default = parent.join(format!("{}{}.mp4", stem, suffix));
    // In-place guard first: an in-place source's own on-disk presence must never trigger a rename.
    if default.as_path() == path {
        return default.to_string_lossy().to_string();
    }
    let default_str = default.to_string_lossy().to_string();
    if !is_taken(&default_str) {
        return default_str;
    }
    let mut n = 1;
    loop {
        let candidate = parent
            .join(format!("{} ({}){}.mp4", stem, n, suffix))
            .to_string_lossy()
            .to_string();
        if !is_taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Reads the three sets the cheap skip checks test against: active-queue source paths, the
/// pre-migration legacy history paths (scoped by `skip_already_converted`, plus in-place rows),
/// and the always-on `(size, mtime)` fingerprints of completed conversions.
fn fetch_skip_sets(
    conn: &rusqlite::Connection,
    skip_already_converted: bool,
) -> Result<(HashSet<String>, HashSet<String>, HashSet<(i64, i64)>), String> {
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

    // One pass over completed jobs feeds two skip signals:
    // - `converted_identities`: (size, mtime) fingerprints for the always-on identity check.
    // - `legacy_history_paths`: source paths of PRE-MIGRATION rows that carry no fingerprint.
    //   In-place rows (source_path == output_path) are always included to prevent a re-encode
    //   cascade; other legacy rows are included only when `skip_already_converted` is on, which
    //   preserves the historical history-skip behavior for upgraded databases.
    let mut converted_identities: HashSet<(i64, i64)> = HashSet::new();
    let mut legacy_history_paths: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT source_path, output_path, source_size, source_mtime
                 FROM jobs WHERE status IN ('done', 'skipped')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (source_path, output_path, size, mtime) in rows.flatten() {
            match (size, mtime) {
                (Some(s), Some(m)) => {
                    converted_identities.insert((s, m));
                }
                _ => {
                    let in_place = Path::new(&source_path) == Path::new(&output_path);
                    if in_place || skip_already_converted {
                        legacy_history_paths.insert(source_path);
                    }
                }
            }
        }
    }

    Ok((queued_paths, legacy_history_paths, converted_identities))
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
    let (queued_paths, legacy_history_paths, converted_identities) =
        fetch_skip_sets(conn, skip_already_converted)?;
    Ok(paths
        .iter()
        .filter(|p| {
            let identity = file_identity(p).map(|i| (i.size, i.mtime));
            cheap_skip_reason(
                p,
                suffix,
                identity,
                &queued_paths,
                &legacy_history_paths,
                &converted_identities,
            )
            .is_none()
        })
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

        let suffix_template = crate::commands::settings::read_suffix_template(&conn, &preset);

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
        let hb = hb_path.clone().ok_or("HandBrakeCLI not found")?;
        let metadata = super::handbrake::cached_preset_metadata(state, &hb, &preset)?;
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
            let metadata = super::handbrake::cached_preset_metadata(state, hb, &preset)?;
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
    let (queued_paths, legacy_history_paths, converted_identities) =
        fetch_skip_sets(conn, skip_already_converted)?;

    let mut queue_order = get_next_queue_order(conn)?;
    let mut added = Vec::new();
    // Output names claimed earlier in THIS batch, so two new sources never resolve to one name.
    let mut assigned: HashSet<String> = HashSet::new();
    let (mut n_not_video, mut n_queued, mut n_converted, mut n_output_exists) =
        (0u32, 0u32, 0u32, 0u32);

    for path_str in paths {
        let identity = file_identity(path_str);
        let id_tuple = identity.map(|i| (i.size, i.mtime));
        if let Some(reason) = cheap_skip_reason(
            path_str,
            suffix,
            id_tuple,
            &queued_paths,
            &legacy_history_paths,
            &converted_identities,
        ) {
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
        // Never clobber an existing output: if the default name is taken (on disk, by another job
        // row, or earlier in this batch) the base name is renumbered. In-place jobs are returned
        // verbatim by `choose_output_path` and never renumbered. The closure's borrow of
        // `assigned` ends with this block, before the `assigned.insert` below.
        let output_str = {
            let is_taken = |name: &str| -> bool {
                assigned.contains(name)
                    || Path::new(name).exists()
                    || conn
                        .query_row(
                            "SELECT 1 FROM jobs WHERE output_path = ?1 LIMIT 1",
                            params![name],
                            |_| Ok(()),
                        )
                        .is_ok()
            };
            choose_output_path(path_str, suffix, &is_taken)
        };
        assigned.insert(output_str.clone());

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        // Preserve original_size for the space-saved display even when only mtime is unreadable.
        let original_size = identity
            .map(|i| i.size)
            .or_else(|| std::fs::metadata(path).map(|m| m.len() as i64).ok());
        let source_size = identity.map(|i| i.size);
        let source_mtime = identity.map(|i| i.mtime);

        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, source_size, source_mtime, queue_order, created_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                path_str,
                output_str,
                preset,
                original_size,
                source_size,
                source_mtime,
                queue_order,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        added.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_str,
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
pub async fn add_files(app: AppHandle, paths: Vec<String>) -> Result<AddResult, String> {
    // add_files_inner runs a blocking HandBrakeCLI probe per file (source-media skip), so a large
    // drop would freeze the main-thread event loop. Offload to a blocking thread; the AddResult
    // still returns to the awaiting frontend. Same hazard the watcher avoids via scan_existing_background.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        add_files_inner(&state, &paths)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    // scan_video_files stats every entry of an unbounded recursive walk; a deep tree
    // or network volume would freeze the UI if this ran on the main thread.
    tauri::async_runtime::spawn_blocking(move || scan_folder_inner(path))
        .await
        .map_err(|e| e.to_string())?
}

fn scan_folder_inner(path: String) -> Result<FolderScanResult, String> {
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
pub async fn confirm_folder_add(app: AppHandle, path: String) -> Result<AddResult, String> {
    if !Path::new(&path).is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // Both the recursive scan and the per-file probe block; run them off the main thread so
    // confirming a large folder doesn't freeze the UI (same hazard as add_files).
    tauri::async_runtime::spawn_blocking(move || {
        let files = scan_video_files(Path::new(&path));
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect();
        let state = app.state::<AppState>();
        add_files_inner(&state, &paths)
    })
    .await
    .map_err(|e| e.to_string())?
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
pub async fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
    // Dropped folders get the same recursive walk as scan_folder — off the main thread.
    tauri::async_runtime::spawn_blocking(move || classify_paths_inner(paths))
        .await
        .map_err(|e| e.to_string())?
}

fn classify_paths_inner(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
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

    /// A completed job carrying an explicit source-identity fingerprint (post-migration row).
    fn insert_done_with_identity(
        conn: &Connection,
        id: &str,
        source: &str,
        output: &str,
        source_size: i64,
        source_mtime: i64,
    ) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, source_size, source_mtime, queue_order, created_at)
             VALUES (?1, ?2, ?3, 'preset', 'done', ?4, ?4, ?5, 0, '2020-01-01T00:00:00Z')",
            params![id, source, output, source_size, source_mtime],
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
    fn add_files_renumbers_when_output_name_is_taken() {
        // A pre-existing output with no completed-job fingerprint to prove it belongs to this
        // source must NOT cause a skip (that was the recycled-filename bug). The source is queued
        // to a renumbered output so the unrelated existing file is never clobbered.
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip-conv.mp4"), b"x").unwrap();
        let source = dir.path().join("clip.mov").to_string_lossy().to_string();

        let result = add_files_to_db(&conn, &[source], "preset", "-conv", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "a taken output name renumbers, not skips"
        );
        assert!(
            result.added[0].output_path.ends_with("clip (1)-conv.mp4"),
            "output must be renumbered on the base name, got {}",
            result.added[0].output_path
        );
        assert!(result.skipped.is_empty(), "renumbering is not a skip");
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

    // ---- cheap_skip_reason: identity is the authoritative "already converted" signal ----

    fn ids(pairs: &[(i64, i64)]) -> HashSet<(i64, i64)> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn cheap_skip_reason_skips_on_matching_identity_regardless_of_name() {
        // The point of the fix: a file whose (size, mtime) matches a completed conversion is
        // already done even though its name matches nothing we recorded.
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/whatever.mp4",
            ".h265",
            Some((500, 42)),
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, Some(SkipReason::AlreadyConverted));
    }

    #[test]
    fn cheap_skip_reason_keeps_a_different_identity_at_a_recycled_path() {
        // A different video (different size/mtime) that recycled a converted file's name must NOT
        // be skipped — this is the exact bug. Neither the identity set nor legacy paths match.
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/recycled.mp4",
            ".h265",
            Some((999, 7)),
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn cheap_skip_reason_never_matches_identity_when_stat_failed() {
        // Unreadable identity (None) must never match — uncertainty keeps the file (it converts).
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/gone.mp4",
            ".h265",
            None,
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn cheap_skip_reason_legacy_path_still_skips_but_never_output_exists() {
        // Pre-migration fallback: a source_path in the legacy set skips as AlreadyConverted.
        let legacy: HashSet<String> = ["/m/old.mp4".to_string()].into_iter().collect();
        assert_eq!(
            cheap_skip_reason(
                "/m/old.mp4",
                ".h265",
                Some((1, 1)),
                &HashSet::new(),
                &legacy,
                &HashSet::new(),
            ),
            Some(SkipReason::AlreadyConverted),
        );
        // The suffix-ends guard is untouched.
        assert_eq!(
            cheap_skip_reason(
                "/m/clip.h265.mp4",
                ".h265",
                None,
                &HashSet::new(),
                &HashSet::new(),
                &HashSet::new(),
            ),
            Some(SkipReason::AlreadyConverted),
        );
        // OutputExists is gone: a bare unknown video is kept, never skipped by a filename check.
        assert_eq!(
            cheap_skip_reason(
                "/m/fresh.mp4",
                ".h265",
                Some((5, 5)),
                &HashSet::new(),
                &HashSet::new(),
                &HashSet::new(),
            ),
            None,
        );
    }

    // ---- choose_output_path: renumber the base name, never clobber, never renumber in-place ----

    /// Normalizes path separators so assertions written with `/` match the values
    /// `choose_output_path` builds with the platform separator (`\` on Windows).
    fn norm(path: &str) -> String {
        path.replace('\\', "/")
    }

    #[test]
    fn choose_output_path_renumbers_the_base_name_when_taken() {
        let free = |_: &str| false;
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &free)),
            "/m/clip.h265.mp4",
            "an untaken default name is used as-is"
        );

        let taken1: HashSet<String> = ["/m/clip.h265.mp4".to_string()].into_iter().collect();
        let is_taken1 = |n: &str| taken1.contains(&norm(n));
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &is_taken1)),
            "/m/clip (1).h265.mp4"
        );

        let taken2: HashSet<String> = ["/m/clip.h265.mp4", "/m/clip (1).h265.mp4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let is_taken2 = |n: &str| taken2.contains(&norm(n));
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &is_taken2)),
            "/m/clip (2).h265.mp4"
        );
    }

    #[test]
    fn choose_output_path_never_renumbers_an_in_place_job() {
        // An in-place source's own on-disk presence must never trigger a rename, or the converter
        // would route it through the distinct-file overwrite path.
        let always_taken = |_: &str| true;
        assert_eq!(
            norm(&choose_output_path("/m/clip.mp4", "", &always_taken)),
            "/m/clip.mp4"
        );
    }

    #[test]
    fn choose_output_path_dedupes_within_a_batch() {
        let mut assigned: HashSet<String> = HashSet::new();
        let first = {
            let is_taken = |n: &str| assigned.contains(n);
            choose_output_path("/m/clip.mov", ".h265", &is_taken)
        };
        assigned.insert(first.clone());
        let second = {
            let is_taken = |n: &str| assigned.contains(n);
            choose_output_path("/m/clip.mov", ".h265", &is_taken)
        };
        assert_eq!(norm(&first), "/m/clip.h265.mp4");
        assert_eq!(
            norm(&second),
            "/m/clip (1).h265.mp4",
            "a batch never assigns one name twice"
        );
    }

    // ---- fetch_skip_sets: always-on identity vs. flag-gated legacy fallback ----

    #[test]
    fn fetch_skip_sets_fingerprinted_row_is_identity_only_and_flag_independent() {
        let conn = test_conn();
        insert_done_with_identity(&conn, "h1", "/m/a.mp4", "/m/a.h265.mp4", 500, 42);

        for flag in [false, true] {
            let (_, legacy, identities) = fetch_skip_sets(&conn, flag).unwrap();
            assert!(
                identities.contains(&(500, 42)),
                "a fingerprinted row is always in the identity set (flag={flag})"
            );
            assert!(
                !legacy.contains("/m/a.mp4"),
                "a fingerprinted non-in-place row must NOT be path-matched (recycling guard, flag={flag})"
            );
        }
    }

    #[test]
    fn fetch_skip_sets_null_mtime_row_is_legacy_only_when_flag_on() {
        let conn = test_conn();
        // insert_history writes no fingerprint -> a pre-migration row.
        insert_history(
            &conn,
            "h1",
            "/m/b.mkv",
            "done",
            0,
            1000,
            "2020-01-01T00:00:00Z",
        );

        let (_, legacy_off, _) = fetch_skip_sets(&conn, false).unwrap();
        assert!(
            !legacy_off.contains("/m/b.mkv"),
            "flag off: no legacy path skip"
        );

        let (_, legacy_on, _) = fetch_skip_sets(&conn, true).unwrap();
        assert!(
            legacy_on.contains("/m/b.mkv"),
            "flag on: legacy path skip preserved"
        );
    }

    #[test]
    fn fetch_skip_sets_null_mtime_in_place_row_is_always_legacy() {
        let conn = test_conn();
        // A pre-migration in-place row (source == output by Path equality, note the `//`) must be
        // path-skipped regardless of the flag, or re-scanning it triggers a re-encode cascade.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('h1', '/m//c.mp4', '/m/c.mp4', 'preset', 'done', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        for flag in [false, true] {
            let (_, legacy, _) = fetch_skip_sets(&conn, flag).unwrap();
            assert!(
                legacy.contains("/m//c.mp4"),
                "a legacy in-place row is path-skipped regardless of flag (flag={flag})"
            );
        }
    }

    // ---- end-to-end recycling scenarios against real files ----

    #[test]
    fn add_files_skips_a_recycled_path_whose_identity_matches() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.mkv");
        std::fs::write(&src, b"same-content").unwrap();
        let src_str = src.to_string_lossy().to_string();
        let id = file_identity(&src_str).unwrap();
        insert_done_with_identity(
            &conn,
            "h1",
            &src_str,
            "/m/movie.h265.mp4",
            id.size,
            id.mtime,
        );

        let result = add_files_to_db(&conn, &[src_str], "preset", ".h265", false).unwrap();

        assert!(
            result.added.is_empty(),
            "an unchanged, already-converted file is skipped"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );
    }

    #[test]
    fn add_files_converts_and_renumbers_when_a_different_file_recycles_the_path() {
        // The reported bug: source A is converted, its path is later reused by a DIFFERENT video B
        // while A's output still sits on disk. B must be queued (not skipped) to a renumbered
        // output, leaving A's output untouched.
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.mkv");
        std::fs::write(&src, b"A").unwrap();
        let src_str = src.to_string_lossy().to_string();
        let id_a = file_identity(&src_str).unwrap();

        let old_output = dir.path().join("movie.h265.mp4");
        std::fs::write(&old_output, b"A-converted").unwrap();
        insert_done_with_identity(
            &conn,
            "h1",
            &src_str,
            &old_output.to_string_lossy(),
            id_a.size,
            id_a.mtime,
        );

        // A different video B recycles the same path.
        std::fs::write(&src, b"B is a completely different, longer video").unwrap();

        let result = add_files_to_db(&conn, &[src_str], "preset", ".h265", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "the different file must be queued, not skipped"
        );
        assert!(
            result.added[0].output_path.ends_with("movie (1).h265.mp4"),
            "queued to a renumbered output, got {}",
            result.added[0].output_path
        );
        assert!(result.skipped.is_empty());
        assert_eq!(
            std::fs::read(&old_output).unwrap(),
            b"A-converted",
            "the earlier conversion's output must never be clobbered"
        );
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
