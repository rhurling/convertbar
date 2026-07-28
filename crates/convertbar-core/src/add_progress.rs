use crate::events::{EventSink, EventSinkExt};
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Serialize)]
struct StartedPayload {
    op_id: String,
    label: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    op_id: String,
    label: String,
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
pub struct AddOp {
    events: Arc<dyn EventSink>,
    op_id: String,
    label: String,
}

impl AddOp {
    pub fn new(events: Arc<dyn EventSink>, label: String) -> Self {
        let op_id = uuid::Uuid::new_v4().to_string();
        events.emit_t(
            "add-started",
            StartedPayload {
                op_id: op_id.clone(),
                label: label.clone(),
            },
        );
        Self {
            events,
            op_id,
            label,
        }
    }

    /// Emit one per-file progress tick. `done` counts probed files so far; `total` is the
    /// probe-candidate count for this batch.
    pub fn report(&self, done: u32, total: u32) {
        self.events.emit_t(
            "add-progress",
            ProgressPayload {
                op_id: self.op_id.clone(),
                label: self.label.clone(),
                done,
                total,
            },
        );
    }
}

impl Drop for AddOp {
    fn drop(&mut self) {
        self.events.emit_t(
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
    use crate::events::TestSink;

    #[test]
    fn emits_started_with_label_on_new_and_finished_on_drop() {
        let sink = Arc::new(TestSink::default());
        {
            let _op = AddOp::new(sink.clone(), "My Folder".to_string());
            assert_eq!(
                sink.payloads("add-started").len(),
                1,
                "started fires immediately"
            );
            assert_eq!(sink.payloads("add-started")[0]["label"], "My Folder");
            assert!(sink.payloads("add-finished").is_empty(), "not finished yet");
        }
        assert_eq!(
            sink.payloads("add-finished").len(),
            1,
            "finished fires on drop"
        );
    }

    #[test]
    fn finished_fires_even_on_early_return() {
        fn guarded(sink: Arc<dyn EventSink>, bail: bool) {
            let _op = AddOp::new(sink, String::new());
            if bail {
                return;
            }
        }
        let sink = Arc::new(TestSink::default());
        guarded(sink.clone(), true);
        assert_eq!(sink.payloads("add-finished").len(), 1);
    }

    #[test]
    fn report_emits_progress_with_op_id_and_label() {
        let sink = Arc::new(TestSink::default());
        let op = AddOp::new(sink.clone(), "Clips".to_string());
        op.report(1, 3);

        let started = sink.payloads("add-started");
        let op_id = started[0]["op_id"].as_str().unwrap();

        let progress = sink.payloads("add-progress");
        assert_eq!(progress.len(), 1);
        assert_eq!(
            progress[0]["op_id"].as_str().unwrap(),
            op_id,
            "progress carries the op's id"
        );
        assert_eq!(
            progress[0]["label"].as_str().unwrap(),
            "Clips",
            "progress carries the label"
        );
        assert_eq!(progress[0]["done"].as_u64().unwrap(), 1);
        assert_eq!(progress[0]["total"].as_u64().unwrap(), 3);
    }
}
