//! Queue control: start/pause/resume/cancel, the pause-after-current fallback, and low-disk
//! status. Moved from the desktop command layer so a future server head can drive the
//! same queue lifecycle through `Ctx` instead.
//!
//! Mid-encode pause (SIGSTOP/SIGCONT) is `#[cfg(unix)]`, not `#[cfg(target_os = "macos")]`:
//! desktop Linux (and a future Linux server head) get true mid-encode pause too. Windows keeps
//! the pause-after-current fallback (`ConverterState::can_pause_process` reports `cfg!(unix)`).

use std::sync::Arc;

use rusqlite::params;

use crate::converter::{self, ConverterState, LowDiskPause, MenuBarUpdate};
use crate::ctx::Ctx;
use crate::events::EventSinkExt;

/// (Re)starts the queue if it isn't already running, clearing any remembered pause first — a
/// user (re)starting the queue (Resume button, or a drag-drop add which routes through this)
/// clears any remembered pause.
pub fn start_queue(ctx: &Arc<Ctx>) -> Result<(), String> {
    let is_running = *ctx.converter.is_running.lock().map_err(|e| e.to_string())?;
    if is_running {
        return Ok(());
    }

    // A user (re)starting the queue — Resume button, or a drag-drop add which routes through
    // startQueue — clears any remembered pause.
    if let Ok(conn) = ctx.db.lock() {
        converter::set_queue_paused(&conn, false);
    }

    converter::run_queue(ctx.clone());
    Ok(())
}

#[cfg_attr(not(unix), allow(unused_variables))]
pub fn pause_conversion(ctx: &Ctx) -> Result<(), String> {
    // On non-unix, fall back to queue-level pause (pause_after_current)
    if !ConverterState::can_pause_process() {
        *ctx.converter
            .pause_after_current
            .lock()
            .map_err(|e| e.to_string())? = true;
        return Ok(());
    }

    #[cfg(unix)]
    {
        let pid_val = {
            let pid = ctx
                .converter
                .current_pid
                .lock()
                .map_err(|e| e.to_string())?;
            *pid
        };
        let job_id_val = {
            let job_id = ctx
                .converter
                .current_job_id
                .lock()
                .map_err(|e| e.to_string())?;
            job_id.clone()
        };

        if let Some(pid) = pid_val {
            unsafe {
                libc::kill(pid as i32, libc::SIGSTOP);
            }
            *ctx.converter.is_paused.lock().map_err(|e| e.to_string())? = true;

            if let Some(ref job_id) = job_id_val {
                {
                    let db = ctx.db.lock().map_err(|e| e.to_string())?;
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'paused' WHERE id = ?1",
                        params![job_id],
                    );
                    converter::set_queue_paused(&db, true);
                } // db must be dropped before these emits: the tray listener re-locks ctx.db
                  // synchronously on this same thread, and std::sync::Mutex is not reentrant —
                  // holding the guard here self-deadlocks.

                ctx.events.emit_t(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job_id,
                        "old_status": "encoding",
                        "new_status": "paused",
                        "status": "paused",
                    }),
                );

                ctx.events.emit_t(
                    "menu-bar-update",
                    MenuBarUpdate {
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

#[cfg_attr(not(unix), allow(unused_variables))]
pub fn resume_conversion(ctx: &Ctx) -> Result<(), String> {
    // Resuming un-pauses the queue on either platform; drop the remembered pause.
    if let Ok(conn) = ctx.db.lock() {
        converter::set_queue_paused(&conn, false);
    }

    // On non-unix, cancel the queue-level pause
    if !ConverterState::can_pause_process() {
        *ctx.converter
            .pause_after_current
            .lock()
            .map_err(|e| e.to_string())? = false;
        return Ok(());
    }

    #[cfg(unix)]
    {
        let pid_val = {
            let pid = ctx
                .converter
                .current_pid
                .lock()
                .map_err(|e| e.to_string())?;
            *pid
        };
        let job_id_val = {
            let job_id = ctx
                .converter
                .current_job_id
                .lock()
                .map_err(|e| e.to_string())?;
            job_id.clone()
        };

        if let Some(pid) = pid_val {
            unsafe {
                libc::kill(pid as i32, libc::SIGCONT);
            }
            *ctx.converter.is_paused.lock().map_err(|e| e.to_string())? = false;

            if let Some(ref job_id) = job_id_val {
                let file_name = {
                    let db = ctx.db.lock().map_err(|e| e.to_string())?;
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                        params![job_id],
                    );

                    let source: Option<String> = db
                        .query_row(
                            "SELECT source_path FROM jobs WHERE id = ?1",
                            params![job_id],
                            |row| row.get(0),
                        )
                        .ok();
                    source.and_then(|p| {
                        std::path::Path::new(&p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string())
                    })
                }; // db must be dropped before these emits: the tray listener re-locks ctx.db
                   // synchronously on this same thread for its "encoding" branch, and
                   // std::sync::Mutex is not reentrant — holding the guard here self-deadlocks.

                ctx.events.emit_t(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job_id,
                        "old_status": "paused",
                        "new_status": "encoding",
                        "status": "encoding",
                    }),
                );

                ctx.events.emit_t(
                    "menu-bar-update",
                    MenuBarUpdate {
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

/// Cancel ordering contract (do not reorder): status write (with `completed_at` +
/// `failure_class`) → SIGCONT-if-paused → kill → wait/reap → clear child handle → clear pid →
/// delete partial output (in-place-aware: `in_place_temp_path` for in-place jobs, never
/// `output_path`).
pub fn cancel_conversion(ctx: &Ctx) -> Result<(), String> {
    // Cancelling the current job doesn't stop the queue (it continues with the next job), so a
    // pause remembered from an earlier SIGSTOP must be dropped — otherwise the next launch would
    // wrongly stay paused for a queue that was actively running.
    if let Ok(conn) = ctx.db.lock() {
        converter::set_queue_paused(&conn, false);
    }

    let job_id_val = {
        let job_id = ctx
            .converter
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
            let db = ctx.db.lock().map_err(|e| e.to_string())?;
            let paths: Option<(String, String)> = db
                .query_row(
                    "SELECT source_path, output_path FROM jobs WHERE id = ?1",
                    params![job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok();
            // completed_at must be set here, not left for the next launch's backfill: until
            // then the row sorts to the bottom of History (ordered by completed_at DESC)
            // instead of showing up as the most recent entry, which is where a just-cancelled
            // job belongs.
            let update_result = db.execute(
                "UPDATE jobs SET status = 'error', error_message = 'Cancelled by user', \
                 failure_class = ?2, completed_at = ?3 WHERE id = ?1",
                params![
                    job_id,
                    crate::failure_class::FailureClass::Environment.as_str(),
                    chrono::Utc::now().to_rfc3339(),
                ],
            );
            (paths, Some(update_result))
        }
        None => (None, None),
    };

    // Kill the child process using cross-platform Child::kill(). Runs even if the status
    // write above failed, so a cancel always stops the process.
    {
        let mut child_guard = ctx
            .converter
            .current_child
            .lock()
            .map_err(|e| e.to_string())?;
        if let Some(ref mut child) = *child_guard {
            // On unix, resume first in case it's paused (SIGSTOP)
            #[cfg(unix)]
            {
                let pid = ctx
                    .converter
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

    // Clear the recorded PID too. Leaving it set while current_child is None lets a
    // concurrent quit (kill_active_child) SIGCONT a now-reaped, possibly-recycled PID on
    // unix until the queue loop clears it ~one poll interval later.
    *ctx.converter
        .current_pid
        .lock()
        .map_err(|e| e.to_string())? = None;

    // Surface a status-write failure now that the process has been killed.
    if let Some(res) = update_result {
        res.map_err(|e| e.to_string())?;
    }

    if let Some(ref job_id) = job_id_val {
        if let Some((ref source_path, ref output_path)) = paths {
            // For an in-place job output_path == source_path, so deleting output_path would delete
            // the user's original. Remove the temp instead; otherwise remove the partial output.
            let target = if converter::is_in_place(source_path, output_path) {
                converter::in_place_temp_path(source_path)
            } else {
                std::path::PathBuf::from(output_path)
            };
            let _ = std::fs::remove_file(&target);
        }

        ctx.events.emit_t(
            "job-status-changed",
            serde_json::json!({
                "job_id": job_id,
                "old_status": "encoding",
                "new_status": "error",
                "status": "error",
            }),
        );

        ctx.events.emit_t(
            "job-error",
            serde_json::json!({
                "job_id": job_id,
                "message": "Cancelled by user",
            }),
        );

        ctx.events.emit_t(
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
    }

    Ok(())
}

pub fn pause_after_current(ctx: &Ctx) -> Result<(), String> {
    *ctx.converter
        .pause_after_current
        .lock()
        .map_err(|e| e.to_string())? = true;
    Ok(())
}

pub fn cancel_pause_after_current(ctx: &Ctx) -> Result<(), String> {
    *ctx.converter
        .pause_after_current
        .lock()
        .map_err(|e| e.to_string())? = false;
    Ok(())
}

pub fn get_pause_after_current(ctx: &Ctx) -> bool {
    ctx.converter.is_pause_after_current()
}

pub fn get_low_disk_pause(ctx: &Ctx) -> Option<LowDiskPause> {
    ctx.converter.low_disk_pause()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Mutex, OnceLock};

    fn test_ctx(conn: Connection) -> Arc<Ctx> {
        Ctx::new(
            conn,
            Arc::new(crate::events::TestSink::default()),
            Arc::new(crate::dispose::DeleteDisposer),
        )
    }

    /// Probe sink for the regression test below. Mirrors the desktop tray listener
    /// (src-tauri/src/lib.rs), which locks `ctx.db` synchronously on the emitting thread to read
    /// `menubar_show_*` settings whenever a "menu-bar-update" event arrives. If `db` is set,
    /// `emit` does a non-blocking `try_lock()` on it: on the SAME thread, `std::sync::Mutex` is
    /// not reentrant, so `try_lock()` against a guard this thread already holds returns
    /// `WouldBlock` rather than deadlocking -- which is exactly what lets this probe fail loud
    /// (a recorded violation) instead of hanging if pause/resume ever regress to emitting while
    /// still holding the db guard.
    #[derive(Default)]
    struct LockProbeSink {
        db: OnceLock<Arc<Mutex<rusqlite::Connection>>>,
        violations: Mutex<Vec<String>>,
        events: Mutex<Vec<String>>,
    }

    impl crate::events::EventSink for LockProbeSink {
        fn emit(&self, event: &str, _payload: serde_json::Value) {
            self.events.lock().unwrap().push(event.to_string());
            if let Some(db) = self.db.get() {
                if db.try_lock().is_err() {
                    self.violations.lock().unwrap().push(event.to_string());
                }
            }
        }
        fn notify(&self, _title: &str, _body: &str) {}
    }

    #[test]
    fn pause_capability_is_unix_wide() {
        // Widened from macOS-only: the Linux container (and desktop Linux) get true
        // mid-encode pause; Windows keeps the pause-after-current fallback.
        assert_eq!(ConverterState::can_pause_process(), cfg!(unix));
    }

    // Regression test for the cancel ordering contract: kill → reap (wait) → delete
    // the partial output. On Windows the dying process holds the output file handle
    // until it is reaped, so deleting before the wait silently leaves the partial
    // behind. A reorder passes on Unix regardless — the assertion with teeth runs in
    // the advisory windows CI job (test-windows.yml); on Unix the test still pins
    // the DB write, the handle clearing, and that the delete happens at all.
    #[test]
    fn cancel_reaps_the_child_before_deleting_the_partial_output() {
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

        let ctx = test_ctx(conn);

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

        *ctx.converter.current_pid.lock().unwrap() = Some(child.id());
        *ctx.converter.current_child.lock().unwrap() = Some(child);
        *ctx.converter.current_job_id.lock().unwrap() = Some("j1".into());

        cancel_conversion(&ctx).unwrap();

        assert!(
            !output.exists(),
            "partial output must be gone — on Windows this fails if the delete runs before the child is reaped"
        );
        let (status, msg, class, completed_at): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = ctx
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, error_message, failure_class, completed_at FROM jobs WHERE id = 'j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "error");
        assert_eq!(msg.as_deref(), Some("Cancelled by user"));
        assert_eq!(
            class.as_deref(),
            Some("environment"),
            "a cancellation is an external action, never the file's fault — and NULL would \
             wrongly read as 'row predates this feature'"
        );
        assert!(
            completed_at.is_some(),
            "F11: completed_at must be set on cancel, or the row sorts to the bottom of \
             History (ordered by completed_at DESC) until the next launch's backfill repairs it"
        );
        assert!(
            ctx.converter.current_child.lock().unwrap().is_none(),
            "the reaped handle must be cleared so the queue loop takes its cancel branch"
        );
        assert!(
            ctx.converter.current_pid.lock().unwrap().is_none(),
            "the recorded PID must be cleared too, so a racing quit can't SIGCONT a reaped, \
             possibly-recycled PID before the queue loop clears it"
        );
    }

    #[test]
    fn start_queue_clears_the_persisted_pause() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // A remembered pause; disable notifications so this test only asserts the pause flag.
        conn.execute(
            "UPDATE settings SET value='false' WHERE key IN ('notifications_per_file','notifications_queue_done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('queue_paused', 'true')
             ON CONFLICT(key) DO UPDATE SET value = 'true'",
            [],
        )
        .unwrap();
        let ctx = test_ctx(conn);

        // Resume: clears the remembered pause (synchronously, before spawning the queue thread).
        start_queue(&ctx).unwrap();

        let paused: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key='queue_paused'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(paused, "false", "Resume clears the remembered pause");
    }

    // Regression test for the resume/pause self-deadlock: the desktop tray listener's
    // "menu-bar-update" handler re-locks ctx.db synchronously on the emitting thread (see
    // src-tauri/src/lib.rs), so pause/resume must drop the db guard before emitting.
    //
    // The PID here is a real spawned child, not this test's own process: SIGSTOP directed at
    // `std::process::id()` was verified (outside this suite) to actually stop the whole test
    // binary -- all threads, including the one that would assert the result -- until an
    // external SIGCONT arrives, which nothing in this test sends. Using a throwaway child
    // process keeps the SIGSTOP/SIGCONT harmless, same as the `cancel_conversion` test above.
    #[cfg(unix)]
    #[test]
    fn resume_and_pause_do_not_hold_db_lock_across_tray_bound_emits() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.mp4");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j1', ?1, ?1, 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            params![source.to_str().unwrap()],
        )
        .unwrap();

        let sink = Arc::new(LockProbeSink::default());
        let ctx = Ctx::new(conn, sink.clone(), Arc::new(crate::dispose::DeleteDisposer));
        sink.db.set(ctx.db.clone()).unwrap();

        // Stand-in for the paused encode's process.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();

        *ctx.converter.current_pid.lock().unwrap() = Some(pid);
        *ctx.converter.current_job_id.lock().unwrap() = Some("j1".to_string());
        *ctx.converter.is_paused.lock().unwrap() = true;

        resume_conversion(&ctx).unwrap();
        pause_conversion(&ctx).unwrap();

        // Clean up: the child is SIGSTOPed from pause_conversion above, so CONT before kill.
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            sink.violations.lock().unwrap().is_empty(),
            "db lock was held during emit(s): {:?}",
            sink.violations.lock().unwrap()
        );
        let events = sink.events.lock().unwrap();
        assert!(events.contains(&"job-status-changed".to_string()));
        assert!(events.contains(&"menu-bar-update".to_string()));
    }
}
