//! `EventSink` implementation for the server head: broadcasts to SSE subscribers
//! instead of emitting through a Tauri `AppHandle`.

use tokio::sync::broadcast;

use convertbar_core::events::EventSink;

/// Fire-and-forget broadcast sink. `emit` never blocks and never locks: it is called
/// from the converter's std::thread under core locks (repo-wide emit-under-lock
/// invariant), so any blocking here would be a deadlock risk. A `send` with zero
/// subscribers is not an error — the web UI simply hasn't connected an SSE client yet.
pub struct ServerSink(pub broadcast::Sender<(String, serde_json::Value)>);

impl EventSink for ServerSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.send((event.to_string(), payload));
    }

    /// Web UI is live via SSE; desktop-style OS notifications don't apply here.
    fn notify(&self, _title: &str, _body: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_reaches_a_subscriber() {
        let (tx, mut rx) = broadcast::channel(16);
        let sink = ServerSink(tx);

        sink.emit("conversion-progress", serde_json::json!({"id": 1}));

        let (name, payload) = rx.try_recv().expect("subscriber should receive the event");
        assert_eq!(name, "conversion-progress");
        assert_eq!(payload, serde_json::json!({"id": 1}));
    }

    #[test]
    fn emit_with_zero_subscribers_does_not_panic() {
        let (tx, _rx) = broadcast::channel::<(String, serde_json::Value)>(16);
        drop(_rx);
        let sink = ServerSink(tx);

        sink.emit("conversion-progress", serde_json::json!({}));
    }

    #[test]
    fn notify_is_a_no_op() {
        let (tx, _rx) = broadcast::channel(16);
        let sink = ServerSink(tx);

        sink.notify("title", "body");
    }
}
