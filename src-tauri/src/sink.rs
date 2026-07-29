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

pub struct TrashDisposer;
impl convertbar_core::dispose::FileDisposer for TrashDisposer {
    /// macOS: NSFileManager's `trashItemAtURL`, NOT the `trash` crate's default.
    ///
    /// That default (`DeleteMethod::Finder`) shells out to `osascript` to tell Finder to
    /// delete, which is an Apple Event and therefore needs the Automation TCC grant. TCC pins
    /// that grant to the bundle's cdhash, and ConvertBar ships adhoc-signed — so every release
    /// build changes the cdhash and silently invalidates the grant. v2.0.0 shipped into exactly
    /// that: Trash was refused for a whole queue and no original was ever removed.
    /// `trashItemAtURL` needs no permission, so it survives an unsigned rebuild.
    ///
    /// Trade-off: no Finder trash sound, and on some systems the Trash entry has no "Put Back"
    /// (files are still recoverable by dragging them out).
    #[cfg(target_os = "macos")]
    fn dispose(&self, path: &str) -> bool {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut cx = trash::TrashContext::new();
        cx.set_delete_method(DeleteMethod::NsFileManager);
        cx.delete(path).is_ok()
    }

    /// Windows and Linux have no equivalent permission gate — the crate default is correct.
    #[cfg(not(target_os = "macos"))]
    fn dispose(&self, path: &str) -> bool {
        trash::delete(path).is_ok()
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
