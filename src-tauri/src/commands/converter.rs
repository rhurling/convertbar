use std::sync::Arc;
use tauri::{AppHandle, State};

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
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
    if let Some(pid) = *pid {
        unsafe {
            libc::kill(pid as i32, libc::SIGSTOP);
        }
        *converter_state.is_paused.lock().map_err(|e| e.to_string())? = true;
    }
    Ok(())
}

#[tauri::command]
pub fn resume_conversion(
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
    if let Some(pid) = *pid {
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
        }
        *converter_state.is_paused.lock().map_err(|e| e.to_string())? = false;
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_conversion(
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let pid = converter_state.current_pid.lock().map_err(|e| e.to_string())?;
    let job_id = converter_state.current_job_id.lock().map_err(|e| e.to_string())?;

    if let Some(pid) = *pid {
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }

    if let Some(ref job_id) = *job_id {
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
    }

    Ok(())
}
