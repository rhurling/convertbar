use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::ServerConfig;

pub mod info;

/// State threaded through every axum handler. `events_tx` has no subscriber until Task 3
/// wires `ServerSink` + the SSE route, but the channel exists from this task so the
/// struct is total (no `Option` to unwrap at every call site).
#[derive(Clone)]
pub struct ServerState {
    pub ctx: Arc<convertbar_core::ctx::Ctx>,
    pub config: Arc<ServerConfig>,
    pub events_tx: broadcast::Sender<(String, Value)>,
}

/// Nests all `/api` routes; the caller (`main.rs`) adds the static/embed fallback.
pub fn api_router(state: ServerState) -> Router {
    let api = Router::new().route("/info", get(info::get_app_info));

    Router::new().nest("/api", api).with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use rusqlite::Connection;
    use tower::ServiceExt;

    use convertbar_core::ctx::Ctx;
    use convertbar_core::dispose::RecordingDisposer;
    use convertbar_core::events::TestSink;

    use super::*;

    // Inlined for this task; Task 5 promotes this to a shared `test_state()` helper once
    // more route modules need it.
    fn test_state() -> ServerState {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        convertbar_core::db::init_db(&conn).expect("init db");
        let ctx = Ctx::new(
            conn,
            Arc::new(TestSink::default()),
            Arc::new(RecordingDisposer::default()),
        );
        let (events_tx, _rx) = broadcast::channel(256);
        ServerState {
            ctx,
            config: Arc::new(
                ServerConfig::from_vars(
                    &[("CONVERTBAR_NO_AUTH".to_string(), "1".to_string())].into(),
                )
                .expect("valid test config"),
            ),
            events_tx,
        }
    }

    #[tokio::test]
    async fn get_api_info_returns_the_four_fields() {
        let app = api_router(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["head"], "server");
        assert_eq!(json["can_pause_process"], cfg!(unix));
        // AuthMode::Open in test_state() -> auth is not required.
        assert_eq!(json["auth_required"], false);
    }
}
