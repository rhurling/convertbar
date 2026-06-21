use regex::Regex;
use rusqlite::{params, Connection};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

use crate::types::JobInfo;
use tauri_plugin_notification::NotificationExt;

/// Filename marker for an in-flight in-place encode. A recognizable, non-suffix token so
/// `is_video_file` can exclude it — a folder scan or watched folder must never enqueue a temp.
pub(crate) const IN_PLACE_TEMP_MARKER: &str = ".convertbar-tmp.";

/// A job re-encodes a file onto itself exactly when its stored output path equals its source.
pub(crate) fn is_in_place(source_path: &str, output_path: &str) -> bool {
    source_path == output_path
}

/// Temp output path for an in-place encode: a hidden, marked sibling in the SAME directory so the
/// final `rename` is atomic (same filesystem). Keeps `.mp4` so HandBrake's container matches the
/// distinct-file path.
pub(crate) fn in_place_temp_path(source_path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(source_path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    parent.join(format!(".{stem}{IN_PLACE_TEMP_MARKER}mp4"))
}

/// Filesystem action for an in-place job once the keep/discard decision is made. Pure mapping so
/// it can be table-tested apart from the side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InPlaceAction {
    /// Re-encode won — overwrite the source with the temp (cleanup_mode = delete).
    RenameTempOverSource,
    /// Re-encode won — move the source to Trash first, then put the temp in its place (trash mode).
    TrashSourceThenRename,
    /// Re-encode lost or produced nothing usable — drop the temp, keep the source.
    RemoveTemp,
}

fn in_place_action(kept: KeptFile, cleanup_mode: &str) -> InPlaceAction {
    match kept {
        KeptFile::Converted => {
            if cleanup_mode == "delete" {
                InPlaceAction::RenameTempOverSource
            } else {
                InPlaceAction::TrashSourceThenRename
            }
        }
        KeptFile::Original | KeptFile::Neither => InPlaceAction::RemoveTemp,
    }
}

fn apply_in_place_action(
    action: InPlaceAction,
    temp: &std::path::Path,
    source: &std::path::Path,
) -> std::io::Result<()> {
    match action {
        InPlaceAction::RenameTempOverSource => std::fs::rename(temp, source),
        InPlaceAction::TrashSourceThenRename => {
            let _ = trash::delete(source);
            std::fs::rename(temp, source)
        }
        InPlaceAction::RemoveTemp => std::fs::remove_file(temp),
    }
}

pub struct ConverterState {
    pub current_pid: Mutex<Option<u32>>,
    pub current_child: Mutex<Option<Child>>,
    pub current_job_id: Mutex<Option<String>>,
    pub is_paused: Mutex<bool>,
    pub is_running: Mutex<bool>,
    pub pause_after_current: Mutex<bool>,
}

impl ConverterState {
    pub fn new() -> Self {
        Self {
            current_pid: Mutex::new(None),
            current_child: Mutex::new(None),
            current_job_id: Mutex::new(None),
            is_paused: Mutex::new(false),
            is_running: Mutex::new(false),
            pause_after_current: Mutex::new(false),
        }
    }

    /// Returns true if the current platform supports real process pause/resume (SIGSTOP/SIGCONT).
    pub fn can_pause_process() -> bool {
        cfg!(target_os = "macos")
    }
}

/// Which file `process_queue` keeps after a successful encode. The variant *is* the cleanup
/// decision — it tells the call site which file (if any) to delete with an irreversible
/// `trash`/`remove_file`. `Original` and `Neither` are distinct so the call site can keep
/// the original while choosing whether to also delete the converted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeptFile {
    /// Converted output is smaller — keep it, delete the original source.
    Converted,
    /// Original is smaller or equal — keep it, delete the converted output.
    Original,
    /// No usable converted output (size 0) — keep the original, delete nothing.
    Neither,
}

/// Pure cleanup decision for a finished encode: from the original and converted byte sizes,
/// decide which file to keep, the space saved (signed — negative when the converted file is
/// larger), and the resulting job status. Behavior-preserving extraction of the logic that
/// used to be inlined in `process_queue`; the actual deletion stays at the call site.
fn decide_cleanup(original_size: i64, converted_size: i64) -> (KeptFile, i64, &'static str) {
    let (kept_file, space_saved) = if converted_size > 0 && converted_size < original_size {
        (KeptFile::Converted, original_size - converted_size)
    } else if converted_size > 0 {
        (KeptFile::Original, original_size - converted_size)
    } else {
        (KeptFile::Neither, 0)
    };

    // Original and Neither both record kept_file = "original" in the DB, so the
    // skipped/done check treats them identically — exactly as the pre-refactor code did.
    let status = if matches!(kept_file, KeptFile::Original | KeptFile::Neither)
        && converted_size >= original_size
    {
        "skipped"
    } else {
        "done"
    };

    (kept_file, space_saved, status)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversionProgress {
    pub job_id: String,
    pub percent: f64,
    pub fps: f64,
    pub avg_fps: f64,
    pub eta_seconds: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MenuBarUpdate {
    pub status: String,
    pub percent: Option<f64>,
    pub file_name: Option<String>,
    pub eta_seconds: Option<u64>,
    pub queue_count: Option<usize>,
    pub fps: Option<f64>,
}

fn parse_progress(line: &str) -> Option<(f64, f64, f64, u64)> {
    use std::sync::OnceLock;

    static FULL_RE: OnceLock<Regex> = OnceLock::new();
    static SIMPLE_RE: OnceLock<Regex> = OnceLock::new();

    // Only match lines containing "Encoding:" to avoid false positives from log lines
    if !line.contains("Encoding:") {
        return None;
    }

    // Try full format: percent + fps + ETA
    let full_re = FULL_RE.get_or_init(|| {
        Regex::new(
            r"Encoding:.*?(\d+\.?\d*)\s*%\s*\((\d+\.?\d*)\s*fps,\s*avg\s*(\d+\.?\d*)\s*fps,\s*ETA\s*(\d+)h(\d+)m(\d+)s\)"
        ).unwrap()
    });

    if let Some(caps) = full_re.captures(line) {
        let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
        let fps: f64 = caps.get(2)?.as_str().parse().ok()?;
        let avg_fps: f64 = caps.get(3)?.as_str().parse().ok()?;
        let hours: u64 = caps.get(4)?.as_str().parse().ok()?;
        let minutes: u64 = caps.get(5)?.as_str().parse().ok()?;
        let seconds: u64 = caps.get(6)?.as_str().parse().ok()?;
        let eta = hours * 3600 + minutes * 60 + seconds;
        return Some((percent, fps, avg_fps, eta));
    }

    // Fallback: percent only (early progress lines without fps/ETA)
    let simple_re = SIMPLE_RE.get_or_init(|| Regex::new(r"Encoding:.*?(\d+\.?\d*)\s*%").unwrap());

    if let Some(caps) = simple_re.captures(line) {
        let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
        return Some((percent, 0.0, 0.0, 0));
    }

    None
}

fn get_next_job(db: &Connection) -> Option<JobInfo> {
    let mut stmt = db
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                kept_file, space_saved, error_message, queue_order, created_at, completed_at
         FROM jobs WHERE status = 'queued'
         ORDER BY queue_order ASC LIMIT 1",
        )
        .ok()?;

    stmt.query_row([], |row| {
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
    })
    .ok()
}

fn get_handbrake_path(db: &Connection) -> Option<String> {
    let configured: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    crate::handbrake::detect_handbrake_path()
}

fn get_cleanup_mode(db: &Connection) -> String {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'cleanup_mode'",
        [],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| "trash".to_string())
}

/// Core queue processing logic. Call from a background thread.
/// The `is_running` flag must be set to true before calling this.
fn process_queue(app: &AppHandle, db: &Arc<Mutex<Connection>>, converter: &ConverterState) {
    let mut had_errors = false;
    loop {
        let job;
        let handbrake_path;
        let cleanup_mode;
        {
            let db = db.lock().unwrap();
            job = match get_next_job(&db) {
                Some(j) => j,
                None => break,
            };
            handbrake_path = match get_handbrake_path(&db) {
                Some(p) => p,
                None => {
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'error', error_message = 'HandBrakeCLI not found', completed_at = ?2 WHERE id = ?1",
                        params![job.id, chrono::Utc::now().to_rfc3339()],
                    );
                    let _ = app.emit(
                        "job-error",
                        serde_json::json!({
                            "job_id": job.id,
                            "error": "HandBrakeCLI not found"
                        }),
                    );
                    continue;
                }
            };
            cleanup_mode = get_cleanup_mode(&db);

            let _ = db.execute(
                "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                params![job.id],
            );
        }

        *converter.current_job_id.lock().unwrap() = Some(job.id.clone());
        *converter.is_paused.lock().unwrap() = false;

        let _ = app.emit(
            "job-status-changed",
            serde_json::json!({
                "job_id": job.id,
                "status": "encoding"
            }),
        );

        let file_name = std::path::Path::new(&job.source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Count remaining queued jobs for tray info
        let queue_count: usize = {
            let db = db.lock().unwrap();
            db.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap_or(0)
        };

        let _ = app.emit(
            "menu-bar-update",
            MenuBarUpdate {
                status: "encoding".to_string(),
                percent: Some(0.0),
                file_name: Some(file_name.clone()),
                eta_seconds: None,
                queue_count: Some(queue_count),
                fps: None,
            },
        );

        // Spawn HandBrakeCLI
        let child = Command::new(&handbrake_path)
            .arg("-Z")
            .arg(&job.preset)
            .arg("-O")
            .arg("-i")
            .arg(&job.source_path)
            .arg("-o")
            .arg(&job.output_path)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = db.lock().unwrap().execute(
                    "UPDATE jobs SET status = 'error', error_message = ?2, completed_at = ?3 WHERE id = ?1",
                    params![
                        job.id,
                        format!("Failed to start HandBrakeCLI: {}", e),
                        chrono::Utc::now().to_rfc3339()
                    ],
                );
                let _ = app.emit(
                    "job-error",
                    serde_json::json!({
                        "job_id": job.id,
                        "error": format!("Failed to start HandBrakeCLI: {}", e)
                    }),
                );
                *converter.current_job_id.lock().unwrap() = None;
                continue;
            }
        };

        let pid = child.id();
        *converter.current_pid.lock().unwrap() = Some(pid);

        // Read stdout for progress (HandBrakeCLI sends progress to stdout when piped)
        let progress_stream = child.stdout.take();

        // Drain stderr so the process doesn't block on a full pipe buffer
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut reader = stderr;
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
            });
        }

        // Store child handle for cross-platform cancel support
        *converter.current_child.lock().unwrap() = Some(child);

        let job_id = job.id.clone();
        let app_clone = app.clone();
        let file_name_clone = file_name.clone();

        let progress_thread = if let Some(stdout) = progress_stream {
            let handle = std::thread::spawn(move || {
                let mut reader = stdout;
                let mut buf = [0u8; 1024];
                let mut partial = String::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                            while let Some(pos) = partial.find(|c: char| c == '\r' || c == '\n') {
                                let line = partial[..pos].to_string();
                                partial = partial[pos + 1..].to_string();
                                if !line.is_empty() {
                                    if let Some((percent, fps, avg_fps, eta)) =
                                        parse_progress(&line)
                                    {
                                        let _ = app_clone.emit(
                                            "conversion-progress",
                                            ConversionProgress {
                                                job_id: job_id.clone(),
                                                percent,
                                                fps,
                                                avg_fps,
                                                eta_seconds: eta,
                                            },
                                        );
                                        let _ = app_clone.emit(
                                            "menu-bar-update",
                                            MenuBarUpdate {
                                                status: "encoding".to_string(),
                                                percent: Some(percent),
                                                file_name: Some(file_name_clone.clone()),
                                                eta_seconds: Some(eta),
                                                queue_count: None,
                                                fps: Some(fps),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
            Some(handle)
        } else {
            None
        };

        let exit_status = wait_for_active_child(converter);

        if let Some(handle) = progress_thread {
            let _ = handle.join();
        }

        *converter.current_pid.lock().unwrap() = None;
        *converter.current_child.lock().unwrap() = None;
        *converter.current_job_id.lock().unwrap() = None;

        match exit_status {
            Ok(status) if status.success() => {
                let converted_size = std::fs::metadata(&job.output_path)
                    .map(|m| m.len() as i64)
                    .ok();
                let original_size = job.original_size.unwrap_or(0);
                let conv_size = converted_size.unwrap_or(0);

                let (kept, space_saved, status_str) = decide_cleanup(original_size, conv_size);

                // Act on the decision: delete the file we're not keeping (Neither deletes nothing).
                match kept {
                    KeptFile::Converted => match cleanup_mode.as_str() {
                        "delete" => {
                            let _ = std::fs::remove_file(&job.source_path);
                        }
                        _ => {
                            let _ = trash::delete(&job.source_path);
                        }
                    },
                    KeptFile::Original => match cleanup_mode.as_str() {
                        "delete" => {
                            let _ = std::fs::remove_file(&job.output_path);
                        }
                        _ => {
                            let _ = trash::delete(&job.output_path);
                        }
                    },
                    KeptFile::Neither => {}
                }

                let kept_file = match kept {
                    KeptFile::Converted => "converted",
                    KeptFile::Original | KeptFile::Neither => "original",
                };

                let now = chrono::Utc::now().to_rfc3339();

                {
                    let db = db.lock().unwrap();
                    let _ = db.execute(
                        "UPDATE jobs SET status = ?2, converted_size = ?3, kept_file = ?4, space_saved = ?5, completed_at = ?6 WHERE id = ?1",
                        params![job.id, status_str, converted_size, kept_file, space_saved, now],
                    );
                }

                let _ = app.emit(
                    "job-completed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                        "kept_file": kept_file,
                        "space_saved": space_saved,
                    }),
                );

                let _ = app.emit(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                    }),
                );

                // Notification logic for successful/skipped jobs
                {
                    let (notify_per_file, errors_only) = {
                        let db = db.lock().unwrap();
                        let get = |k: &str, default: bool| -> bool {
                            db.query_row(
                                "SELECT value FROM settings WHERE key=?1",
                                params![k],
                                |r| r.get::<_, String>(0),
                            )
                            .map(|v| v == "true")
                            .unwrap_or(default)
                        };
                        (
                            get("notifications_per_file", true),
                            get("notifications_errors_only", false),
                        )
                    };

                    if notify_per_file {
                        let is_error = status_str == "error";
                        let should_notify = if errors_only { is_error } else { true };

                        if should_notify {
                            let body = match status_str {
                                "done" => format!(
                                    "{} converted — saved {}",
                                    file_name,
                                    format_bytes_short(space_saved)
                                ),
                                "skipped" => {
                                    format!("{} — kept original (converted was larger)", file_name)
                                }
                                _ => format!("{} failed", file_name),
                            };
                            let _ = app
                                .notification()
                                .builder()
                                .title("ConvertBar")
                                .body(&body)
                                .show();
                        }
                    }
                }

                // Check if we should pause after this job
                if *converter.pause_after_current.lock().unwrap() {
                    *converter.pause_after_current.lock().unwrap() = false;
                    let _ = app.emit(
                        "menu-bar-update",
                        MenuBarUpdate {
                            status: "idle".to_string(),
                            percent: None,
                            file_name: None,
                            eta_seconds: None,
                            queue_count: None,
                            fps: None,
                        },
                    );
                    break;
                }
            }
            Ok(_) | Err(_) => {
                had_errors = true;
                let _ = std::fs::remove_file(&job.output_path);

                let current_status: Option<String> = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM jobs WHERE id = ?1",
                        params![job.id],
                        |row| row.get(0),
                    )
                    .ok();

                if current_status.as_deref() != Some("error") {
                    let _ = db.lock().unwrap().execute(
                        "UPDATE jobs SET status = 'error', error_message = 'Conversion failed', completed_at = ?2 WHERE id = ?1",
                        params![job.id, chrono::Utc::now().to_rfc3339()],
                    );
                    let _ = app.emit(
                        "job-error",
                        serde_json::json!({
                            "job_id": job.id,
                            "error": "Conversion failed"
                        }),
                    );
                    let _ = app.emit(
                        "job-status-changed",
                        serde_json::json!({
                            "job_id": job.id,
                            "status": "error",
                        }),
                    );

                    // Error notification
                    let notify_per_file = {
                        let db = db.lock().unwrap();
                        db.query_row(
                            "SELECT value FROM settings WHERE key='notifications_per_file'",
                            params![],
                            |r| r.get::<_, String>(0),
                        )
                        .map(|v| v == "true")
                        .unwrap_or(true)
                    };
                    if notify_per_file {
                        let _ = app
                            .notification()
                            .builder()
                            .title("ConvertBar")
                            .body(&format!("{} failed", file_name))
                            .show();
                    }
                }
            }
        }
    }

    // No more jobs — queue done notification
    {
        let notify_queue_done = {
            let db = db.lock().unwrap();
            db.query_row(
                "SELECT value FROM settings WHERE key='notifications_queue_done'",
                params![],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(true)
        };
        if notify_queue_done {
            let _ = app
                .notification()
                .builder()
                .title("ConvertBar")
                .body("Queue complete")
                .show();
        }
    }

    let final_status = if had_errors { "error" } else { "idle" };
    let _ = app.emit(
        "menu-bar-update",
        MenuBarUpdate {
            status: final_status.to_string(),
            percent: None,
            file_name: None,
            eta_seconds: None,
            queue_count: None,
            fps: None,
        },
    );

    *converter.is_running.lock().unwrap() = false;
}

/// Starts queue processing in a new background thread.
/// Sets `is_running` to true atomically before spawning.
pub fn run_queue(app: AppHandle, db: Arc<Mutex<Connection>>, converter: Arc<ConverterState>) {
    {
        let mut running = converter.is_running.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }

    std::thread::spawn(move || {
        process_queue(&app, &db, &converter);
    });
}

/// Waits for the active child process to exit, polling with `try_wait` so the
/// `current_child` lock is released between checks. This lets `cancel_conversion`
/// acquire the lock and kill the process instead of deadlocking against a lock held
/// for the entire blocking `wait()`. Progress is reported by the separate stdout
/// thread, so the poll interval is invisible to the user.
fn wait_for_active_child(converter: &ConverterState) -> std::io::Result<std::process::ExitStatus> {
    loop {
        {
            let mut child_guard = converter.current_child.lock().unwrap();
            match child_guard.as_mut() {
                Some(child) => {
                    if let Some(status) = child.try_wait()? {
                        return Ok(status);
                    }
                    // None: still running — release the lock and poll again
                }
                None => {
                    // Child was already taken (e.g. by cancel), treat as failure
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Process handle missing",
                    ));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn format_bytes_short(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    if abs >= 1_073_741_824 {
        format!("{:.1}GB", abs as f64 / 1_073_741_824.0)
    } else if abs >= 1_048_576 {
        format!("{:.0}MB", abs as f64 / 1_048_576.0)
    } else if abs >= 1024 {
        format!("{:.0}KB", abs as f64 / 1024.0)
    } else {
        format!("{}B", abs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_progress_line() {
        let line = "Encoding: task 1 of 1, 42.50 % (123.45 fps, avg 120.00 fps, ETA 00h02m30s)";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 42.5);
        assert_eq!(fps, 123.45);
        assert_eq!(avg_fps, 120.0);
        assert_eq!(eta, 150); // 2m30s
    }

    #[test]
    fn falls_back_to_percent_only() {
        let line = "Encoding: task 1 of 1, 5.00 %";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 5.0);
        assert_eq!(fps, 0.0);
        assert_eq!(avg_fps, 0.0);
        assert_eq!(eta, 0);
    }

    #[test]
    fn ignores_non_encoding_lines() {
        assert!(parse_progress("Scanning title 1 of 1").is_none());
    }

    #[test]
    fn format_bytes_short_picks_units() {
        assert_eq!(format_bytes_short(0), "0B");
        assert_eq!(format_bytes_short(1024), "1KB");
        assert_eq!(format_bytes_short(1_048_576), "1MB");
        assert_eq!(format_bytes_short(1_073_741_824), "1.0GB");
    }

    // Regression test for the cancel-freeze deadlock: the queue thread must not hold
    // the `current_child` lock across the blocking wait, or `cancel_conversion` (which
    // needs that same lock to kill the child) blocks the main thread and freezes the UI.
    #[cfg(unix)]
    #[test]
    fn cancel_can_kill_child_while_wait_in_progress() {
        use std::sync::mpsc;
        use std::time::Duration;

        let converter = Arc::new(ConverterState::new());

        // A long-running child stands in for an active HandBrakeCLI encode. It must
        // outlive the timeouts below so a premature exit can't mask the deadlock.
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        *converter.current_child.lock().unwrap() = Some(child);

        // Thread A: the queue loop's wait.
        let waiter_rx = {
            let converter = converter.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = wait_for_active_child(&converter);
                let _ = tx.send(());
            });
            rx
        };

        // Let Thread A enter the wait before cancel races for the lock.
        std::thread::sleep(Duration::from_millis(200));

        // Thread B: emulate cancel_conversion acquiring the lock and killing the child.
        let killed_rx = {
            let converter = converter.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                if let Some(child) = converter.current_child.lock().unwrap().as_mut() {
                    let _ = child.kill();
                }
                let _ = tx.send(());
            });
            rx
        };

        // Cancel must acquire `current_child` and kill promptly. If the wait holds the
        // lock across the blocking wait, this lock never frees and recv_timeout fails.
        killed_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cancel could not acquire current_child lock — wait holds it across the wait");

        // And the waiter must observe the killed process and return.
        waiter_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait did not return after the child was killed");
    }

    #[test]
    fn decide_cleanup_matrix() {
        // (original_size, converted_size) -> (kept file, space_saved, status).
        // This decision drives an irreversible delete, so every cell is pinned.
        let cases = [
            // Converted is smaller: keep converted, delete the original, count the win.
            (1000i64, 600i64, KeptFile::Converted, 400i64, "done"),
            // Converted is larger: keep original, delete the converted, flag skipped
            // (space_saved is the negative delta, matching the pre-refactor behavior).
            (1000, 1500, KeptFile::Original, -500, "skipped"),
            // Converted equals original: no win, keep original, skipped.
            (1000, 1000, KeptFile::Original, 0, "skipped"),
            // No/zero output but the source had a size: keep original, delete nothing, done.
            (1000, 0, KeptFile::Neither, 0, "done"),
            // Both zero (degenerate): keep original, delete nothing, skipped.
            (0, 0, KeptFile::Neither, 0, "skipped"),
        ];
        for (orig, conv, want_kept, want_saved, want_status) in cases {
            let (kept, saved, status) = decide_cleanup(orig, conv);
            assert_eq!(kept, want_kept, "kept for ({orig}, {conv})");
            assert_eq!(saved, want_saved, "space_saved for ({orig}, {conv})");
            assert_eq!(status, want_status, "status for ({orig}, {conv})");
        }
    }

    #[test]
    fn is_in_place_only_when_paths_match() {
        assert!(is_in_place("/m/clip.mp4", "/m/clip.mp4"));
        assert!(!is_in_place("/m/clip.mkv", "/m/clip.mp4"));
        assert!(!is_in_place("/m/clip.mp4", "/m/clip-conv.mp4"));
    }

    #[test]
    fn in_place_temp_path_is_marked_hidden_sibling() {
        let temp = in_place_temp_path("/movies/clip.mp4");
        assert_eq!(
            temp,
            std::path::Path::new("/movies/.clip.convertbar-tmp.mp4")
        );
        assert!(temp.to_string_lossy().contains(IN_PLACE_TEMP_MARKER));
    }

    #[test]
    fn in_place_action_maps_decision_to_filesystem_op() {
        assert_eq!(
            in_place_action(KeptFile::Converted, "delete"),
            InPlaceAction::RenameTempOverSource
        );
        assert_eq!(
            in_place_action(KeptFile::Converted, "trash"),
            InPlaceAction::TrashSourceThenRename
        );
        assert_eq!(
            in_place_action(KeptFile::Original, "delete"),
            InPlaceAction::RemoveTemp
        );
        assert_eq!(
            in_place_action(KeptFile::Neither, "trash"),
            InPlaceAction::RemoveTemp
        );
    }

    #[test]
    fn apply_rename_replaces_source_with_temp() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"reencoded").unwrap();

        apply_in_place_action(InPlaceAction::RenameTempOverSource, &temp, &source).unwrap();

        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"reencoded",
            "source now holds the re-encode"
        );
        assert!(!temp.exists(), "temp was consumed by the rename");
    }

    #[test]
    fn apply_remove_temp_keeps_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"bigger-reencode").unwrap();

        apply_in_place_action(InPlaceAction::RemoveTemp, &temp, &source).unwrap();

        assert!(!temp.exists(), "temp was removed");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"original",
            "source is left exactly as it was"
        );
    }
}
