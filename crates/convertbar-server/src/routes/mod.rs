use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
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
    // A nested router with no `.fallback()` of its own inherits the OUTER router's
    // fallback (axum 0.8 documented behavior) — without this, an unmatched `/api/*`
    // path would fall through to the SPA embed handler and could serve `index.html`
    // with 200 instead of a 404.
    let api = Router::new()
        .route("/info", get(info::get_app_info))
        .fallback(api_not_found);

    Router::new().nest("/api", api).with_state(state)
}

async fn api_not_found() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({"error": "not found"})),
    )
}

/// The full app: `api_router` plus the embedded SPA fallback, exactly as `main.rs` serves
/// it. Factored out so tests can exercise the real composition (nest + outer fallback
/// together) rather than `api_router` alone.
pub fn app(state: ServerState) -> Router {
    api_router(state).fallback(crate::embed::fallback)
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

    /// Regression: a nested router with no `.fallback()` of its own inherits the OUTER
    /// router's fallback (axum 0.8 documented behavior). Without an api-specific
    /// fallback, an unmatched `/api/*` path fell through to the SPA embed handler and
    /// would serve `index.html` with 200 once `dist-web` is populated in production —
    /// silently masked in tests because an empty `dist-web` makes the SPA fallback 404
    /// too. Asserting a JSON error body (not just a 404 status) proves the response came
    /// from the api fallback, not the embed one, regardless of `dist-web`'s contents.
    #[tokio::test]
    async fn unregistered_api_path_returns_json_404_not_the_spa_fallback() {
        let app = app(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value =
            serde_json::from_slice(&body).expect("api fallback must return a JSON body");
        assert_eq!(json["error"], "not found");
    }
}
