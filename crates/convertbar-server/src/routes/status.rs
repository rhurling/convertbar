//! `GET /api/status`: a flat snapshot for pollers outside the app (the NAS dashboard's
//! Homepage tile). The four field names are a contract with configs this repo cannot see, so
//! change them only as a breaking change.

use axum::extract::State;
use axum::response::Response;
use serde::Serialize;

use super::{blocking_json, ServerState};

#[derive(Serialize)]
pub struct StatusSnapshot {
    /// `paused | encoding | low_disk | stopped | idle`, decided in that order.
    pub state: &'static str,
    /// 0–100. Never null: Homepage's `percent` format renders a null as "NaN%", and `state`
    /// already tells a real 0 % apart from nothing encoding.
    pub percent: f64,
    pub queued: i64,
    /// Every error row, as the Queue view lists them: cumulative until History → Clear, and
    /// including user cancellations, which the core records as errors.
    pub errors: i64,
}

pub async fn get_status(State(s): State<ServerState>) -> Response {
    let progress = s.progress.borrow().clone();
    blocking_json(move || {
        let (queued, errors, paused, encoding, in_flight) = {
            let db = s.ctx.db.lock().map_err(|e| e.to_string())?;
            db.query_row(
                "SELECT COALESCE(SUM(status = 'queued'), 0), COALESCE(SUM(status = 'error'), 0),
                        COALESCE(SUM(status = 'paused'), 0), COALESCE(SUM(status = 'encoding'), 0),
                        MAX(CASE WHEN status IN ('encoding', 'paused') THEN id END)
                 FROM jobs",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)? > 0,
                        row.get::<_, i64>(3)? > 0,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?
        };
        // Only the job in flight's own progress: the cache keeps the last value until the next
        // job's first progress line, which must not be shown as that job's.
        let percent = match (progress, in_flight) {
            (Some(p), Some(id)) if p.job_id == id => p.percent,
            _ => 0.0,
        };
        // Read after the db guard is dropped: never hold two core locks at once here.
        let running = s.ctx.converter.is_running();
        let low_disk = s.ctx.converter.low_disk_pause().is_some();

        let state = if paused {
            "paused"
        } else if encoding || running {
            // `is_running` covers the gap between two jobs, when no row is encoding yet.
            "encoding"
        } else if low_disk && queued > 0 {
            // The reason is cleared only when the next pass starts, so it outlives a queue
            // emptied by hand; without work behind it, it is no longer a pause.
            "low_disk"
        } else if queued > 0 {
            "stopped"
        } else {
            "idle"
        };

        Ok(StatusSnapshot {
            state,
            percent,
            queued,
            errors,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;
    use convertbar_core::converter::ConversionProgress;
    use convertbar_core::events::EventSinkExt;
    use rusqlite::params;
    use serde_json::{json, Value};

    use crate::routes::tests::{request_json, test_state, test_state_with_locator};
    use crate::routes::{api_router, ServerState};

    async fn send(
        state: &ServerState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        request_json(api_router(state.clone()), method, uri, body).await
    }

    async fn status(state: &ServerState) -> Value {
        let (code, json) = send(state, "GET", "/api/status", None).await;
        assert_eq!(code, StatusCode::OK, "GET /api/status answered {json}");
        json
    }

    fn insert_job(state: &ServerState, id: &str, status: &str, order: i32) {
        state
            .ctx
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES (?1, ?2, ?3, 'Fast 1080p30', ?4, ?5, '2020-01-01T00:00:00Z')",
                params![id, format!("/tmp/{id}-src.mp4"), format!("/tmp/{id}-out.mp4"), status, order],
            )
            .unwrap();
    }

    fn set_running(state: &ServerState, running: bool) {
        *state.ctx.converter.is_running.lock().unwrap() = running;
    }

    #[tokio::test]
    async fn an_empty_install_is_idle_with_zeroed_counts() {
        // The whole shape, pinned: these four names are what a dashboard's config refers to,
        // so renaming one breaks every consumer while the server stays green.
        assert_eq!(
            status(&test_state()).await,
            json!({"state": "idle", "percent": 0.0, "queued": 0, "errors": 0})
        );
    }

    #[tokio::test]
    async fn counts_cover_only_their_own_status() {
        // History rows (done, skipped) and the job in flight are neither waiting nor failed;
        // counting them would make the tile's Queued/Errors drift from the Queue view.
        let state = test_state();
        insert_job(&state, "q1", "queued", 1);
        insert_job(&state, "q2", "queued", 2);
        insert_job(&state, "e1", "error", 3);
        insert_job(&state, "d1", "done", 4);
        insert_job(&state, "s1", "skipped", 5);
        insert_job(&state, "now", "encoding", 0);
        set_running(&state, true);

        let json = status(&state).await;
        assert_eq!(json["queued"], 2);
        assert_eq!(json["errors"], 1);
        assert_eq!(json["state"], "encoding");
    }

    #[tokio::test]
    async fn a_paused_job_reads_paused_although_the_queue_thread_still_runs() {
        // A paused encode keeps its queue thread (parked on the SIGSTOPped child), so
        // `is_running` alone would call it encoding.
        let state = test_state();
        insert_job(&state, "now", "paused", 0);
        insert_job(&state, "next", "queued", 1);
        set_running(&state, true);

        let json = status(&state).await;
        assert_eq!(json["state"], "paused");
        // The paused job is the one in flight, not one waiting.
        assert_eq!(json["queued"], 1);
        assert_eq!(json["errors"], 0);
    }

    #[tokio::test]
    async fn the_gap_between_two_jobs_still_reads_encoding() {
        // Between one job's finish and the next one's claim there is no encoding row; the
        // tile must not flicker to Stopped while the queue is plainly still working.
        let state = test_state();
        insert_job(&state, "next", "queued", 1);
        set_running(&state, true);

        let json = status(&state).await;
        assert_eq!(json["state"], "encoding");
        assert_eq!(json["percent"], 0.0);
    }

    #[tokio::test]
    async fn queued_work_with_nothing_running_reads_stopped_not_idle() {
        // The silent stall this state exists for: after "pause after current", or a queue
        // nobody started, work sits there and an "Idle" tile would hide it.
        let state = test_state();
        insert_job(&state, "q1", "queued", 1);

        assert_eq!(status(&state).await["state"], "stopped");
    }

    #[tokio::test]
    async fn a_low_disk_pause_reads_low_disk_until_its_queue_is_emptied() {
        // Driven through the real gate rather than by poking the pause reason: a floor no disk
        // can meet stops the queue before HandBrake is ever spawned, leaving the job queued
        // and the converter not running — exactly the state `stopped` would otherwise claim.
        let (state, _shutdown) = test_state_with_locator(std::sync::Arc::new(
            convertbar_core::handbrake::StubLocator("/opt/fake/HandBrakeCLI".into()),
        ));
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.mp4");
        std::fs::write(&src, b"not really a video").unwrap();
        state
            .ctx
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES ('j1', ?1, ?2, 'Fast 1080p30', 'queued', 0, '2020-01-01T00:00:00Z')",
                params![
                    src.to_str().unwrap(),
                    dir.path().join("out.mp4").to_str().unwrap()
                ],
            )
            .unwrap();
        let (code, _) = send(
            &state,
            "PUT",
            "/api/settings/low_disk_min_gb",
            Some(json!({"value": "1000000000"})),
        )
        .await;
        assert_eq!(code, StatusCode::NO_CONTENT);

        let (code, _) = send(&state, "POST", "/api/converter/start", None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        // The queue runs on its own thread; wait for it to stop at the gate.
        let mut waited = Duration::ZERO;
        while convertbar_core::control::get_low_disk_pause(&state.ctx).is_none()
            || *state.ctx.converter.is_running.lock().unwrap()
        {
            assert!(
                waited < Duration::from_secs(10),
                "the low-disk gate never tripped"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            waited += Duration::from_millis(20);
        }

        assert_eq!(status(&state).await["state"], "low_disk");

        // Removing the last job leaves the pause reason behind (it is cleared only when the
        // next pass starts); a stale "Low disk" over an empty queue would be a false alarm.
        let (code, _) = send(&state, "DELETE", "/api/queue/jobs/j1", None).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert!(convertbar_core::control::get_low_disk_pause(&state.ctx).is_some());
        assert_eq!(status(&state).await["state"], "idle");
    }

    fn set_job_status(state: &ServerState, id: &str, status: &str) {
        state
            .ctx
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET status = ?2 WHERE id = ?1",
                params![id, status],
            )
            .unwrap();
    }

    /// Emits progress the way the converter does — the core's own payload type through the
    /// server's real sink — and waits until the cache has taken it, so the next request
    /// cannot race the cache task.
    async fn emit_progress(state: &ServerState, job_id: &str, percent: f64) {
        let mut seen = state.progress.clone();
        seen.borrow_and_update();
        crate::sink::ServerSink(state.events_tx.clone()).emit_t(
            "conversion-progress",
            ConversionProgress {
                job_id: job_id.to_string(),
                percent,
                fps: 24.0,
                avg_fps: 23.5,
                eta_seconds: 90,
            },
        );
        tokio::time::timeout(Duration::from_secs(2), seen.changed())
            .await
            .expect("the progress cache never took the event")
            .expect("the progress cache stopped");
    }

    #[tokio::test]
    async fn progress_follows_the_job_in_flight_and_never_a_finished_one() {
        let state = test_state();
        insert_job(&state, "now", "encoding", 0);
        set_running(&state, true);

        emit_progress(&state, "now", 42.5).await;
        let json = status(&state).await;
        assert_eq!(json["state"], "encoding");
        assert_eq!(json["percent"], 42.5);

        // A paused encode is frozen where it stopped; 0 % would read as a restart.
        set_job_status(&state, "now", "paused");
        let json = status(&state).await;
        assert_eq!(json["state"], "paused");
        assert_eq!(json["percent"], 42.5);

        // The next job starts before its first progress line: the cache still holds the
        // finished job's value, which must not be shown as the new job's.
        set_job_status(&state, "now", "done");
        insert_job(&state, "next", "encoding", 1);
        let json = status(&state).await;
        assert_eq!(json["state"], "encoding");
        assert_eq!(json["percent"], 0.0);
    }

    #[tokio::test]
    async fn a_burst_that_overruns_the_broadcast_does_not_end_the_cache() {
        // The broadcast holds 256 events; a burst of queue updates while the cache task is not
        // scheduled makes it lag. Lagging must skip ahead, never stop listening, or progress
        // freezes for the rest of the process with no error anywhere.
        let state = test_state();
        insert_job(&state, "now", "encoding", 0);
        set_running(&state, true);
        for _ in 0..300 {
            let _ = state
                .events_tx
                .send(("queue-updated".to_string(), json!({})));
        }

        emit_progress(&state, "now", 7.5).await;
        assert_eq!(status(&state).await["percent"], 7.5);
    }
}
