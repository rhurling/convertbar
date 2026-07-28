use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::config::ServerConfig;

#[cfg(test)]
mod contract_test;

pub mod converter;
pub mod events;
pub mod fs;
pub mod handbrake;
pub mod history;
pub mod info;
pub mod login;
pub mod queue;
pub mod settings;
pub mod watch;

/// Maps a core `Err(String)` to the `500 {"error": ...}` shape shared by every route.
pub fn core_err(e: String) -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
}

/// State threaded through every axum handler. `events_tx` is fed by `ServerSink` (the
/// `EventSink` the converter emits through) and consumed fresh per-request by the SSE
/// route. `shutdown_rx` flips to `true` when the server begins graceful shutdown; the
/// SSE route watches it so an open `/api/events` connection doesn't block the drain
/// forever (the sender lives in `main.rs`, wired up in a later task).
#[derive(Clone)]
pub struct ServerState {
    pub ctx: Arc<convertbar_core::ctx::Ctx>,
    pub config: Arc<ServerConfig>,
    pub events_tx: broadcast::Sender<(String, Value)>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

/// Nests all `/api` routes; the caller (`main.rs`) adds the static/embed fallback.
pub fn api_router(state: ServerState) -> Router {
    // A nested router with no `.fallback()` of its own inherits the OUTER router's
    // fallback (axum 0.8 documented behavior) — without this, an unmatched `/api/*`
    // path would fall through to the SPA embed handler and could serve `index.html`
    // with 200 instead of a 404.
    let api = Router::new()
        .route("/info", get(info::get_app_info))
        .route("/login", post(login::login))
        .route("/events", get(events::sse_handler))
        .route("/queue/files", post(queue::add_files))
        .route("/folders/scan", post(queue::scan_folder))
        .route("/queue/folder", post(queue::confirm_folder_add))
        .route("/paths/classify", post(queue::classify_paths))
        .route("/queue", get(queue::get_queue).delete(queue::clear_queue))
        .route("/queue/jobs/{id}", delete(queue::remove_job))
        .route("/queue/order", put(queue::reorder_queue))
        .route("/bad-sources", get(queue::get_bad_sources))
        .route("/bad-sources/purge", post(queue::purge_bad_sources))
        .route("/history", get(history::get_history))
        .route("/history/summary", get(history::get_history_summary))
        .route("/history/{id}", delete(history::remove_history_entry))
        .route("/history/clear", post(history::clear_completed))
        .route("/converter/start", post(converter::start_queue))
        .route("/converter/pause", post(converter::pause_conversion))
        .route("/converter/resume", post(converter::resume_conversion))
        .route("/converter/cancel", post(converter::cancel_conversion))
        .route(
            "/converter/pause-after-current",
            post(converter::pause_after_current)
                .delete(converter::cancel_pause_after_current)
                .get(converter::get_pause_after_current),
        )
        .route(
            "/converter/low-disk-pause",
            get(converter::get_low_disk_pause),
        )
        .route("/settings", get(settings::get_settings))
        .route("/settings/{key}", put(settings::update_setting))
        .route(
            "/presets/{preset}/suffix",
            get(settings::get_preset_suffix).put(settings::set_preset_suffix),
        )
        .route(
            "/presets/{preset}/suffix/generate",
            post(handbrake::generate_preset_suffix),
        )
        .route("/suffix/resolve", post(handbrake::resolve_suffix_template))
        .route("/handbrake/detect", get(handbrake::detect_handbrake))
        .route("/handbrake/presets", get(handbrake::list_handbrake_presets))
        .route("/handbrake/validate", get(handbrake::validate_handbrake))
        .route(
            "/watched",
            get(watch::get_watched_directories).post(watch::add_watched_directory),
        )
        .route(
            "/watched/{id}",
            put(watch::update_watched_directory).delete(watch::remove_watched_directory),
        )
        .route(
            "/watched/{id}/enabled",
            put(watch::set_watched_directory_enabled),
        )
        .route("/fs/list", get(fs::fs_list))
        .fallback(api_not_found);

    Router::new().nest("/api", api).with_state(state)
}

async fn api_not_found() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({"error": "not found"})),
    )
}

/// The full app: `api_router` plus the embedded SPA fallback, wrapped in the security guard
/// stack — exactly as `main.rs` serves it. Factored out so tests can exercise the real
/// composition (nest + outer fallback + guards together) rather than `api_router` alone.
///
/// Guard layering order (outermost first): `host_guard` (always on, anti DNS-rebinding) ->
/// `auth_guard` (bearer/cookie token check) -> `json_content_guard` (CSRF belt) -> routes.
/// Each `.layer()` call below wraps everything added before it, so the LAST layer added ends
/// up OUTERMOST — hence `host_guard` is added last. See `auth.rs` for each guard's contract.
pub fn app(state: ServerState) -> Router {
    api_router(state.clone())
        .fallback(crate::embed::fallback)
        .layer(axum::middleware::from_fn(crate::auth::json_content_guard))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::auth_guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::host_guard,
        ))
}

// `pub(crate)` so sibling route modules' own `#[cfg(test)]` submodules (e.g.
// `routes::events::tests`) can reach the shared helpers below via
// `crate::routes::tests::test_state()`.
#[cfg(test)]
pub(crate) mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use rusqlite::Connection;
    use tower::ServiceExt;

    use convertbar_core::ctx::Ctx;
    use convertbar_core::dispose::RecordingDisposer;
    use convertbar_core::events::TestSink;

    use super::*;

    /// Shared temp-db `ServerState` for every route module's integration tests. Drops its
    /// own shutdown sender immediately, which is fine for anything that doesn't touch
    /// `/api/events` — see `test_state_with_shutdown` for tests that need to control it.
    pub(crate) fn test_state() -> ServerState {
        test_state_with_shutdown().0
    }

    /// Same as `test_state`, but also returns the shutdown watch's sender. Needed by
    /// `routes::events`'s tests, which flip the shutdown flag mid-test — dropping the
    /// sender immediately (as `test_state` does) would flip `wait_for_shutdown` true on
    /// first poll and end an SSE stream before any assertions ran.
    pub(crate) fn test_state_with_shutdown() -> (ServerState, tokio::sync::watch::Sender<bool>) {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        convertbar_core::db::init_db(&conn).expect("init db");
        let ctx = Ctx::new(
            conn,
            Arc::new(TestSink::default()),
            Arc::new(RecordingDisposer::default()),
        );
        let (events_tx, _rx) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let state = ServerState {
            ctx,
            config: Arc::new(
                ServerConfig::from_vars(
                    &[("CONVERTBAR_NO_AUTH".to_string(), "1".to_string())].into(),
                )
                .expect("valid test config"),
            ),
            events_tx,
            shutdown_rx,
        };
        (state, shutdown_tx)
    }

    /// Sends `body` as a JSON request to `method`/`uri` against `app`, returning the
    /// decoded response status and (if any) JSON body — `null` for an empty (e.g. 204)
    /// body. Shared by every route test below to keep the request/response boilerplate
    /// out of each individual test.
    async fn request_json(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        let request_body = match body {
            Some(v) => {
                builder = builder.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .oneshot(builder.body(request_body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("response body must be valid JSON")
        };
        (status, json)
    }

    #[tokio::test]
    async fn get_queue_when_empty_returns_an_empty_array() {
        let (status, json) =
            request_json(api_router(test_state()), "GET", "/api/queue", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!([]));
    }

    #[tokio::test]
    async fn add_files_with_empty_paths_returns_empty_added_and_skipped() {
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"added": [], "skipped": []}));
    }

    #[tokio::test]
    async fn remove_job_deletes_a_seeded_queued_row() {
        let app = api_router(test_state());

        // Seed via the real add_files route rather than raw SQL: a fake .mp4 path is
        // enough to insert a 'queued' row (add_files_inner only checks the extension and
        // stats the file for size — a missing file just yields a null original_size).
        let (_, added) = request_json(
            app.clone(),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/nonexistent/seed-video.mp4"]})),
        )
        .await;
        let id = added["added"][0]["id"]
            .as_str()
            .expect("seeded job id")
            .to_string();

        let (status, _) = request_json(
            app.clone(),
            "DELETE",
            &format!("/api/queue/jobs/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, queue) = request_json(app, "GET", "/api/queue", None).await;
        assert_eq!(queue, json!([]), "removed job must be gone from the queue");
    }

    #[tokio::test]
    async fn reorder_queue_accepts_camelcase_job_ids() {
        let app = api_router(test_state());

        let (_, added_a) = request_json(
            app.clone(),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/nonexistent/a.mp4"]})),
        )
        .await;
        let (_, added_b) = request_json(
            app.clone(),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/nonexistent/b.mp4"]})),
        )
        .await;
        let id_a = added_a["added"][0]["id"].as_str().unwrap().to_string();
        let id_b = added_b["added"][0]["id"].as_str().unwrap().to_string();

        // "jobIds" (camelCase) must deserialize into the request struct's `job_ids` field.
        let (status, _) = request_json(
            app.clone(),
            "PUT",
            "/api/queue/order",
            Some(json!({"jobIds": [id_b.clone(), id_a.clone()]})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, queue) = request_json(app, "GET", "/api/queue", None).await;
        assert_eq!(queue[0]["id"], id_b, "b must now sort first");
        assert_eq!(queue[1]["id"], id_a, "a must now sort second");
    }

    #[tokio::test]
    async fn get_history_with_limit_and_offset_returns_an_empty_page() {
        let (status, json) = request_json(
            api_router(test_state()),
            "GET",
            "/api/history?limit=10&offset=0",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"jobs": [], "total": 0}));
    }

    #[tokio::test]
    async fn get_history_summary_when_empty_returns_zeroed_totals() {
        let (status, json) = request_json(
            api_router(test_state()),
            "GET",
            "/api/history/summary",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"total_saved_bytes": 0, "total_files": 0}));
    }

    #[tokio::test]
    async fn remove_history_entry_on_a_nonexistent_id_is_a_no_op_204() {
        // remove_history_entry's DELETE is unconditional on row existence (matches
        // remove_job); a nonexistent id simply deletes zero rows and still returns Ok(()).
        let (status, _) = request_json(
            api_router(test_state()),
            "DELETE",
            "/api/history/does-not-exist",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn clear_completed_and_clear_queue_return_204() {
        let app = api_router(test_state());
        let (status, _) = request_json(
            app.clone(),
            "POST",
            "/api/history/clear",
            Some(json!({"mode": "all"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = request_json(app, "DELETE", "/api/queue", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn get_bad_sources_when_empty_returns_an_empty_array() {
        let (status, json) =
            request_json(api_router(test_state()), "GET", "/api/bad-sources", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!([]));
    }

    #[tokio::test]
    async fn purge_bad_sources_with_no_ids_returns_an_empty_array() {
        // ids=[] never touches HandBrake resolution's outcome (the map iterator is empty),
        // so this is deterministic regardless of whether HandBrakeCLI is on the test host.
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/bad-sources/purge",
            Some(json!({"ids": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!([]));
    }

    #[tokio::test]
    async fn classify_paths_with_no_paths_returns_empty_files_and_folders() {
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/paths/classify",
            Some(json!({"paths": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"files": [], "folders": []}));
    }

    #[tokio::test]
    async fn confirm_folder_add_on_an_empty_tempdir_adds_nothing() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/queue/folder",
            Some(json!({"path": dir.path().to_str().unwrap()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"added": [], "skipped": []}));
    }

    #[tokio::test]
    async fn scan_folder_on_a_nonexistent_path_maps_the_core_error_to_500() {
        // scan_folder deterministically errors on any non-directory path, unlike
        // clear_completed (any mode string other than "errors" is treated as "all" and
        // never errors) — chosen as the core-error-mapping proof for exactly that reason.
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/folders/scan",
            Some(json!({"path": "/definitely/does/not/exist-convertbar"})),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json, json!({"error": "Path is not a directory"}));
    }

    #[tokio::test]
    async fn settings_round_trip_persists_and_reads_back_a_value() {
        let app = api_router(test_state());

        let (status, _) = request_json(
            app.clone(),
            "PUT",
            "/api/settings/preset",
            Some(json!({"value": "My Custom Preset"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) = request_json(app, "GET", "/api/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["preset"], "My Custom Preset");
    }

    #[tokio::test]
    async fn update_setting_with_an_invalid_key_returns_the_core_error_message() {
        // settings_ops::update_setting rejects anything outside ALLOWED_KEYS with this exact
        // message; the route must surface it verbatim in the 500 body, not a generic string.
        let (status, json) = request_json(
            api_router(test_state()),
            "PUT",
            "/api/settings/not_a_real_key",
            Some(json!({"value": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            json,
            json!({"error": "Invalid setting key: not_a_real_key"})
        );
    }

    #[tokio::test]
    async fn pause_after_current_lifecycle_flips_the_flag() {
        let app = api_router(test_state());

        let (status, _) = request_json(
            app.clone(),
            "POST",
            "/api/converter/pause-after-current",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) = request_json(
            app.clone(),
            "GET",
            "/api/converter/pause-after-current",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!(true));

        let (status, _) = request_json(
            app.clone(),
            "DELETE",
            "/api/converter/pause-after-current",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) =
            request_json(app, "GET", "/api/converter/pause-after-current", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!(false));
    }

    #[tokio::test]
    async fn watched_directory_crud_round_trips_on_a_tempdir() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let app = api_router(test_state());

        let (status, added) = request_json(
            app.clone(),
            "POST",
            "/api/watched",
            Some(json!({
                "path": dir.path().to_str().unwrap(),
                "recursive": false,
                "stabilityDelaySecs": 5,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(added["recursive"], false);
        assert_eq!(added["stability_delay_secs"], 5);
        assert_eq!(added["enabled"], true);
        let id = added["id"].as_str().expect("watched dir id").to_string();

        let (status, list) = request_json(app.clone(), "GET", "/api/watched", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);

        let (status, _) = request_json(
            app.clone(),
            "PUT",
            &format!("/api/watched/{id}"),
            Some(json!({"recursive": true, "stabilityDelaySecs": 10})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = request_json(
            app.clone(),
            "PUT",
            &format!("/api/watched/{id}/enabled"),
            Some(json!({"enabled": false})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, list) = request_json(app.clone(), "GET", "/api/watched", None).await;
        assert_eq!(list[0]["recursive"], true, "update must have persisted");
        assert_eq!(
            list[0]["enabled"], false,
            "enabled toggle must have persisted"
        );

        let (status, _) =
            request_json(app.clone(), "DELETE", &format!("/api/watched/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, list) = request_json(app, "GET", "/api/watched", None).await;
        assert_eq!(list, json!([]), "removed directory must be gone");
    }

    #[tokio::test]
    async fn preset_suffix_round_trip_persists_and_reads_back() {
        let app = api_router(test_state());

        let (status, _) = request_json(
            app.clone(),
            "PUT",
            "/api/presets/My%20Preset/suffix",
            Some(json!({"suffix": ".custom"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, json) =
            request_json(app, "GET", "/api/presets/My%20Preset/suffix", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!(".custom"));
    }

    #[tokio::test]
    async fn detect_handbrake_smoke_returns_200_with_valid_json() {
        // CI has no HandBrakeCLI, but the test host might: assert only status + shape, never
        // the specific value (a real path vs null both satisfy Option<String>'s JSON encoding).
        let (status, json) = request_json(
            api_router(test_state()),
            "GET",
            "/api/handbrake/detect",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.is_string() || json.is_null());
    }

    #[tokio::test]
    async fn get_api_info_returns_the_five_fields() {
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
        // test_state() sets no CONVERTBAR_BROWSE_ROOTS, so ServerConfig defaults to ["/"].
        assert_eq!(json["browse_roots"], json!(["/"]));
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
                // host_guard (Task 8) is now ALWAYS on, even in this test's Open auth mode —
                // a Host header is required for the request to get past it at all.
                Request::builder()
                    .uri("/api/does-not-exist")
                    .header("Host", "localhost")
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
