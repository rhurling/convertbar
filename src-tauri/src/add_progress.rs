use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use uuid::Uuid;

#[derive(Clone, Serialize)]
struct StartedPayload {
    op_id: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    op_id: String,
    done: u32,
    total: u32,
}

#[derive(Clone, Serialize)]
struct FinishedPayload {
    op_id: String,
}

/// RAII guard bracketing a file-intake operation (scan + duplicate/format checks) with
/// `add-started` / `add-finished` events, and emitting `add-progress` per probed file.
/// The finish fires in `Drop`, so the UI spinner always clears — even on an early return,
/// `?`-propagated error, or a panic-unwind (the app has no `panic = "abort"` profile).
///
/// Generic over the runtime so it can be driven by `MockRuntime` in unit tests, matching
/// the other emitters in this codebase (see `converter.rs`).
pub struct AddOp<R: Runtime> {
    app: AppHandle<R>,
    op_id: String,
}

impl<R: Runtime> AddOp<R> {
    pub fn new(app: &AppHandle<R>) -> Self {
        let op_id = Uuid::new_v4().to_string();
        let _ = app.emit(
            "add-started",
            StartedPayload {
                op_id: op_id.clone(),
            },
        );
        Self {
            app: app.clone(),
            op_id,
        }
    }

    /// Emit one per-file progress tick. `done` counts probed files so far; `total` is the
    /// probe-candidate count for this batch.
    pub fn report(&self, done: u32, total: u32) {
        let _ = self.app.emit(
            "add-progress",
            ProgressPayload {
                op_id: self.op_id.clone(),
                done,
                total,
            },
        );
    }
}

impl<R: Runtime> Drop for AddOp<R> {
    fn drop(&mut self) {
        let _ = self.app.emit(
            "add-finished",
            FinishedPayload {
                op_id: self.op_id.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    fn record(app: &tauri::App<tauri::test::MockRuntime>, name: &str) -> Arc<Mutex<Vec<String>>> {
        let store = Arc::new(Mutex::new(Vec::new()));
        let sink = store.clone();
        app.listen_any(name.to_string(), move |e| {
            sink.lock().unwrap().push(e.payload().to_string());
        });
        store
    }

    #[test]
    fn emits_started_on_new_and_finished_on_drop() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let finished = record(&app, "add-finished");
        {
            let _op = AddOp::new(app.handle());
            assert_eq!(
                started.lock().unwrap().len(),
                1,
                "started fires immediately"
            );
            assert_eq!(finished.lock().unwrap().len(), 0, "not finished yet");
        }
        assert_eq!(finished.lock().unwrap().len(), 1, "finished fires on drop");
    }

    #[test]
    fn finished_fires_even_on_early_return() {
        // Simulates the enqueue_and_start Err arm: the guard is in scope, an early
        // return drops it, and add-finished must still fire so the spinner clears.
        fn guarded(app: &tauri::AppHandle<tauri::test::MockRuntime>, bail: bool) {
            let _op = AddOp::new(app);
            if bail {
                return;
            }
        }
        let app = mock_app();
        let finished = record(&app, "add-finished");
        guarded(app.handle(), true);
        assert_eq!(finished.lock().unwrap().len(), 1);
    }

    #[test]
    fn report_emits_progress_with_the_same_op_id() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let progress = record(&app, "add-progress");
        let op = AddOp::new(app.handle());
        op.report(1, 3);
        op.report(2, 3);

        let started = started.lock().unwrap();
        let op_id: serde_json::Value = serde_json::from_str(&started[0]).unwrap();
        let op_id = op_id["op_id"].as_str().unwrap();

        let progress = progress.lock().unwrap();
        assert_eq!(progress.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&progress[0]).unwrap();
        assert_eq!(
            first["op_id"].as_str().unwrap(),
            op_id,
            "progress carries the op's id"
        );
        assert_eq!(first["done"].as_u64().unwrap(), 1);
        assert_eq!(first["total"].as_u64().unwrap(), 3);
    }
}
