use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::converter::{self, ConverterState};
use crate::AppState;

#[tauri::command]
pub fn start_queue(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let is_running = *converter_state.is_running.lock().map_err(|e| e.to_string())?;
    if is_running {
        return Ok(());
    }

    let db = state.db.clone();
    let conv = (*converter_state).clone();

    converter::run_queue(app, db, conv);
    Ok(())
}

#[tauri::command]
pub fn pause_conversion(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid_val = {
        let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
        *pid
    };
    let job_id_val = {
        let job_id = converter_state.current_job_id.lock().map_err(|e| e.to_string())?;
        job_id.clone()
    };

    if let Some(pid) = pid_val {
        unsafe {
            libc::kill(pid as i32, libc::SIGSTOP);
        }
        *converter_state.is_paused.lock().map_err(|e| e.to_string())? = true;

        if let Some(ref job_id) = job_id_val {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let _ = db.execute(
                "UPDATE jobs SET status = 'paused' WHERE id = ?1",
                rusqlite::params![job_id],
            );

            let _ = app.emit("job-status-changed", serde_json::json!({
                "job_id": job_id,
                "old_status": "encoding",
                "new_status": "paused",
                "status": "paused",
            }));

            let _ = app.emit("menu-bar-update", crate::converter::MenuBarUpdate {
                status: "paused".to_string(),
                percent: None,
                file_name: None,
                eta_seconds: None,
                queue_count: None,
            });
        }
    }
    Ok(())
}

#[tauri::command]
pub fn resume_conversion(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid_val = {
        let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
        *pid
    };
    let job_id_val = {
        let job_id = converter_state.current_job_id.lock().map_err(|e| e.to_string())?;
        job_id.clone()
    };

    if let Some(pid) = pid_val {
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
        }
        *converter_state.is_paused.lock().map_err(|e| e.to_string())? = false;

        if let Some(ref job_id) = job_id_val {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let _ = db.execute(
                "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                rusqlite::params![job_id],
            );

            let _ = app.emit("job-status-changed", serde_json::json!({
                "job_id": job_id,
                "old_status": "paused",
                "new_status": "encoding",
                "status": "encoding",
            }));

            let file_name = {
                let source: Option<String> = db.query_row(
                    "SELECT source_path FROM jobs WHERE id = ?1",
                    rusqlite::params![job_id],
                    |row| row.get(0),
                ).ok();
                source.and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                })
            };

            let _ = app.emit("menu-bar-update", crate::converter::MenuBarUpdate {
                status: "encoding".to_string(),
                percent: None,
                file_name,
                eta_seconds: None,
                queue_count: None,
            });
        }
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_conversion(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid_val = {
        let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
        *pid
    };
    let job_id_val = {
        let job_id = converter_state.current_job_id.lock().map_err(|e| e.to_string())?;
        job_id.clone()
    };

    if let Some(pid) = pid_val {
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }

    if let Some(ref job_id) = job_id_val {
        let db = state.db.lock().map_err(|e| e.to_string())?;

        let output_path: Option<String> = db.query_row(
            "SELECT output_path FROM jobs WHERE id = ?1",
            rusqlite::params![job_id],
            |row| row.get(0),
        ).ok();

        if let Some(path) = output_path {
            let _ = std::fs::remove_file(&path);
        }

        db.execute(
            "UPDATE jobs SET status = 'error', error_message = 'Cancelled by user' WHERE id = ?1",
            rusqlite::params![job_id],
        ).map_err(|e| e.to_string())?;

        let _ = app.emit("job-status-changed", serde_json::json!({
            "job_id": job_id,
            "old_status": "encoding",
            "new_status": "error",
            "status": "error",
        }));

        let _ = app.emit("job-error", serde_json::json!({
            "job_id": job_id,
            "message": "Cancelled by user",
        }));

        let _ = app.emit("menu-bar-update", crate::converter::MenuBarUpdate {
            status: "idle".to_string(),
            percent: None,
            file_name: None,
            eta_seconds: None,
            queue_count: None,
        });
    }

    Ok(())
}
