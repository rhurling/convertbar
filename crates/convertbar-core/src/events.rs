use serde::Serialize;
use std::sync::Mutex;

/// Head-agnostic event/notification sink. Desktop wraps AppHandle (emit + toast);
/// the server head broadcasts to SSE and no-ops notify. Emit call sites MUST pass
/// the event name as a string literal — the ipc-contract test greps for them.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value);
    fn notify(&self, title: &str, body: &str);
}

/// Typed convenience over the object-safe trait.
pub trait EventSinkExt {
    fn emit_t<T: Serialize>(&self, event: &str, payload: T);
}

impl<S: EventSink + ?Sized> EventSinkExt for S {
    fn emit_t<T: Serialize>(&self, event: &str, payload: T) {
        if let Ok(v) = serde_json::to_value(payload) {
            self.emit(event, v);
        }
    }
}

/// Recording sink for tests (replaces the MockRuntime + Listener pattern).
#[derive(Default)]
pub struct TestSink {
    pub events: Mutex<Vec<(String, serde_json::Value)>>,
    pub notifications: Mutex<Vec<(String, String)>>,
}

impl TestSink {
    pub fn payloads(&self, name: &str) -> Vec<serde_json::Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl EventSink for TestSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), payload));
    }
    fn notify(&self, title: &str, body: &str) {
        self.notifications
            .lock()
            .unwrap()
            .push((title.to_string(), body.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_t_serializes_and_records() {
        #[derive(Serialize)]
        struct P {
            x: u32,
        }
        let sink = TestSink::default();
        sink.emit_t("my-event", P { x: 7 });
        let got = sink.payloads("my-event");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["x"], 7);
    }

    #[test]
    fn notify_records_title_and_body() {
        let sink = TestSink::default();
        sink.notify("T", "B");
        assert_eq!(
            sink.notifications.lock().unwrap()[0],
            ("T".into(), "B".into())
        );
    }
}
