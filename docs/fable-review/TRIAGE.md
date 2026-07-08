# Implementation Triage — Fable Review

Source: the seven verified reports in this directory (55 High/Medium findings:
49 confirmed, 5 partial, 1 refuted-and-replaced). Findings are grouped into
batches by fix-shape; each batch = one branch + one PR. Low/Nit findings are
not tracked here — they ride along with whichever batch touches their file, or
are dropped.

Items marked `→ D<n>` are blocked on a decision in [DECISIONS.md](DECISIONS.md).

Status: update the checkbox + PR link as batches land.

## Batches

### B1 — Linux default preset name (trivial, ship first)
- [x] (#63) `db.rs:17`: `"H.265 MKV 1080p"` is not a valid preset in current
  HandBrake (real name `"H.265 MKV 1080p30"`) — default conversions on Linux
  fail outright. Found during verification (rust-queue-watch.md, verification
  pass). Consider a startup/validation fallback so an invalid stored preset
  fails loudly instead of silently.

### B2 — Remaining main-thread blockers
Refs: rust-core.md:36, rust-app-shell.md:38-41, rust-queue-watch.md:47,49
- [x] Make the 4 sync commands in `commands/handbrake.rs` async +
  `spawn_blocking` (detect, preset-list, generate_preset_suffix, validate —
  give validate a timeout like probe.rs).
- [x] Stop holding `preset_cache` mutex across the metadata shell-out in
  `queue.rs:335-344/363-372` (check → unlock → fetch → relock → insert);
  extract the duplicated block into a helper. (Verified: ~0.19s hitch, Medium.)
- [x] Make `scan_folder`/`classify_paths` (queue.rs:560,810) async +
  `spawn_blocking` like `add_files`.
- Verify with the cross-platform-reviewer agent; same pattern as PR #55.

### B3 — Failure visibility (backend)
Refs: rust-core.md:49,13; rust-app-shell zero-byte finding
- [x] Keep a bounded stderr tail (last ~4KB/20 lines) in the drain thread
  (converter.rs:413-423) and surface it in `error_message` instead of the
  literal "Conversion failed" (line 714/721).
- [x] Zero-byte output after successful exit → semantics per **D3**.
- [x] Drain probe stdout on a thread while polling (probe.rs:73-84) to kill
  the pipe-buffer 30s-stall.

### B4 — Process & window lifecycle robustness
Refs: rust-app-shell.md:6,7,8,26,27
- [x] Kill (SIGCONT-if-paused first) the active HandBrake child on quit —
  `quit_app`, tray quit, or a `RunEvent::ExitRequested` handler (none exists).
- [x] Cancel: `child.wait()` after `kill()` before deleting partial output
  (Windows file-handle lock; the Unix "flush after unlink" claim was refuted).
- [x] Tray title truncation `&name[..19]` (lib.rs:258) → char-boundary-safe
  (panics on multi-byte UTF-8; requires opt-in `menubar_show_filename`).
- [x] Screen confinement: fall back to `primary_monitor()` when
  `current_monitor()` is None/Err (lib.rs:143).
- [x] Updater: handle `unwrap()`/errors; notify-vs-silent per **D5**.

### B5 — Watcher correctness
Refs: rust-queue-watch.md:21,31
- [x] Canonicalize watched paths before insert/UNIQUE check (watch.rs:45-60).
- [x] `reconcile` purges `pending` entries under removed/disabled watches
  (watcher.rs:440-483); mind the mode-flip re-add case.

### B6 — ACL + CSP hardening
Refs: rust-app-shell.md:57,67-70,79; claude-automation.md:53,81 → **D1**
- [x] Remove `fs:default`, `notification:default`, 3× `autostart:allow-*`,
  2× `window-state:*` grants (all verified unused, incl. dynamic invokes).
- [x] Remove `tauri-plugin-fs` entirely (dep + registration + grant); check
  `tauri-plugin-opener` usage likewise.
- [x] Set a real CSP in tauri.conf.json (currently `null`) per **D1**.
- Verify with the acl-auditor agent + manual run (notifications, watched
  folders, autostart toggle, window position persistence).

### B7 — Windows filename display
Refs: tests-quality.md:83; frontend.md:99
- [x] `fileName()` splits on `/` only; backend emits OS-native `\` paths on
  Windows → every queue/history/active row shows the full path. Fix split to
  `/[\\/]/`, add `C:\...` test case, delete WatchedFoldersPage's duplicate
  `basename()`. (Notifications unaffected — built backend-side.)

### B8 — Frontend state & IPC hygiene
Refs: frontend.md:31,45,46,62,72,136,137,138 (all 9 verified confirmed)
- [x] SettingsPage text inputs (handbrake_path, watch_skip_marker, suffix
  template): draft + commit-on-blur/Enter (WatchRow pattern) — kills the
  per-keystroke IPC race.
- [x] `updateSetting` optimistic merge; fix the validate-races-write ordering.
- [x] `useQueue` refresh: monotonic counter to drop stale `getQueue` responses.
- [x] Move suffix default out of `refresh()` read path into Rust. Backend now
  owns `DEFAULT_SUFFIX_TEMPLATE`: `get_preset_suffix` returns it when unset AND
  the conversion read path (`queue.rs`) falls back to it, so removing the
  frontend write doesn't regress unconfigured presets to in-place encoding.
- [x] Expose backend `pause_after_current` flag via `get_pause_after_current`
  query; ActiveJob seeds its button from it on mount (tab remount = fresh read),
  dropping the local mirror and its 2 extra desync vectors.
- [x] Replace SettingsPage `resolveTemplate` JS copy with a backend command
  (proven divergent: `..h265` vs `.h265`).
- [x] Surface rejected invokes (DropZone folder-confirm + ActiveJob controls +
  QueueItem remove double-click guard).

### B9 — Release pipeline hardening
Refs: ci-release.md:18,32,33 → **D4**
- [ ] Tag the PR's `mergeCommit` oid, assert HEAD matches after pull
  (release.sh:155-162; `--ff-only` gives zero staleness protection).
- [ ] Failed-build recovery: restore bumped manifests or print the recovery
  command (note: same-version retry dies at the strictly-newer preflight,
  not clean-tree).
- [ ] `workflow_dispatch` in build.yml: fix or remove per **D4**.

### B10 — CI coverage
Refs: ci-release.md:7 → **D2**; tests-quality.md:47,65 → **D6**, 90
- [ ] Advisory (non-required) windows-latest PR job per **D2**.
- [ ] De-`#[cfg(unix)]` the cancel-deadlock test (platform-neutral child).
- [ ] Scheduled/main-only job running `cargo test -- --ignored` with
  HandBrakeCLI+ffmpeg per **D6**.
- [ ] IPC contract check: extract `invoke("...")`/`listen("...")` literals
  from src/ and check against `#[tauri::command]`/`emit` literals in src-tauri.

### B11 — Test backfill
Refs: tests-quality.md:8,36,45,46,56
- [ ] Old-schema migration test (hand-create pre-fingerprint jobs table,
  run init_db, assert columns + writability).
- [ ] Extract + fixture-test `parse_preset_list` (indentation-based parsing).
- [ ] Extract `process_queue` per-job state machine into testable function;
  cover status transitions, pause gate, pause_after_current reset.
- [ ] Reaper single-tick test (growing file stays pending; stable file
  enqueues exactly once).

### B12 — Automation & docs refresh
Refs: claude-automation.md:13→**D8**, 22→**D7**, 72,73,89,90,108,109
- [ ] Rewrite sqlite-migration-reviewer agent: describe the existing
  idempotent-ALTER+backfill pattern (db.rs:73-87), derive tables from db.rs.
- [ ] Stop-hook version-sync warning output mode per **D7**.
- [ ] rustfmt PostToolUse hook strategy per **D8**.
- [ ] SPEC.md: mark macOS-only scope + hardcoded-paths sections superseded.
- [ ] RECOMMENDATIONS.md: move autostart to implemented; delete "no tests".
- [ ] CLAUDE.md/acl-auditor/add-tauri-plugin: consistent after B6 lands.

## Recommended sequence

1. **B1** — one-line user-facing breakage, ship immediately.
2. **B2 → B3 → B4** — backend correctness users feel; B2 first (established
   pattern), B3/B4 independent of each other.
3. **B7** — small, high-visibility Windows fix; pairs naturally with the
   B10 windows-CI item if D2 is approved.
4. **B5, B6** — independent; B6 needs D1 decided.
5. **B8** — largest frontend batch; can run in parallel with backend batches
   (worktree isolation if parallel agents).
6. **B9, B10** — after decisions D2/D4/D6; low urgency (release cadence is
   occasional).
7. **B11, B12** — anytime; B11 ideally lands before/with B2-B4 so extracted
   functions get tests as they're touched. B12's ACL doc updates depend on B6.

Batches are mutually independent except: B12 doc-consistency after B6;
B10's windows job benefits B7 verification. TDD per repo rules: every bug
fix lands with the test that would have caught it.
