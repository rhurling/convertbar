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

/// Maps a core `Err(String)` — a failure the server means, such as a missing HandBrakeCLI —
/// to the `500 {"error": ...}` shape shared by every route. A `spawn_blocking` join failure is
/// NOT one of these: it goes through [`join_err`], which adds the discriminator that tells the
/// two apart.
pub fn core_err(e: String) -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
}

/// Maps a `spawn_blocking` join failure — a panic in a blocking task, i.e. a bug in this
/// server — to a 500 a client can tell apart from [`core_err`]'s deliberate failures.
///
/// Both shapes stay 500: both are genuinely "the server could not answer", and moving
/// deliberate failures onto 4xx would be a far larger contract change than this distinction
/// needs. The `kind` field is therefore the entire discriminator, and it appears on this shape
/// only — `core_err` bodies carry `error` alone. Before it existed, the only way to tell a
/// server bug from an expected condition was to pattern-match the message text
/// (RECOMMENDATIONS item 16); this is the single definition, so a route cannot grow its own
/// divergent copy.
///
/// The panic detail stays on the wire, as it was before: the API is auth-gated by default and
/// the threat model is single-user LAN, so debuggability wins over withholding it.
///
/// A `JoinError` can in principle mean "cancelled" rather than "panicked". Nothing here aborts
/// these handles — every one is awaited inline at its own call site — so in practice this is
/// always a panic, and a runtime-shutdown cancellation would land in the same arm. Reporting
/// both as `panic` is accepted: the client's conclusion (a server bug, not a condition to
/// handle) is identical either way.
pub fn join_err(e: tokio::task::JoinError) -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": task_failure(e), "kind": "panic" })),
    )
}

/// What actually went wrong with the task, rather than the runtime's description of it.
///
/// `JoinError`'s own `Display` is `task 12 panicked with message "boom"`, where the number is a
/// tokio task counter: the same crash reads differently on every run, so two clients reporting
/// one bug produce two strings that do not group. Reaching past it to the payload also drops a
/// second "panicked" from a message [`join_err`] already labels as a panic — and the cancelled
/// arm avoids it for the same reason, rather than deferring to a `Display` that would put the id
/// back and read as "task panicked: task 12 was cancelled".
///
/// Kept identical to the desktop head's copy (`src-tauri/src/commands/error.rs`) so a panic reads
/// the same wherever the shared frontend is running. Nothing enforces that across the two crates
/// — the wording is pinned by a test on each side.
fn task_failure(join: tokio::task::JoinError) -> String {
    match join.try_into_panic() {
        // `panic!("literal")` yields a `&str`; anything formatted yields a `String`.
        Ok(payload) => {
            let detail = payload
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a non-string panic payload".to_string());
            format!("task panicked: {detail}")
        }
        // Not a panic, so the task was cancelled. It still reports `kind: "panic"` — the
        // client's conclusion (a server bug) is the same — but the message stays honest.
        Err(_) => "task was cancelled".to_string(),
    }
}

/// Runs `f` on the blocking pool and maps its three outcomes to this API's three shapes:
/// `Json` on success, [`core_err`] on a failure the server means, [`join_err`] on a panic.
///
/// Handlers call this instead of writing the `spawn_blocking` match out themselves, which is
/// what makes item 16's fix enforceable rather than merely applied. When ten handlers each
/// spelled their own join arm, nine agreed and the tenth quietly differed; a handler that
/// cannot write the arm cannot disagree with it. A handler whose blocking work produces a
/// ready `Response` rather than a `Result` uses [`blocking_response`] — between the two, no
/// route module needs to spawn anything itself.
pub async fn blocking_json<T, F>(f: F) -> axum::response::Response
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => join_err(join).into_response(),
    }
}

/// [`blocking_json`]'s sibling for blocking work that builds its own `Response` — `fs::fs_list`
/// chooses between 200/403/404/500 itself, so there is no `Result` for `blocking_json` to map.
///
/// It exists so that handler needs no exemption from the "never spawn your own" rule. It was
/// the tenth site — the one that diverged and that a `core_err` grep missed — so leaving it as
/// the one allowed exception would have left the tripwire blind to a second endpoint in exactly
/// the module where the divergence happened before.
pub async fn blocking_response<F>(f: F) -> axum::response::Response
where
    F: FnOnce() -> axum::response::Response + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(response) => response,
        Err(join) => join_err(join).into_response(),
    }
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
    /// Per-source failed-credential ramp, shared by `auth_guard` and the login route
    /// so failures at either accumulate together.
    pub login_throttle: Arc<crate::throttle::LoginThrottle>,
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
        test_state_with_locator(Arc::new(convertbar_core::handbrake::PanickingLocator))
    }

    /// Same as `test_state_with_shutdown`, but lets the caller declare whether HandBrake is
    /// installed instead of inheriting `PanickingLocator`'s fail-loud default — needed by tests
    /// that exercise HandBrake resolution itself.
    pub(crate) fn test_state_with_locator(
        locator: Arc<dyn convertbar_core::handbrake::HandbrakeLocator>,
    ) -> (ServerState, tokio::sync::watch::Sender<bool>) {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        convertbar_core::db::init_db(&conn).expect("init db");
        let ctx = Ctx::new(
            conn,
            Arc::new(TestSink::default()),
            Arc::new(RecordingDisposer::default()),
            locator,
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
            // Zero base delay so the existing suite does not sleep. Tests that
            // exercise the ramp construct their own policy.
            login_throttle: Arc::new(crate::throttle::LoginThrottle::new(
                crate::throttle::ThrottlePolicy {
                    base: std::time::Duration::ZERO,
                    ..Default::default()
                },
            )),
        };
        (state, shutdown_tx)
    }

    /// `test_state` for route tests that add files and expect success: the "HandBrake installed"
    /// world. Needs the cache seed as well as the locator — see `seed_preset_cache` for why.
    /// Without it every add would surface the metadata fetch's `Err` as a 500.
    fn test_state_installed() -> ServerState {
        let (state, _tx) = test_state_with_locator(Arc::new(
            convertbar_core::handbrake::StubLocator("/opt/fake/HandBrakeCLI".into()),
        ));
        convertbar_core::handbrake::seed_preset_cache(&state.ctx);
        state
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
    async fn join_err_reports_the_panic_detail_alongside_the_discriminator() {
        // A real `JoinError` from a real panicking blocking task — the same value every route's
        // `Err(join)` arm receives. Constructing one instead of asserting on a hand-built body
        // is what makes this a test of the mapping rather than of `json!`.
        let handle: tokio::task::JoinHandle<()> = tokio::task::spawn_blocking(|| panic!("boom"));
        let join = handle
            .await
            .expect_err("a panicking task joins as an error");

        let (status, Json(body)) = join_err(join);
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        // Pinned as a literal on purpose, same tactic as `HANDBRAKE_NOT_FOUND`'s assertion:
        // reading the discriminator back from the code that produces it would assert nothing,
        // and this value is what a client branches on.
        assert_eq!(body["kind"], "panic");
        // The detail is deliberately still on the wire (auth-gated, single-user LAN): a client
        // hitting the API directly keeps the same debuggability it had before the discriminator.
        // Pinned whole rather than by prefix: asserting the prefix alone let a mapping that
        // dropped the payload entirely pass (verified by mutation), and the whole-string form
        // additionally pins that tokio's task id is NOT here — it is a per-run counter, so
        // leaking it would give two clients two different strings for one bug.
        //
        // This exact value is also what the desktop head produces (`commands::error::blocking`),
        // so the shared frontend renders a panic identically wherever it runs. Nothing across
        // the two crates enforces that; it is pinned by this assertion and its desktop twin.
        assert_eq!(body["error"], "task panicked: boom");
    }

    #[tokio::test]
    async fn join_err_keeps_a_formatted_panic_message() {
        // `panic!("{}", x)` boxes a String where `panic!("literal")` boxes a &str, and every
        // other test here panics with a literal — so the String arm of `task_failure` was
        // unexercised on this head even after the desktop twin pinned it.
        //
        // `black_box` is load-bearing: with a literal argument rustc flattens `format_args!` to
        // one static piece, and the payload is a &str again — which is exactly how the desktop
        // version of this test passed while testing nothing.
        let reason = std::hint::black_box(String::from("something"));
        let handle: tokio::task::JoinHandle<()> =
            tokio::task::spawn_blocking(move || panic!("{reason} went wrong"));
        let join = handle
            .await
            .expect_err("a panicking task joins as an error");

        let (_, Json(body)) = join_err(join);
        assert_eq!(body["error"], "task panicked: something went wrong");
    }

    #[tokio::test]
    async fn join_err_does_not_call_a_cancelled_task_a_panic() {
        // Unreachable in this server — every handle is awaited inline — but the arm exists, and
        // deferring to `JoinError`'s Display there would ship "task panicked: task 12 was
        // cancelled": self-contradictory, and carrying the per-run id the panic arm drops.
        let handle = tokio::task::spawn(std::future::pending::<()>());
        handle.abort();
        let join = handle.await.expect_err("an aborted task joins as an error");

        let (status, Json(body)) = join_err(join);
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "task was cancelled");
        // Still a bug from the client's point of view, so the discriminator stays.
        assert_eq!(body["kind"], "panic");
    }

    /// `fs::fs_list`'s join arm, which no request can reach: `fs_list_blocking` is written not
    /// to panic (it maps every io error itself), so there is no input that produces a real
    /// `JoinError` through that route. Testing the arm where it actually lives is what
    /// `blocking_response` bought — before it existed, this site was pinned only by grepping
    /// the source for `join_err`.
    #[tokio::test]
    async fn blocking_response_reports_a_panic_with_the_same_discriminator() {
        let response = blocking_response(|| panic!("boom")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body must be readable");
        let body: Value = serde_json::from_slice(&bytes).expect("body must be JSON");
        assert_eq!(body["kind"], "panic");
        assert!(
            body["error"]
                .as_str()
                .expect("error string")
                .contains("boom"),
            "the panic detail must survive here too, got: {body}"
        );
    }

    /// RECOMMENDATIONS item 16: a `spawn_blocking` join failure is a server bug, and a client
    /// must be able to tell it apart from an expected condition without pattern-matching the
    /// message text. Both stay 500 — the status deliberately does not move, which is exactly
    /// why the `kind` discriminator has to carry the distinction.
    #[tokio::test]
    async fn a_panicked_task_and_a_deliberate_failure_are_told_apart_by_kind() {
        // `test_state()`'s PanickingLocator makes these routes panic INSIDE `spawn_blocking` —
        // real join failures at real call sites, not simulated ones. Two different route
        // modules (`handbrake.rs` and `queue.rs`), so the wiring is pinned at more than the one
        // site a single-route test would reach. The panics' own messages hit stderr while this
        // test runs; that noise is expected, not a failure.
        let (panic_status, panic_body) = request_json(
            api_router(test_state()),
            "GET",
            "/api/handbrake/detect",
            None,
        )
        .await;
        let (queue_panic_status, queue_panic_body) = request_json(
            api_router(test_state()),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/tmp/clip.mp4"]})),
        )
        .await;

        // The mirror world: a failure the server means. Same route and same request as the
        // queue panic above — only the declared world differs — so the two bodies isolate the
        // distinction itself rather than any difference between endpoints.
        let (state, _tx) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator));
        let (core_status, core_body) = request_json(
            api_router(state),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/tmp/clip.mp4"]})),
        )
        .await;

        // The premise: status alone cannot separate them. If this ever stops holding, the
        // discriminator below is no longer the only thing carrying the distinction.
        assert_eq!(panic_status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(queue_panic_status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(core_status, StatusCode::INTERNAL_SERVER_ERROR);

        assert_eq!(
            panic_body["kind"], "panic",
            "a panicked task must say so: {panic_body}"
        );
        assert_eq!(
            queue_panic_body["kind"], "panic",
            "the queue module's join arm must map the same way: {queue_panic_body}"
        );
        assert!(
            core_body.get("kind").is_none(),
            "a deliberate failure must not claim to be a server bug: {core_body}"
        );
    }

    /// Tripwire, in the spirit of `contract_test`'s `EXPECTED_ROUTE_COUNT`, pinning the thing
    /// that makes item 16 stay fixed: a handler that cannot write the join arm cannot spell it
    /// differently. Two invariants, both learned from how the tenth site came to differ.
    ///
    /// 1. No route module runs its own blocking task — `blocking_json`/`blocking_response` own
    ///    that mapping. Without this, a new handler could map its join arm through `core_err`
    ///    with fresh wording and silently reintroduce the indistinguishability, passing every
    ///    other test here (verified: that exact mutation survived the suite before this
    ///    assertion existed).
    /// 2. No route module hand-builds the panic body, which is how `fs.rs` diverged.
    ///
    /// There is no exemption. `fs::fs_list` had the only blocking work that `blocking_json`
    /// could not express, and it got `blocking_response` rather than an allowlist entry —
    /// exempting the file would have left this blind to a *second* endpoint in the very module
    /// where the divergence happened before.
    ///
    /// The walk reads the tree rather than listing modules, so a route module added later is
    /// covered without anyone remembering to add it here. It is a backstop, not a proof: it
    /// reads source text, so a blocking helper defined outside `src/routes` and merely called
    /// from a handler would slip past it. An alias declared inside a route module would not —
    /// its `use` line still names the identifier. Nothing outside `src/routes` spawns blocking
    /// work today.
    #[test]
    fn route_modules_never_map_their_own_blocking_failures() {
        // Comments are stripped before matching, so these checks read code rather than prose:
        // several modules mention `spawn_blocking` in their docs precisely to say they do not
        // call it. Stripping also means the bare identifier can be matched, which a turbofish
        // (`spawn_blocking::<_>(`) would otherwise slip past.
        fn code_only(source: &str) -> String {
            source
                .lines()
                .map(|line| match line.find("//") {
                    Some(i) => &line[..i],
                    None => line,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        // Assembled rather than written out, so this test's own source does not read as a call
        // site — `mod.rs`'s count below would otherwise include the needles searched for here.
        let spawn = concat!("spawn_", "blocking");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes");
        // Only THIS file is exempt, matched by full path rather than by name: `Path::ends_with`
        // matches a trailing component, so a later `queue/mod.rs` would silently inherit an
        // exemption meant for the file holding the helpers.
        let helpers = root.join("mod.rs");

        let mut checked = 0;
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir).expect("route module directory must be readable");
            for entry in entries {
                let path = entry.expect("readable dir entry").path();
                // Splitting a grown module into a directory is a routine refactor; a
                // non-recursive walk would drop it from coverage without saying a word.
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path == helpers {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("route module must be readable");
                let source = code_only(&source);
                let module = path.file_name().expect("named file").to_string_lossy();

                assert!(
                    !source.contains("task panicked"),
                    "{module} builds the panic body itself; go through `join_err` so every join \
                     failure keeps one definition"
                );
                assert!(
                    !source.contains(spawn),
                    "{module} runs its own blocking task; call `blocking_json` (or \
                     `blocking_response`, if the work builds its own Response) instead, so the \
                     panic-vs-deliberate-failure distinction cannot be spelled a second way"
                );
                checked += 1;
            }
        }

        // `mod.rs` is exempt because the two helpers — and the test that panics a task on
        // purpose — must spawn. Pin how many times, so a handler added to THIS file cannot
        // quietly bring its own: `api_not_found` already lives here, so handler code in
        // `mod.rs` is precedent rather than hypothesis.
        let helper_source =
            code_only(&std::fs::read_to_string(&helpers).expect("mod.rs must be readable"));
        assert_eq!(
            helper_source.matches(spawn).count(),
            4,
            "mod.rs should reach the blocking pool in exactly four places (`blocking_json`, \
             `blocking_response`, and the two tests that panic a real task — one with a literal, \
             one with a formatted String, because the payload arms differ) — a fifth means \
             something here maps a blocking failure without going through the two helpers"
        );

        // Guards the walk itself: a walk that matched nothing would pass every assertion above
        // while checking no module at all.
        assert!(
            checked >= 8,
            "expected to scan the route modules, only reached {checked} — did the walk break?"
        );
    }

    #[tokio::test]
    async fn get_queue_when_empty_returns_an_empty_array() {
        let (status, json) =
            request_json(api_router(test_state()), "GET", "/api/queue", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!([]));
    }

    #[tokio::test]
    async fn add_files_with_empty_paths_never_reaches_handbrake_resolution() {
        // `test_state()`'s PanickingLocator is the assertion: an empty add must return before it
        // reaches HandBrake resolution. Declaring an installed world here would hide a
        // regression — the route would resolve, succeed, and return this same body either way.
        // A panic inside the blocking task surfaces as a 500 carrying `"kind": "panic"`, so the
        // status assertion below catches it.
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
    async fn add_files_route_reports_the_error_when_handbrake_is_absent() {
        // The default suffix template needs HandBrake to expand. Absent, the route must return
        // the core error deliberately. Asserting the exact body (not just the status) is what
        // separates that from an accidental 500: a panicking locator would unwind inside the
        // blocking task and surface as `{"error": "task panicked: ...", "kind": "panic"}` with
        // the same 500 — a difference this body assertion sees and a status check would not.
        let (state, _tx) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator));
        let (status, json) = request_json(
            api_router(state),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/tmp/clip.mp4"]})),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            json,
            json!({"error": convertbar_core::handbrake::HANDBRAKE_NOT_FOUND})
        );
    }

    #[tokio::test]
    async fn add_files_route_queues_with_the_expanded_suffix_when_handbrake_is_installed() {
        // The mirror world. Asserting the expanded suffix (not merely a 200) is what proves the
        // template round-tripped: `.{resolution}-{codec}` against the seeded metadata.
        let (status, json) = request_json(
            api_router(test_state_installed()),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/nonexistent/clip.mp4"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["added"][0]["output_path"],
            "/nonexistent/clip.1080p-h265.mp4"
        );
    }

    #[tokio::test]
    async fn remove_job_deletes_a_seeded_queued_row() {
        let app = api_router(test_state_installed());

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
        let app = api_router(test_state_installed());

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
        // `test_state()`'s PanickingLocator is the assertion, as it is for the empty add above:
        // an empty batch must return before it resolves HandBrake. This test used to declare
        // AbsentLocator because the route resolved unconditionally — a world it named but never
        // consumed, which is precisely the tell that the resolution was pointless.
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
    async fn confirm_folder_add_on_an_empty_tempdir_never_reaches_handbrake_resolution() {
        // The scenario this fix exists for: a folder with no videos in it reaches intake with
        // zero paths. It used to need an installed HandBrake to succeed, because intake expanded
        // the suffix template before looking at `paths`. `test_state()`'s PanickingLocator now
        // asserts that it does not.
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

    // The route returns `Json(Option<String>)` — a bare JSON null or a bare JSON string, with no
    // wrapper object. These two tests replace a single smoke test that asserted only "string or
    // null", i.e. it reported whatever the host happened to have installed and could not fail.

    #[tokio::test]
    async fn detect_handbrake_reports_absent_when_handbrake_is_not_installed() {
        // Pins the CI world: 200 with a null body, not a 500.
        let (state, _tx) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator));
        let (status, json) =
            request_json(api_router(state), "GET", "/api/handbrake/detect", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            json.is_null(),
            "absent HandBrake must report null, got {json:?}"
        );
    }

    #[tokio::test]
    async fn detect_handbrake_reports_the_located_path_when_handbrake_is_installed() {
        let (state, _tx) = test_state_with_locator(Arc::new(
            convertbar_core::handbrake::StubLocator("/opt/fake/HandBrakeCLI".into()),
        ));
        let (status, json) =
            request_json(api_router(state), "GET", "/api/handbrake/detect", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.as_str(), Some("/opt/fake/HandBrakeCLI"));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn get_api_info_advertises_roots_in_the_namespace_listings_use() {
        // `/api/fs/list` answers in canonical paths (it has to — containment is checked after
        // canonicalization). A root advertised as the raw config string is then a prefix of
        // nothing the client will ever be shown: with CONVERTBAR_BROWSE_ROOTS=/media symlinked
        // to /mnt/pool/media, the picker holds root "/media" and entries "/mnt/pool/media/...",
        // and every path-relative calculation it makes from the pair is nonsense.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("create symlink");

        let mut state = test_state();
        let mut config = (*state.config).clone();
        config.browse_roots = vec![link.clone()];
        state.config = std::sync::Arc::new(config);

        let response = api_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        let canonical = std::fs::canonicalize(&real).unwrap();
        assert_eq!(json["browse_roots"], json!([canonical.to_string_lossy()]));
    }

    #[tokio::test]
    async fn get_api_info_still_advertises_a_root_that_does_not_resolve_yet() {
        // A root can legitimately not exist at startup — a NAS mount that attaches later (see
        // `routes::fs`). Canonicalizing must not drop it from the list, or the picker offers
        // the user nothing at all until the mount appears and the server is restarted.
        let mut state = test_state();
        let mut config = (*state.config).clone();
        config.browse_roots = vec![std::path::PathBuf::from("/nope-not-mounted-yet")];
        state.config = std::sync::Arc::new(config);

        let response = api_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["browse_roots"], json!(["/nope-not-mounted-yet"]));
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

    /// The one line `oneshot` cannot cover. Goes through `startup::serve` — the
    /// SAME function `main.rs` calls — rather than a duplicated `axum::serve(...)`
    /// call, so a regression that drops `into_make_service_with_connect_info` from
    /// the production wiring is actually caught here instead of only in a copy of
    /// it. Without that wiring there is no `ConnectInfo`, so `client_id` cannot
    /// recognise 127.0.0.1 as a trusted proxy, never reads `X-Forwarded-For`, and
    /// every client collapses into one shared bucket.
    #[tokio::test]
    async fn served_requests_are_bucketed_per_forwarded_client() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut state, _shutdown_tx) = test_state_with_shutdown();
        // Same code path production uses for CONVERTBAR_TRUSTED_PROXIES, rather than
        // duplicating IpNet parsing here (a bare "127.0.0.1" has no prefix to parse).
        state.config = Arc::new(
            ServerConfig::from_vars(
                &[
                    (
                        "CONVERTBAR_AUTH_TOKEN".to_string(),
                        "abcdefghijklmnop".to_string(),
                    ),
                    (
                        "CONVERTBAR_TRUSTED_PROXIES".to_string(),
                        "127.0.0.1".to_string(),
                    ),
                ]
                .into(),
            )
            .expect("valid test config"),
        );
        // `free: 0` and a long base close the gate after one failure and keep it
        // shut — the test asserts on status alone and needs no clock control.
        state.login_throttle = Arc::new(crate::throttle::LoginThrottle::new(
            crate::throttle::ThrottlePolicy {
                free: 0,
                base: std::time::Duration::from_secs(3600),
                ..Default::default()
            },
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            crate::startup::serve(listener, app(state), std::future::pending()).await
        });

        async fn request_from(addr: std::net::SocketAddr, forwarded: &str, token: &str) -> String {
            let request = format!(
                "GET /api/queue HTTP/1.1\r\nHost: localhost\r\n\
                 Authorization: Bearer {token}\r\nX-Forwarded-For: {forwarded}\r\n\
                 Connection: close\r\n\r\n"
            );
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }

        // Close the first forwarded client's gate with one wrong-credential attempt.
        let response = request_from(addr, "203.0.113.1", "wrong").await;
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "unexpected: {response}"
        );

        // The gate is shut, so even the CORRECT token is refused for this source.
        let response = request_from(addr, "203.0.113.1", "abcdefghijklmnop").await;
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "gate was open, so the same source's correct token was still evaluated: {response}"
        );

        // A DIFFERENT forwarded client is a separate bucket: its correct token is
        // still evaluated and succeeds. Without connect info both collapse into
        // the Unknown bucket and this would also be refused — the discrimination
        // this test exists to prove.
        let response = request_from(addr, "203.0.113.2", "abcdefghijklmnop").await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "second forwarded client inherited the first's shut gate: {response}"
        );
    }
}
