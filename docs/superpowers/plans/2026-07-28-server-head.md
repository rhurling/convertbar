# Server Head (convertbar-server) — Implementation Plan (Plan 2 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the headless, Docker-deployable ConvertBar: an axum HTTP+SSE server over the existing `convertbar-core` engine, the same React frontend in a `VITE_HEAD=server` build, a CPU-only Docker image, and GHCR publishing on release tags.

**Architecture:** `crates/convertbar-server` is the second head on core — it constructs `Ctx` with a `ServerSink` (tokio broadcast → SSE) and `DeleteDisposer`, replays the desktop shell's startup sequence, and maps ~30 routes 1:1 onto `queue_ops`/`control`/`settings_ops`/`handbrake`/watcher functions. The frontend splits its one-file transport into tauri/http implementations selected at build time; `src/lib/events.ts` (the seam built in Plan 1) swaps its internals for one shared `EventSource`. Spec: `docs/superpowers/specs/2026-07-28-docker-web-ui-design.md`.

**Tech Stack:** axum 0.8, tokio 1, tower-http 0.6, rust-embed 8, axum-extra (cookie), constant_time_eq; React 19 + Vite (existing); Docker multi-stage on Debian stable slim + apt `handbrake-cli`; GHCR via GitHub Actions.

## Global Constraints

- **The core engine does not change semantically in this plan.** Task 1's cleanup touches visibility, names, comments, and adds tests — never behavior. Everything after Task 1 only *consumes* core.
- **Emit-under-lock invariant:** no code may hold `ctx.db` (or any core mutex) across `ctx.events.emit_t`/`notify`. `ServerSink::emit` must not acquire locks that route handlers hold.
- **Event names stay string literals** at emit and listen sites; the contract tests grep for them.
- **spawn_blocking discipline:** every handler that probes files or scans folders (`add_files`, `scan_folder`, `confirm_folder_add`, `classify_paths`, `purge_bad_sources`) runs the core call inside `tokio::task::spawn_blocking` — same rule the desktop's async commands follow. Simple DB reads/mutations may run inline (they're the same short-lock calls sync Tauri commands make today).
- **HTTP JSON contract:** request bodies use the **camelCase** keys the existing transport already sends (`jobIds`, `sortBy`, `stabilityDelaySecs`) via `#[serde(rename_all = "camelCase")]` on request structs; responses are core's serde types verbatim (snake_case fields — the frontend types already match). Route errors: core `Err(String)` → HTTP 500 with body `{"error": "<string>"}`; malformed bodies use axum's built-in 400/422.
- **Auth defaults (spec-settled):** startup requires `CONVERTBAR_AUTH_TOKEN` or explicit `CONVERTBAR_NO_AUTH=1`, else refuse to start. Cookie `convertbar_token`: HttpOnly, SameSite=Strict, Path=/, no Secure (plain-HTTP LAN). All token comparisons constant-time. Host-header validation always on (421 on failure), even in no-auth mode.
- **Every task ends green:** `cargo test --workspace` AND `npm test` pass before the task's commit. Suites at task start: 247 pass + 4 ignored (Rust: 8 src-tauri + 239 core), 142 frontend.
- **Desktop behavior unchanged** by the frontend split: the desktop build keeps identical transport behavior; `npm test` must pass with no test rewrites except where a test file imports moved paths.
- Commits: conventional (`feat:`/`refactor:`/`test:`/`ci:`/`docs:`), per task as specified. GPG signing via 1Password — if `git commit` fails with a 1Password agent error: retry once, never commit unsigned, report BLOCKED (the user unlocks and you retry).
- Work happens in the worktree `.claude/worktrees/server-head-plan2`, branch `feature/server-head`. Use worktree-absolute paths.
- GitHub Actions in this repo are SHA-pinned: any new action reference uses the action's current release commit SHA (resolve at implementation time with `gh api repos/<owner>/<repo>/git/ref/tags/<tag>`), never a floating tag.
- `main` is protected; nothing in this plan pushes. Release-tag wiring is exercised only after merge.

## The route table (single source of truth)

`crates/convertbar-server/routes.json` is checked in and consumed by BOTH contract tests (Rust: table ↔ registered router; TS: http transport ↔ table). Format: array of `{"command": str, "method": str, "path": str}`. Full contents (from the spec, camelCase commands matching the transport):

```json
[
  {"command": "add_files",                  "method": "POST",   "path": "/api/queue/files"},
  {"command": "scan_folder",                "method": "POST",   "path": "/api/folders/scan"},
  {"command": "confirm_folder_add",         "method": "POST",   "path": "/api/queue/folder"},
  {"command": "classify_paths",             "method": "POST",   "path": "/api/paths/classify"},
  {"command": "get_queue",                  "method": "GET",    "path": "/api/queue"},
  {"command": "remove_job",                 "method": "DELETE", "path": "/api/queue/jobs/{id}"},
  {"command": "reorder_queue",              "method": "PUT",    "path": "/api/queue/order"},
  {"command": "clear_queue",                "method": "DELETE", "path": "/api/queue"},
  {"command": "start_queue",                "method": "POST",   "path": "/api/converter/start"},
  {"command": "pause_conversion",           "method": "POST",   "path": "/api/converter/pause"},
  {"command": "resume_conversion",          "method": "POST",   "path": "/api/converter/resume"},
  {"command": "cancel_conversion",          "method": "POST",   "path": "/api/converter/cancel"},
  {"command": "pause_after_current",        "method": "POST",   "path": "/api/converter/pause-after-current"},
  {"command": "cancel_pause_after_current", "method": "DELETE", "path": "/api/converter/pause-after-current"},
  {"command": "get_pause_after_current",    "method": "GET",    "path": "/api/converter/pause-after-current"},
  {"command": "get_low_disk_pause",         "method": "GET",    "path": "/api/converter/low-disk-pause"},
  {"command": "get_history",                "method": "GET",    "path": "/api/history"},
  {"command": "get_history_summary",        "method": "GET",    "path": "/api/history/summary"},
  {"command": "remove_history_entry",       "method": "DELETE", "path": "/api/history/{id}"},
  {"command": "clear_completed",            "method": "POST",   "path": "/api/history/clear"},
  {"command": "get_bad_sources",            "method": "GET",    "path": "/api/bad-sources"},
  {"command": "purge_bad_sources",          "method": "POST",   "path": "/api/bad-sources/purge"},
  {"command": "get_settings",               "method": "GET",    "path": "/api/settings"},
  {"command": "update_setting",             "method": "PUT",    "path": "/api/settings/{key}"},
  {"command": "get_preset_suffix",          "method": "GET",    "path": "/api/presets/{preset}/suffix"},
  {"command": "set_preset_suffix",          "method": "PUT",    "path": "/api/presets/{preset}/suffix"},
  {"command": "generate_preset_suffix",     "method": "POST",   "path": "/api/presets/{preset}/suffix/generate"},
  {"command": "resolve_suffix_template",    "method": "POST",   "path": "/api/suffix/resolve"},
  {"command": "detect_handbrake",           "method": "GET",    "path": "/api/handbrake/detect"},
  {"command": "list_handbrake_presets",     "method": "GET",    "path": "/api/handbrake/presets"},
  {"command": "validate_handbrake",         "method": "GET",    "path": "/api/handbrake/validate"},
  {"command": "get_watched_directories",    "method": "GET",    "path": "/api/watched"},
  {"command": "add_watched_directory",      "method": "POST",   "path": "/api/watched"},
  {"command": "update_watched_directory",   "method": "PUT",    "path": "/api/watched/{id}"},
  {"command": "set_watched_directory_enabled", "method": "PUT", "path": "/api/watched/{id}/enabled"},
  {"command": "remove_watched_directory",   "method": "DELETE", "path": "/api/watched/{id}"},
  {"command": "fs_list",                    "method": "GET",    "path": "/api/fs/list"},
  {"command": "get_app_info",               "method": "GET",    "path": "/api/info"},
  {"command": "login",                      "method": "POST",   "path": "/api/login"}
]
```

Not present (desktop-only, the server UI never calls them): `check_paths_exist`, `open_path`, `reveal_in_dir`, `quit_app`, `hide_window`, `pick_folder`, `get_platform_capabilities` (subsumed by `get_app_info`), updater APIs. `GET /api/events` (SSE) is transport, not a command — it is asserted separately (every `listen()` event name ↔ core emit names).

---

### Task 1: Pre-Plan-2 core cleanup batch

The deferred follow-ups from Plan 1's reviews, done before a second consumer exists. Visibility/naming/comments/tests plus verbatim cross-crate moves — zero behavior change.

**Files:**
- Create: `crates/convertbar-core/src/watch_ops.rs`
- Modify: `crates/convertbar-core/src/converter.rs`, `crates/convertbar-core/src/control.rs`, `crates/convertbar-core/src/queue_ops.rs`, `crates/convertbar-core/src/failure_class.rs`, `crates/convertbar-core/src/settings_ops.rs`, `crates/convertbar-core/src/handbrake.rs`, `crates/convertbar-core/src/lib.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/commands/settings.rs`, `src-tauri/src/commands/watch.rs`, `src-tauri/src/commands/handbrake.rs`

**Interfaces:**
- Produces: `ConverterState::is_running(&self) -> bool` accessor (for src-tauri's tray handlers); `settings_ops::set_preset_suffix(ctx: &Ctx, preset: &str, suffix: &str) -> Result<(), String>` (the server routes need it in core); narrowed core surface — the *intended* cross-crate API is: `queue_ops::*` (the Task-5 signatures from Plan 1), `control::*`, `settings_ops::*`, `watcher::{start, reconcile, refresh_skip_marker}`, `handbrake::{resolve_handbrake_path, cached_preset_metadata, resolve_suffix_template, detect_handbrake_path, handbrake_version}` (last one moved in Step 6), `watch_ops::*` (moved in Step 5), `converter::{run_queue, recover_interrupted_jobs, is_queue_paused, should_auto_resume, kill_active_child, ConverterState (opaque), MenuBarUpdate, LowDiskPause, ConversionProgress}`, `types::*`, `db::*`, `events::*`, `ctx::Ctx`, `dispose::*`, `add_progress::AddOp`.

- [ ] **Step 1: Visibility narrowing (compiler-driven).** Add the `is_running()` accessor to `ConverterState`; switch `src-tauri/src/lib.rs`'s two tray-handler reads (`ctx.converter.is_running.lock()`) to it. Then narrow `ConverterState`'s raw fields, `PurgeAction`, and watcher/queue internal helpers from `pub` to `pub(crate)`; run `cargo check --workspace`; restore `pub` ONLY on items the compiler proves have cross-crate consumers (list each survivor + its consumer in a code comment is NOT needed — just report them). Expected survivors: the intended-API list above.
- [ ] **Step 2: Renames/comments.** `queue_ops.rs`'s private `resolve_handbrake_path` (~:98) → `resolve_from_configured` (fix its ~2 intra-file callers). Delete the vestigial `#[cfg_attr(not(unix), allow(unused_variables))]` at `control.rs:36,107`. Trim the duplicated "clears any remembered pause" sentence at `control.rs:17-19` to one line. Fix `failure_class.rs:166`'s stale "callers below" pointer (they live in `converter.rs` now).
- [ ] **Step 3: New tests (TDD each: write → red where achievable → green).**

```rust
// queue_ops.rs tests — pin the RAII bracketing Plan 1 preserved by hand:
#[test]
fn add_files_emits_finished_before_queue_updated() {
    let (ctx, sink, _d) = test_ctx(test_conn());
    // an empty add still brackets: add-started → add-finished → queue-updated
    let _ = queue_ops::add_files(&ctx, &[]);
    let names: Vec<String> = sink.events.lock().unwrap().iter().map(|(n, _)| n.clone()).collect();
    let fin = names.iter().position(|n| n == "add-finished").expect("add-finished emitted");
    let upd = names.iter().position(|n| n == "queue-updated").expect("queue-updated emitted");
    assert!(fin < upd, "spinner must clear before the queue refetch signal");
}
```

```rust
// converter.rs tests — the TrashSourceThenRename arm, now cheap with RecordingDisposer:
#[test]
fn in_place_trash_mode_disposes_source_then_renames_temp() {
    // build a temp source + temp file, call apply_in_place_action with cleanup_mode "trash"
    // via the existing decision fn, assert: disposer recorded the SOURCE path, temp renamed
    // over source, file content is the temp's. Follow the existing in_place_action tests'
    // setup helpers in this file.
}
```

Positive notify coverage: extend one existing successful-fake-encode test (they use the scripted fake HandBrake) to set `notifications_per_file=true` and assert `sink.notifications` contains an entry whose body contains the file name — or add a sibling test if extension muddies the original's intent.
`apply_in_place_action`'s `None` arm (non-UTF-8 path): add the pinning comment + `debug_assert!(false, "source paths come from the DB as UTF-8 Strings")`.

- [ ] **Step 4: Move `set_preset_suffix`'s write body to core** (`settings_ops::set_preset_suffix(ctx, preset, suffix)` — the INSERT..ON CONFLICT currently inline in `src-tauri/src/commands/settings.rs:49-61`); the desktop wrapper delegates. Move/adapt any covering test.
- [ ] **Step 5: Move the watched-dir CRUD bodies to core** (adversarial-review-mandated: core exports NO watched-dir CRUD today — the five implementations live inline in `src-tauri/src/commands/watch.rs:12-163` with raw SQL, `canonical_watch_path` (dunce), uuid/chrono, the 1-second delay floor, and UNIQUE-error mapping; Task 6's server routes need them callable). Create `crates/convertbar-core/src/watch_ops.rs` with:

```rust
pub fn get_watched_directories(ctx: &Ctx) -> Result<Vec<WatchedDirectory>, String>
pub fn add_watched_directory(ctx: &Arc<Ctx>, path: &str, recursive: bool, stability_delay_secs: i64) -> Result<WatchedDirectory, String>
pub fn update_watched_directory(ctx: &Arc<Ctx>, id: &str, recursive: bool, stability_delay_secs: i64) -> Result<(), String>
pub fn set_watched_directory_enabled(ctx: &Arc<Ctx>, id: &str, enabled: bool) -> Result<(), String>
pub fn remove_watched_directory(ctx: &Arc<Ctx>, id: &str) -> Result<(), String>
```

Bodies move VERBATIM (including `canonical_watch_path` + its tests and the `watcher::reconcile`/`scan_existing_background` calls, which take the ctx already threaded); `commands/watch.rs` shrinks to thin wrappers + `pick_folder`. Core's Cargo.toml already has uuid/chrono/dunce/rusqlite — no new deps. (`&Arc<Ctx>` where the body calls reconcile/scan which need the Arc; match the actual call needs when moving.)
- [ ] **Step 6: Move `handbrake_version` + `VERSION_CHECK_TIMEOUT` to core** (`src-tauri/src/commands/handbrake.rs:90-107` — the private version-probe with its `probe::wait_with_timeout` timeout discipline; Task 6's `validate_handbrake` route needs it). New `pub fn handbrake_version(hb_path: &str) -> Option<String>` (exact current body — it is `Option` plumbing via `.ok()?`/`wait_with_timeout`, and rewriting it behind a `Result` would force semantic error-path decisions the zero-behavior-change rule forbids) in `crates/convertbar-core/src/handbrake.rs`; the desktop wrapper delegates (`.unwrap_or_default()` behavior unchanged).
- [ ] **Step 7: Suites + commit.** `cargo test --workspace && npm test` → green (247+new tests). One commit: `refactor: narrow core API surface and close Plan 1 review deferrals`.

---

### Task 2: Server crate scaffold — config, embed, /api/info

**Files:**
- Create: `crates/convertbar-server/Cargo.toml`, `crates/convertbar-server/build.rs`, `crates/convertbar-server/src/main.rs`, `crates/convertbar-server/src/config.rs`, `crates/convertbar-server/src/embed.rs`, `crates/convertbar-server/src/routes/mod.rs`, `crates/convertbar-server/src/routes/info.rs`, `crates/convertbar-server/routes.json` (seed: only `get_app_info` + `login` rows for now — grows with each route task)
- Modify: root `Cargo.toml` (members)

**Interfaces:**
- Produces:

```rust
// config.rs — pure, fully unit-tested
pub struct ServerConfig {
    pub bind: std::net::SocketAddr,          // CONVERTBAR_BIND (default "0.0.0.0") + CONVERTBAR_PORT (default 8080)
    pub auth: AuthMode,                      // Token(String) | Open  — from CONVERTBAR_AUTH_TOKEN / CONVERTBAR_NO_AUTH
    pub allowed_hosts: Vec<String>,          // CONVERTBAR_ALLOWED_HOSTS, comma-separated ONLY (colon would mangle host:port / IPv6)
    pub browse_roots: Vec<std::path::PathBuf>, // CONVERTBAR_BROWSE_ROOTS, colon-separated; empty = ["/"]
}
pub enum AuthMode { Token(String), Open }
pub enum ConfigError { MissingAuth, BadBind(String) }
impl ServerConfig {
    /// vars: injected map for testability; from_env() wraps std::env::vars().
    pub fn from_vars(vars: &std::collections::HashMap<String, String>) -> Result<Self, ConfigError>;
    pub fn from_env() -> Result<Self, ConfigError>;
}
```

```rust
// AppState threaded through axum (routes/mod.rs)
#[derive(Clone)]
pub struct ServerState {
    pub ctx: std::sync::Arc<convertbar_core::ctx::Ctx>,
    pub config: std::sync::Arc<ServerConfig>,
    pub events_tx: tokio::sync::broadcast::Sender<(String, serde_json::Value)>,
    // main.rs creates the broadcast::channel(256) already in THIS task so ServerState is
    // total; nothing consumes it until Task 3 wires ServerSink + the SSE route.
}
pub fn api_router(state: ServerState) -> axum::Router;   // nests all /api routes; static/embed added in main
```

- `embed.rs`: `#[derive(rust_embed::Embed)] #[folder = "../../dist-web"] pub struct WebAssets;` + a fallback handler serving embedded files with `mime_guess`, `/` and unknown non-API paths → `index.html` (SPA), 404 for missing assets with extensions.
- `/api/info` response: `{"version": env!("CARGO_PKG_VERSION"), "head": "server", "can_pause_process": true_on_unix, "auth_required": bool}` — struct `AppInfo` with `#[serde(rename_all = "snake_case")]` fields `version, head, can_pause_process, auth_required` (matches frontend snake_case type conventions).

- [ ] **Step 1: Cargo.toml** (versions verbatim; add member to root Cargo.toml):

```toml
[package]
name = "convertbar-server"
description = "Headless HTTP/SSE head for ConvertBar"
version.workspace = true
authors.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
convertbar-core = { path = "../convertbar-core" }
axum = "0.8"
axum-extra = { version = "0.10", features = ["cookie"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
tokio-stream = { version = "0.1", features = ["sync"] }
tower-http = { version = "0.6", features = ["trace"] }
tracing = "0.1"
tracing-subscriber = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rust-embed = "8"
mime_guess = "2"
constant_time_eq = "0.3"

[dev-dependencies]
tower = { version = "0.5", features = ["util"] }
tempfile = "3"
```

`build.rs` (the CI-critical piece — rust-embed fails compilation if the folder is missing and CI never builds frontend assets):

```rust
fn main() {
    // dist-web is produced by `npm run build:web`; CI and fresh checkouts don't have it.
    // rust-embed's derive requires the folder to EXIST — an empty dir embeds nothing and
    // the server serves API-only, which is exactly right for tests.
    std::fs::create_dir_all("../../dist-web").expect("create dist-web placeholder");
    println!("cargo:rerun-if-changed=../../dist-web");
}
```

- [ ] **Step 2: TDD config.rs.** Failing tests first (in-module): defaults (no vars + token set → 0.0.0.0:8080, roots ["/"], empty allowed_hosts); `MissingAuth` when neither token nor `CONVERTBAR_NO_AUTH=1`; `CONVERTBAR_NO_AUTH=1` → `Open`; port/bind parsing incl. `BadBind` on garbage; roots split on `:`; hosts split on `,`. Run red → implement `from_vars` → green. `from_env` is a thin untested wrapper.
- [ ] **Step 3: embed.rs + info route + router skeleton + main.rs.** `main.rs`: parse config (exit code 1 with the `MissingAuth` message on refusal — print exactly: `convertbar-server: set CONVERTBAR_AUTH_TOKEN or CONVERTBAR_NO_AUTH=1 (see docs)`), open db via `convertbar_core::db::get_db_path()`, build a placeholder `Ctx` (real sink arrives in Task 3 — use `TestSink`-free stub: a `NullSink` struct in main.rs implementing `EventSink` with empty bodies, replaced in Task 3), serve `api_router` + embed fallback on `config.bind`. Integration test (tower oneshot): `GET /api/info` returns 200 with the four fields.
- [ ] **Step 4: routes.json seed + `.gitignore` + suites + commit.** Add `/dist-web/` to the root `.gitignore` NOW (build.rs creates the dir from this task onward; the existing `dist` entry does not match `dist-web`). The `/api/info` test inlines a minimal state helper here; Task 5 promotes it to the shared `test_state()`. `cargo test --workspace && npm test` green. Commit: `feat: scaffold convertbar-server with config, embedded assets, and /api/info`.

---

### Task 3: ServerSink + SSE

**Files:**
- Create: `crates/convertbar-server/src/sink.rs`, `crates/convertbar-server/src/routes/events.rs`
- Modify: `main.rs` (replace NullSink), `routes/mod.rs`

**Interfaces:**
- Produces:

```rust
// sink.rs
pub struct ServerSink(pub tokio::sync::broadcast::Sender<(String, serde_json::Value)>);
impl convertbar_core::events::EventSink for ServerSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        // fire-and-forget: no subscribers is not an error; NEVER blocks, NEVER locks.
        let _ = self.0.send((event.to_string(), payload));
    }
    fn notify(&self, _title: &str, _body: &str) {} // web UI is live via SSE; spec: no-op
}
```

- `GET /api/events`: `axum::response::sse::Sse` over `tokio_stream::wrappers::BroadcastStream` of a fresh `subscribe()`, each item → `Event::default().event(name).data(payload.to_string())`; `KeepAlive::new().interval(Duration::from_secs(15))`. Lagged receivers (`BroadcastStream` yields `Err(Lagged)`) are skipped with a `tracing::warn!` — the client's reconnect-refetch heals gaps.
- Channel: `tokio::sync::broadcast::channel(256)` created in `main.rs`; the SAME sender goes into `ServerSink` (into `Ctx::new`) and `ServerState.events_tx`.
- **Shutdown-aware streams (adversarial-review-mandated):** `ServerState` gains `shutdown_rx: tokio::sync::watch::Receiver<bool>` (sender lives in main.rs). The SSE stream is wrapped so it ENDS when the watch flips (`tokio_stream::StreamExt::take_while` on the watch state, or select against `shutdown_rx.changed()`): an open EventSource otherwise never completes, hyper's graceful drain never finishes, and `docker stop` escalates to SIGKILL with a paused HandBrake child orphaned — defeating the spec's shutdown invariant in the *common* case of a browser tab being open.

- [ ] **Step 1: TDD sink**: unit test — emit through the `EventSink` trait, assert a subscriber receives `(name, payload)`; emit with zero subscribers does not error/panic.
- [ ] **Step 2: SSE route + integration test**: oneshot-style test spawning the router, `GET /api/events`, then `state.events_tx.send(...)`, read the response body stream and assert the SSE frame contains `event: conversion-progress` and the JSON data line. (Use `tokio::time::timeout` around the body read.)
- [ ] **Step 3: Wire into main.rs** (drop NullSink; `Ctx::new(conn, Arc::new(ServerSink(tx.clone())), Arc::new(DeleteDisposer))`). Suites + commit: `feat: add ServerSink broadcast and SSE endpoint`.

---

### Task 4: Server startup/shutdown sequence + settings normalization

**Files:**
- Create: `crates/convertbar-server/src/startup.rs`
- Modify: `main.rs`

**Interfaces:**
- Produces:

```rust
// startup.rs — mirrors src-tauri/src/lib.rs's setup sequence (the spec: replicated, not discarded)
pub fn normalize_server_settings(ctx: &Ctx);   // cleanup_mode/bad_source_action "trash" → "delete" + tracing::warn per key changed
pub fn boot(ctx: &Arc<Ctx>);                   // recover_interrupted_jobs → auto-resume check → run_queue if warranted → watcher::start(ctx.clone())
pub async fn shutdown_signal();                // resolves on SIGTERM or ctrl_c
```

- `main.rs` order: config → db open + `init_db` → Ctx → `normalize_server_settings` → `boot` → axum `serve(...).with_graceful_shutdown(async move { shutdown_signal().await; convertbar_core::converter::kill_active_child(&ctx.converter); let _ = shutdown_tx.send(true); })`. The child is killed AT signal time (not after serve returns — belt), and the watch send closes every SSE stream so the graceful drain actually completes (braces — see Task 3). A second `kill_active_child` after serve returns is harmless and keeps the non-SSE path covered.
- `boot` body is a direct port of the desktop setup block (`recover_interrupted_jobs(&db)`, `has_queued` COUNT, `is_queue_paused`, `should_auto_resume(has_queued, queue_paused)` → `run_queue(ctx.clone())`, then `watcher::start(ctx.clone())`) — copy the queries verbatim from `src-tauri/src/lib.rs`.

- [ ] **Step 1: TDD normalize_server_settings**: temp-db ctx with `cleanup_mode='trash'`, `bad_source_action='trash'` → both rows read back `'delete'`; a db already at `'delete'` is untouched (no warn — assert by absence of writes is overkill; assert values only).
- [ ] **Step 2: TDD boot** (the testable core): with a queued job + `queue_paused='false'` → `ctx.converter.is_running()` becomes true (poll briefly); with `queue_paused='true'` → stays false. Use the existing core test patterns (jobs with a nonexistent handbrake path fail fast — that's fine, `is_running` flips during the attempt; follow `should_auto_resume` test precedents in core rather than inventing timing-fragile assertions — if flakiness threatens, assert on `should_auto_resume` inputs read from the db instead and leave `run_queue` uncalled in tests).
- [ ] **Step 3: shutdown**: unit-testing SIGTERM is not worth the harness — verify by the Task 13 smoke (docker/ctrl-c). Keep `shutdown_signal` trivial (tokio::signal). Suites + commit: `feat: server startup sequence, settings normalization, graceful shutdown`.

---

### Task 5: Routes — queue, history, bad-sources

**Files:**
- Create: `crates/convertbar-server/src/routes/queue.rs`, `crates/convertbar-server/src/routes/history.rs`
- Modify: `routes/mod.rs`, `routes.json` (add the 14 rows for these routes)

**Interfaces:**
- Consumes (exact core signatures from Plan 1 Task 5): `queue_ops::{add_files(&Arc<Ctx>, &[String]) -> Result<AddResult, String>, scan_folder(String) -> Result<FolderScanResult, String>, confirm_folder_add(&Arc<Ctx>, String) -> Result<AddResult, String>, classify_paths(Vec<String>) -> Result<ClassifiedPaths, String>, get_queue(&Ctx) -> Result<Vec<JobInfo>, String>, remove_job(&Ctx, &str), reorder_queue(&Ctx, &[String]), clear_queue(&Ctx), clear_completed(&Ctx, &str), get_bad_sources(&Ctx), purge_bad_sources(&Arc<Ctx>, Vec<String>) -> Result<Vec<PurgeResult>, String>, get_history(&Ctx, u32, u32, Option<String>, Option<String>) -> Result<HistoryPage, String>, get_history_summary(&Ctx, Option<String>) -> Result<HistorySummary, String>, remove_history_entry(&Ctx, &str)}`
- Produces: one error helper used by ALL route tasks:

```rust
// routes/mod.rs
pub fn core_err(e: String) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({ "error": e })))
}
```

Handler patterns (write each handler by picking the matching pattern — every handler in this plan is one of these four):

```rust
// (a) blocking-core POST with camelCase body
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddFilesBody { paths: Vec<String> }
async fn add_files(State(s): State<ServerState>, Json(b): Json<AddFilesBody>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || queue_ops::add_files(&ctx, &b.paths)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => core_err(format!("task panicked: {join}")).into_response(),
    }
}
// (b) inline GET:            get_queue -> match queue_ops::get_queue(&s.ctx) { Ok->Json, Err->core_err }
// (c) path-param mutation:   remove_job(Path(id): Path<String>) -> queue_ops::remove_job(&s.ctx, &id) -> 204 on Ok
// (d) query-param GET:       get_history(Query(q): Query<HistoryQuery>) with
//                            #[derive(Deserialize)] #[serde(rename_all="camelCase")]
//                            struct HistoryQuery { limit: u32, offset: u32, search: Option<String>, sort_by: Option<String> }
```

Bodies for the rest: `reorder_queue` `{jobIds: Vec<String>}`; `clear_completed` `{mode: String}`; `purge_bad_sources` `{ids: Vec<String>}`; `scan_folder`/`confirm_folder_add` `{path: String}`; `classify_paths` `{paths: Vec<String>}`. Mutations returning `Ok(())` → `204 No Content`.

- [ ] **Step 1: Write the route handlers + register in `api_router` + add routes.json rows.**
- [ ] **Step 2: Integration tests (tower `ServiceExt::oneshot` against a temp-db `ServerState`, one shared `fn test_state() -> ServerState` helper in `routes/mod.rs`'s test module):** at minimum — `GET /api/queue` empty → `[]`; `POST /api/queue/files` with `{"paths": []}` → 200 `{"added":[],"skipped":[…]}`; `DELETE /api/queue/jobs/{id}` on a seeded row → 204 and row gone; `GET /api/history?limit=10&offset=0` → 200 shape; a core-error path (e.g. `POST /api/history/clear` with an invalid mode if core rejects it, else force an error by closing… keep it simple: assert `core_err` mapping via a route whose core fn errors on bad input) → 500 `{"error": …}`; camelCase key acceptance (`jobIds`) on reorder.
- [ ] **Step 3: Suites + commit:** `feat: server queue, history, and bad-source routes`.

---

### Task 6: Routes — converter control, settings, presets, handbrake, watched dirs

**Files:**
- Create: `crates/convertbar-server/src/routes/converter.rs`, `crates/convertbar-server/src/routes/settings.rs`, `crates/convertbar-server/src/routes/handbrake.rs`, `crates/convertbar-server/src/routes/watch.rs`
- Modify: `routes/mod.rs`, `routes.json` (remaining non-fs/info rows)

**Interfaces:**
- Consumes: `control::{start_queue(&Arc<Ctx>), pause_conversion(&Ctx), resume_conversion(&Ctx), cancel_conversion(&Ctx), pause_after_current(&Ctx), cancel_pause_after_current(&Ctx), get_pause_after_current(&Ctx) -> bool, get_low_disk_pause(&Ctx) -> Option<LowDiskPause>}`; `settings_ops::{get_settings(&Ctx) -> Result<Settings,_>, update_setting(&Ctx, &str, &str), read_suffix_template(&Connection, &str) -> String, set_preset_suffix(&Ctx, &str, &str)}` (Task 1's addition); `handbrake::{resolve_handbrake_path(&Ctx) -> Result<Option<String>,String>, cached_preset_metadata(&Ctx, &str, &str) -> Result<PresetMetadata,String>, handbrake_version(&str) -> Option<String>, resolve_suffix_template(...)}` — the `validate_handbrake` and `generate_preset_suffix` handlers port the (post-Task-1, now-thin) wrapper bodies from `src-tauri/src/commands/handbrake.rs` (the `HandbrakeStatus` assembly and the cache-check flow — ~15 lines each, all calling core fns); `watch_ops::{get_watched_directories, add_watched_directory, update_watched_directory, set_watched_directory_enabled, remove_watched_directory}` (Task 1 Step 5's signatures).
- Body/param shapes: `update_setting` PUT body `{"value": String}` (key from path); `set_preset_suffix` PUT body `{"suffix": String}`; `resolve_suffix_template` POST `{"template": String, "metadata": PresetMetadata}`; `add_watched_directory` POST `{"path": String, "recursive": bool, "stabilityDelaySecs": i64}`; `update_watched_directory` PUT `{"recursive": bool, "stabilityDelaySecs": i64}`; `set_watched_directory_enabled` PUT `{"enabled": bool}`. GET responses: `get_settings` → `Settings` as-is (stored `launch_at_login`; the server UI hides it); `detect_handbrake` → `Json(Option<String>)`; `get_pause_after_current` → `Json(bool)`; `get_low_disk_pause` → `Json(Option<LowDiskPause>)`.
- **The handbrake probe/scan handlers (`detect`, `presets`, `validate`, `generate`) wrap core in `spawn_blocking`** — they shell out to HandBrakeCLI, same as the desktop's async commands.

- [ ] **Step 1: handlers + registration + routes.json rows** (patterns from Task 5; settings/watch mutations are inline (short DB locks), handbrake ones spawn_blocking).
- [ ] **Step 2: Integration tests:** settings round-trip (`PUT /api/settings/preset` `{"value":"X"}` → 204; `GET /api/settings` shows it); invalid key → 500 with core's exact `Invalid setting key: …` message in `{"error"}`; pause-after-current POST → GET true → DELETE → GET false; watched-dir CRUD round-trip on a tempdir; suffix round-trip. Do NOT test the handbrake shell-out handlers beyond a smoke that they return 200/500-shaped JSON when HandBrakeCLI is absent (`detect` → 200 `null`).
- [ ] **Step 3: Suites + commit:** `feat: server control, settings, handbrake, and watched-dir routes`.

---

### Task 7: fs/list — the file browser endpoint

**Files:**
- Create: `crates/convertbar-server/src/routes/fs.rs`
- Modify: `routes/mod.rs`, `routes.json` (fs_list row)

**Interfaces:**
- Produces: `GET /api/fs/list?path=/media` → `{"entries": [{"name": String, "path": String, "is_dir": bool, "size": u64|null}]}`, sorted directories-first then name (case-insensitive). Roots come from `ServerConfig.browse_roots`.
- Security contract (spec-mandated): request path is canonicalized (`std::fs::canonicalize` — resolves symlinks) BEFORE checks; each configured root is canonicalized at config load; matching is component-aware via `Path::starts_with` (which is per-component in Rust — `/media` does NOT admit `/media2`); outside all roots → 403 `{"error":"path outside allowed roots"}`; nonexistent/unreadable → 404/500 JSON error, never a panic; entries that fail to stat are skipped, not fatal. Add a code comment acknowledging the accepted TOCTOU window (a symlink swapped between canonicalize and read_dir can escape the root) — tolerable under the single-user LAN threat model, documented so nobody "discovers" it later.

- [ ] **Step 1: TDD the pure check:** `fn path_allowed(canonical: &Path, roots: &[PathBuf]) -> bool` with tests: exact root, child, `/media2` vs root `/media` → false, `/` root admits everything.
- [ ] **Step 2: handler + traversal integration tests** on a tempdir root: list returns the seeded entries dirs-first; `?path=<root>/../` → 403 (canonicalize resolves it out); a symlink inside the root pointing outside → 403 (canonicalize follows it); unreadable path → non-200 JSON, process alive.
- [ ] **Step 3: Suites + commit:** `feat: server file-browser endpoint with root confinement`.

---

### Task 8: Auth — token, cookie, login, host validation, CSRF guard

**Files:**
- Create: `crates/convertbar-server/src/auth.rs`, `crates/convertbar-server/src/routes/login.rs`
- Modify: `routes/mod.rs`, `main.rs` (layer ordering) — routes.json already carries the login row from Task 2's seed

**Interfaces:**
- Produces (middleware, applied as axum layers in this order — host check outermost, then auth, then content-type):

```rust
// auth.rs
pub async fn host_guard(State(s): State<ServerState>, req: Request, next: Next) -> Response;
//   Host header (or :authority) → allow if: parses as IPv4/IPv6 literal (strip port/brackets),
//   equals "localhost" (any port), or is in config.allowed_hosts (case-insensitive, port stripped).
//   Else 421 MISDIRECTED_REQUEST with {"error":"host not allowed"}. ALWAYS on, even AuthMode::Open.
pub async fn auth_guard(State(s): State<ServerState>, req: Request, next: Next) -> Response;
//   AuthMode::Open → pass. Skip for POST /api/login and any non-/api path (static assets).
//   Accept: Authorization: Bearer <token>  OR  cookie convertbar_token=<token>.
//   Comparison: constant_time_eq::constant_time_eq(a.as_bytes(), b.as_bytes()).
//   Fail → 401 {"error":"unauthorized"}.
pub async fn json_content_guard(req: Request, next: Next) -> Response;
//   For POST/PUT/DELETE under /api (login included): require Content-Type starting
//   "application/json" UNLESS the request has no body semantics (DELETE with no CT is allowed
//   only when Content-Length is 0/absent). Fail → 415. Cross-site HTML forms cannot send
//   application/json without a CORS preflight the server never answers — this is the CSRF belt.
```

- `POST /api/login` body `{"token": String}`: constant-time compare; on success `Set-Cookie: convertbar_token=<token>; HttpOnly; SameSite=Strict; Path=/` (axum-extra `CookieJar`), 204; on failure 401. In `AuthMode::Open` → 204 without a cookie (login screen never shows, but the route stays total).
- SSE note: `GET /api/events` is under auth_guard and authenticates via the cookie (EventSource can't send headers) — a test must cover this.

- [ ] **Step 1: TDD the pure pieces:** host-allow decision fn (`fn host_allowed(host: &str, allowed: &[String]) -> bool`) — IPv4, `[::1]:8080`, `localhost:8080`, `nas.local` (only when listed), `evil.example.com` → false. Token compare wrapper.
- [ ] **Step 2: middleware + login + layer wiring; integration test matrix:** with `AuthMode::Token`: no credential → 401; bad bearer → 401; good bearer → 200; login with bad token → 401 no cookie; login good → 204 + cookie flags (assert `HttpOnly`, `SameSite=Strict`, `Path=/` literally in the Set-Cookie header); subsequent request with that cookie → 200; SSE GET with cookie → 200 stream. With `AuthMode::Open`: no credential → 200, but bad Host → 421. Content-type: POST without JSON CT → 415; static asset GET (`/`) unauthenticated → 200 even in Token mode.
- [ ] **Step 3: Suites + commit:** `feat: server auth, host validation, and CSRF guards`.

---

### Task 9: Rust-side route-contract test

**Files:**
- Create: `crates/convertbar-server/src/routes/contract_test.rs` (a `#[cfg(test)]` module wired via `mod` in routes/mod.rs, or inline in mod.rs's test module)

**Interfaces:**
- Consumes: `routes.json` (now complete — every row landed with its task).

- [ ] **Step 1: Write the parity test:** parse `routes.json` (include_str!); for each row, fire a request with the row's method+path (path params filled with a dummy id) at a `test_state()` router in `AuthMode::Open` and assert the response is NOT 404/405 (anything else — 200/204/400/415/422/500 — proves the route is registered with that method). Then the reverse guard: assert `routes.json` has no duplicate command names and its row count matches a literal (update the literal when adding routes — a human-visible tripwire).
- [ ] **Step 2: Suites + commit:** `test: pin routes.json to the registered router`.

---

### Task 10: Frontend transport split (pure refactor, desktop unchanged)

**Files:**
- Create: `src/lib/transport/types.ts`, `src/lib/transport/tauri.ts`
- Modify: `src/lib/tauri.ts` (becomes the selecting shim), `vite.config.ts` (nothing yet — selection lands in Task 11)

**Interfaces:**
- Produces: `src/lib/transport/types.ts` exports ALL the existing interfaces/type aliases currently in `src/lib/tauri.ts` (JobInfo … WatchedDirectory) plus:

```ts
// The command surface both transports implement. Derived from the existing `commands`
// object so the desktop shape is definitionally authoritative:
import type { tauriCommands } from "./tauri";
export type Transport = typeof tauriCommands;
// New in this plan (both transports implement):
export interface AppInfo { version: string; head: "desktop" | "server"; can_pause_process: boolean; auth_required: boolean; }
// Server-only (file browser; desktop never calls these):
export interface FsEntry { name: string; path: string; is_dir: boolean; size: number | null; }
export interface FsListResult { entries: FsEntry[]; }
```

- `src/lib/transport/tauri.ts`: the current `commands` object moved verbatim, renamed export `tauriCommands`, plus `getAppInfo(): Promise<AppInfo>` synthesized desktop-side from `getVersion()` (`@tauri-apps/api/app`) + `get_platform_capabilities` → `{version, head: "desktop", can_pause_process, auth_required: false}`.
- `src/lib/tauri.ts` (the path every consumer already imports) becomes:

```ts
export * from "./transport/types";
export { tauriCommands as commands } from "./transport/tauri";
```

(The build-time selection replaces this file's second line in Task 11 — consumers never change.)

- [ ] **Step 1: Move + shim exactly as above.** No call-site changes anywhere.
- [ ] **Step 2: Verify:** `npm test` → 142 green unmodified (suites that `vi.mock("../lib/tauri")` keep working — the module's export names are unchanged; suites that mock `@tauri-apps/api/core` still intercept through transport/tauri.ts). `npm run build` green. `cargo test --workspace` untouched but run once.
- [ ] **Step 3: Commit:** `refactor: split transport behind src/lib/transport with selecting shim`.

---

### Task 11: HTTP transport + EventSource events + login screen

**Files:**
- Create: `src/lib/transport/http.ts`, `src/components/LoginScreen.tsx`, `src/components/LoginScreen.test.tsx`
- Modify: `src/lib/tauri.ts` (build-time selection), `src/lib/events.ts` (EventSource internals for server head), `src/App.tsx` (401 → LoginScreen gate), `src/hooks/useQueue.ts` + `src/hooks/useHistory.ts` (reconnect-refetch window listeners per the design below), `vite.config.ts` (VITE_HEAD define passthrough is automatic via import.meta.env — no config change needed for env vars prefixed VITE_)

**Interfaces:**
- Produces: `httpCommands` in `transport/http.ts`, typed `Transport & { login(token: string): Promise<void>; fsList(path: string): Promise<FsListResult> }` (a plain `: Transport` annotation rejects the extra members via excess-property checking; use the intersection type, or `satisfies Transport` on the object literal plus exported extras). Every method maps to its routes.json row:

```ts
const api = async <T>(method: string, path: string, body?: unknown): Promise<T> => {
  const res = await fetch(path, {
    method,
    // ALWAYS send the JSON content-type on mutating methods, even with no body:
    // the server's CSRF guard requires it (bodyless POSTs like /api/converter/start
    // would 415 otherwise), and it is harmless on an empty body.
    headers: method === "GET" ? {} : { "Content-Type": "application/json" },
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) { window.dispatchEvent(new Event("convertbar:unauthorized")); throw new Error("unauthorized"); }
  if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error ?? `HTTP ${res.status}`);
  return res.status === 204 ? (undefined as T) : res.json();
};
// examples of the mapping (write all of them; keys camelCase per the route contract):
// addFiles: (paths) => api("POST", "/api/queue/files", { paths }),
// reorderQueue: (jobIds) => api("PUT", "/api/queue/order", { jobIds }),
// getHistory: (limit, offset, search?, sortBy?) =>
//   api("GET", `/api/history?limit=${limit}&offset=${offset}` + (search ? `&search=${encodeURIComponent(search)}` : "") + (sortBy ? `&sortBy=${sortBy}` : "")),
// removeJob: (id) => api("DELETE", `/api/queue/jobs/${encodeURIComponent(id)}`),
// login (extra, server-only): (token) => api("POST", "/api/login", { token }),
```

  Desktop-only members (`openPath`, `revealInDir`, `checkPathsExist`, `quitApp`, `hideWindow`, `pickFolder`, `getPlatformCapabilities`) exist on the object but `throw new Error("not available on server")` — the server UI never calls them (Task 12 gates their callers); the throw is the tripwire if gating regresses.
- `src/lib/tauri.ts` selection (statically eliminable):

```ts
export * from "./transport/types";
import { tauriCommands } from "./transport/tauri";
export const commands = import.meta.env.VITE_HEAD === "server"
  ? (await import("./transport/http")).httpCommands
  : tauriCommands;
```

  If top-level await trips the build, use the synchronous form: import both, select by ternary — tree-shaking of the unused branch is a nice-to-have, not a requirement; correctness first.
- `src/lib/events.ts` server internals: one shared `EventSource("/api/events")` (same-origin cookie applies automatically); `listen(event, cb)` registers `addEventListener(event, e => cb({ payload: JSON.parse(e.data) }))` — preserving the Tauri callback shape `{payload}` — and **returns `Promise<UnlistenFn>`** (resolve immediately with the remover): consumers rely on the promise shape (`unlisteners.forEach((p) => p.then((u) => u()))` in useQueue.ts:47/useAddProgress.ts:54, `await listen(…)` in SettingsPage.tsx:467) — a sync return breaks every hook's cleanup. Reconnect design (one pattern, no alternatives): events.ts dispatches `window.dispatchEvent(new Event("convertbar:events-reconnected"))` on `onopen`-after-error; `useQueue` and `useHistory` each add a `window.addEventListener("convertbar:events-reconnected", refetch)` alongside their existing listen registrations (server and desktop alike — the event simply never fires on desktop). No App.tsx wiring, no state library. Desktop path: unchanged re-export of Tauri listen.
- `LoginScreen`: token input + submit → `httpCommands.login(token)` → on 204 reload data (window.location.reload() is acceptable); rendered by App.tsx when a `convertbar:unauthorized` event fires (server head only).

- [ ] **Step 1: http.ts with ALL Transport members** (routes.json is the checklist); events.ts internals; selection shim; LoginScreen + App gate.
- [ ] **Step 2: Tests:** LoginScreen (renders, submits token, fires reload path mocked); http transport unit tests with mocked `fetch` for: camelCase body keys (`reorderQueue` sends `{"jobIds":[…]}`), **bodyless POST still sends `Content-Type: application/json` (`startQueue` — the CSRF-guard contract)**, 204 → undefined, error body → thrown message, 401 → unauthorized event; events.ts server path with a mocked EventSource class (listen wraps payload, unlisten removes).
- [ ] **Step 3: Suites (desktop build still green: `npm test`, `npm run build`) + commit:** `feat: http transport, SSE event source, and login screen for the server head`.

---

### Task 12: Server-UI capability gating + file-browser modal

**Files:**
- Create: `src/components/FileBrowserModal.tsx`, `src/components/FileBrowserModal.test.tsx`, `src/lib/head.ts`
- Modify: `src/App.tsx` (Esc/hide gating, validate flow), `src/components/TabBar.tsx` (hide button), `src/components/ActiveJob.tsx` (capabilities via getAppInfo), `src/pages/SettingsPage.tsx` (hide updater/launch-at-login/menubar/notifications/trash + version from getAppInfo), `src/pages/HistoryPage.tsx` (gate context menu + open/reveal), `src/hooks/useFileIntake.ts` (drag-drop desktop-only; server add via modal), `src/hooks/useWatchedDirectories.ts` (pickFolder → modal)

**Interfaces:**
- Produces: `src/lib/head.ts`:

```ts
export const isServerHead = import.meta.env.VITE_HEAD === "server";
```

  Gating rule: BUILD-TIME `isServerHead` for UI presence; RUNTIME `getAppInfo()` only for data. Honest mechanics: the Tauri JS packages import inertly in a browser (nothing "fails to compile" without them), so the build-time gate buys correctness-at-runtime and bundle size — in the SERVER build `import.meta.env.VITE_HEAD` is statically replaced and dead branches (updater imports etc.) are eliminated; the DESKTOP build may carry an unused http-transport chunk, which is accepted (correctness over the spec's "tree-shakes the other transport" aspiration — note it in the task report). Runtime data via `getAppInfo()`: version display, `can_pause_process` — ActiveJob already reads a capabilities call; switch it to `commands.getAppInfo()`.
- Server build hides: updater section, launch-at-login toggle, menubar section, notifications section, `bad_source_action`/`cleanup_mode` trash options (render "delete" as the only value with a hint), open/reveal buttons AND the history context menu, quit button, Esc-hide handler, TabBar hide button, native drag-drop registration.
- `FileBrowserModal`: props `{ mode: "files" | "directory", onSelect(paths: string[]), onClose() }`; lists via the http transport's `fsList(path)` (created in Task 11 as a server-only extra, like `login`); breadcrumb up-navigation; multi-select files in `files` mode, single dir select in `directory` mode. Wire: intake page "Add files…" button (server) → `classifyPaths` → `addFiles`/`confirmFolderAdd` flow (same as drag-drop's downstream); watched-dir setup button (server) → `directory` mode replacing `pickFolder`.

- [ ] **Step 1: head.ts + gate every listed site** (each gate is `isServerHead ? … : …` or early-return; keep diffs minimal per file).
- [ ] **Step 2: FileBrowserModal + tests** (renders entries from mocked fetch, navigates into a dir, selects, calls onSelect with paths; directory mode returns the current dir).
- [ ] **Step 3: Both builds green:** `npm test`; `npm run build` (desktop); `VITE_HEAD=server npx vite build --outDir dist-web --emptyOutDir` compiles (the script lands in Task 13 — invoke inline here to verify). Commit: `feat: server-head UI gating and file-browser intake`.

---

### Task 13: build:web pipeline + contract sibling tests + local end-to-end smoke

**Files:**
- Modify: `package.json` (script), `src/test/ipc-contract.test.ts` (sibling assertions)
- Create: none

**Interfaces:**
- Produces: `"build:web": "tsc && VITE_HEAD=server vite build --outDir dist-web --emptyOutDir"` in package.json scripts (`/dist-web/` is already gitignored since Task 2).
- Contract sibling (extend `src/test/ipc-contract.test.ts`): (1) parse `crates/convertbar-server/routes.json`; every `Transport` method invoked in `transport/http.ts` (grep `api("` calls' method+path template literals — normalize `${…}` segments to `{param}`) must match a routes.json row, and every non-desktop-only routes.json command must appear in http.ts — match TS method names to routes.json commands by stripping underscores and lowercasing both (`fsList` ↔ `fs_list`, `getAppInfo` ↔ `get_app_info`). Path matching MUST apply two normalizations on both sides or the test can never pass: (1) strip everything from the first `?` (the `getHistory`/`fsList` templates carry query strings; routes.json rows don't), (2) replace every `{anything}` / `${anything}` segment with the single token `{}` before comparing (routes.json says `{id}`/`{preset}`/`{key}`; the templates interpolate arbitrary expressions); (2) every `listen("event-name"` in frontend sources must be an event name emitted in core (existing scan) — unchanged but now ALSO assert the SSE route exists in routes.json… (the events row isn't in routes.json — assert instead that `routes/events.rs` registers `/api/events` by grepping the Rust source for the literal `"/api/events"`).

- [ ] **Step 1: script + contract test extensions; `npm test` green.**
- [ ] **Step 2: End-to-end smoke (scripted, run locally, output in the report):**

```bash
npm run build:web
cargo build -p convertbar-server
CONVERTBAR_DATA_DIR=$(mktemp -d) CONVERTBAR_AUTH_TOKEN=smoke-token CONVERTBAR_PORT=8199 \
  ./target/debug/convertbar-server &   # note the pid
sleep 2
test "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8199/api/info)" = 401   # auth on (-f would abort on 401)
curl -sf -H "Authorization: Bearer smoke-token" http://127.0.0.1:8199/api/info | jq .head   # → "server"
curl -sf http://127.0.0.1:8199/ | head -c 100                             # → index.html bytes (assets embedded)
curl -s -X POST http://127.0.0.1:8199/api/login -H 'Content-Type: application/json' -d '{"token":"smoke-token"}' -D- -o /dev/null | grep -i set-cookie
kill %1   # graceful SIGTERM path exercised
```

Expected: all four probes as annotated; the process exits promptly on kill (graceful shutdown works).
- [ ] **Step 3: Suites + commit:** `feat: server web build pipeline and http contract tests`.

---

### Task 14: Docker image, compose example, GHCR workflow, docs

**Files:**
- Create: `Dockerfile`, `.dockerignore`, `docker-compose.example.yml`, `.github/workflows/docker.yml`
- Modify: `README.md` (server section), `CLAUDE.md` (server-head note), `docs/OPEN_ISSUES.md` (close out the issue)

**Interfaces / exact contents:**

`Dockerfile` (multi-stage; CPU-only per spec):

```dockerfile
# 1: web assets
FROM node:24-bookworm-slim AS web
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY tsconfig*.json vite.config.ts index.html ./
COPY public ./public
COPY src ./src
RUN npx tsc && VITE_HEAD=server npx vite build --outDir dist-web --emptyOutDir

# 2: server binary
FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri/Cargo.toml ./src-tauri/Cargo.toml
# src-tauri is a workspace member but is NOT built; give cargo a stub lib so the
# workspace parses without the tauri sources or GUI deps:
RUN mkdir -p src-tauri/src && echo "" > src-tauri/src/lib.rs
COPY --from=web /app/dist-web ./dist-web
RUN cargo build --release -p convertbar-server

# 3: runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends handbrake-cli ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/convertbar-server /usr/local/bin/convertbar-server
ENV CONVERTBAR_DATA_DIR=/config
VOLUME /config
EXPOSE 8080
ENTRYPOINT ["convertbar-server"]
```

(Caveat for the implementer: the stub-lib trick must not poison the real build — it lives only in the image build context. If `cargo build -p convertbar-server` pulls tauri via the workspace anyway, fall back to `--workspace --exclude convertbar` … it does not: `-p` builds only the selected package's dependency closure, and convertbar-server does not depend on the src-tauri crate. If the stub approach fails in practice, the fallback is copying the real src-tauri sources — heavier context, same result. State which was needed in the report.)

`.dockerignore`: `target`, `node_modules`, `dist`, `dist-web`, `.git`, `.claude`, `docs`, `.superpowers`.

`docker-compose.example.yml`:

```yaml
services:
  convertbar:
    image: ghcr.io/rhurling/convertbar:latest
    ports:
      - "8080:8080"
    environment:
      CONVERTBAR_AUTH_TOKEN: "change-me"     # always set a token
      # CONVERTBAR_BROWSE_ROOTS: "/media"    # optional: restrict the file browser
    volumes:
      - ./config:/config                     # SQLite db + probe cache
      - /path/to/media:/media                # bind-mount LOCAL disks for watched folders:
                                             # inotify is event-blind on NFS/SMB mounts —
                                             # watched dirs on network filesystems only ingest
                                             # on container restart.
```

`.github/workflows/docker.yml`: ONE workflow-level `on:` block (per-job `on:` is invalid Actions syntax):

```yaml
on:
  push:
    tags: ["v*"]
  workflow_dispatch:
  pull_request:
    paths: [Dockerfile, ".dockerignore", "crates/convertbar-server/**", ".github/workflows/docker.yml"]
```

Two jobs gated by event: (a) `publish` with `if: github.event_name != 'pull_request'`; `permissions: {contents: read, packages: write}`; checkout → `docker/login-action` (ghcr.io, `${{ github.actor }}` / `GITHUB_TOKEN`) → `docker/build-push-action` with `push: true`, `platforms: linux/amd64`, `tags: ghcr.io/rhurling/convertbar:${VERSION},ghcr.io/rhurling/convertbar:latest` (VERSION = tag minus `v` via a shell step; on `workflow_dispatch` use `latest` only). (b) `build-pr` with `if: github.event_name == 'pull_request'`, build-only (`push: false`), NOT a required check (the `paths:` filter gates only the pull_request event). All action refs SHA-pinned (resolve current release SHAs at implementation time, matching the pinning style in `.github/workflows/test.yml`).

Docs: README gains a "Server (Docker)" section — image, compose example pointer, env-var table (the six vars), the NFS/SMB caveat, the auth stance (token or explicit `CONVERTBAR_NO_AUTH=1`), reverse-proxy note for HTTPS, and volume permissions (the image runs as any `--user <uid>:<gid>`; `/config` and the media mounts must be writable by that uid — spec: "runs as any --user uid"). CLAUDE.md Workspace Layout sentence gains `+ crates/convertbar-server (headless HTTP/SSE head; routes.json is the route contract)`. `docs/OPEN_ISSUES.md`: replace the issue's status line with `**Status:** shipped — Plan 1 (workspace split, PR #124) + Plan 2 (server head). Remaining follow-ups tracked in the Plan 2 doc.`

- [ ] **Step 1: Dockerfile + .dockerignore + compose; local verification:** `docker build -t convertbar-server-test .` succeeds; `docker run --rm -e CONVERTBAR_NO_AUTH=1 -p 8199:8080 convertbar-server-test` then `curl -sf http://127.0.0.1:8199/api/info | jq .head` → `"server"`; `docker stop` returns promptly (SIGTERM handled). If Docker is unavailable on the machine, report BLOCKED for this step — do not mark the task complete on an untested Dockerfile.
- [ ] **Step 2: docker.yml + docs edits.**
- [ ] **Step 3: Suites + commit:** `feat: docker image, compose example, and GHCR publish workflow`.

---

## Acceptance criteria (from the spec, Plan-2 scope)

1. `cargo test --workspace` and `npm test` green; desktop app unchanged (`npm run build` + existing suites).
2. `docker compose up` serves the web UI; auth enforced; server refuses to start with neither `CONVERTBAR_AUTH_TOKEN` nor `CONVERTBAR_NO_AUTH=1`.
3. Watched-folder intake converts end-to-end in the container; ad-hoc add via the file browser works; SSE progress updates live without refresh. (Manual NAS gate — release-blocking for the feature, performed by the user.)
4. Mid-encode pause/resume works in the container; restart auto-resumes.
5. The next `v*` release tag publishes `ghcr.io/rhurling/convertbar` (amd64) from CI.

## Deferred (post-Plan-2, per spec)

arm64 image; QSV/VAAPI/NVENC passthrough recipe; uploads; webhook notifications; multi-user auth; HTTPS (reverse proxy); polling fallback for network-mount watched folders; GHCR publishing from `release.sh`.
