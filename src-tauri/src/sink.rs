use convertbar_core::events::EventSink;
use tauri::Emitter;

pub struct TauriSink<R: tauri::Runtime>(pub tauri::AppHandle<R>);

impl<R: tauri::Runtime> EventSink for TauriSink<R> {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
    fn notify(&self, title: &str, body: &str) {
        use tauri_plugin_notification::NotificationExt;
        let _ = self
            .0
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    #[test]
    fn emit_passes_through_to_tauri_events() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_store = seen.clone();
        app.listen_any("probe-event", move |e| {
            sink_store.lock().unwrap().push(e.payload().to_string());
        });
        let sink = TauriSink(app.handle().clone());
        sink.emit("probe-event", serde_json::json!({ "k": 1 }));
        assert_eq!(seen.lock().unwrap().len(), 1);
        // notify is not observable under MockRuntime (no notification plugin) — the
        // desktop notify path keeps its existing manual verification.
    }
}
