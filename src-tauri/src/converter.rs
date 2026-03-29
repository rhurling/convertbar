use regex::Regex;
use rusqlite::{params, Connection};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

use crate::types::JobInfo;

pub struct ConverterState {
    pub current_pid: Mutex<Option<u32>>,
    pub current_job_id: Mutex<Option<String>>,
    pub is_paused: Mutex<bool>,
    pub is_running: Mutex<bool>,
    pub pause_after_current: Mutex<bool>,
}

impl ConverterState {
    pub fn new() -> Self {
        Self {
            current_pid: Mutex::new(None),
            current_job_id: Mutex::new(None),
            is_paused: Mutex::new(false),
            is_running: Mutex::new(false),
            pause_after_current: Mutex::new(false),
        }
    }
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
    let simple_re = SIMPLE_RE.get_or_init(|| {
        Regex::new(r"Encoding:.*?(\d+\.?\d*)\s*%").unwrap()
    });

    if let Some(caps) = simple_re.captures(line) {
        let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
        return Some((percent, 0.0, 0.0, 0));
    }

    None
}

fn get_next_job(db: &Connection) -> Option<JobInfo> {
    let mut stmt = db.prepare(
        "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                kept_file, space_saved, error_message, queue_order, created_at, completed_at
         FROM jobs WHERE status = 'queued'
         ORDER BY queue_order ASC LIMIT 1"
    ).ok()?;

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
    }).ok()
}

fn get_handbrake_path(db: &Connection) -> Option<String> {
    let configured: Option<String> = db.query_row(
        "SELECT value FROM settings WHERE key = 'handbrake_path'",
        [],
        |row| row.get(0),
    ).ok();

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
    ).unwrap_or_else(|_| "trash".to_string())
}

/// Core queue processing logic. Call from a background thread.
/// The `is_running` flag must be set to true before calling this.
fn process_queue(
    app: &AppHandle,
    db: &Arc<Mutex<Connection>>,
    converter: &ConverterState,
) {
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
                        "UPDATE jobs SET status = 'error', error_message = 'HandBrakeCLI not found' WHERE id = ?1",
                        params![job.id],
                    );
                    let _ = app.emit("job-error", serde_json::json!({
                        "job_id": job.id,
                        "error": "HandBrakeCLI not found"
                    }));
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

        let _ = app.emit("job-status-changed", serde_json::json!({
            "job_id": job.id,
            "status": "encoding"
        }));

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
            ).unwrap_or(0)
        };

        let _ = app.emit("menu-bar-update", MenuBarUpdate {
            status: "encoding".to_string(),
            percent: Some(0.0),
            file_name: Some(file_name.clone()),
            eta_seconds: None,
            queue_count: Some(queue_count),
        });

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
            .stdout(Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = db.lock().unwrap().execute(
                    "UPDATE jobs SET status = 'error', error_message = ?2 WHERE id = ?1",
                    params![job.id, format!("Failed to start HandBrakeCLI: {}", e)],
                );
                let _ = app.emit("job-error", serde_json::json!({
                    "job_id": job.id,
                    "error": format!("Failed to start HandBrakeCLI: {}", e)
                }));
                *converter.current_job_id.lock().unwrap() = None;
                continue;
            }
        };

        let pid = child.id();
        *converter.current_pid.lock().unwrap() = Some(pid);

        // Read stderr for progress on a separate thread
        let stderr = child.stderr.take();
        let job_id = job.id.clone();
        let app_clone = app.clone();
        let file_name_clone = file_name.clone();

        let progress_thread = if let Some(stderr) = stderr {
            let handle = std::thread::spawn(move || {
                let mut reader = stderr;
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
                                    if let Some((percent, fps, avg_fps, eta)) = parse_progress(&line) {
                                        let _ = app_clone.emit("conversion-progress", ConversionProgress {
                                            job_id: job_id.clone(),
                                            percent,
                                            fps,
                                            avg_fps,
                                            eta_seconds: eta,
                                        });
                                        let _ = app_clone.emit("menu-bar-update", MenuBarUpdate {
                                            status: "encoding".to_string(),
                                            percent: Some(percent),
                                            file_name: Some(file_name_clone.clone()),
                                            eta_seconds: Some(eta),
                                            queue_count: None,
                                        });
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

        let exit_status = child.wait();

        if let Some(handle) = progress_thread {
            let _ = handle.join();
        }

        *converter.current_pid.lock().unwrap() = None;
        *converter.current_job_id.lock().unwrap() = None;

        match exit_status {
            Ok(status) if status.success() => {
                let converted_size = std::fs::metadata(&job.output_path)
                    .map(|m| m.len() as i64)
                    .ok();
                let original_size = job.original_size.unwrap_or(0);
                let conv_size = converted_size.unwrap_or(0);

                let (kept_file, space_saved) = if conv_size > 0 && conv_size < original_size {
                    // Converted is smaller — keep converted, remove original
                    match cleanup_mode.as_str() {
                        "delete" => { let _ = std::fs::remove_file(&job.source_path); }
                        _ => { let _ = trash::delete(&job.source_path); }
                    }
                    ("converted".to_string(), original_size - conv_size)
                } else if conv_size > 0 {
                    // Original is smaller or same — keep original, remove converted
                    match cleanup_mode.as_str() {
                        "delete" => { let _ = std::fs::remove_file(&job.output_path); }
                        _ => { let _ = trash::delete(&job.output_path); }
                    }
                    ("original".to_string(), original_size - conv_size)
                } else {
                    ("original".to_string(), 0i64)
                };

                let now = chrono::Utc::now().to_rfc3339();
                let status_str = if kept_file == "original" && conv_size >= original_size {
                    "skipped"
                } else {
                    "done"
                };

                {
                    let db = db.lock().unwrap();
                    let _ = db.execute(
                        "UPDATE jobs SET status = ?2, converted_size = ?3, kept_file = ?4, space_saved = ?5, completed_at = ?6 WHERE id = ?1",
                        params![job.id, status_str, converted_size, kept_file, space_saved, now],
                    );
                }

                let _ = app.emit("job-completed", serde_json::json!({
                    "job_id": job.id,
                    "status": status_str,
                    "kept_file": kept_file,
                    "space_saved": space_saved,
                }));

                let _ = app.emit("job-status-changed", serde_json::json!({
                    "job_id": job.id,
                    "status": status_str,
                }));

                // Check if we should pause after this job
                if *converter.pause_after_current.lock().unwrap() {
                    *converter.pause_after_current.lock().unwrap() = false;
                    let _ = app.emit("menu-bar-update", MenuBarUpdate {
                        status: "idle".to_string(),
                        percent: None,
                        file_name: None,
                        eta_seconds: None,
                        queue_count: None,
                    });
                    break;
                }
            }
            Ok(_) | Err(_) => {
                let _ = std::fs::remove_file(&job.output_path);

                let current_status: Option<String> = db.lock().unwrap().query_row(
                    "SELECT status FROM jobs WHERE id = ?1",
                    params![job.id],
                    |row| row.get(0),
                ).ok();

                if current_status.as_deref() != Some("error") {
                    let _ = db.lock().unwrap().execute(
                        "UPDATE jobs SET status = 'error', error_message = 'Conversion failed' WHERE id = ?1",
                        params![job.id],
                    );
                    let _ = app.emit("job-error", serde_json::json!({
                        "job_id": job.id,
                        "error": "Conversion failed"
                    }));
                    let _ = app.emit("job-status-changed", serde_json::json!({
                        "job_id": job.id,
                        "status": "error",
                    }));
                }
            }
        }
    }

    // No more jobs
    let _ = app.emit("menu-bar-update", MenuBarUpdate {
        status: "idle".to_string(),
        percent: None,
        file_name: None,
        eta_seconds: None,
        queue_count: None,
    });

    *converter.is_running.lock().unwrap() = false;
}

/// Starts queue processing in a new background thread.
/// Sets `is_running` to true atomically before spawning.
pub fn run_queue(
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    converter: Arc<ConverterState>,
) {
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

