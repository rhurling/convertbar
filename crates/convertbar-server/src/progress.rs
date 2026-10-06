//! The latest `conversion-progress`, remembered for `GET /api/status`. Progress is otherwise
//! only broadcast to live SSE clients, so a poller that asks between events would have nothing.
//!
//! A task on the broadcast rather than a slot written by `ServerSink`: the sink is called from
//! the converter thread under core locks and must never take a lock of its own.

use convertbar_core::converter::ConversionProgress;
use serde_json::Value;
use tokio::sync::{broadcast, watch};

/// Subscribes to `events_tx` and keeps its last `conversion-progress` in the returned watch.
/// The subscription is taken here, before the task first runs, so nothing sent in between is
/// missed. Must be called inside a tokio runtime.
pub fn spawn_progress_cache(
    events_tx: &broadcast::Sender<(String, Value)>,
) -> watch::Receiver<Option<ConversionProgress>> {
    let mut events = events_tx.subscribe();
    let (tx, rx) = watch::channel(None);
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok((name, payload)) if name == "conversion-progress" => {
                    if let Ok(progress) = serde_json::from_value::<ConversionProgress>(payload) {
                        tx.send_replace(Some(progress));
                    }
                }
                Ok(_) => {}
                // Skipped events are older than the ones still queued; the next progress line
                // replaces whatever was lost.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    rx
}
