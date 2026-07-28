# Workspace Split & Head-Agnostic Core — Implementation Plan (Plan 1 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure ConvertBar into a Cargo workspace (`convertbar-core` + `src-tauri`) where all portable logic lives in a Tauri-free core crate behind three seams (EventSink, Ctx, FileDisposer), with the desktop app behaviorally unchanged — so Plan 2 can add `convertbar-server` without touching the engine.

**Architecture:** Pure refactor, green-to-green: every task ends with `cargo test --workspace` and `npm test` passing. Tauri stays only in `src-tauri` (thin command adapters + shell); the core crate owns the queue engine, watcher, db, probe, and settings logic, parameterized by `Arc<Ctx>` instead of `AppHandle`. Spec: `docs/superpowers/specs/2026-07-28-docker-web-ui-design.md`.

**Tech Stack:** Rust 2021, Tauri 2, rusqlite 0.40 (bundled), notify 8, React 19 + Vite (unchanged), Vitest.

## Global Constraints

- **Behavior-preserving.** Only two deliberate behavior changes in this whole plan: (1) `CONVERTBAR_DATA_DIR` env override for the data dir, (2) mid-encode pause widened from macOS-only to `cfg(unix)` (desktop Linux gains true pause). Everything else must behave identically — especially `claim_job` (atomic job claim), the cancel ordering contract (status write → kill → reap → delete partial), and the in-place cleanup guard (`in_place_temp_path`, never `remove_file(output_path)` on in-place jobs). These move verbatim; do not "improve" them.
- **Every task ends green:** `cargo test --workspace` AND `npm test` pass before the task's commit. No skipped tests.
- **Dependency versions are copied verbatim** from `src-tauri/Cargo.toml` — no version bumps in this plan.
- **`libc` call sites use `#[cfg(...)]` attributes, never the `cfg!()` macro** (the macro still links libc on every platform). `ConverterState::can_pause_process`'s runtime `cfg!()` boolean is the one legitimate `cfg!()` use.
- **Event names stay string literals at emit call sites** — the ipc-contract test greps for them.
- **Never edit the version fields by hand**; this plan changes *where* the version lives (Task 1), not its value (stays `1.0.0`).
- Commits: conventional (`refactor:`, `feat:`, `test:`, `ci:`, `docs:`), one per task, run from the worktree root. GPG signing is automatic via git config.
- Work happens in the current worktree (`.claude/worktrees/docker-web-ui-spec`, branch `chore/docker-web-ui-spec`). Use worktree-absolute paths in every file operation.
- `mod` declarations in `src-tauri/src/lib.rs` are replaced by `pub(crate) use convertbar_core::<mod>;` aliases as modules move, so `crate::db::…` paths inside remaining src-tauri files keep resolving — this is the churn-limiting trick the whole plan leans on.

## The dependency map (why the tasks are ordered this way)

Cross-module calls that constrain the move order (verified against the code; getting this wrong makes a task un-compilable because core can never reference src-tauri):

| Caller | Callee (defined in) | Consequence |
|---|---|---|
| `db.rs:140` | `converter::promote_stored_diagnostic` (converter.rs:614) | hoist to core `failure_class.rs` in Task 2, before db.rs moves |
| `converter.rs:70,92` | `commands::queue::file_identity` (queue.rs:83) | hoist to core `probe_cache.rs` in Task 2, before converter.rs moves (Task 4) |
| `queue.rs:826,397` | `commands::settings::{read_suffix_template, normalize_bad_source_action}` | hoist to core `settings_ops.rs` in Task 2, before queue moves (Task 5) |
| `queue.rs:866,885` | `commands::handbrake::cached_preset_metadata(&AppState, …)` (handbrake cmd :38, uses `preset_cache`) | preset_cache moves into `Ctx` and `cached_preset_metadata` into core handbrake.rs at the START of Task 5 |
| `watcher.rs:111,443,528,535` | `queue::{is_video_file, add_files_inner, scan_video_files}` | queue_ops (Task 5) moves BEFORE the watcher (Task 6) |
| `converter.rs:212–225` | `libc::kill` (macOS, non-test) | core needs the macOS libc dependency already in Task 4, not first in Task 7 |

Resulting order: scaffold → pure-helper hoists + portable modules → EventSink/AddOp → Ctx/disposer/converter → preset-cache+queue_ops → watcher → control (+unix pause) → settings wrappers → AppState removal → frontend shim → verification.

---

### Task 1: Workspace scaffold + CI/release plumbing

No crate moves yet — `src-tauri` becomes a workspace member, the version moves to `[workspace.package]`, and every path that assumed `src-tauri/target` or `src-tauri/Cargo.toml` is updated. All workflow edits land here (they are inert-but-correct while the workspace has one member).

**Files:**
- Create: `Cargo.toml` (workspace root)
- Modify: `src-tauri/Cargo.toml` (package section only), `.gitignore`, `scripts/release.sh`, `.github/workflows/test.yml`, `.github/workflows/test-windows.yml`, `.github/workflows/e2e-ignored.yml`, `.github/workflows/build.yml`, `.github/dependabot.yml`
- Move: `src-tauri/Cargo.lock` → `Cargo.lock`

**Interfaces:**
- Produces: workspace root whose `[workspace.package] version` is the single version source (`release.sh` bumps it); root `target/`; `cargo test --workspace` as the canonical test command.

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["src-tauri"]

[workspace.package]
version = "1.0.0"
edition = "2021"
authors = ["Rouven Hurling"]
license = "MIT"
repository = "https://github.com/rhurling/convertbar"
```

- [ ] **Step 2: Make `src-tauri/Cargo.toml` inherit workspace fields**

Replace its `[package]` section (keep everything from `[lib]` down untouched):

```toml
[package]
name = "convertbar"
version.workspace = true
description = "Menu bar app for batch video conversion using HandBrakeCLI"
authors.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
```

- [ ] **Step 3: Move the lockfile, ignore the root target dir, retarget dependabot**

```bash
git mv src-tauri/Cargo.lock Cargo.lock
```

Append to the root `.gitignore` (it currently has no target entry; the old one lives in `src-tauri/.gitignore`, which becomes vestigial but harmless):

```
# Cargo workspace build dir
/target/
```

In `.github/dependabot.yml`, the cargo ecosystem entry's `directory: "/src-tauri"` becomes `directory: "/"` — the lockfile and (after Task 2) half the dependencies live at the workspace root; leaving the old path silently degrades cargo updates.

- [ ] **Step 4: Verify the workspace builds and tests green**

Run: `cargo test --workspace`
Expected: all 238 tests pass (4 ignored), compiled into the root `target/`. If cargo complains about the lockfile, run `cargo generate-lockfile` — but the moved lock should be accepted as-is with only a workspace-structure touch.

- [ ] **Step 5: Update `scripts/release.sh` for the new version location**

In `bump_manifests()` (line ~105), change the Cargo.toml perl target:

```bash
  # Root Cargo.toml: first line-anchored version is [workspace.package] — the single
  # Rust version source; src-tauri inherits it via version.workspace = true.
  perl -0pi -e 's/^version = "[^"]*"/version = "'"$target"'"/m' Cargo.toml
```

In `build_app()` (lines ~123–124), update BOTH restore lists (the `git checkout --` command and the warning echo) to:

```
package.json package-lock.json src-tauri/tauri.conf.json Cargo.toml Cargo.lock
```

- [ ] **Step 6: Verify release.sh syntax and dry-run**

Run: `bash -n scripts/release.sh && ./scripts/release.sh 99.0.0 --dry-run`
Expected: syntax OK; dry-run prints the plan and exits without changing anything.

Also verify the perl one-liner's anchor assumption: `grep -n '^version = ' Cargo.toml` must match ONLY the `[workspace.package]` line.

- [ ] **Step 7: Update all four workflows**

`.github/workflows/test.yml`:
- rust-cache step: `workspaces: "./src-tauri -> target"` → `workspaces: ". -> target"`
- non-Windows run: `cargo test --manifest-path src-tauri/Cargo.toml` → `cargo test --workspace`
- Windows run (keep the RUSTFLAGS env exactly as-is; the manifest flag is harmless for core):

```yaml
      - run: cargo test --lib -p convertbar && cargo test --lib -p convertbar-core
        shell: bash
        if: runner.os == 'Windows'
        env:
          RUSTFLAGS: -Clink-arg=/MANIFEST:EMBED -Clink-arg=/MANIFESTINPUT:${{ github.workspace }}\src-tauri\windows-test-manifest.xml
```

(Two invocations, not `-p a -p b --lib`, so no dependence on cargo's multi-package target-selection behavior. `-p convertbar-core` fails until Task 2 creates the crate — acceptable: these commits never run in CI individually and no PR is opened mid-plan; the branch lands as one unit. `shell: bash` makes `&&` portable on the Windows runner. `-p convertbar` selects by *package* name; the lib being named `convertbar_lib` doesn't matter.)

`.github/workflows/test-windows.yml`:
- Same cache-path change; same two-invocation command change (keep its RUSTFLAGS).
- `paths:` trigger becomes:

```yaml
    paths:
      - "src-tauri/**"
      - "crates/**"
      - "Cargo.toml"
      - "Cargo.lock"
      - ".github/workflows/test-windows.yml"
```

`.github/workflows/e2e-ignored.yml`:
- Same cache-path change; `cargo test --manifest-path src-tauri/Cargo.toml -- --ignored` → `cargo test --workspace -- --ignored`

`.github/workflows/build.yml`:
- Cache-path change only (`workspaces: ". -> target"`). Its `save-if: false` stays.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: convert to cargo workspace with version in workspace.package"
```

---

### Task 2: Core crate — seven portable modules + the three pure-helper hoists

The hoists (dependency-map rows 1–3) are what make the later moves compile: `promote_stored_diagnostic`, `file_identity`, and the settings suffix helpers must reach core before `db.rs` lands there / before converter and queue move.

**Files:**
- Create: `crates/convertbar-core/Cargo.toml`, `crates/convertbar-core/src/lib.rs`, `crates/convertbar-core/src/settings_ops.rs`
- Move (git mv): `src-tauri/src/{types,failure_class,media_skip,probe,probe_cache,handbrake,db}.rs` → `crates/convertbar-core/src/`
- Modify: root `Cargo.toml` (members), `src-tauri/Cargo.toml` (dep), `src-tauri/src/lib.rs` (mod → use aliases), `src-tauri/src/converter.rs` (helper hoisted out), `src-tauri/src/commands/queue.rs` (helper hoisted out), `src-tauri/src/commands/settings.rs` (helpers hoisted out, re-exported), `src/test/ipc-contract.test.ts`
- Test: moved modules carry their own `#[cfg(test)]` tests; new test in `crates/convertbar-core/src/db.rs`

**Interfaces:**
- Produces: crate `convertbar-core` (lib name `convertbar_core`) exporting `pub mod types, failure_class, media_skip, probe, probe_cache, handbrake, db, settings_ops`.
- Produces (hoists — exact new homes):
  - `failure_class::promote_stored_diagnostic(message: &str) -> Option<String>` (from converter.rs:614, with its 2 tests at converter.rs:~2955/~2972)
  - `probe_cache::file_identity(path: &str) -> Option<FileIdentity>` (from commands/queue.rs:83 — its return type already lives in probe_cache)
  - `settings_ops::{DEFAULT_SUFFIX_TEMPLATE, read_suffix_template(conn, preset) -> String, normalize_bad_source_action(value) -> &'static str}` (from commands/settings.rs, with the 4 suffix/normalize tests)
- Produces: `db::get_db_path()` honoring `CONVERTBAR_DATA_DIR`; `db::get_db_path_from(override_base: Option<PathBuf>) -> PathBuf` (pure, testable).
- Consumes: workspace from Task 1.

- [ ] **Step 1: Scaffold the crate**

`crates/convertbar-core/Cargo.toml`:

```toml
[package]
name = "convertbar-core"
description = "Head-agnostic core for ConvertBar: queue engine, watcher, db, probe"
version.workspace = true
authors.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[lib]
name = "convertbar_core"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.40", features = ["bundled"] }
uuid = { version = "1", features = ["v4"] }
regex = "1"
dirs = "6"
chrono = { version = "0.4", features = ["serde"] }
dunce = "1.0.5"
notify = "8"
fs4 = { version = "0.13", features = ["sync"] }

[dev-dependencies]
tempfile = "3"
```

Root `Cargo.toml`: `members = ["crates/convertbar-core", "src-tauri"]`.
`src-tauri/Cargo.toml` `[dependencies]`: add `convertbar-core = { path = "../crates/convertbar-core" }`.

- [ ] **Step 2: Move the seven modules**

```bash
mkdir -p crates/convertbar-core/src
for m in types failure_class media_skip probe probe_cache handbrake db; do
  git mv "src-tauri/src/$m.rs" "crates/convertbar-core/src/$m.rs"
done
```

`crates/convertbar-core/src/lib.rs`:

```rust
pub mod db;
pub mod failure_class;
pub mod handbrake;
pub mod media_skip;
pub mod probe;
pub mod probe_cache;
pub mod settings_ops;
pub mod types;
```

- [ ] **Step 3: The three hoists**

1. Cut `promote_stored_diagnostic` (+ its doc comment) from `src-tauri/src/converter.rs:614` into `crates/convertbar-core/src/failure_class.rs`; move its two tests along. `db.rs:140`'s call becomes `crate::failure_class::promote_stored_diagnostic(...)`. In converter.rs, replace the definition with nothing and fix its intra-file callers to `crate::failure_class::promote_stored_diagnostic` (resolves via the Step 4 alias until converter itself moves).
2. Cut `file_identity` from `src-tauri/src/commands/queue.rs:83` into `crates/convertbar-core/src/probe_cache.rs`; in queue.rs add `pub(crate) use crate::probe_cache::file_identity;` so `converter.rs:70,92`'s `crate::commands::queue::file_identity` calls and queue.rs's own uses keep resolving unchanged.
3. Create `crates/convertbar-core/src/settings_ops.rs` containing `DEFAULT_SUFFIX_TEMPLATE`, `read_suffix_template`, `normalize_bad_source_action`, **and `ALLOWED_KEYS`** (cut from `commands/settings.rs`, doc comments intact) plus the 4 tests that cover them — the fourth test (`bad_source_action_is_writable_…`) asserts against `ALLOWED_KEYS`, which is why the const must hoist now, not in Task 8. In `commands/settings.rs` add:

```rust
pub(crate) use convertbar_core::settings_ops::{
    normalize_bad_source_action, read_suffix_template, ALLOWED_KEYS, DEFAULT_SUFFIX_TEMPLATE,
};
```

so `commands/queue.rs:826,397`'s `crate::commands::settings::…` paths keep resolving.

- [ ] **Step 4: Fix visibility and cross-crate references (scripted)**

```bash
# pub(crate) has no meaning across the crate boundary — the old consumers now live in src-tauri.
sed -i '' 's/pub(crate) /pub /g' crates/convertbar-core/src/*.rs
```

In `src-tauri/src/lib.rs`, delete the seven `mod` lines and add (top of file):

```rust
pub(crate) use convertbar_core::{db, failure_class, handbrake, media_skip, probe, probe_cache, types};
```

This keeps every `crate::db::…` / `crate::types::…` path in the remaining src-tauri files compiling unchanged. Fix any residual errors the compiler reports.

- [ ] **Step 5: Run the suite**

Run: `cargo test --workspace`
Expected: same total test count (moved, not lost), now split between two crates.

- [ ] **Step 6: Write the failing test for the data-dir override (TDD — new behavior)**

Append to the `tests` module in `crates/convertbar-core/src/db.rs`:

```rust
#[test]
fn get_db_path_from_prefers_the_override_base() {
    let dir = tempfile::tempdir().unwrap();
    let p = get_db_path_from(Some(dir.path().to_path_buf()));
    assert_eq!(p, dir.path().join("convertbar.db"));
    assert!(dir.path().exists(), "base dir is created");
}

#[test]
fn get_db_path_from_falls_back_to_platform_data_dir() {
    let p = get_db_path_from(None);
    let s = p.to_string_lossy();
    assert!(s.contains("com.convertbar.app") && s.ends_with("convertbar.db"));
}
```

Run: `cargo test -p convertbar-core get_db_path_from`
Expected: FAIL — `get_db_path_from` not found.

- [ ] **Step 7: Implement the override**

Refactor `db::get_db_path` (currently a hardcoded `dirs::data_dir()` resolver at `db.rs:52`) into:

```rust
/// Resolve the data dir: an explicit base (from CONVERTBAR_DATA_DIR) wins; otherwise the
/// platform data dir + com.convertbar.app. Creates the directory either way.
pub fn get_db_path_from(override_base: Option<PathBuf>) -> PathBuf {
    let base = override_base.unwrap_or_else(|| {
        dirs::data_dir()
            .expect("Failed to resolve data directory")
            .join("com.convertbar.app")
    });
    std::fs::create_dir_all(&base).expect("Failed to create data directory");
    base.join("convertbar.db")
}

pub fn get_db_path() -> PathBuf {
    get_db_path_from(std::env::var_os("CONVERTBAR_DATA_DIR").map(PathBuf::from))
}
```

Preserve the original function's exact `expect` messages if they differ — read the current body first and keep its strings. The env var is read only here (no test mutates process env — the pure fn is the tested surface).

Run: `cargo test -p convertbar-core` → PASS.

- [ ] **Step 8: Widen the ipc-contract scan (frontend stays green as later tasks move emit sites)**

In `src/test/ipc-contract.test.ts`:

```ts
const rustFiles = [
  ...walk(join(root, "src-tauri", "src"), [".rs"]),
  ...walk(join(root, "crates", "convertbar-core", "src"), [".rs"]),
];
```

and widen the emit regex (line 66) so the upcoming `EventSink::emit` / `emit_t` call sites still register:

```ts
const emittedEvents = collect(rustFiles, /\.emit(?:_t|_to|_filter)?\s*\(\s*"([^"]+)"/g);
```

Run: `npm test` → PASS (surface counts unchanged today; the scan is now future-proof).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: extract portable modules and pure helpers into convertbar-core"
```

---

### Task 3: EventSink seam + AddOp port

**Files:**
- Create: `crates/convertbar-core/src/events.rs`, `src-tauri/src/sink.rs`
- Move: `src-tauri/src/add_progress.rs` → `crates/convertbar-core/src/add_progress.rs` (tests rewritten onto `TestSink`)
- Modify: `crates/convertbar-core/src/lib.rs`, `src-tauri/src/lib.rs`, the three `AddOp::new` call sites (`src-tauri/src/watcher.rs:441`, `src-tauri/src/commands/queue.rs` in `add_files` and `confirm_folder_add`)

**Interfaces:**
- Produces (core): `trait EventSink { fn emit(&self, event: &str, payload: serde_json::Value); fn notify(&self, title: &str, body: &str); }`; `trait EventSinkExt { fn emit_t<T: Serialize>(&self, event: &str, payload: T); }` (blanket impl); `pub struct TestSink { pub events: Mutex<Vec<(String, serde_json::Value)>>, pub notifications: Mutex<Vec<(String, String)>> }` with `fn payloads(&self, name: &str) -> Vec<serde_json::Value>`; `AddOp::new(events: Arc<dyn EventSink>, label: String)`.
- Produces (src-tauri): `pub struct TauriSink<R: tauri::Runtime>(pub tauri::AppHandle<R>);` implementing `EventSink`.

- [ ] **Step 1: Write the core events module with its tests first**

`crates/convertbar-core/src/events.rs`:

```rust
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
        self.events.lock().unwrap().push((event.to_string(), payload));
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
        struct P { x: u32 }
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
        assert_eq!(sink.notifications.lock().unwrap()[0], ("T".into(), "B".into()));
    }
}
```

Add `pub mod events;` to core `lib.rs`. Run: `cargo test -p convertbar-core events` → PASS.

- [ ] **Step 2: Port AddOp to the sink**

`git mv src-tauri/src/add_progress.rs crates/convertbar-core/src/add_progress.rs`; add `pub mod add_progress;` to core lib.rs. Rewrite the struct — the three payload structs and every emitted field stay byte-identical; only the transport handle changes:

```rust
use crate::events::{EventSink, EventSinkExt};
use std::sync::Arc;

pub struct AddOp {
    events: Arc<dyn EventSink>,
    op_id: String,
    label: String,
}

impl AddOp {
    pub fn new(events: Arc<dyn EventSink>, label: String) -> Self {
        let op_id = uuid::Uuid::new_v4().to_string();
        events.emit_t("add-started", StartedPayload { op_id: op_id.clone(), label: label.clone() });
        Self { events, op_id, label }
    }

    pub fn report(&self, done: u32, total: u32) {
        self.events.emit_t("add-progress", ProgressPayload {
            op_id: self.op_id.clone(), label: self.label.clone(), done, total,
        });
    }
}

impl Drop for AddOp {
    fn drop(&mut self) {
        self.events.emit_t("add-finished", FinishedPayload { op_id: self.op_id.clone() });
    }
}
```

Rewrite its three tests onto `TestSink` — same assertions, e.g.:

```rust
#[test]
fn emits_started_with_label_on_new_and_finished_on_drop() {
    let sink = Arc::new(TestSink::default());
    {
        let _op = AddOp::new(sink.clone(), "My Folder".to_string());
        assert_eq!(sink.payloads("add-started").len(), 1);
        assert_eq!(sink.payloads("add-started")[0]["label"], "My Folder");
        assert!(sink.payloads("add-finished").is_empty());
    }
    assert_eq!(sink.payloads("add-finished").len(), 1);
}
```

(Keep the early-return test and the report test, same translation. The RAII drop-fires-on-panic property is unchanged.)

- [ ] **Step 3: Desktop TauriSink with an emit-through adapter test**

`src-tauri/src/sink.rs`:

```rust
use convertbar_core::events::EventSink;
use tauri::Emitter;

pub struct TauriSink<R: tauri::Runtime>(pub tauri::AppHandle<R>);

impl<R: tauri::Runtime> EventSink for TauriSink<R> {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
    fn notify(&self, title: &str, body: &str) {
        use tauri_plugin_notification::NotificationExt;
        let _ = self
            .0
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    #[test]
    fn emit_passes_through_to_tauri_events() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_store = seen.clone();
        app.listen_any("probe-event", move |e| {
            sink_store.lock().unwrap().push(e.payload().to_string());
        });
        let sink = TauriSink(app.handle().clone());
        sink.emit("probe-event", serde_json::json!({ "k": 1 }));
        assert_eq!(seen.lock().unwrap().len(), 1);
        // notify is not observable under MockRuntime (no notification plugin) — the
        // desktop notify path keeps its existing manual verification.
    }
}
```

Add `mod sink;` to `src-tauri/src/lib.rs`.

- [ ] **Step 4: Fix the three AddOp callers (transitional sinks)**

At `watcher.rs:441` and the two `commands/queue.rs` sites, replace `crate::add_progress::AddOp::new(app, label)` with:

```rust
let sink: std::sync::Arc<dyn convertbar_core::events::EventSink> =
    std::sync::Arc::new(crate::sink::TauriSink(app.clone()));
let op = convertbar_core::add_progress::AddOp::new(sink, label);
```

(These become `ctx.events` in Tasks 5–6; the ad-hoc construction is deliberate scaffolding.) Also update `src-tauri/src/lib.rs`: drop `mod add_progress;`, add `add_progress` to the `pub(crate) use convertbar_core::{…}` alias list.

- [ ] **Step 5: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS.

```bash
git add -A
git commit -m "refactor: introduce EventSink seam; port AddOp to convertbar-core"
```

---

### Task 4: Ctx + FileDisposer + move the converter engine

The heart of the refactor. `converter.rs` (3569 lines, 67 tests) moves to core; `AppHandle<R>` threading becomes `&Arc<Ctx>`; the three notification sends become `ctx.events.notify`; the three trash sites become `ctx.disposer`.

**Files:**
- Create: `crates/convertbar-core/src/ctx.rs`, `crates/convertbar-core/src/dispose.rs`
- Move: `src-tauri/src/converter.rs` → `crates/convertbar-core/src/converter.rs`
- Modify: core `lib.rs` + `Cargo.toml` (libc entries), `src-tauri/src/lib.rs` (construct Ctx, dual-manage), `src-tauri/src/sink.rs` (add TrashDisposer), `src-tauri/src/commands/converter.rs` (start_queue only + its test), `src-tauri/src/watcher.rs` (run_queue call site)

**Interfaces:**
- Produces (core `ctx.rs`):

```rust
pub struct Ctx {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub converter: Arc<crate::converter::ConverterState>,
    pub events: Arc<dyn crate::events::EventSink>,
    pub disposer: Arc<dyn crate::dispose::FileDisposer>,
}

impl Ctx {
    pub fn new(
        conn: rusqlite::Connection,
        events: Arc<dyn crate::events::EventSink>,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            converter: Arc::new(crate::converter::ConverterState::new()),
            events,
            disposer,
        })
    }
}
```

  (`preset_cache` joins in Task 5, `watcher` in Task 6 — the struct grows as owners move.)
- Produces (core `dispose.rs`):

```rust
/// The trash primitive, injected per head: desktop = OS trash, server = permanent delete.
/// The cleanup_mode / bad_source_action DECISION logic stays in core and is unchanged.
pub trait FileDisposer: Send + Sync {
    /// Returns true on success (matches the bool contract of trash_delete_primitive).
    fn dispose(&self, path: &str) -> bool;
}

pub struct DeleteDisposer;
impl FileDisposer for DeleteDisposer {
    fn dispose(&self, path: &str) -> bool {
        std::fs::remove_file(path).is_ok()
    }
}

/// Test disposer: records what was disposed, then deletes — the test-harness default,
/// and the behavior queue_ops' old #[cfg(test)] trash stub relied on (Task 5).
#[derive(Default)]
pub struct RecordingDisposer(pub std::sync::Mutex<Vec<String>>);
impl FileDisposer for RecordingDisposer {
    fn dispose(&self, path: &str) -> bool {
        self.0.lock().unwrap().push(path.to_string());
        std::fs::remove_file(path).is_ok()
    }
}
```

- Produces (core `converter.rs`): `pub fn run_queue(ctx: Arc<Ctx>)`, `fn process_queue(ctx: &Ctx)`, `record_job_error*(ctx: &Ctx, …)`, everything else keeping its current signature minus the `app`/`R` parameter. Test harness:

```rust
pub? (test-mod-local) fn test_ctx(conn: rusqlite::Connection) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>)
```

  (define it in converter.rs's test module; later tasks' test modules re-declare the same 6-line helper locally rather than sharing test code across modules).
- Produces (src-tauri `sink.rs`): `pub struct TrashDisposer;` → `impl FileDisposer` via `trash::delete(path).is_ok()`.
- Consumes: `EventSink`/`TestSink` (Task 3), `probe_cache::file_identity` + `failure_class::promote_stored_diagnostic` (Task 2 hoists).

- [ ] **Step 1: Create `ctx.rs` and `dispose.rs`** exactly as in the Interfaces block; add `pub mod ctx; pub mod dispose;` to core lib.rs. Add to core `Cargo.toml` BOTH libc entries — the moved `kill_active_child` (converter.rs:212–225) is macOS *production* code, and the geteuid test needs unix-wide dev coverage:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"

[target.'cfg(unix)'.dev-dependencies]
# geteuid() for the converter no-read-permission test (skipped as root). The production
# entry above widens to cfg(unix) in the pause task, which then subsumes this one.
libc = "0.2"
```

- [ ] **Step 2: Move and de-Taurify the engine**

`git mv src-tauri/src/converter.rs crates/convertbar-core/src/converter.rs`; `pub(crate) → pub` sweep (`sed -i '' 's/pub(crate) /pub /g'` on the file); add `pub mod converter;` to core lib.rs; in src-tauri lib.rs replace `mod converter;` with the alias-list entry. Fix the Task 2 hoist call sites: `crate::commands::queue::file_identity` → `crate::probe_cache::file_identity`.

Mechanical rules (apply throughout the file):
1. `fn f<R: tauri::Runtime>(app: &AppHandle<R>, db: &Arc<Mutex<Connection>>, converter: &ConverterState, …)` → `fn f(ctx: &Ctx, …)`; inside, `app` uses become `ctx.events`, `db` → `&ctx.db`, `converter` → `&ctx.converter`.
2. `let _ = app.emit("name", payload);` → `ctx.events.emit_t("name", payload);` (12 sites; import `crate::events::EventSinkExt`).
3. The three `.notification().builder().title(T).body(B).show()` chains → `ctx.events.notify(T, B);` with the exact same title/body expressions (the settings-based gating around them is untouched).
4. `trash::delete(path)` sites (`apply_in_place_action` ~:137, post-encode source cleanup ~:1143, kept-original cleanup ~:1151) → `ctx.disposer.dispose(path)` with the same success/failure handling each site has today.
5. `run_queue` becomes `pub fn run_queue(ctx: Arc<Ctx>)` and spawns its thread with the moved `ctx`.
6. Remove the now-unused `use tauri::…` imports and the `tauri_plugin_notification` import.

- [ ] **Step 3: Rewrite the converter tests' harness**

Define `test_ctx` (Interfaces block) in the test module. Every test that built a MockRuntime app now builds a ctx; event assertions switch from `Listener`-recorded strings to `sink.payloads("event-name")`. Tests that previously could NOT observe notifications under MockRuntime can now assert `sink.notifications` — add those assertions where a test already exercises a notification-gated path, but do not write new scenario tests (this is a refactor). Note: converter's own trash-mode tests fail the encode before cleanup runs, so no converter test observes the disposer today — RecordingDisposer is simply the harness default.

- [ ] **Step 4: Wire the desktop shell**

In `src-tauri/src/lib.rs` `run()`: move DB opening into `.setup()` (capture `conn` in the closure) and construct:

```rust
let events: Arc<dyn EventSink> = Arc::new(sink::TauriSink(app.handle().clone()));
let ctx = Ctx::new(conn, events, Arc::new(sink::TrashDisposer));
app.manage(ctx.clone());
// Transitional dual-manage — same Arcs, so there is one db and one ConverterState:
app.manage(AppState { db: ctx.db.clone(), preset_cache: Mutex::new(HashMap::new()) });
app.manage(ctx.converter.clone());
```

Auto-resume block: `converter::run_queue(ctx.clone())`. Exit handler: `app.state::<Arc<Ctx>>()` → `converter::kill_active_child(&ctx.converter)`.

**Signature changes in this task are limited to `start_queue`** (forced by `run_queue`'s new signature): it becomes `start_queue(ctx: State<'_, Arc<Ctx>>)` and its test (`start_queue_clears_the_persisted_pause`, commands/converter.rs:~508) changes in two places: the scaffolding becomes `let ctx = Ctx::new(conn, Arc::new(TestSink::default()), Arc::new(DeleteDisposer)); app.manage(ctx.clone());`, and the later assertion fetch (`let state: State<'_, AppState> = app.state();` at ~:535) is replaced by querying `ctx.db` directly — `AppState` is no longer managed by this test, so the old fetch would panic. `cancel_conversion`, `pause/resume`, and queue.rs's `clear_queue` keep their `State<AppState>` / `State<Arc<ConverterState>>` signatures — the dual-manage serves exactly them — and their mock tests stay untouched until their own move tasks.

`src-tauri/src/watcher.rs` (still desktop, moves in Task 6): its `run_queue(app.clone(), db, conv)` call in `enqueue_and_start` becomes:

```rust
let ctx = app.state::<std::sync::Arc<convertbar_core::ctx::Ctx>>().inner().clone();
convertbar_core::converter::run_queue(ctx);
```

Add `TrashDisposer` to `sink.rs`:

```rust
pub struct TrashDisposer;
impl convertbar_core::dispose::FileDisposer for TrashDisposer {
    fn dispose(&self, path: &str) -> bool {
        trash::delete(path).is_ok()
    }
}
```

- [ ] **Step 5: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS, no test-count loss (67 converter tests now in core).

```bash
git add -A
git commit -m "refactor: move converter engine to core behind Ctx/EventSink/FileDisposer"
```

---

### Task 5: preset cache into Ctx, then split commands/queue.rs → core queue_ops

Order inside the task matters: `add_files_inner` calls `cached_preset_metadata` (queue.rs:866,885), which lives in `commands/handbrake.rs:38` and reads `AppState.preset_cache` — so the cache and that function move to core first, then the queue logic follows.

**Files:**
- Create: `crates/convertbar-core/src/queue_ops.rs`
- Modify: core `ctx.rs` (+`preset_cache`), core `handbrake.rs` (+`cached_preset_metadata`, `resolve_handbrake_path`), `src-tauri/src/commands/handbrake.rs` (wrappers onto ctx), `src-tauri/src/commands/queue.rs` (becomes thin wrappers), `src-tauri/src/watcher.rs` (queue fn paths), core `lib.rs`

**Interfaces:**
- Produces: `Ctx.preset_cache: Mutex<HashMap<String, crate::handbrake::PresetMetadata>>` (init `Mutex::new(HashMap::new())` in `Ctx::new`).
- Produces (core `handbrake.rs`): `pub fn cached_preset_metadata(ctx: &Ctx, hb_path: &str, preset: &str) -> Result<PresetMetadata, String>` and `pub fn resolve_handbrake_path(ctx: &Ctx) -> Result<Option<String>, String>` — same bodies as today's `commands/handbrake.rs` versions with `&AppState` → `&Ctx`, keeping the parameter lists otherwise IDENTICAL: `hb_path` stays a parameter (callers resolve the path once; folding resolution in would add a DB lock + `which` shell-out per cache miss), and the `Option` return is load-bearing (`detect_handbrake` returns `Ok(None)`; `validate_handbrake` maps `None` to `HandbrakeStatus { found: false, .. }`).
- Produces (core `queue_ops.rs`), signatures the wrappers and Plan 2's routes both call:

```rust
pub fn add_files(ctx: &Arc<Ctx>, paths: &[String]) -> Result<AddResult, String>      // AddOp + add_files_inner + queue-updated emit
pub fn scan_folder(path: String) -> Result<FolderScanResult, String>
pub fn confirm_folder_add(ctx: &Arc<Ctx>, path: String) -> Result<AddResult, String>
pub fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String>
pub fn get_queue(ctx: &Ctx) -> Result<Vec<JobInfo>, String>
pub fn remove_job(ctx: &Ctx, id: &str) -> Result<(), String>
pub fn remove_history_entry(ctx: &Ctx, id: &str) -> Result<(), String>
pub fn reorder_queue(ctx: &Ctx, job_ids: &[String]) -> Result<(), String>
pub fn clear_completed(ctx: &Ctx, mode: &str) -> Result<(), String>
pub fn clear_queue(ctx: &Ctx) -> Result<(), String>
pub fn get_bad_sources(ctx: &Ctx) -> Result<Vec<JobInfo>, String>
pub fn purge_bad_sources(ctx: &Arc<Ctx>, ids: Vec<String>) -> Result<Vec<PurgeResult>, String>
pub fn get_history(ctx: &Ctx, limit: u32, offset: u32, search: Option<String>, sort_by: Option<String>) -> Result<HistoryPage, String>
pub fn get_history_summary(ctx: &Ctx, search: Option<String>) -> Result<HistorySummary, String>
pub fn is_video_file(path: &Path) -> bool          // unchanged, needed by watcher in Task 6
pub fn scan_video_files(dir: &Path) -> Vec<PathBuf> // unchanged, needed by watcher in Task 6
pub fn add_files_inner(ctx: &Ctx, paths: &[String], progress: Option<&dyn Fn(u32, u32)>) -> Result<AddResult, String>  // &AppState → &Ctx
```

  All result types (`AddResult`, `FolderScanResult`, `ClassifiedPaths`, `HistoryPage`, `HistorySummary`, `PurgeResult`, `JobInfo`) already live in core `types.rs` — import from there.
- Consumes: `Ctx`, `AddOp`, `FileDisposer`, `settings_ops` helpers (Task 2), `probe_cache::file_identity` (Task 2). The `trash_delete_primitive` body becomes `ctx.disposer.dispose(path)`; its `#[cfg(test)]` remove_file stub is deleted — tests inject `RecordingDisposer`, which preserves the record+delete behavior.

- [ ] **Step 1: Move the preset cache and the two handbrake command helpers**

Add the `preset_cache` field to `Ctx`. Cut `cached_preset_metadata` and `resolve_handbrake_path` from `src-tauri/src/commands/handbrake.rs` into core `handbrake.rs`, re-parameterized `&AppState` → `&Ctx` (`state.preset_cache` → `ctx.preset_cache`, `state.db` → `ctx.db`). Update `commands/handbrake.rs`'s five `#[tauri::command]`s to fetch `State<'_, Arc<Ctx>>` and delegate (keeping their `async` + `spawn_blocking` bodies — the blocking probe must stay off the main thread). Move any tests covering the two helpers along; command-level tests keep working against the wrappers.

- [ ] **Step 2: Move the queue file, then carve out the wrappers**

`git mv src-tauri/src/commands/queue.rs crates/convertbar-core/src/queue_ops.rs`; `pub(crate) → pub` sweep; core lib.rs `pub mod queue_ops;`. Inside `queue_ops.rs`:
- `crate::commands::settings::…` → `crate::settings_ops::…`; `super::handbrake::cached_preset_metadata(state, …)` → `crate::handbrake::cached_preset_metadata(ctx, …)`; the local `pub(crate) use` re-export of `file_identity` from Task 2 is deleted (callers use `crate::probe_cache::file_identity`).
- `add_files_inner(state: &AppState, …)` → `add_files_inner(ctx: &Ctx, …)`.
- Delete the `#[tauri::command]` wrappers and their `AppHandle`/`State` imports; `add_files`/`confirm_folder_add` absorb the AddOp + `queue-updated` emit logic via `ctx.events` (`AddOp::new(ctx.events.clone(), label)`).
- `trash_delete_primitive`'s body → `ctx.disposer.dispose(path)`; delete the `#[cfg(test)]` stub.

Then create a fresh `src-tauri/src/commands/queue.rs` containing ONLY thin wrappers:

```rust
use convertbar_core::ctx::Ctx;
use convertbar_core::queue_ops;
use convertbar_core::types::AddResult;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn add_files(ctx: State<'_, Arc<Ctx>>, paths: Vec<String>) -> Result<AddResult, String> {
    let ctx = ctx.inner().clone();
    // spawn_blocking is load-bearing: add_files probes every file; on the main thread it
    // freezes the UI (see the 4-entry-point probe-hazard fix).
    tauri::async_runtime::spawn_blocking(move || queue_ops::add_files(&ctx, &paths))
        .await
        .map_err(|e| e.to_string())?
}
```

Repeat the pattern for every command in the Interfaces list (async + spawn_blocking exactly where the current file is async + spawn_blocking: `add_files`, `scan_folder`, `confirm_folder_add`, `classify_paths`, `purge_bad_sources`; the rest stay sync). `clear_queue` drops its `State<Arc<ConverterState>>` parameter — core `clear_queue(ctx)` reads `ctx.converter`.

Watcher call sites (`watcher.rs:111,443,528,535`, still desktop until Task 6): `queue::is_video_file` / `queue::scan_video_files` / `queue::add_files_inner` → `convertbar_core::queue_ops::…`, with `add_files_inner` taking the ctx fetched from state (the Task 4 pattern).

- [ ] **Step 3: Relocate the two mock-runtime tests**

The two mock-app tests in the old queue.rs (~:2456, ~:2498) are **clear_queue** tests (low-disk-pause drop on clear; persisted-pause clear). They assert core logic — rewrite them in `queue_ops.rs` on a locally-declared `test_ctx` harness (same 6-line helper as Task 4) and delete the mock-app scaffolding. The remaining ~55 tests move as-is (they already test `_inner` fns with plain Connections); tests touching purge/trash now construct `RecordingDisposer` and assert against its recorded paths where they previously relied on the cfg(test) stub's deletions.

- [ ] **Step 4: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS. The ipc-contract test still finds every command name (wrappers keep the exact fn names) and every emit (core scan).

```bash
git add -A
git commit -m "refactor: move preset cache and queue logic into core queue_ops"
```

---

### Task 6: Move the watcher onto Ctx

Safe now: everything the watcher calls (`queue_ops`, `run_queue`, `AddOp`, settings helpers) already lives in core.

**Files:**
- Move: `src-tauri/src/watcher.rs` → `crates/convertbar-core/src/watcher.rs`
- Modify: core `ctx.rs` (+`watcher` field), core `lib.rs`, `src-tauri/src/lib.rs` (start call, drop WatcherState manage), `src-tauri/src/commands/watch.rs` (State<Arc<Ctx>>), `src-tauri/src/commands/settings.rs` (refresh call)

**Interfaces:**
- Produces: `Ctx.watcher: crate::watcher::WatcherState` (constructed in `Ctx::new` via `WatcherState::new()`); `watcher::start(ctx: Arc<Ctx>)`; `watcher::refresh_skip_marker(ctx: &Ctx)`; `watcher::reconcile(ctx: &Arc<Ctx>)`; `watcher::scan_existing_background(ctx: &Arc<Ctx>, dir: PathBuf, recursive: bool)`; internal fns (`enqueue_and_start`, `filter_*`, `read_enabled_configs`, `spawn_reaper`) take `&Arc<Ctx>`.
- Produces: `WatcherState.skip_marker` becomes `pub` (it currently has no visibility modifier at watcher.rs:205, so the sed sweep misses it; Task 8's settings test reads it through `ctx.watcher`).
- Consumes: `Ctx`, `EventSink`, `AddOp`, `queue_ops` from Tasks 3–5.

- [ ] **Step 1: Move + sweep** (`git mv`, `pub(crate) → pub` sed, core lib.rs `pub mod watcher;`, src-tauri alias list). Explicitly make the `skip_marker` field `pub` (the sed does not catch bare private fields).

- [ ] **Step 2: Replace the service locator**

Mechanical rules:
- Every `app: &AppHandle` / `app: AppHandle` parameter → `ctx: &Arc<Ctx>` / `ctx: Arc<Ctx>`.
- `app.state::<WatcherState>()` → `&ctx.watcher`; `app.state::<AppState>()` (db access) → `&ctx.db`; `app.state::<Arc<ConverterState>>()` → `&ctx.converter`; the Task 4/5 transitional `app.state::<Arc<Ctx>>()` fetches collapse to the `ctx` parameter.
- The transitional AddOp sink from Task 3 → `AddOp::new(ctx.events.clone(), label)`.
- `app.emit("queue-updated", ())` → `ctx.events.emit_t("queue-updated", ())`.
- `spawn_reaper` / `scan_existing_background` threads capture `Arc<Ctx>` clones instead of AppHandle clones.

Add the field to `Ctx`: `pub watcher: crate::watcher::WatcherState,` initialized with `WatcherState::new()` in `Ctx::new`.

- [ ] **Step 3: Update desktop callers**

`src-tauri/src/lib.rs`: drop `.manage(watcher::WatcherState::new())`; `watcher::start(app.handle().clone())` → `watcher::start(ctx.clone())`. `commands/watch.rs`: the five CRUD commands switch to `State<'_, Arc<Ctx>>` and call the core fns; `pick_folder` keeps its `AppHandle` (dialog plugin, desktop-only). `commands/settings.rs`'s `refresh_skip_marker(&app)` call → fetch `State<Arc<Ctx>>` and pass `&ctx` (interim; the body moves in Task 8).

- [ ] **Step 4: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS (34 watcher tests now in core).

```bash
git add -A
git commit -m "refactor: move watcher to core, replace app.state lookups with Ctx"
```

---

### Task 7: control.rs — de-Taurify pause/resume/cancel/start + widen pause to cfg(unix)

**Files:**
- Create: `crates/convertbar-core/src/control.rs`
- Modify: `src-tauri/src/commands/converter.rs` (thin wrappers + the desktop-only commands), core `converter.rs` (`kill_active_child`, `can_pause_process`), core `Cargo.toml` (libc widening), core `lib.rs`

**Interfaces:**
- Produces (core `control.rs`):

```rust
pub fn start_queue(ctx: &Arc<Ctx>) -> Result<(), String>
pub fn pause_conversion(ctx: &Ctx) -> Result<(), String>      // SIGSTOP under #[cfg(unix)]
pub fn resume_conversion(ctx: &Ctx) -> Result<(), String>     // SIGCONT under #[cfg(unix)]
pub fn cancel_conversion(ctx: &Ctx) -> Result<(), String>
pub fn pause_after_current(ctx: &Ctx) -> Result<(), String>
pub fn cancel_pause_after_current(ctx: &Ctx) -> Result<(), String>
pub fn get_pause_after_current(ctx: &Ctx) -> bool
pub fn get_low_disk_pause(ctx: &Ctx) -> Option<LowDiskPause>
```

- Consumes: `Ctx`, `EventSinkExt`. The bodies are the current command bodies (`src-tauri/src/commands/converter.rs:8–384`) with `State<…>` params replaced by `ctx` fields and emits going through `ctx.events.emit_t`.

- [ ] **Step 1: Write the platform test first (TDD — the one new behavior)**

In the new `control.rs` tests module:

```rust
#[test]
fn pause_capability_is_unix_wide() {
    // Widened from macOS-only: the Linux container (and desktop Linux) get true
    // mid-encode pause; Windows keeps the pause-after-current fallback.
    assert_eq!(
        crate::converter::ConverterState::can_pause_process(),
        cfg!(unix)
    );
}
```

Run: `cargo test -p convertbar-core pause_capability`
Honest caveat: on macOS (the dev machine) this passes both before and after the change — `cfg!(target_os = "macos")` and `cfg!(unix)` are both true here. Its red→green proof lives on the ubuntu CI leg, where the pre-change code returns `false`. Write it anyway; it pins the contract on every platform from now on.

- [ ] **Step 2: Implement**

- Move the four command bodies into `control.rs` per the mechanical rules of Task 4 (State→ctx, emit→emit_t). The `MenuBarUpdate` emits keep their exact payloads.
- Change every `#[cfg(target_os = "macos")]` signal block in `control.rs` and the one in `converter::kill_active_child` (SIGCONT-before-kill) to `#[cfg(unix)]`; the paired attributes — written in the code as `#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]` (commands/converter.rs:34,111) — become `#[cfg_attr(not(unix), allow(unused_variables))]`.
- `ConverterState::can_pause_process()` → `cfg!(unix)` (runtime boolean — the legitimate `cfg!` use).
- Core `Cargo.toml`: replace the macOS dependency + unix dev-dependency pair from Task 4 with a single entry that covers both production and test call sites:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- `src-tauri/Cargo.toml`: verify no libc entries remain (both were removed when their last src-tauri call sites moved; if either survives, delete it — no libc call sites remain in src-tauri).
- `src-tauri/src/commands/converter.rs` shrinks to: thin wrappers over `control::*` (each `#[tauri::command]` fetches `State<'_, Arc<Ctx>>`), plus the unchanged desktop-only pieces: `PlatformCapabilities`, `get_platform_capabilities`, `quit_app`.
- Move the mock-app test `cancel_reaps_the_child_before_deleting_the_partial_output` into `control.rs`, rewritten on `test_ctx` — same child stand-in (`sleep 30` / `ping`), same assertions (partial-output gone, status/failure_class/completed_at written, child + pid cleared), no mock app. Move `start_queue_clears_the_persisted_pause` (already Ctx-managed since Task 4) the same way.

- [ ] **Step 3: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS.

```bash
git add -A
git commit -m "feat: move queue control to core and widen mid-encode pause to all unix"
```

(`feat:` not `refactor:` — desktop Linux gains real pause; the capability flag flips there.)

---

### Task 8: settings_ops bodies + desktop autostart overlay

The pure helpers landed in core in Task 2; this task moves the two command bodies and leaves the plugin overlay desktop-side.

**Files:**
- Modify: `crates/convertbar-core/src/settings_ops.rs` (add get/update), `src-tauri/src/commands/settings.rs` (wrappers + overlay)

**Interfaces:**
- Produces (core, added to `settings_ops.rs`):

```rust
pub fn get_settings(ctx: &Ctx) -> Result<Settings, String>            // returns STORED launch_at_login
pub fn update_setting(ctx: &Ctx, key: &str, value: &str) -> Result<(), String>  // ALLOWED_KEYS check + skip-marker refresh
```

- Consumes: `watcher::refresh_skip_marker(ctx)` (Task 6). The autostart plugin overlay stays desktop-side.

- [ ] **Step 1: Move the bodies**

Move `get_settings`/`update_setting` logic from `commands/settings.rs` into `settings_ops.rs`, minus the two autostart touches (`ALLOWED_KEYS` is already in core since Task 2). `update_setting` keeps the `watch_skip_marker → refresh_skip_marker(ctx)` hook (core-internal now).

- [ ] **Step 2: Shrink the desktop wrappers**

```rust
#[tauri::command]
pub fn get_settings(app: AppHandle, ctx: State<'_, Arc<Ctx>>) -> Result<Settings, String> {
    let mut settings = convertbar_core::settings_ops::get_settings(&ctx)?;
    // Autostart plugin is the source of truth on desktop; core returns the stored value.
    settings.launch_at_login = app.autolaunch().is_enabled().unwrap_or(settings.launch_at_login);
    Ok(settings)
}

#[tauri::command]
pub fn update_setting(app: AppHandle, ctx: State<'_, Arc<Ctx>>, key: String, value: String) -> Result<(), String> {
    convertbar_core::settings_ops::update_setting(&ctx, &key, &value)?;
    if key == "launch_at_login" {
        let autostart = app.autolaunch();
        if value == "true" { let _ = autostart.enable(); } else { let _ = autostart.disable(); }
    }
    Ok(())
}
```

`get_preset_suffix` / `set_preset_suffix` wrappers delegate the same way (no overlay).

- [ ] **Step 3: Add the refresh-hook test in core (it was previously untested through the command)**

```rust
#[test]
fn update_setting_refreshes_the_watcher_skip_marker() {
    let (ctx, _sink, _d) = test_ctx(test_conn());
    settings_ops::update_setting(&ctx, "watch_skip_marker", ".uploading").unwrap();
    assert_eq!(
        ctx.watcher.skip_marker.lock().unwrap().as_deref(),
        Some(".uploading")
    );
}
```

(`skip_marker` is `pub` since Task 6; confirm its exact type — a `Mutex<Option<String>>` guarded by `valid_marker` — and adjust the assertion accordingly.)

- [ ] **Step 4: Full suite + commit**

Run: `cargo test --workspace && npm test` → PASS.

```bash
git add -A
git commit -m "refactor: move settings logic to core with desktop autostart overlay"
```

---

### Task 9: Drop AppState and the dual-manage

**Files:**
- Modify: `src-tauri/src/lib.rs` (single manage, tray listener via ctx), any straggler `State<'_, AppState>` / `State<'_, Arc<ConverterState>>` signatures

- [ ] **Step 1: Sweep the stragglers**

`grep -rn "AppState" src-tauri/src crates/` — migrate every remaining `State<'_, AppState>` to `State<'_, Arc<Ctx>>`, then delete the `AppState` struct and the two transitional `.manage(…)` calls in lib.rs. The tray `menu-bar-update` listener's `db_for_tray` comes from `ctx.db.clone()`; the tray/exit handlers' `app.state::<Arc<ConverterState>>()` become `app.state::<Arc<Ctx>>()` + `.converter`.

- [ ] **Step 2: Prove the boundary**

Run: `cargo test --workspace && npm test` → PASS. Then:

```bash
grep -rn "tauri" crates/convertbar-core/src crates/convertbar-core/Cargo.toml
```

Expected: NO output — the compiler-enforced boundary, verified.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: fold AppState into Ctx; core is tauri-free"
```

---

### Task 10: Frontend events shim (listen centralization)

**Files:**
- Create: `src/lib/events.ts`
- Modify: `src/hooks/useQueue.ts`, `src/hooks/useAddProgress.ts`, `src/hooks/useHistory.ts`, `src/hooks/useBadSources.ts`, `src/pages/QueuePage.tsx`, `src/pages/SettingsPage.tsx` (imports only)

**Interfaces:**
- Produces: `src/lib/events.ts` re-exporting the Tauri listen with an identical signature — Plan 2 swaps this file's internals for an EventSource-backed impl without touching any consumer:

```ts
// Single seam for backend events. Desktop: Tauri's event system. The server build
// (Plan 2) replaces the internals with one shared EventSource; consumers never change.
export { listen } from "@tauri-apps/api/event";
export type { UnlistenFn, Event } from "@tauri-apps/api/event";
```

- [ ] **Step 1: Create the shim**, then change the six files' imports from `@tauri-apps/api/event` to the shim path (`../lib/events` from both hooks and pages — same depth). No call-site body changes.

- [ ] **Step 2: Verify tests still pass unmodified**

Run: `npm test`
Expected: PASS — suites mock `@tauri-apps/api/event`, which the shim re-exports, so `vi.mock` still intercepts. The ipc-contract listen regex still matches (call sites still read `listen("event-name"`).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: centralize event listen sites behind src/lib/events shim"
```

---

### Task 11: Verification sweep + docs

**Files:**
- Modify: `CLAUDE.md` (Cross-Platform section + workspace note), `docs/OPEN_ISSUES.md` (progress note only — the issue stays open until Plan 2 ships)

- [ ] **Step 1: Full local gate**

```bash
cargo test --workspace
npm test
npm run build
npm run tauri build -- --no-bundle
```

Expected: all green; the last command produces a runnable desktop binary against the workspace layout (bundling/signing is CI's job).

- [ ] **Step 2: Launch the desktop app once** (`npm run tauri dev`, or the built binary) and manually verify: tray appears, queue page loads, settings load. This is the only manual check — everything else is covered by the suites.

- [ ] **Step 3: Dispatch the cross-platform reviewer**

Run the `cross-platform-reviewer` agent over the diff (libc/cfg moves, Windows test scoping) and fix anything it confirms. The libc rule changed: the dependency now lives in core as `cfg(unix)`; signal call sites are `#[cfg(unix)]` attributes.

- [ ] **Step 4: Update CLAUDE.md**

Rewrite the Cross-Platform libc bullet to match reality:

```markdown
- `libc` (SIGSTOP/SIGCONT) is a `[target.'cfg(unix)'.dependencies]` entry in `crates/convertbar-core/Cargo.toml`, and the signal call sites are gated with `#[cfg(unix)]` attributes — never the `cfg!()` macro, which only skips code at runtime and would still require linking libc on every platform. Mid-encode pause works on macOS and Linux; Windows falls back to queue-level pause.
```

Add a short "Workspace layout" note near the top of CLAUDE.md:

```markdown
## Workspace Layout

Cargo workspace: `crates/convertbar-core` (head-agnostic engine: converter, watcher, queue_ops, control, settings_ops, db — zero tauri deps, enforced by the crate graph) + `src-tauri` (desktop shell: thin `#[tauri::command]` adapters, tray, updater, dialogs, `TauriSink`/`TrashDisposer`). The version lives in the root `Cargo.toml` `[workspace.package]`; `release.sh` bumps it there. Run tests with `cargo test --workspace`.
```

Append one line to the Docker section of `docs/OPEN_ISSUES.md`: `**Status:** core extraction landed (workspace split, Plan 1); server head is Plan 2.`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: document workspace layout and unix-wide pause"
```

---

## Deferred to Plan 2 (do NOT build here)

`convertbar-server` crate, axum routes, SSE, auth, fs browser, `VITE_HEAD` transport split, `dist-web`/rust-embed, Dockerfile, `docker.yml`, compose example. Plan 2 is written after this plan lands, against the post-refactor tree.
