# Persist Queue Pause Across Restart — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A queue the user deliberately paused (macOS **Pause** / **"Pause after this"**) stays stopped across an app restart until they click **Resume**; a low-disk auto-pause is NOT remembered (re-checks the disk on restart). Adding files clears the pause and starts the queue.

**Architecture:** A persisted `queue_paused` boolean in the `settings` key-value table (read defaults to `false`; not seeded, not in `ALLOWED_KEYS`, no UI). Set when the queue enters a user-initiated pause; cleared on any (re)start/resume/add/clear. Launch auto-start is gated on `!queue_paused`. Backend-only — the existing Resume button already renders in the "stopped with pending jobs" state.

**Tech Stack:** Rust (Tauri 2, rusqlite), `cargo test`. Spec: `docs/superpowers/specs/2026-07-23-persist-queue-pause-design.md`.

**Branch:** `feature/persist-queue-pause`.

---

## Task 1: Persistence helpers + launch predicate (`converter.rs`)

Pure/near-pure, unit-tested foundation. No wiring yet.

**Files:**
- Modify: `src-tauri/src/converter.rs`
- Test: `src-tauri/src/converter.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing unit tests**

In `converter.rs`'s `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn queue_paused_round_trips_and_defaults_false() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_db(&conn).unwrap();
    // Absent row -> false (no seed, existing DBs need no migration).
    assert!(!is_queue_paused(&conn));
    set_queue_paused(&conn, true);
    assert!(is_queue_paused(&conn));
    set_queue_paused(&conn, false);
    assert!(!is_queue_paused(&conn));
}

#[test]
fn should_auto_resume_only_when_queued_and_not_paused() {
    assert!(should_auto_resume(true, false), "queued + not paused -> auto-start");
    assert!(!should_auto_resume(true, true), "a remembered pause blocks auto-start");
    assert!(!should_auto_resume(false, false), "nothing queued -> nothing to start");
    assert!(!should_auto_resume(false, true));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib -- queue_paused_round_trips should_auto_resume`
Expected: FAIL to compile — `is_queue_paused`/`set_queue_paused`/`should_auto_resume` not found.

- [ ] **Step 3: Implement the helpers**

In `converter.rs`, add near the other DB helpers (e.g. by `get_cleanup_mode` / `get_low_disk_min_gb`):

```rust
/// Persisted "the user deliberately stopped the queue" flag, stored in the settings table.
/// Read-with-default (no seed) so existing databases need no migration and the settings-count
/// guard test is untouched. It is backend runtime state — NOT in ALLOWED_KEYS, NOT in the UI.
fn set_queue_paused(db: &Connection, paused: bool) {
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES ('queue_paused', ?1)
         ON CONFLICT(key) DO UPDATE SET value = ?1",
        params![if paused { "true" } else { "false" }],
    );
}

fn is_queue_paused(db: &Connection) -> bool {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'queue_paused'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v == "true")
    .unwrap_or(false)
}

/// Whether launch should auto-start the queue: only when jobs are queued AND the user did not
/// leave the queue deliberately paused. Pure so the launch decision is unit-testable.
pub(crate) fn should_auto_resume(has_queued: bool, queue_paused: bool) -> bool {
    has_queued && !queue_paused
}
```

Note: `set_queue_paused`/`is_queue_paused` are used across modules in Task 2 (`commands::converter`, `commands::queue`, `watcher`, `lib`). Mark them `pub(crate)` now (change `fn` → `pub(crate) fn` for both) so Task 2 compiles; they'll be unused until Task 2, producing an expected dead-code warning — leave it, do NOT add `#[allow(dead_code)]`.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test --lib -- queue_paused_round_trips should_auto_resume`
Expected: PASS (2 tests). Note: the tests themselves use all three helpers, so `cargo test --lib` is warning-free here; a dead-code warning only appears under a non-test `cargo build --lib` during the Task 1→2 window (CI has no `-D warnings`, so it's harmless). Do NOT add `#[allow(dead_code)]`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/converter.rs
git commit -m "feat: add persisted queue_paused helpers + launch predicate"
```

---

## Task 2: Wire set/clear points + launch guard + integration test

Wire the helpers everywhere and gate launch auto-start.

**Files:**
- Modify: `src-tauri/src/converter.rs` (SET at pause-after-current; test helper + integration test)
- Modify: `src-tauri/src/commands/converter.rs` (SET at macOS Pause; CLEAR at start/resume)
- Modify: `src-tauri/src/watcher.rs` (CLEAR at enqueue_and_start)
- Modify: `src-tauri/src/commands/queue.rs` (CLEAR at clear_queue; test)
- Modify: `src-tauri/src/lib.rs` (launch guard)
- Test: `converter.rs` + `commands/queue.rs`

- [ ] **Step 1: Write the failing integration tests**

In `converter.rs` `#[cfg(test)] mod tests`, add a success-producing fake HandBrake helper (mirrors the arg-shifting of the existing `slow_fake_handbrake_script`, but writes a small output and exits 0 so a job genuinely completes):

```rust
// A stand-in for HandBrakeCLI that writes a small non-empty output (the last CLI arg, like -o)
// and exits 0 — a job that completes successfully, so process_queue reaches its success/cleanup
// and pause-after-current path.
fn successful_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let p = dir.join("hb-ok.cmd");
        std::fs::write(
            &p,
            "@echo off\r\n:loop\r\nif not \"%~2\"==\"\" (\r\nshift\r\ngoto loop\r\n)\r\necho done> \"%~1\"\r\nexit /b 0\r\n",
        )
        .unwrap();
        p
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("hb-ok.sh");
        std::fs::write(&p, "#!/bin/sh\nfor a; do out=\"$a\"; done\necho done > \"$out\"\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

#[test]
fn pause_after_current_firing_persists_queue_paused() {
    // When "Pause after this" is armed, the job completes and the queue stops — and that stop
    // must be REMEMBERED (queue_paused persisted) so the next launch does not auto-resume.
    let app = mock_app();
    let db = test_db();
    let converter = ConverterState::new();
    set_setting(&db, "cleanup_mode", "delete"); // keep cleanup filesystem-local (no Trash)
    let dir = tempfile::tempdir().unwrap();
    let script = successful_fake_handbrake_script(dir.path());
    set_setting(&db, "handbrake_path", script.to_str().unwrap());
    let src = dir.path().join("in.mp4");
    std::fs::write(&src, b"0123456789").unwrap(); // real source (10 bytes) so cleanup/metadata work
    let out = dir.path().join("out.mp4");
    queue_job(&db, "j1", src.to_str().unwrap(), out.to_str().unwrap(), 10);
    // Arm "pause after this" before the run.
    *converter.pause_after_current.lock().unwrap() = true;

    process_queue(app.handle(), &db, &converter);

    assert_eq!(job_row(&db, "j1").0, "done", "the job completes");
    assert!(
        is_queue_paused(&db.lock().unwrap()),
        "pause-after-current firing must persist the paused state"
    );
}
```

In `commands/converter.rs` `#[cfg(test)] mod tests` (alongside the existing `cancel_reaps_the_child_before_deleting_the_partial_output` test), add a test that the **Resume path** (`start_queue`) clears the flag. NOTE: this requires `start_queue` to be generic over the runtime — see Step 5, which changes its signature to `start_queue<R: tauri::Runtime>(app: AppHandle<R>, …)` (mirroring `cancel_conversion`); write this test against that generic form. No queued jobs are inserted (so the spawned queue thread finds nothing, breaks immediately, and the fire-and-forget emits are harmless), and notifications are disabled so the jobless drain's "Queue complete" path doesn't touch the plugin-less mock app:

```rust
#[test]
fn start_queue_clears_the_persisted_pause() {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_db(&conn).unwrap();
    // A remembered pause; disable notifications (mock app has no notification plugin).
    conn.execute(
        "UPDATE settings SET value='false' WHERE key IN ('notifications_per_file','notifications_queue_done')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('queue_paused', 'true')
         ON CONFLICT(key) DO UPDATE SET value = 'true'",
        [],
    )
    .unwrap();
    app.manage(crate::AppState {
        db: Arc::new(Mutex::new(conn)),
        preset_cache: Mutex::new(Default::default()),
    });
    app.manage(Arc::new(ConverterState::new()));

    // Resume: clears the remembered pause (synchronously, before spawning the queue thread).
    start_queue(app.handle().clone(), app.state(), app.state()).unwrap();

    let state: State<'_, AppState> = app.state();
    let paused: String = state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT value FROM settings WHERE key='queue_paused'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(paused, "false", "Resume clears the remembered pause");
}
```

(`Connection`, `Arc`, `Mutex`, `State` are already imported in `commands/converter.rs`'s test module / file per the existing `cancel_reaps…` test; add any that aren't.)

In `commands/queue.rs` `#[cfg(test)] mod tests`, add a test that `clear_queue` clears the flag (mock-app style, like the existing `cancel_reaps_the_child_before_deleting_the_partial_output` test in `commands/converter.rs` — manage `AppState` + `Arc<ConverterState>`):

```rust
#[test]
fn clear_queue_clears_the_persisted_pause() {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_db(&conn).unwrap();
    // A remembered pause + a queued job.
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('queue_paused', 'true')
         ON CONFLICT(key) DO UPDATE SET value = 'true'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
         VALUES ('j', '/s.mp4', '/o.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    app.manage(crate::AppState {
        db: std::sync::Arc::new(std::sync::Mutex::new(conn)),
        preset_cache: std::sync::Mutex::new(Default::default()),
    });
    app.manage(std::sync::Arc::new(crate::converter::ConverterState::new()));

    clear_queue(app.state(), app.state()).unwrap();

    let state: State<'_, AppState> = app.state();
    let db = state.db.lock().unwrap();
    let paused: String = db
        .query_row("SELECT value FROM settings WHERE key='queue_paused'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(paused, "false", "clearing the queue drops the remembered pause");
    let n: i64 = db.query_row("SELECT COUNT(*) FROM jobs WHERE status='queued'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib -- pause_after_current_firing_persists clear_queue_clears_the_persisted_pause`
Expected: both FAIL — `pause_after_current_firing_persists...` fails its `is_queue_paused` assertion (nothing sets it yet); `clear_queue_clears...` fails because `clear_queue` doesn't clear the flag yet (and may not compile until the helper is used — that's fine, it's the same crate).

- [ ] **Step 3: SET at the pause-after-current break (`converter.rs`)**

In `process_queue`, in the `if take_pause_after_current(converter) {` block, add the persist right after the block opens (before the menu-bar emit):

```rust
                if take_pause_after_current(converter) {
                    set_queue_paused(&db.lock().unwrap(), true);
                    let _ = app.emit(
```

(The rest of the block — the idle menu-bar emit and `break` — is unchanged. No db lock is held at this point, so `db.lock()` here is safe.)

- [ ] **Step 4: SET at the macOS Pause (`commands/converter.rs`)**

In `pause_conversion`, macOS branch, inside `if let Some(ref job_id) = job_id_val {` after the existing `db.execute("UPDATE jobs SET status = 'paused' ...")`, add:

```rust
                let _ = db.execute(
                    "UPDATE jobs SET status = 'paused' WHERE id = ?1",
                    rusqlite::params![job_id],
                );
                crate::converter::set_queue_paused(&db, true);
```

(`db` is the locked connection already in scope. The non-macOS branch only arms `pause_after_current`; it does not stop the queue, so the flag is set later when that fires — case Step 3 — not here.)

- [ ] **Step 5: CLEAR at start_queue, resume_conversion, and cancel_conversion (`commands/converter.rs`)**

First, make `start_queue` generic over the runtime so it is callable on a mock app in tests (mirrors `cancel_conversion`, which is already `<R: tauri::Runtime>`). `run_queue` is already generic, so passing `app` through still works. Change the signature:

```rust
#[tauri::command]
pub fn start_queue<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    converter_state: State<'_, Arc<ConverterState>>,
) -> Result<(), String> {
```

(The `#[tauri::command]` macro supports a generic runtime — `cancel_conversion` in this same file proves it, and registration in `lib.rs`'s handler is unchanged.)

In `start_queue`, after the `is_running` early-return and before `run_queue`:

```rust
    let db = state.db.clone();
    let conv = (*converter_state).clone();

    // A user (re)starting the queue — Resume button, or a drag-drop add which routes through
    // startQueue — clears any remembered pause.
    if let Ok(conn) = state.db.lock() {
        crate::converter::set_queue_paused(&conn, false);
    }

    converter::run_queue(app, db, conv);
    Ok(())
```

In `resume_conversion`, add a clear that runs on BOTH platforms — put it at the very top of the function body, before the `can_pause_process` check:

```rust
) -> Result<(), String> {
    // Resuming un-pauses the queue on either platform; drop the remembered pause.
    if let Ok(conn) = state.db.lock() {
        crate::converter::set_queue_paused(&conn, false);
    }

    // On non-macOS, cancel the queue-level pause
    if !ConverterState::can_pause_process() {
```

Also clear in `cancel_conversion`: cancelling the current job does NOT stop the queue — the loop continues with the next job — so a remembered pause set by a prior macOS Pause would otherwise wrongly persist. At the top of `cancel_conversion`'s body (before the existing `job_id_val` read), add:

```rust
    // Cancelling the current job doesn't stop the queue (it continues with the next job), so a
    // pause remembered from an earlier SIGSTOP must be dropped — otherwise the next launch would
    // wrongly stay paused for a queue that was actively running.
    if let Ok(conn) = state.db.lock() {
        crate::converter::set_queue_paused(&conn, false);
    }
```

- [ ] **Step 6: CLEAR at the watcher's enqueue_and_start (`watcher.rs`)**

In `enqueue_and_start`, right before the final `converter::run_queue(...)`:

```rust
    let db = app_state.db.clone();
    let converter = (*app.state::<Arc<ConverterState>>()).clone();
    // A watched-folder file arriving is an add; per the design, adding files starts the queue,
    // so clear any remembered pause before running.
    if let Ok(conn) = app_state.db.lock() {
        crate::converter::set_queue_paused(&conn, false);
    }
    converter::run_queue(app.clone(), db, converter);
    let _ = app.emit("queue-updated", ());
```

- [ ] **Step 7: CLEAR at clear_queue (`commands/queue.rs`)**

In `clear_queue`, after the existing `low_disk_pause` clear:

```rust
    *converter_state
        .low_disk_pause
        .lock()
        .map_err(|e| e.to_string())? = None;
    // A cleared queue has no jobs to stay paused for.
    crate::converter::set_queue_paused(&conn, false);
    Ok(())
```

(`conn` is the locked connection already in scope from the DELETE above.)

- [ ] **Step 8: Launch guard (`lib.rs`)**

In the auto-resume setup block, read the flag inside the existing db-lock scope and gate the run. Change:

```rust
                has_queued = db.query_row(
                    "SELECT COUNT(*) > 0 FROM jobs WHERE status = 'queued'",
                    [],
                    |row| row.get::<_, bool>(0),
                ).unwrap_or(false);
            }

            if has_queued {
```

to:

```rust
                has_queued = db.query_row(
                    "SELECT COUNT(*) > 0 FROM jobs WHERE status = 'queued'",
                    [],
                    |row| row.get::<_, bool>(0),
                ).unwrap_or(false);
                queue_paused = crate::converter::is_queue_paused(&db);
            }

            if converter::should_auto_resume(has_queued, queue_paused) {
```

and add the declaration next to `let has_queued;` (before the db-lock scope):

```rust
            let has_queued;
            let queue_paused;
```

- [ ] **Step 9: Run the full suite**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS — the two new integration tests plus all pre-existing tests (`take_pause_after_current_consumes_the_flag_exactly_once`, the low-disk tests, `cancel_reaps_the_child...`, etc.). No dead-code warning remains (the helpers are now used).

- [ ] **Step 10: fmt + commit**

```bash
cd src-tauri && cargo fmt && cd ..
git add src-tauri/src/converter.rs src-tauri/src/commands/converter.rs \
        src-tauri/src/watcher.rs src-tauri/src/commands/queue.rs src-tauri/src/lib.rs
git commit -m "feat: persist queue pause across restart (user pauses only)"
```

---

## Final Verification

- [ ] `cd src-tauri && cargo test --lib` → all pass; `cargo fmt --check` clean; `cargo build --lib` warning-free.
- [ ] Frontend untouched — no `npm` changes (confirm `git diff --stat` shows only the 5 Rust files + the two plan/spec docs).
- [ ] Manual (macOS): queue jobs → "Pause after this", let a job finish so the queue stops → quit and relaunch → queue stays stopped with Resume shown (not auto-converting); click Resume → it runs. Separately: low-disk pause → relaunch → it re-checks and runs if space was freed. Add a file while paused → it starts.
- [ ] Cross-platform: the SET/CLEAR are plain SQL + the pure predicate; no platform gating needed. The `successful_fake_handbrake_script` has a Windows `.cmd` branch mirroring the proven `slow_fake_handbrake_script` pattern — watch the advisory `rust-windows` check on the PR.

## Execution Handoff

Subagent-driven: fresh implementer per task + spec then code-quality review each. Then a final review and finishing-a-development-branch.
