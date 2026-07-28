# Round 2 — rust-app-shell (verification pass, 2026-07-08)

Status: in progress — findings appended incrementally by the reviewing subagent.

## Fix verification

All file:line references are to current `main` @ daf4f8e.

1. **Tray filename truncation byte-slice panic** (was High, lib.rs:258) — **FIXED**.
   `truncate_tray_title()` (src-tauri/src/lib.rs:27-33) counts and takes `chars()`, no byte slicing; used at lib.rs:276. Regression test `tray_title_truncation_is_char_boundary_safe` (lib.rs:419-434) covers umlauts, emoji, and the exactly-20-chars boundary (the old code also truncated at >20 *bytes*, which the test pins).

2. **Silent updater + `updater().unwrap()`** (was Medium, lib.rs:362-365) — **FIXED** (per D5 option a).
   src-tauri/src/lib.rs:376-395: `let Ok(updater) = handle.updater() else { return; }` replaces the unwrap; on successful install a notification "Updated to {version} — restart ConvertBar to apply" is shown via the backend notification plugin. Check/download failures stay quiet by explicit design comment (lib.rs:373-375, "normal offline behavior") — consistent with D5, which only mandated notify-on-install. Note: a *failed install* (download_and_install Err) is also silent; acceptable under D5 but repeated install failures remain invisible.

3. **Screen confinement skipped when `current_monitor()` yields nothing** (was Medium, lib.rs:143) — **FIXED**.
   src-tauri/src/lib.rs:156-160: `window.current_monitor().ok().flatten().or_else(|| window.primary_monitor().ok().flatten())`, exactly the recommended fallback; clamping then runs against whichever monitor resolved.

4. **Quit orphans running HandBrakeCLI** (was Medium, commands/converter.rs:341-344 + tray quit) — **FIXED**.
   `.run()` now takes a callback handling `RunEvent::ExitRequested` (src-tauri/src/lib.rs:401-411) which calls `converter::kill_active_child()` (src-tauri/src/converter.rs:135-152). That covers every exit path — `quit_app` (`app.exit(0)`, commands/converter.rs:356-358), tray Quit (lib.rs:133), Cmd+Q. Kill logic verified adversarially:
   - **Pause interaction**: SIGCONT is sent first via `current_pid` on macOS (converter.rs:136-145) — a SIGSTOPped child would otherwise never die to SIGKILL delivery ordering issues; correct.
   - **Deadlock**: `kill_active_child` releases the pid lock before taking `current_child` (separate blocks); the queue loop's `wait_for_active_child` releases `current_child` between `try_wait` polls (converter.rs:890-905, documented), so the exit handler can always acquire it. No lock-order cycle.
   - **Double-kill vs cancel**: `cancel_conversion` sets `*child_guard = None` after its own kill+wait (commands/converter.rs:264), so `kill_active_child` finds `None` and no-ops.

5. **Cancel deletes partial output without `wait()`** (was Medium/Partial, commands/converter.rs:254,272) — **FIXED**.
   src-tauri/src/commands/converter.rs:254-264: `child.kill()` → `child.wait()` (comment documents the Windows handle-held rationale) → `*child_guard = None`, and only then the partial/temp delete at :272-281. The refuted Unix "flush after unlink" claim was correctly dropped from the fix rationale (TRIAGE B4).

6. **`list_handbrake_presets` blocks main thread** (was High) — **FIXED**. src-tauri/src/commands/handbrake.rs:74-85: `async fn` + `tauri::async_runtime::spawn_blocking`.

7. **`generate_preset_suffix` blocks main thread** (was High) — **FIXED**. handbrake.rs:87-108: `async fn` + `spawn_blocking`; cache hit short-circuits before any DB lock or `which` (:96-101).

8. **`validate_handbrake` blocks main thread, no timeout** (was Medium) — **FIXED**. handbrake.rs:110-132: `async fn` + `spawn_blocking`; `handbrake_version()` (:145-162) runs `--version` under `probe::wait_with_timeout` with a 10s deadline (`VERSION_CHECK_TIMEOUT`, :10), and `wait_with_timeout` kills+reaps on overrun (src-tauri/src/probe.rs:107-123) — no leaked child on a hung network mount.

9. **`detect_handbrake` blocks main thread** (was Medium) — **FIXED**. handbrake.rs:64-72: `async fn` + `spawn_blocking`. Bonus: the triplicated path-resolution logic (round-1 Nit) is now one `resolve_handbrake_path()` helper (:12-32) that releases the DB lock before shelling out.

10. **`csp: null`** (was High, tauri.conf.json:26) — **FIXED** (per D1 option a).
    src-tauri/tauri.conf.json:26: `default-src 'self'; connect-src ipc: http://ipc.localhost; img-src 'self' data:; style-src 'self' 'unsafe-inline'`. Adequacy verified, not assumed:
    - Production bundle has no inline scripts (`dist/index.html` contains a single external `<script type="module" src="/assets/...">`), so `script-src` falling back to `default-src 'self'` works; Tauri nonces its own injected snippets.
    - IPC: `ipc:` covers macOS/Linux (`ipc://localhost`), `http://ipc.localhost` covers Windows.
    - The only remote-ish asset is a `data:` SVG in src/App.css:367 — covered by `img-src data:`. No fonts, no fetch/WebSocket in src/ (grepped).
    - Dev mode is NOT broken by the strict policy: confirmed in tauri-2.11.5 source (src/manager/mod.rs `csp()`/`csp_header`) that the CSP is applied via the tauri asset protocol response header, which an http `devUrl` (Vite at localhost:1420) never goes through — so Vite HMR/react-refresh are unaffected.

11. **`fs:default` grant unused** (was High) — **FIXED**. src-tauri/capabilities/default.json:13-22 now lists exactly 8 per-call grants; no `fs:*`.

12. **`notification:default` grant unused** (was Medium) — **FIXED**. Removed; notifications remain backend-only (plugin still registered, lib.rs:49, used by converter + the new updater notification — backend needs no ACL).

13. **`autostart:allow-*` grants unused** (was Medium) — **FIXED**. Removed; autostart stays Rust-side via `app.autolaunch()` (src-tauri/src/commands/settings.rs:83,141-146).

14. **`window-state:*` grants unused** (was Medium) — **FIXED**. Removed; plugin backend-registered (lib.rs:45).

15. **`tauri-plugin-fs` dead weight** (was Medium, Cargo.toml + lib.rs) — **FIXED**. src-tauri/Cargo.toml:20-42 has neither `tauri-plugin-fs` nor `tauri-plugin-opener` (the opener Low was handled too); lib.rs:43-49 registers neither.

**ACL regression cross-check** (did the removals break anything?): every remaining frontend Tauri API call maps to a surviving grant — `listen`/`unlisten` (useQueue/useHistory/SettingsPage + DropZone `onDragDropEvent`, which is event-listen under the hood) → `core:event:allow-listen/unlisten`; `getCurrentWebviewWindow().hide()` (src/lib/tauri.ts:153) → `core:window:allow-hide`; `data-tauri-drag-region` (src/components/TabBar.tsx:19,29) → `core:window:allow-start-dragging`; `getVersion` → `core:app:allow-version`; `check`/`downloadAndInstall` (SettingsPage) → `updater:allow-check/download-and-install`; `relaunch` → `process:allow-restart` (relaunch invokes `plugin:process|restart`). No missing grants; no dynamic `invoke('plugin:...')` anywhere in src/. The B10 IPC-contract test additionally pins app-command names in CI.

**Fix-verification tally: 15/15 FIXED** (0 partial, 0 not fixed, 0 regressed, 0 N/A-by-decision — D1/D5 shaped fixes rather than waiving them).

## New findings

- **[Low]** src-tauri/src/converter.rs:146-151 — `kill_active_child` kills and reaps the child but leaves the reaped `Child` in `current_child` (unlike `cancel_conversion`, which clears it). During app teardown the queue thread's `wait_for_active_child` poll (converter.rs:890-905) can observe the cached kill status first and take its failure branch, writing `status='error'` / "Conversion failed" to the DB before the process exits. Failure scenario: user quits mid-encode expecting the documented resume-on-relaunch (lib.rs:402-406 comment); on some quits the job instead shows as errored next launch and is not auto-resumed (auto-resume only resets `encoding`/`paused`, lib.rs:339-353). Window is milliseconds and the outcome is a cosmetic wrong status, not data loss — clearing the handle doesn't fix it either (the "handle missing" branch also errors); a real fix needs an "exiting" flag the queue loop checks before writing terminal status. Fine to accept as-is.

- **[Low]** src-tauri/src/lib.rs:383-393 — Update *install failure* is fully silent (only success notifies). A persistently failing download/install (e.g. broken disk permissions after a manual move to a non-writable location) leaves the user permanently on an old version with no signal, while the SettingsPage manual check remains the only way to notice. D5 only mandated notify-on-success, so this is a gap in spirit, not in letter. One-line fix if wanted: notify on `Err` too.

Still-open round-1 Low/Nit items (explicitly untracked by TRIAGE — listed for completeness, no action implied):
- lib.rs:339-346 — auto-resume still `.unwrap()`s `db.prepare`/`query_map`; a corrupt DB panics at launch.
- lib.rs:226-244,261-269 — `menu-bar-update` listener still does per-tick SQLite settings reads under the shared DB mutex.
- lib.rs:106 — tray icon `Image::from_bytes(...).unwrap()` (compile-time asset; benign).
- src-tauri/src/commands/settings.rs:142-146 — `autostart.enable()/disable()` errors still swallowed.
- settings.rs:129-137 — `update_setting` values still unvalidated (`handbrake_path` accepts any string). Materially mitigated since round 1: the strict CSP now fronts the XSS chain, and `resolve_handbrake_path` (handbrake.rs:25-29) only uses the configured path if it exists on disk.
- Preset cache still never invalidated on `handbrake_path` change (handbrake.rs:38-58); stale metadata until restart.

Fresh-pass positives worth recording: the round-1 Low `start_queue` check-then-act race is fixed as a ride-along — `run_queue` now owns the `is_running` guard atomically under the lock (converter.rs:875-884), making the command-side pre-check advisory only. main.rs unchanged and clean.

## Summary

All 15 High/Medium round-1 findings in the rust-app-shell scope are genuinely fixed in current main — verified in source, not from TRIAGE checkboxes. The three riskiest fixes hold up under adversarial review: the quit-path kill handler has correct SIGCONT-before-kill ordering, no lock-order deadlock, and no double-kill against cancel; the CSP was confirmed compatible with production assets, both IPC transports, and (via tauri crate source) dev-mode Vite; the ACL trim was cross-checked against every surviving frontend API call with zero missing grants. The B2 async conversion of commands/handbrake.rs is complete, including the timeout the round-1 report asked for, and it incidentally cleaned up the path-resolution duplication.

New issues found: 2, both Low (quit-vs-queue-loop status race producing an occasional cosmetic 'error' instead of auto-resume; silent update-install failure). No Critical/High/Medium regressions introduced by the fix work.

## Recommendations

1. Nothing blocks release from this scope.
2. Optional polish, in priority order: (a) notify on updater install failure (one line, closes the invisible-stale-version gap); (b) replace the auto-resume `.unwrap()`s with log-and-skip so a corrupt DB degrades instead of panicking at launch; (c) an "exiting" flag checked by the queue loop before terminal status writes, if the occasional quit-mid-encode 'error' status ever bothers users.
3. Keep the acl-auditor + IPC-contract test discipline — it is what makes this ACL trim safe to maintain.
