# Docker Web-UI Server — Design

## Problem

ConvertBar is a menu-bar app: the encode engine only runs while a desktop session
is open on a machine with a GUI. The natural home for batch video conversion is a
NAS/home server — media already lives there, it is always on, and a watched
folder on a mounted volume is a better intake than dragging files onto a tray
window. Today there is no way to run ConvertBar headless: the core is compiled
into a Tauri shell, the UI speaks Tauri IPC, and several behaviors
(`trash::delete`, native dialogs, notifications, updater) assume a desktop.

## Evidence

From a full coupling audit of `src-tauri/src` (2026-07-28):

- **The domain logic is already nearly Tauri-free.** `types.rs`, `db.rs`,
  `probe.rs`, `probe_cache.rs`, `handbrake.rs`, `media_skip.rs`,
  `failure_class.rs` import no Tauri at all. `converter.rs` uses `AppHandle`
  only as an event/notification sink — DB and `ConverterState` are explicit
  arguments (`run_queue`, `converter.rs:1379`). The queue mutations live in
  Tauri-free `_inner` functions (`add_files_inner`, `commands/queue.rs:809`;
  `add_files_to_db`, `:949`).
- **The abstraction seam already half-exists.** `run_queue`, `process_queue`,
  `AddOp`, `start_queue`, `cancel_conversion` are generic over
  `R: tauri::Runtime` — introduced for `MockRuntime` tests. Swapping that bound
  for an event-sink trait is mechanical. (`pause_conversion` /
  `resume_conversion` are **not** generic — they take bare `AppHandle` and
  need real de-Taurifying, not just a bound swap.)
- **The event surface is small and enumerable:** 10 event names, 25
  fire-and-forget `app.emit` sites (`menu-bar-update`, `job-status-changed`,
  `queue-updated`, `job-error`, `job-completed`, `conversion-progress`,
  `queue-paused-low-disk`, `add-started`, `add-progress`, `add-finished`).
- **The frontend transport is one file deep.** All 42 `invoke()` wrappers live
  in `src/lib/tauri.ts`; no component calls `invoke` directly. `listen()` has
  12 call sites across 6 files (4 hooks, 2 pages).
- **Desktop-only leaves are isolated:** tray/updater/window shell (`lib.rs`,
  462 lines, discardable wholesale), `pick_folder`, `open_path` /
  `reveal_in_dir`, `quit_app`, notifications (3 sites in `converter.rs`),
  autostart, and `trash::delete` (4 sites, one already stubbed for tests at
  `commands/queue.rs:451`).
- **Linux already builds and tests green in CI** (`rust (ubuntu-22.04)` is a
  required check), which de-risks the container target.
- **`app.state::<T>()` service-locator lookups are concentrated in
  `watcher.rs`** (~10 sites); everything else takes state as arguments.

## Goal

A headless, Docker-deployable ConvertBar with a browser UI: same queue engine,
same SQLite database schema, same React frontend — a second "head" on the
existing core, not a fork. Watched folders on mounted volumes are the primary
intake; a server-side file browser covers ad-hoc adds and watched-dir setup.
The macOS and Windows desktop apps do not change behavior; desktop Linux
gains true mid-encode pause as a deliberate side effect, and both heads gain
a new opt-in data-dir override.

## Decisions (settled with the user)

| Topic | Decision |
|---|---|
| Audience | Personal NAS/home server first; published in the repo as-is |
| Input model | Watched folders + a small server-side file browser (also replaces `pick_folder` for watched-dir setup) |
| Auth | Static token (`CONVERTBAR_AUTH_TOKEN`; Bearer header or cookie via login page). Running open requires an explicit `CONVERTBAR_NO_AUTH=1` (reverse-proxy setups) — tightened from "unset = open" after adversarial review, see Auth section |
| Hardware accel | CPU-only software encode (x264/x265). No device passthrough in MVP |
| Distribution | CI-published GHCR image on release tags, amd64 only |
| Code split | Cargo workspace: `convertbar-core` + `src-tauri` + `convertbar-server` |
| Event transport | SSE (not WebSocket) |

## Rejected alternatives

- **Single crate, server binary links Tauri.** Tauri on Linux pulls
  webkit2gtk/GTK — a headless image would need the desktop GUI stack to
  compile. Dead on arrival.
- **Single crate + `desktop`/`server` feature flags.** Smallest diff, but the
  boundary lives in `cfg` attributes, `build.rs` needs `CARGO_FEATURE_*`
  gating of `tauri_build`, mock-runtime tests become feature-gated, and
  nothing stops server code from reaching a Tauri type again. The trait work
  is identical either way; the workspace buys a compiler-enforced boundary.
- **WebSocket for events.** All client→server traffic is request/response;
  SSE is one-way push with `EventSource` auto-reconnect for free.
- **Browser upload as intake.** Impractical for multi-GB video; the media is
  already on the server's volumes.
- **Web file browser deferred in favor of path text entry.** Rejected because
  some path-picking mechanism is required regardless — watched-dir
  configuration currently uses the native `pick_folder` dialog.

## Architecture

```
Cargo.toml                    workspace root; version in [workspace.package]
crates/convertbar-core/       lib — all portable logic, zero tauri/trash deps
src-tauri/                    existing desktop crate, shrunk to a thin shell
                              (stays in place: tauri.conf.json, bundling, ACL untouched)
crates/convertbar-server/     bin — axum HTTP + SSE + embedded web assets
```

Dependency direction is compiler-enforced: `core` depends on neither head;
both heads depend on `core`.

**Moves to `convertbar-core`:** `types`, `db`, `probe`, `probe_cache`,
`handbrake`, `media_skip`, `failure_class`, `add_progress`, the trait-ified
`converter` and `watcher`, the `_inner` halves of `commands/queue.rs`
(add/scan/classify, queue mutations, history queries, bad-source purge), and
the bodies of `commands/converter.rs` — pause/resume/cancel,
pause-after-current, the low-disk getter. The SIGSTOP/SIGCONT logic lives
inside those command bodies today (`commands/converter.rs:50-69, 132-151,
270-278`), so it must move for the server to pause at all; pause/resume are
de-Taurified here, not just bound-swapped. Settings read/write logic moves
too: core owns the DB access and the skip-marker refresh (the watcher is
core), while the desktop adapter overlays the autostart plugin as source of
truth for `launch_at_login` and its enable/disable side effect
(`commands/settings.rs:89, 161-173`); the server returns the stored
`launch_at_login` value, which its UI hides. Inline `#[cfg(test)]` tests
move with their modules.

**Stays in `src-tauri`:** `lib.rs` (tray, updater, window shell, plugins),
all `#[tauri::command]` wrappers (now thin adapters over core), and the
desktop-only commands: `pick_folder`, `open_path`, `reveal_in_dir`,
`quit_app`. `trash` becomes a `src-tauri` dependency only.

### The three seams

1. **`EventSink`** — object-safe trait in core:

   ```rust
   pub trait EventSink: Send + Sync {
       fn emit(&self, event: &str, payload: serde_json::Value);
       fn notify(&self, title: &str, body: &str);
   }
   ```

   Desktop impl wraps `AppHandle` (emit + `tauri_plugin_notification`);
   server impl broadcasts to SSE subscribers and no-ops `notify`. The
   existing `R: tauri::Runtime` generics on `run_queue` / `process_queue` /
   `AddOp` / `record_job_error*` become `Arc<dyn EventSink>`. A generic
   convenience wrapper (`emit_t<T: Serialize>`) keeps call sites tidy —
   with one constraint: event names stay string literals at the call sites,
   because the contract test greps for them (see Testing).

2. **Core context struct** — owns `db: Arc<Mutex<Connection>>`, the preset
   cache (today's `AppState`), `Arc<ConverterState>`, `WatcherState`, the
   sink, and the disposer. Constructed once per head. Replaces every
   `app.state::<T>()` lookup (the bulk of the change is `watcher.rs`,
   especially `enqueue_and_start`, `watcher.rs:426`).

3. **`FileDisposer`** — replaces the 4 `trash::delete` sites
   (`converter.rs:137`, `:1143`, `:1151`, `commands/queue.rs:451`). Desktop
   impl trashes; server impl `fs::remove_file`s. The `cleanup_mode` /
   `bad_source_action` *decision* logic stays in core unchanged — only the
   trash primitive is injected.

The queue engine's concurrency invariants (atomic job claim, cancel/clear
semantics, the in-place partial-cleanup guard in
`converter::recover_interrupted_jobs`) move to core **unchanged**.

## Server head (`convertbar-server`)

axum + tokio. Handlers call core functions via `spawn_blocking` (SQLite
behind a mutex; probes never block the async runtime — same discipline as the
desktop's async commands). The encode engine keeps its `std::thread` model.

**Startup mirrors the desktop shell's sequence** (`lib.rs:352-380`), which is
replicated, not discarded: open db → `recover_interrupted_jobs` → start
watcher → `should_auto_resume` → `run_queue` → serve HTTP. **Shutdown:**
SIGTERM/SIGINT (`docker stop`) triggers graceful shutdown — stop accepting
requests, then `kill_active_child` (SIGCONT-then-kill, same as desktop exit,
`lib.rs:412-421`) so a paused HandBrake child is never orphaned until
Docker's SIGKILL.

### Routes

JSON under `/api/`, one route per portable command, command names preserved
in a route table so the contract test can match them:

| Command | Route |
|---|---|
| `get_queue` | `GET /api/queue` |
| `add_files` | `POST /api/queue/files` |
| `scan_folder` | `POST /api/folders/scan` |
| `confirm_folder_add` | `POST /api/queue/folder` |
| `classify_paths` | `POST /api/paths/classify` |
| `remove_job` | `DELETE /api/queue/jobs/{id}` |
| `reorder_queue` | `PUT /api/queue/order` |
| `clear_queue` | `DELETE /api/queue` |
| `start_queue` | `POST /api/converter/start` |
| `pause_conversion` / `resume_conversion` / `cancel_conversion` | `POST /api/converter/{pause,resume,cancel}` |
| `pause_after_current` / `cancel_…` / `get_…` | `POST` / `DELETE` / `GET /api/converter/pause-after-current` |
| `get_low_disk_pause` | `GET /api/converter/low-disk-pause` |
| `get_history` / `get_history_summary` | `GET /api/history`, `GET /api/history/summary` |
| `remove_history_entry` | `DELETE /api/history/{id}` |
| `clear_completed` | `POST /api/history/clear` |
| `get_bad_sources` / `purge_bad_sources` | `GET /api/bad-sources`, `POST /api/bad-sources/purge` |
| `get_settings` / `update_setting` | `GET /api/settings`, `PUT /api/settings/{key}` |
| `get_preset_suffix` / `set_preset_suffix` / `generate_preset_suffix` | `GET` / `PUT` / `POST …/generate` on `/api/presets/{preset}/suffix` |
| `resolve_suffix_template` | `POST /api/suffix/resolve` |
| `detect_handbrake` / `list_handbrake_presets` / `validate_handbrake` | `GET /api/handbrake/{detect,presets,validate}` |
| watched-dir CRUD | `GET/POST /api/watched`, `PUT/DELETE /api/watched/{id}`, `PUT /api/watched/{id}/enabled` |

New, server-only:

- `GET /api/fs/list?path=…` — file browser: `{ entries: [{name, path, is_dir, size}] }`.
  The request path is canonicalized (symlinks resolved) before checks; if
  `CONVERTBAR_BROWSE_ROOTS` (colon-separated) is set, the roots are
  canonicalized too and matching is path-component-aware (`/media` does not
  admit `/media2`); listings outside → 403. Unset defaults to `/` — inside a
  container, the mounts are the sandbox.
- `GET /api/info` — version, `head: "server"`, and the capability flags the
  UI gates on (subsumes `get_platform_capabilities`).
- `GET /api/events` — SSE stream.
- `POST /api/login` — token exchange for an HttpOnly cookie.

Not implemented server-side (desktop-only, UI never calls them there):
`pick_folder`, `open_path`, `reveal_in_dir`, `check_paths_exist`,
`quit_app`, window hide, updater.

### Events

The server `EventSink` feeds a tokio broadcast channel; `GET /api/events`
subscribes and forwards every emit as `event: <name>` / `data: <json>`. All
10 event names flow through; the web UI consumes the same subset the hooks
use today (`menu-bar-update` is forwarded but currently unconsumed — the
tray and the updater one-shot listener are desktop concerns). On
`EventSource` reconnect the frontend refetches queue/history, so events
missed during a disconnect heal themselves.

### Auth & request hardening

- **Token auth.** Startup requires either `CONVERTBAR_AUTH_TOKEN` or an
  explicit `CONVERTBAR_NO_AUTH=1` (for reverse-proxy-authenticated setups);
  with neither, the server refuses to start with a clear message. This
  tightens the originally-settled "unset = open" stance: an unauthenticated
  server can browse every mount, permanently delete files
  (`purge_bad_sources` under forced-delete), and point `handbrake_path` at
  an arbitrary binary that the next encode executes — "open" must be a
  choice, not a default. The compose example always sets a token.
- With auth on, middleware covers everything except `POST /api/login` and
  static assets: `Authorization: Bearer <token>` or the session cookie,
  constant-time comparison. The cookie (needed because `EventSource` cannot
  send headers) carries the raw token: `HttpOnly; SameSite=Strict; Path=/`,
  session-lived; `Secure` deliberately omitted (plain-HTTP LAN, HTTPS is the
  reverse proxy's job). 401 → the web UI shows a token prompt. No users, no
  password storage — one shared token.
- **CSRF.** `SameSite=Strict` on the cookie, plus all state-changing routes
  (POST/PUT/DELETE) require `Content-Type: application/json` — a cross-site
  HTML form cannot send that without a CORS preflight, which the server
  never answers.
- **DNS rebinding.** Every request validates the `Host` header: accepted
  when it is an IP literal, `localhost`, or listed in
  `CONVERTBAR_ALLOWED_HOSTS`; anything else → 421. Zero-config
  `http://<nas-ip>:8080` keeps working, while a malicious page re-resolving
  its own hostname to the NAS IP is refused even in no-auth mode.

### Configuration (env)

| Variable | Default | Purpose |
|---|---|---|
| `CONVERTBAR_DATA_DIR` | `dirs::data_dir()/com.convertbar.app` (image sets `/config`) | db + probe cache location. **New behavior**: an env override added to core `get_db_path` (`db.rs:52` is a hardcoded resolver today), honored by both heads |
| `CONVERTBAR_BIND` / `CONVERTBAR_PORT` | `0.0.0.0` / `8080` | listen address |
| `CONVERTBAR_AUTH_TOKEN` | — | the shared token; required unless `CONVERTBAR_NO_AUTH=1` |
| `CONVERTBAR_NO_AUTH` | unset | explicit opt-out of auth (reverse-proxy setups) |
| `CONVERTBAR_ALLOWED_HOSTS` | unset | extra `Host` values beyond IP literals / `localhost` |
| `CONVERTBAR_BROWSE_ROOTS` | unset (`/`) | file-browser allowlist, component-aware |

## Behavioral deltas on the server

| Behavior | Desktop | Server |
|---|---|---|
| `cleanup_mode` / `bad_source_action` | `trash` default | Forced `delete`: disposer deletes, UI hides trash options, a stray `"trash"` db value is treated as delete with a log line (the `trash` crate would litter `.Trash-<uid>` dirs on NAS mounts) |
| Notifications | 3 toast sites | No-op (`notify`); web UI is live via SSE. Settings hidden |
| `launch_at_login`, `menubar_*`, updater | Active | Hidden/inert; image updates via `docker pull` |
| Mid-encode pause | macOS only (SIGSTOP/SIGCONT) | **Widened to `cfg(unix)`**: the signal call sites keep `#[cfg]` *attribute* gating (never the `cfg!()` macro — `libc` must not link on Windows), now `cfg(unix)`; `can_pause_process` (`converter.rs:196`, a runtime boolean, where `cfg!()` is fine) widens to match; the `libc` target-dependency entry moves to **core's** Cargo.toml, where the call sites land. The Linux container — and desktop Linux — get true pause; Windows keeps pause-after-current |
| `queue_paused` persistence / auto-resume | On launch | Unchanged — container restart resumes the queue |
| Low-disk pause | statvfs on output volume | Unchanged, works on mounts |
| HandBrake detection | `which` on PATH + `handbrake_path` override | Same; CLI baked into the image |

## Frontend

One React app, two builds.

- **Transport interface** (`src/lib/transport/`): the existing
  `src/lib/tauri.ts` command object becomes the Tauri implementation; an HTTP
  implementation maps the same typed methods onto `fetch`. Build-time
  selection via `VITE_HEAD` so each bundle tree-shakes the other transport.
  Desktop-only methods (`openPath`, `revealInDir`, `pickFolder`,
  `checkPathsExist`, `quitApp`, `hideWindow`, updater) exist only on the
  Tauri side; server UI never renders their callers — including the history
  context menu (`handleItemContextMenu`, `HistoryPage.tsx:167`), which is
  gated off wholesale on the server build, not just its open/reveal buttons.
- **Event shim** (`src/lib/events.ts`): centralizes the 6 scattered
  `listen()` sites (4 hooks, `QueuePage`, `SettingsPage`) behind one
  `listen(event, cb)` — Tauri events on desktop, one shared `EventSource` on
  the server, with reconnect → refetch. Pure refactor for the desktop build,
  done first.
- **Capability gating** from `GET /api/info` (desktop keeps
  `get_platform_capabilities` shape): server build hides updater,
  launch-at-login, menu-bar and notification settings, trash options,
  open/reveal buttons in history, and quit; Esc no longer hides a window.
- **Intake**: native drag-drop (`onDragDropEvent`, `useFileIntake.ts:125`) is
  desktop-only. Server intake = file-browser modal (backed by
  `/api/fs/list`) feeding the existing `classify_paths` → `add_files` /
  `confirm_folder_add` flow. The same modal replaces `pick_folder` in
  watched-dir setup. Browser drag-drop is not wired to intake (dropped
  browser files carry no server paths).
- **Login screen** on 401, storing nothing client-side (cookie is HttpOnly).
- Build outputs: `dist/` (desktop, `tauri.conf.json` untouched) and
  `dist-web/` (server, embedded into the binary via `rust-embed`).

## Docker

Multi-stage Dockerfile at the repo root:

1. `node` stage — `VITE_HEAD=server npm run build:web` → `dist-web/`
2. `rust` stage — `cargo build --release -p convertbar-server` (no GUI deps)
3. Runtime — Debian stable slim + `handbrake-cli` from apt (software
   x264/x265) + CA certs. Single binary + baked CLI; runs as any `--user`
   uid (document volume permissions), `EXPOSE 8080`, volume `/config`.

`docker-compose.example.yml` documents the NAS setup: `/config` volume,
media mounts, port mapping, `CONVERTBAR_AUTH_TOKEN` (always set), and one
caveat called out inline: the watcher's inotify backend is event-blind on
NFS/SMB mounts inside the container — watched folders should sit on
bind-mounted local disks (a polling fallback is a listed follow-up). The
stdout progress
parser (`parse_progress`, `converter.rs:304`) is format-stable across
HandBrake versions; the apt-packaged CLI version is verified by the
existing `validate_handbrake` startup check surfaced in the web UI.

## CI & publishing

- **`dist-web/` must exist at compile time**: `rust-embed`'s derive fails
  compilation when the folder is missing, and the rust CI jobs never build
  frontend assets. The server crate's `build.rs` runs
  `create_dir_all("dist-web")` (embedding an empty folder is fine for
  tests), so `cargo test --workspace` works on a fresh checkout with no
  Node step.
- `test.yml`: ubuntu legs run `cargo test --workspace` — the server crate
  rides the existing required `rust (ubuntu-22.04)` check. The Windows leg
  (main pushes) keeps `--lib` + its RUSTFLAGS and scopes to
  `-p convertbar -p convertbar-core`, as does `test-windows.yml` (the
  server crate has no Windows target).
- **Workflow mechanics that must follow the workspace split:** every
  rust-cache step's `workspaces: "./src-tauri -> target"` path changes to
  the workspace root in all four workflows (`target/` moves to the repo
  root; without this the caches silently cache nothing) — shared-key
  topology and single-writer-per-key stay as they are; `e2e-ignored.yml`'s
  `--manifest-path src-tauri/Cargo.toml` command follows the moved
  `#[ignore]` tests into the core crate; `test-windows.yml`'s `paths:`
  trigger adds `crates/**` so the Windows-fragile path-handling code keeps
  its advisory Windows run.
- New `docker.yml`: on `v*` tag push and `workflow_dispatch` — build amd64
  image, push `ghcr.io/rhurling/convertbar:{X.Y.Z,latest}` using
  `GITHUB_TOKEN` (`packages: write`). The tag comes from the existing
  `release.sh` flow; no release-script coupling beyond that. PRs touching
  the Dockerfile/server crate get an advisory build-only job (not a required
  check).
- `release.sh`: version source of truth moves to the workspace root
  `[workspace.package]`; the script bumps root `Cargo.toml` instead of
  `src-tauri/Cargo.toml` (plus `tauri.conf.json`, `package.json`,
  lockfiles as today; `Cargo.lock` refreshes during the rebuild step). Two
  mechanics follow the move: the failure-restore list and clean-tree
  preflight reference `src-tauri/Cargo.lock` (`release.sh:123-124`), which
  now lives at the repo root; and the root manifest must keep
  `[workspace.package]`'s `version =` as its first version-line match so
  the existing bump one-liner stays correct.

## Error handling

- Core `Result<_, String>` errors → HTTP 500 with `{ "error": "…" }` — no
  attempt to classify the string; the frontend surfaces the message exactly
  as it surfaces rejected invokes today. Malformed request bodies get
  axum's built-in 400/422.
- 401 → login screen; 403 from `fs/list` root escape.
- SSE disconnect → `EventSource` auto-reconnect + refetch on reopen.
- `fs/list` on unreadable/nonexistent paths → error entry, not a crash;
  paths canonicalized (symlink-resolved) before root checks.
- Encode-engine failure semantics (failure classification, bad-source
  review, stderr tails) are core behavior and unchanged.

## Testing

- **Core**: tests move with their modules; event assertions switch from
  `tauri::test::MockRuntime` + `Listener` to a plain `TestSink` collecting
  `(event, payload)` — strictly simpler. The 4 `#[ignore]`d
  HandBrake/ffmpeg integration tests move too and keep running under
  `e2e-ignored.yml`.
- **src-tauri**: keeps mock-runtime tests for the command adapters
  (emit-through, autostart/dialog paths).
- **Server**: axum integration tests via `tower::ServiceExt::oneshot`
  against a temp-db context — route smoke tests, auth middleware
  (with/without token, bad token, cookie flow), `fs/list` traversal
  attempts (`..`, symlinks out of root), and an SSE subscribe/emit test.
- **Contract test**: `src/test/ipc-contract.test.ts` keeps guarding
  invoke↔command and listen↔emit — its emit scan widens from `src-tauri/src`
  to the core crate (`ipc-contract.test.ts:40,66`), where all sink emits
  live after the move; a sibling asserts every HTTP-transport method
  matches a route in the server's route table and every `listen()` event
  name matches an emitted event.
- **Frontend**: existing Vitest suites keep mocking the transport interface;
  new tests for the file-browser modal, login screen, and capability
  gating.
- **Manual gate** (release-blocking for the feature, mirrors the platform
  smoke-test discipline): on the NAS — `docker compose up`, web UI loads,
  token login works, watched folder converts a dropped file end-to-end,
  file-browser add works, pause/resume mid-encode works, container restart
  auto-resumes.

## Out of scope (follow-ups)

- arm64 image; QSV/VAAPI/NVENC device passthrough (documented recipe later)
- Uploads; webhook/email notifications; multi-user auth; HTTPS (reverse
  proxy's job); auto-updating the container
- Polling fallback for watched folders on network filesystems (NFS/SMB)
- GHCR publishing from `release.sh` itself

## Acceptance criteria

1. `cargo test --workspace` and `npm test` green; desktop app builds and
   passes the existing suites with no behavior change.
2. `docker compose up` on the NAS serves the web UI; token auth is
   enforced, and with neither `CONVERTBAR_AUTH_TOKEN` nor
   `CONVERTBAR_NO_AUTH=1` the server refuses to start.
3. Watched-folder intake converts end-to-end in the container; ad-hoc add
   via the file browser works; SSE progress updates live without refresh.
4. Mid-encode pause/resume works in the container; restart auto-resumes.
5. The next `v*` release tag publishes `ghcr.io/rhurling/convertbar` (amd64)
   from CI.
