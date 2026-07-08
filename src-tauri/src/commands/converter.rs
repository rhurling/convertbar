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
    let is_running = *converter_state
        .is_running
        .lock()
        .map_err(|e| e.to_string())?;
    if is_running {
        return Ok(());
    }

    let db = state.db.clone();
    let conv = (*converter_state).clone();

    converter::run_queue(app, db, conv);
    Ok(())
}

#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
#[tauri::command]
pub fn pause_conversion(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    // On non-macOS, fall back to queue-level pause (pause_after_current)
    if !ConverterState::can_pause_process() {
        *converter_state
            .pause_after_current
            .lock()
            .map_err(|e| e.to_string())? = true;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let pid_val = {
            let pid = converter_state
                .current_pid
                .lock()
                .map_err(|e| e.to_string())?;
            *pid
        };
        let job_id_val = {
            let job_id = converter_state
                .current_job_id
                .lock()
                .map_err(|e| e.to_string())?;
            job_id.clone()
        };

        if let Some(pid) = pid_val {
            unsafe {
                libc::kill(pid as i32, libc::SIGSTOP);
            }
            *converter_state
                .is_paused
                .lock()
                .map_err(|e| e.to_string())? = true;

            if let Some(ref job_id) = job_id_val {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                let _ = db.execute(
                    "UPDATE jobs SET status = 'paused' WHERE id = ?1",
                    rusqlite::params![job_id],
                );

                let _ = app.emit(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job_id,
                        "old_status": "encoding",
                        "new_status": "paused",
                        "status": "paused",
                    }),
                );

                let _ = app.emit(
                    "menu-bar-update",
                    crate::converter::MenuBarUpdate {
                        status: "paused".to_string(),
                        percent: None,
                        file_name: None,
                        eta_seconds: None,
                        queue_count: None,
                        fps: None,
                    },
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
#[tauri::command]
pub fn resume_conversion(
    app: AppHandle,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    // On non-macOS, cancel the queue-level pause
    if !ConverterState::can_pause_process() {
        *converter_state
            .pause_after_current
            .lock()
            .map_err(|e| e.to_string())? = false;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let pid_val = {
            let pid = converter_state
                .current_pid
                .lock()
                .map_err(|e| e.to_string())?;
            *pid
        };
        let job_id_val = {
            let job_id = converter_state
                .current_job_id
                .lock()
                .map_err(|e| e.to_string())?;
            job_id.clone()
        };

        if let Some(pid) = pid_val {
            unsafe {
                libc::kill(pid as i32, libc::SIGCONT);
            }
            *converter_state
                .is_paused
                .lock()
                .map_err(|e| e.to_string())? = false;

            if let Some(ref job_id) = job_id_val {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                let _ = db.execute(
                    "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                    rusqlite::params![job_id],
                );

                let _ = app.emit(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job_id,
                        "old_status": "paused",
                        "new_status": "encoding",
                        "status": "encoding",
                    }),
                );

                let file_name = {
                    let source: Option<String> = db
                        .query_row(
                            "SELECT source_path FROM jobs WHERE id = ?1",
                            rusqlite::params![job_id],
                            |row| row.get(0),
                        )
                        .ok();
                    source.and_then(|p| {
                        std::path::Path::new(&p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string())
                    })
                };

                let _ = app.emit(
                    "menu-bar-update",
                    crate::converter::MenuBarUpdate {
                        status: "encoding".to_string(),
                        percent: None,
                        file_name,
                        eta_seconds: None,
                        queue_count: None,
                        fps: None,
                    },
                );
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_conversion<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    let job_id_val = {
        let job_id = converter_state
            .current_job_id
            .lock()
            .map_err(|e| e.to_string())?;
        job_id.clone()
    };

    // Mark the active job cancelled *before* killing the process. The queue loop only
    // observes the dead process after the kill, and its error branch skips its own
    // status write and "failed" notification when the status is already 'error' — so
    // writing first prevents a spurious "failed" event/notification racing the cancel.
    let (paths, update_result) = match job_id_val {
        Some(ref job_id) => {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let paths: Option<(String, String)> = db
                .query_row(
                    "SELECT source_path, output_path FROM jobs WHERE id = ?1",
                    rusqlite::params![job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok();
            let update_result = db.execute(
                "UPDATE jobs SET status = 'error', error_message = 'Cancelled by user' WHERE id = ?1",
                rusqlite::params![job_id],
            );
            (paths, Some(update_result))
        }
        None => (None, None),
    };

    // Kill the child process using cross-platform Child::kill(). Runs even if the status
    // write above failed, so a cancel always stops the process.
    {
        let mut child_guard = converter_state
            .current_child
            .lock()
            .map_err(|e| e.to_string())?;
        if let Some(ref mut child) = *child_guard {
            // On macOS, resume first in case it's paused (SIGSTOP)
            #[cfg(target_os = "macos")]
            {
                let pid = converter_state
                    .current_pid
                    .lock()
                    .map_err(|e| e.to_string())?;
                if let Some(pid) = *pid {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGCONT);
                    }
                }
            }
            let _ = child.kill();
            // Reap before the partial-output delete below: on Windows the dying
            // process holds the output file handle until it is fully gone, so
            // removing the file under an unreaped child silently fails. Kill has
            // been delivered, so this returns promptly.
            let _ = child.wait();
        }
        // Clear the handle so the queue loop takes its designed cancel branch
        // ("Child was already taken") instead of try_wait() on a reaped child,
        // whose behavior differs by platform.
        *child_guard = None;
    }

    // Surface a status-write failure now that the process has been killed.
    if let Some(res) = update_result {
        res.map_err(|e| e.to_string())?;
    }

    if let Some(ref job_id) = job_id_val {
        if let Some((ref source_path, ref output_path)) = paths {
            // For an in-place job output_path == source_path, so deleting output_path would delete
            // the user's original. Remove the temp instead; otherwise remove the partial output.
            let target = if crate::converter::is_in_place(source_path, output_path) {
                crate::converter::in_place_temp_path(source_path)
            } else {
                std::path::PathBuf::from(output_path)
            };
            let _ = std::fs::remove_file(&target);
        }

        let _ = app.emit(
            "job-status-changed",
            serde_json::json!({
                "job_id": job_id,
                "old_status": "encoding",
                "new_status": "error",
                "status": "error",
            }),
        );

        let _ = app.emit(
            "job-error",
            serde_json::json!({
                "job_id": job_id,
                "message": "Cancelled by user",
            }),
        );

        let _ = app.emit(
            "menu-bar-update",
            crate::converter::MenuBarUpdate {
                status: "idle".to_string(),
                percent: None,
                file_name: None,
                eta_seconds: None,
                queue_count: None,
                fps: None,
            },
        );
    }

    Ok(())
}

#[tauri::command]
pub fn pause_after_current(converter_state: State<'_, Arc<ConverterState>>) -> Result<(), String> {
    *converter_state
        .pause_after_current
        .lock()
        .map_err(|e| e.to_string())? = true;
    Ok(())
}

#[tauri::command]
pub fn cancel_pause_after_current(
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
    *converter_state
        .pause_after_current
        .lock()
        .map_err(|e| e.to_string())? = false;
    Ok(())
}

#[tauri::command]
pub fn get_pause_after_current(converter_state: State<'_, Arc<ConverterState>>) -> bool {
    converter_state.is_pause_after_current()
}

#[derive(serde::Serialize)]
pub struct PlatformCapabilities {
    pub can_pause_process: bool,
}

#[tauri::command]
pub fn get_platform_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        can_pause_process: ConverterState::can_pause_process(),
    }
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use std::sync::Mutex;
    use tauri::Manager;

    // Regression test for the cancel ordering contract: kill → reap (wait) → delete
    // the partial output. On Windows the dying process holds the output file handle
    // until it is reaped, so deleting before the wait silently leaves the partial
    // behind. A reorder passes on Unix regardless — the assertion with teeth runs in
    // the advisory windows CI job (test-windows.yml); on Unix the test still pins
    // the DB write, the handle clearing, and that the delete happens at all.
    #[test]
    fn cancel_reaps_the_child_before_deleting_the_partial_output() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.mp4");
        let output = dir.path().join("out.mp4");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j1', ?1, ?2, 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            params![source.to_str().unwrap(), output.to_str().unwrap()],
        )
        .unwrap();
        app.manage(crate::AppState {
            db: Arc::new(Mutex::new(conn)),
            preset_cache: Mutex::new(Default::default()),
        });

        // Stand-in for the active encode: a long-running child whose stdout IS the
        // partial output file, so it holds the handle exactly like HandBrakeCLI.
        let out_handle = std::fs::File::create(&output).unwrap();
        #[cfg(windows)]
        let child = std::process::Command::new("ping")
            .args(["-n", "31", "127.0.0.1"])
            .stdout(out_handle)
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(out_handle)
            .spawn()
            .unwrap();

        let converter = Arc::new(ConverterState::new());
        *converter.current_pid.lock().unwrap() = Some(child.id());
        *converter.current_child.lock().unwrap() = Some(child);
        *converter.current_job_id.lock().unwrap() = Some("j1".into());
        app.manage(converter.clone());

        cancel_conversion(app.handle().clone(), app.state(), app.state()).unwrap();

        assert!(
            !output.exists(),
            "partial output must be gone — on Windows this fails if the delete runs before the child is reaped"
        );
        let state: State<'_, AppState> = app.state();
        let (status, msg): (String, Option<String>) = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, error_message FROM jobs WHERE id = 'j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "error");
        assert_eq!(msg.as_deref(), Some("Cancelled by user"));
        assert!(
            converter.current_child.lock().unwrap().is_none(),
            "the reaped handle must be cleared so the queue loop takes its cancel branch"
        );
    }
}
