# Fable Review: rust-app-shell

## lib.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/lib.rs (done)

- **[High]** src-tauri/src/lib.rs:258-259 — Tray filename truncation uses byte slicing: `&name[..19]` after `name.len() > 20`. For multi-byte UTF-8 filenames (umlauts, CJK, emoji — common in video names) slicing at byte 19 can land mid-codepoint and panic, crashing the tray update path. Fix: truncate on char boundary, e.g. `name.chars().take(19).collect::<String>()`.
- **[Medium]** src-tauri/src/lib.rs:362-365 — Updater silently downloads AND installs on every startup with no user consent, no notification, and no relaunch; errors are swallowed. Also `handle.updater().unwrap()` would panic if the updater plugin ever fails to build. Fix: notify the user (plugin-notification is already registered), handle the Err case, and call `app.restart()` or inform the user a restart is pending after install.
- **[Medium]** src-tauri/src/lib.rs:143 — Screen confinement only runs when `current_monitor()` returns `Ok(Some(...))`. After a monitor is disconnected, the window position may not map to any monitor and (platform-dependent) this can yield `None`/Err, skipping confinement entirely — exactly the layout-change case the feature exists for. Fix: fall back to `primary_monitor()` (or `available_monitors()` nearest match) when `current_monitor()` yields nothing, and clamp against that.
- **[Low]** src-tauri/src/lib.rs:208-226,243-251 — The `menu-bar-update` listener performs synchronous SQLite queries (5 settings reads + optional COUNT) on every progress tick while holding the shared DB mutex. Progress events are frequent during encodes; this adds lock contention with queue commands. Fix: cache the menubar_* settings (invalidate on `update_setting`) instead of re-querying per tick.
- **[Low]** src-tauri/src/lib.rs:326-333 — Startup auto-resume uses `.unwrap()` on `db.prepare`/`query_map`; a corrupt DB panics the app at launch instead of degrading (opening with an empty queue). Fix: log and skip resume on error.
- **[Nit]** src-tauri/src/lib.rs:96 — `Image::from_bytes(...).unwrap()` on the embedded tray icon; safe in practice (compile-time asset) but `?` would match the surrounding setup style.

## main.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/main.rs (done)

Clean. Standard Tauri entry point with the Windows console-suppression attribute in place.

## commands/mod.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/mod.rs (done)

Clean. Pure module declarations.

## commands/converter.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/converter.rs (done)

- **[Medium]** src-tauri/src/commands/converter.rs:341-344 (and lib.rs:123 tray "quit") — `quit_app` calls `app.exit(0)` with no child cleanup; there is no `RunEvent::ExitRequested` handler anywhere in src-tauri. Quitting mid-encode orphans HandBrakeCLI, which keeps encoding (CPU/battery burn for potentially hours). On next launch the auto-resume path deletes the partial `output_path` and spawns a second encoder against the same path while the orphan still holds it (locked file on Windows; unlinked-inode ghost writes on Unix). Fix: in `quit_app`/tray quit (or a `RunEvent::ExitRequested` handler), SIGCONT-if-paused + `Child::kill()` the current child before exiting.
- **[Medium]** src-tauri/src/commands/converter.rs:254,272 — Cancel kills the child and immediately `remove_file`s the partial output without `wait()`ing for the process to exit. On Windows the dying process still holds the file handle, so the delete silently fails (error ignored) and the partial file is left behind; on Unix a last buffered flush can even recreate content post-unlink. Fix: after `child.kill()`, call `child.wait()` (kill has been delivered, so this returns promptly) before removing the file.
- **[Low]** src-tauri/src/commands/converter.rs:30-101 — `pause_conversion` reads `current_pid` and signals SIGSTOP without holding a lock that excludes the queue-loop's job transition: if the job finishes between the read and the `kill`, the signal targets a reaped PID (worst case, a recycled one). Window is tiny but the fix is cheap: hold the `current_child` lock (as `cancel_conversion` does) while signalling.
- **[Low]** src-tauri/src/commands/converter.rs:8-25 — `start_queue` has a check-then-act race on `is_running`: two rapid invokes can both observe `false` and call `run_queue` twice. Whether that double-starts the loop depends on `run_queue`'s own guard; if it has none, two workers race on the queue. Fix: set `is_running = true` under the same lock as the check (or do the guard atomically inside `run_queue`).

Otherwise good: error propagation via `Result<_, String>` is consistent, the cancel path's write-status-before-kill ordering is well reasoned (documented in comments), in-place jobs correctly delete the temp rather than the source, and macOS-only signal code is properly cfg-gated with a queue-level fallback.

## commands/handbrake.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/handbrake.rs (done)

This file is the biggest remaining instance of the known "sync command blocks the main thread" bug class (the same class already fixed at 4 `add_files_inner` entry points). All four commands here are sync `fn`s, and in Tauri 2 non-async commands execute on the main thread:

- **[High]** src-tauri/src/commands/handbrake.rs:33-39 — `list_handbrake_presets` spawns `HandBrakeCLI --preset-list` synchronously; the UI freezes for the full subprocess duration (settings screen opens hit this). Fix: make the command `async fn` (Tauri moves async commands off the main thread) or wrap the spawn in `tauri::async_runtime::spawn_blocking`.
- **[High]** src-tauri/src/commands/handbrake.rs:76 — `generate_preset_suffix` runs `hb::get_preset_metadata` (HandBrakeCLI subprocess) on the main thread on every cache miss. Same fix as above.
- **[Medium]** src-tauri/src/commands/handbrake.rs:111-121 — `validate_handbrake` executes `HandBrakeCLI --version` on the main thread. Usually fast, but if the binary lives on a slow/hung network mount this blocks the UI indefinitely (no timeout, unlike probe.rs which has `wait_with_timeout`). Fix: async + reuse a timeout wrapper.
- **[Medium]** src-tauri/src/commands/handbrake.rs:10-30 — `detect_handbrake` shells out to `which`/`where` via `hb::detect_handbrake_path()` on the main thread; also blocks (briefly) per call and is invoked as a helper by `list_handbrake_presets`. Same async fix.
- **[Low]** src-tauri/src/commands/handbrake.rs:79-84 — `preset_cache` is keyed only by preset name and never invalidated; if the user switches `handbrake_path` to a different HandBrake version, stale metadata/suffixes are served until restart. Fix: clear the cache in `update_setting` when `handbrake_path` changes (or key the cache by (path, preset)).
- **[Nit]** src-tauri/src/commands/handbrake.rs:55-74,99-107 — The "configured path if valid, else auto-detect" resolution logic is duplicated three times (detect_handbrake, generate_preset_suffix, validate_handbrake) with slightly different shapes. Fix: extract one `resolve_handbrake_path(&Connection) -> Option<String>` helper.

## commands/settings.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/settings.rs (done)

Mostly clean — key whitelist (`ALLOWED_KEYS`) for `update_setting` is good hygiene, autostart state is read back from the plugin as source of truth, and errors propagate as `Result<_, String>` consistently.

- **[Low]** src-tauri/src/commands/settings.rs:109-117 — Keys are whitelisted but values are not validated at all. `handbrake_path` accepts any string and is later executed as a binary (commands/handbrake.rs, converter). A compromised webview (XSS) could point it at an arbitrary executable — defense-in-depth gap. Fix: for `handbrake_path`, verify the path exists and is a regular file; for boolean keys, accept only "true"/"false".
- **[Low]** src-tauri/src/commands/settings.rs:122-126 — `autostart.enable()/disable()` failures are silently ignored, so the toggle can appear to succeed while login-item registration failed; the mismatch only surfaces on the next `get_settings`. Fix: propagate the error (`.map_err(|e| e.to_string())?`) so the frontend can show it.
- **[Nit]** src-tauri/src/commands/settings.rs:45 — `value == "true"` string comparison scattered across 11 keys; any casing drift ("True") reads as false silently. Minor since the frontend controls writes.

## tauri.conf.json
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/tauri.conf.json (done)

- **[High]** src-tauri/tauri.conf.json:26 — `"csp": null` disables the Content-Security-Policy entirely. Any script injection into the webview (e.g. via a crafted filename rendered unsafely, or a compromised npm dependency) gets unrestricted access to `invoke()` — including `update_setting("handbrake_path", ...)` which points at a binary the app will execute. Tauri explicitly warns against shipping with a null CSP. Fix: set a strict CSP, e.g. `"default-src 'self'; style-src 'self' 'unsafe-inline'"` (Vite builds work with self-hosted assets; add `ipc:` / `http://ipc.localhost` connect-src as needed) and let Tauri inject its nonces.
- **[Low]** src-tauri/tauri.conf.json:44-46 — Updater endpoint uses GitHub `releases/latest/download/latest.json` over HTTPS with a pinned minisign pubkey — good. Note only: `releases/latest` means a yanked-then-repushed release changes what clients see; acceptable for this project.

Otherwise sound: updater pubkey is pinned, `createUpdaterArtifacts` is on, and the window config (hidden, undecorated, alwaysOnTop, skipTaskbar) matches the menu-bar app design.

## capabilities/default.json (ACL cross-check vs frontend)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/capabilities/default.json (done)

Cross-checked every frontend Tauri API call (all `invoke()` names in src/lib/tauri.ts match registered handlers in lib.rs; plugin imports: updater, process, api/app, api/event, api/webviewWindow). No MISSING permissions found — window hide (lib/tauri.ts:150), drag region (TabBar.tsx `data-tauri-drag-region` → allow-start-dragging), getVersion, event listen/unlisten, updater check/downloadAndInstall, and process relaunch are all covered. The problem is the other direction — unused and over-broad grants:

- **[High]** src-tauri/capabilities/default.json:28 — `fs:default` violates the project's explicit "no `:default` bundles" convention AND is entirely unused: the frontend never imports `@tauri-apps/plugin-fs`. This silently grants the webview file read/write APIs it doesn't need — the worst kind of ACL drift given the CSP is also null. Fix: remove it (keep the Rust-side `tauri_plugin_fs::init()` if backend needs it; backend use needs no ACL).
- **[Medium]** src-tauri/capabilities/default.json:29 — `notification:default` is unused; CLAUDE.md itself documents notifications as backend-only, and no frontend import exists. Also a forbidden `:default` bundle (grants notify + permission prompts to the webview). Fix: remove.
- **[Medium]** src-tauri/capabilities/default.json:23-25 — `autostart:allow-enable`, `autostart:allow-disable`, `autostart:allow-is-enabled` are unused: autostart is toggled Rust-side via `app.autolaunch()` in commands/settings.rs, and the frontend never imports `@tauri-apps/plugin-autostart`. Fix: remove all three.
- **[Medium]** src-tauri/capabilities/default.json:26-27 — `window-state:allow-restore-state` and `window-state:allow-save-window-state` are unused: the plugin persists state from the backend automatically; no frontend import exists. Fix: remove both.
- **[Low]** src-tauri/capabilities/default.json:14 — `core:event:allow-emit` is unused: the frontend only `listen()`s; `emit()` appears solely in test mocks. Per-call convention says drop it.

Suggestion: the repo already defines an `acl-auditor` agent — wiring it (or a small script diffing granted permissions vs frontend imports) into CI would have caught all 9 stale grants.

## Cargo.toml
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/Cargo.toml (done)

- **[OK]** src-tauri/Cargo.toml:37-38 — `libc` is correctly gated to `cfg(target_os = "macos")`, matching the project convention. Desktop-only plugins (autostart, updater, window-state) are gated to non-mobile targets.
- **[Medium]** src-tauri/Cargo.toml:34 + lib.rs:34 — `tauri-plugin-fs` is dead weight: no backend usage (`FsExt`/`tauri_plugin_fs` appears nowhere outside plugin registration) and no frontend import — yet it is registered and granted `fs:default` in the ACL. Fix: remove the dependency, the `.plugin(tauri_plugin_fs::init())` line, and the ACL grant together.
- **[Low]** src-tauri/Cargo.toml:22 + lib.rs:41 — `tauri-plugin-opener` is likewise unused (no `OpenerExt` in backend, no frontend import, no `href`/`openUrl` anywhere). CLAUDE.md describes opener as a backend API, but nothing calls it. Fix: remove, or leave a comment for the planned use.
- **[Nit]** src-tauri/src/lib.rs:38-39 vs Cargo.toml:40-43 — plugins gated out on mobile in Cargo.toml are registered unconditionally in `run()`, while lib.rs carries `#[cfg_attr(mobile, tauri::mobile_entry_point)]`; a mobile build would not compile. Irrelevant for this desktop-only app, but the mobile attribute is misleading.

## SECURITY.md
Reviewed: /Users/rhurling/Sites/convertbar/SECURITY.md (done)

- **[Low]** SECURITY.md:7-9 — Says "one of the following channels" but lists only one channel; no supported-versions table and no response-time expectation ("promptly" is unquantified). Fix: drop "one of the following", optionally add a supported-versions note (e.g. "latest release only" — accurate given the auto-updater) and a rough acknowledgment SLA.

## Summary

Overall the app shell is in decent shape: command error handling is consistent (`Result<_, String>` everywhere, no unwraps in hot command paths), the cancel path's ordering is carefully reasoned and documented, macOS-only signal code is properly cfg-gated with queue-level fallbacks, the updater pubkey is pinned over HTTPS, and every frontend `invoke()` maps to a registered handler.

Three themes dominate the findings:

1. **Security-surface drift.** The two headline issues compound: `csp: null` (no webview hardening at all) plus 9 stale ACL grants — including two forbidden `:default` bundles (`fs:default`, `notification:default`) — and two entirely unused plugins (`fs`, `opener`). Individually low-risk; together they hand a hypothetical XSS far more capability than the app needs, in direct violation of the project's own "explicit per-call permissions, no `:default` bundles" convention.
2. **The known sync-command bug class isn't fully fixed.** All four commands in commands/handbrake.rs still spawn subprocesses (`HandBrakeCLI --preset-list`, `--version`, preset metadata, `which`) synchronously on the main thread — the exact UI-freeze class previously fixed at the `add_files_inner` entry points.
3. **Process-lifecycle edges.** Quit orphans a running HandBrakeCLI (no exit handler anywhere), cancel deletes output without waiting for the killed child (Windows file locks), and the tray title code can panic on multi-byte filenames.

## Recommendations

1. (High) Set a real CSP in tauri.conf.json — single-line change, biggest risk reduction.
2. (High) Prune capabilities/default.json to the 9 permissions actually used: core:event:allow-listen/unlisten, core:window:allow-hide/start-dragging, core:app:allow-version, updater:allow-check/download-and-install, process:allow-restart. Remove `fs:default`, `notification:default`, autostart:*, window-state:*, core:event:allow-emit. Then remove the unused `tauri-plugin-fs` (and probably `tauri-plugin-opener`) entirely.
3. (High) Fix the UTF-8 tray-title panic (`&name[..19]` → char-boundary truncation) — user-triggerable crash via ordinary filenames.
4. (High) Convert the four commands/handbrake.rs commands to `async fn` (or `spawn_blocking`) to finish eradicating the main-thread-blocking command class; add a timeout to the `--version` probe.
5. (Medium) Add exit-time child cleanup (kill current HandBrakeCLI in `quit_app`/tray quit or a `RunEvent::ExitRequested` handler), and `wait()` after `kill()` in `cancel_conversion` before deleting the partial output.
6. (Medium) Make startup auto-update user-visible (notify + explicit restart) instead of silent download-and-install with swallowed errors; it also currently races the manual update flow in SettingsPage.
7. (Medium) Harden the screen-confinement fallback: clamp against `primary_monitor()` when `current_monitor()` returns nothing (the disconnected-monitor case the feature targets).
8. (Low) Validate `update_setting` values (especially `handbrake_path`), invalidate `preset_cache` when the HandBrake path changes, and cache menubar_* settings instead of querying SQLite on every progress tick.
9. (Low) Run the existing `acl-auditor` agent (or a small CI script) on frontend/capability changes so ACL drift like this can't accumulate again.


## Verification pass (2026-07-07)

- **Confirmed** — Tray truncation byte-slice panic (lib.rs:258-259): Code is exactly `if name.len() > 20 { format!("{}…", &name[..19]) }`; `file_name` is the raw OS filename via `Path::file_name().to_str()` (converter.rs:330-333), so multi-byte names reach the slice and `&name[..19]` panics off a char boundary. Only nuance: gated behind `menubar_show_filename`, which defaults to false (lib.rs:223), so it's opt-in — but user-triggerable once enabled.
- **Confirmed** — Silent updater + unwrap (lib.rs:362-365): Verified `handle.updater().unwrap().check()` inside `async_runtime::spawn` with `download_and_install(|_, _| {}, || {})` and errors discarded via `if let Ok`/`let _ =`; no notification, no restart call anywhere in the setup. (The unwrap panic would abort the spawned task, not the whole app — the finding doesn't overclaim.)
- **Confirmed** — Screen confinement skipped when no monitor (lib.rs:143): Confinement is wrapped in `if let Ok(Some(monitor)) = window.current_monitor()` with no else branch and no `primary_monitor()`/`available_monitors()` fallback; `Err`/`None` silently skips clamping.
- **Confirmed** — quit orphans HandBrakeCLI (converter.rs:341-344, lib.rs:123): `quit_app` is bare `app.exit(0)`, tray quit likewise; grep of all of src-tauri/src finds zero `RunEvent`/`ExitRequested`/`on_window_event` handlers and `.run(tauri::generate_context!())` takes no callback (lib.rs:370), so nothing kills `current_child`. Auto-resume (lib.rs:326-341) does delete `output_path` and re-queue on next launch as claimed.
- **Partial** — Cancel deletes output without wait() (converter.rs:254,272): Core holds — `child.kill()` at :254 is followed by `let _ = std::fs::remove_file(...)` at :272 with no `child.wait()`, so the Windows handle-still-held/silent-delete-failure scenario is real. Correction: the Unix "buffered flush can recreate content post-unlink" detail is wrong — kill() is SIGKILL (no userspace flush) and pending writes land in the unlinked inode, never reappearing at the path.
- **Confirmed** — `list_handbrake_presets` blocks main thread (handbrake.rs:32-39): Sync `fn` (no `async` anywhere in the file), calls `hb::list_presets` which does `Command::new(handbrake_path).output()` (handbrake.rs module :40-42); frontend invokes it (src/lib/tauri.ts:137). Matches the project's documented main-thread-probe bug class.
- **Confirmed** — `generate_preset_suffix` blocks main thread (handbrake.rs:42-85): Sync `fn`; on cache miss runs `hb::get_preset_metadata` → `Command::new(...).output()` (module :65-70); invoked from src/lib/tauri.ts:143.
- **Confirmed** — `validate_handbrake` blocks main thread, no timeout (handbrake.rs:88-134): Sync `fn` running `std::process::Command::new(&p).arg("--version").output()` at :111-113 with no timeout; probe.rs does have `wait_with_timeout` (probe.rs:80,90), confirming the asymmetry.
- **Confirmed** — `detect_handbrake` blocks main thread (handbrake.rs:9-30): Sync `fn`; `hb::detect_handbrake_path()` shells out to `which`/`where` via `.output()` (module :13-21), and it is also called inline by `list_handbrake_presets` (handbrake.rs:34).
- **Confirmed** — `csp: null` (tauri.conf.json:26): Read directly: `"csp": null`. The exploit chain also checks out: `handbrake_path` is in `ALLOWED_KEYS` (settings.rs:88) and the stored value is executed as a binary (handbrake.rs:111, converter).
- **Confirmed** — `fs:default` unused (capabilities/default.json:28): Grepped ALL of src/ — zero imports of `@tauri-apps/plugin-fs` and zero dynamic `invoke('plugin:...')` calls of any kind; only backend touchpoint is the `tauri_plugin_fs::init()` registration (lib.rs:34), which needs no ACL. Violates the CLAUDE.md "no `:default` bundles" rule.
- **Confirmed** — `notification:default` unused (capabilities/default.json:29): No `@tauri-apps/plugin-notification` import and no dynamic plugin invoke anywhere in src/; notifications are emitted backend-side in converter.rs (:587,:748), which bypasses the ACL.
- **Confirmed** — autostart grants unused (capabilities/default.json:23-25): No `@tauri-apps/plugin-autostart` import in src/; autostart is toggled entirely Rust-side via `app.autolaunch().enable()/disable()/is_enabled()` (commands/settings.rs:63,121-125).
- **Confirmed** — window-state grants unused (capabilities/default.json:26-27): No `@tauri-apps/plugin-window-state` import and no dynamic invoke in src/; the plugin is backend-registered (lib.rs:36) and persists automatically.
- **Confirmed** — `tauri-plugin-fs` dead weight (Cargo.toml:34, lib.rs:34): `tauri_plugin_fs`/`FsExt` appears nowhere in src-tauri/src except the single `.plugin(tauri_plugin_fs::init())` registration line, and (per above) no frontend usage exists — dependency, registration, and `fs:default` grant are all removable together.

**Tally:** 14 confirmed, 1 partial, 0 refuted (of 15).
