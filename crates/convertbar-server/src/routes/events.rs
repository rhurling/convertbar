//! `GET /api/events`: server-sent events over the broadcast channel fed by `ServerSink`.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use tokio::sync::watch;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use super::ServerState;

pub async fn sse_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let rx = state.events_tx.subscribe();
    let shutdown_rx = state.shutdown_rx.clone();

    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok((name, payload)) => Some(Ok::<_, Infallible>(
            Event::default().event(name).data(payload.to_string()),
        )),
        // The client's reconnect (which refetches full state) heals the gap; there is
        // no way to replay skipped events from a lagged broadcast receiver.
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "SSE subscriber lagged; dropping skipped events");
            None
        }
    });
    // An open EventSource would otherwise never complete: hyper's graceful drain waits
    // on it forever, and `docker stop` escalates to SIGKILL with a paused HandBrake
    // child orphaned underneath. Ending the stream on shutdown lets the response finish
    // so graceful shutdown can proceed. `tokio_stream::StreamExt` (imported above, used
    // for `filter_map`) has no `take_until`, so this call goes through
    // `futures_util::StreamExt` fully-qualified rather than importing it, which would
    // create an ambiguous-method conflict with the sync `filter_map` above.
    let stream = futures_util::StreamExt::take_until(stream, wait_for_shutdown(shutdown_rx));

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// Resolves once the shutdown watch is (or becomes) `true`. Also resolves if every
/// `watch::Sender` is dropped without ever flipping the flag — that shouldn't happen in
/// production (main.rs holds the sender for the process lifetime) but resolving rather
/// than hanging forever is the safer failure mode for a stream-termination guard.
async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use futures_util::StreamExt;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::routes::api_router;
    use crate::routes::tests::test_state_with_shutdown;

    #[tokio::test]
    async fn sse_route_streams_broadcast_events() {
        let (state, _shutdown_tx) = test_state_with_shutdown();
        let events_tx = state.events_tx.clone();
        let app = api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        events_tx
            .send(("conversion-progress".to_string(), json!({"id": 1})))
            .expect("send to a subscribed channel");

        let mut body_stream = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), body_stream.next())
            .await
            .expect("body read timed out")
            .expect("stream ended before yielding a frame")
            .expect("frame read error");

        let frame = String::from_utf8(chunk.to_vec()).expect("frame is valid utf8");
        assert!(
            frame.contains("event: conversion-progress"),
            "frame missing event name: {frame:?}"
        );
        assert!(
            frame.contains(r#"{"id":1}"#),
            "frame missing JSON payload: {frame:?}"
        );
    }

    #[tokio::test]
    async fn sse_stream_ends_when_shutdown_flips() {
        let (state, shutdown_tx) = test_state_with_shutdown();
        let app = api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        shutdown_tx.send(true).expect("watch receiver still alive");

        // Reading to completion only returns once the stream ends; timing out here would
        // mean the SSE stream never terminated after the shutdown flag flipped.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            axum::body::to_bytes(response.into_body(), usize::MAX),
        )
        .await
        .expect("stream did not end within timeout after shutdown flip")
        .expect("body read error");
    }
}
