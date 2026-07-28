# Implementation Triage — Fable Review Round 2

Source: the seven round-2 reports in this directory (4 Medium, 29 Low new
findings; all 56 round-1 High/Medium verdicts FIXED or PARTIAL). Same rules as
round 1: each batch = one branch + one PR; Lows ride along with whichever batch
touches their file, or stay documented here.

## Batches

### R1 — Converter teardown & diagnostics (branch `fix/queue-shutdown-flag`)
- [x] N1 (Medium, rust-core): shutdown `AtomicBool` on `ConverterState`, armed
  by `kill_active_child` before touching the child, checked at the
  `process_queue` loop head and re-checked after the child-handle store —
  closes both orphaned-encoder windows on quit. Test-first (contract test:
  kill arms the latch).
- [x] N3 (Low, rust-core): zero-byte failure joins the stderr drain and keeps
  the diagnostic tail via `empty_output_error_message` (shared
  `message_with_tail`). Test-first.
- [x] N4 (Low, rust-core): `decide_cleanup` matrix rows for zero output now
  document that the call-site guard makes them unreachable (D3), instead of
  reading as licence for "0 bytes = done".

### R2 — process_queue / cancel coverage (branch `test/round2-coverage-gaps`, stacked on R1)
- [x] M2 (tests-quality): mock-runtime harness (tauri `test` feature as
  dev-dependency; `process_queue`/`run_queue`/`record_job_error`/
  `cancel_conversion` genericized over `tauri::Runtime`) + `#[ignore]`
  real-encode test driving queued→encoding→done with ffmpeg+HandBrakeCLI
  (verified locally: passes with a real encode). TRIAGE round-1 B11's false
  e2e-coverage claim corrected in place.
- [x] M3 (tests-quality): zero-byte guard pinned — fake HandBrake script exits
  0 writing nothing; asserts error status, stderr tail in the message, output
  removal, queue continuation, final tray "error". Proven fail-capable by
  neutering the guard (RED) and restoring.
- [x] M4 (tests-quality): cancel ordering pinned — child holds the partial
  output's handle (stdout redirect); asserts kill→reap→delete leaves no file,
  DB row 'error'/"Cancelled by user", handle cleared. The handle-lock
  assertion has real teeth on Windows (test-windows.yml runs it on PRs);
  proven fail-capable by removing the delete.
- [x] N2 (Low, rust-core): HandBrake-not-found and spawn-failure branches now
  route through `record_job_error` + `had_errors` (not-found check moved out
  of the db-lock scope to avoid a deadlock with the shared helper). Test-first
  (RED: no `job-status-changed`, tray ended "idle"). Not-found branch itself
  has no dedicated test — `get_handbrake_path` falls back to PATH detection,
  which can't be forced empty reliably in-process; it shares the exact helper
  the spawn-failure test pins.

### R3 — Hygiene sweep (branch `chore/round2-hygiene`)
- [x] ci-release #1: `permissions: contents: read` in test.yml and
  test-windows.yml.
- [x] ci-release #2: test-windows.yml paths filter includes the workflow file
  itself.
- [x] ci-release #3: merged-but-untagged failure branches in
  `merge_and_tag` now print the full recovery recipe (query + tag + push).
- [x] ci-release #4: manifest-restore failure is no longer masked by
  `|| true` — warns with the manual cleanup command.
- [x] ci-release #6: test-windows.yml header documents the never-make-required
  trap (paths-filtered workflows report no status on frontend-only PRs).
- [x] ci-release #7: e2e-ignored.yml header notes GitHub's ~60-day cron
  auto-disable.
- [x] claude-automation #1: acl-auditor realigned — plugin list is
  updater/process only, dead `autostart:allow-enable` example dropped,
  app-defined `#[tauri::command]` ACL exemption stated in the collect step.
- [x] claude-automation #2: removed the four unused `@tauri-apps/plugin-*` npm
  packages (autostart, dialog, notification, window-state — Rust halves stay).
- [x] claude-automation #4: RECOMMENDATIONS.md residuals (header version note,
  US1/US2 → Done, item 9 no longer says to add the already-registered dialog
  plugin).

## Deferred (documented, deliberately not batched)

- ~~claude-automation #3 — `stop_hook_active` guard~~ **landed in R3 after
  all**: Claude's edit was blocked by the permission classifier (hook
  self-modification), the user applied it by hand, and it shipped in #77 —
  verified against all four fixture cases (in-sync/mismatch × flag on/off).
- ~~**ci-release #5** (trap-based restore covering `bump_manifests` itself)~~
  **kept as-is by decision D16** (see R8 below): the marginal gain over the
  existing build-failure restore is only an unlikely post-build commit/signing
  failure, the outcome is recoverable via the next run's preflight, and the
  script can't be driven past `preflight` in the CI harness — so a trap can't
  meet the failing-test-first bar without a test-only bypass hook.
- ~~**frontend 5 Lows** (in-flight response races, `updatePresetSuffix` blur-retry
  asymmetry, DropZone stale-snapshot filter)~~ **fixed in R5** (see below).
- ~~**rust-queue-watch 4 Lows** (no canonicalization backfill for pre-fix watch
  rows, nested-watch purge overreach, uncancellable in-flight scans, preset
  repair matching user presets by name)~~ **fixed in R7** (D11–D14, see below).
- ~~rust-app-shell Low: quit-vs-queue race~~ **fixed in R4** (see below) — a
  targeted round-3 pass over the R1/R2 diff upgraded it to Medium because the
  R1 latch codified the racy path instead of closing it.
- ~~**rust-app-shell Low: silent updater install failure**~~ **fixed in R8**
  (D15, see below); the rust-core carried-over round-1 Lows were **fixed in R6**.
- ~~**round-3 Low (new):** `cancel_conversion` clears `current_child` but leaves
  `current_pid` set for up to ~100ms~~ **fixed in R6** (see below).

### R4 — Quit preserves the in-flight job (round-3 targeted pass)
- [x] The round-3 reviewer (single Fable agent over `git diff daf4f8e..main`
  for converter/commands/build) found the one gap in R1's latch: after
  `kill_active_child` reaps the child, the queue thread's error arm could
  still win a scheduler race during teardown — deleting the partial, writing
  `status='error'` (which next-launch auto-resume ignores), and firing a
  "failed" notification. Fix: the error arm bails via the loop-head return
  when `is_shutting_down()`. Test-first with a slow fake encoder killed
  mid-encode: row stays `'encoding'`, partial stays on disk, no job-error
  events. Everything else in the R1/R2 diff was attacked and verified sound
  (spawn→store happens-before, double-reap semantics, N2 lock scopes,
  zero-byte join EOF guarantee, generic command registration, dev-dep feature
  isolation, Windows `--lib` coverage).

### R5 — Frontend race guards (branch `fix/frontend-race-guards`, PR #81)
The five round-2 frontend Lows, all unguarded in-flight-response races in the
family the B7/B8 fixes targeted. TDD: a failing test first for each, every one
proven fail-capable (the stale/late response wins on the pre-fix code).
- [x] N1 (Low, frontend): suffix-preview resolve now carries a generation guard
  (an `active` flag flipped in the effect cleanup), so a late resolve of a
  superseded draft can't overwrite the preview or setState post-unmount.
  Test in SettingsPage.test.tsx (older resolve lands after a newer edit).
- [x] N2 (Low, frontend): `updateSetting`'s failure path restores only the
  failed key — pre-edit value captured eagerly from the render closure (a
  lazy capture inside the state updater hadn't run by the time the catch needed
  it) — instead of a whole-object `get_settings` refetch that could resolve out
  of order and clobber a concurrent optimistic edit to a different key.
- [x] N3 (Low, frontend): `updatePresetSuffix` now mirrors `updateSetting` and
  rolls back to the pre-edit suffix on failure, so SettingsPage's
  `suffixDraft !== presetSuffix` commit guard still sees a diff and a re-blur
  retries instead of silently no-op'ing.
- [x] N4 (Low, frontend): preset-scoped suffix+metadata loads unified into a
  `loadPresetData` helper stamped with a monotonic `latestPresetLoad` counter
  (the useQueue pattern); out-of-order resolution of two rapid preset switches
  no longer leaves stale suffix/metadata under the newer preset.
- [x] N5 (Low, frontend): DropZone confirm/skip removals are keyed by
  `folder_path` against a synchronously-updated `pendingRef`, not an index into
  the render-time snapshot, so a slow-resolving confirm can't resurrect an
  already-removed folder or wedge the "last one → startQueue" check.

acl-auditor: ACL-neutral (no new `core:`/`plugin:` surface, `default.json`
unchanged). Full frontend suite green (78 tests), `npm run build` (tsc) clean.

### R6 — rust-core mechanical lows (branch `fix/rust-core-lows`, PR #82)
The carried-over round-1 Lows in rust-core plus the round-3 `current_pid` Low.
Test-first; each behavior change proven fail-capable by neutering the fix.
- [x] `get_preset_metadata` UTF-8-unsafe slice + no exit-status check
  (handbrake.rs): extracted a pure `interpret_preset_export` — a non-zero exit
  now surfaces HandBrake's stderr diagnostic instead of a misleading JSON-parse
  error, and the 200-byte diagnostic slice goes through `truncate_str`, which
  backs up to a char boundary so a multibyte codepoint at byte 200 can't panic.
- [x] `list_presets` silently `Ok(vec![])` on CLI failure (handbrake.rs):
  extracted `interpret_preset_list` — a non-zero exit with nothing parseable
  returns `Err` (so the UI shows "couldn't load presets", not an empty
  dropdown); a non-zero exit that still printed presets keeps them.
- [x] `wait_with_timeout` `Err(_)` leaked the child unreaped (probe.rs:120):
  both give-up paths (timeout and `try_wait` error) now route through a shared
  `kill_and_reap`, tested to actually terminate + reap.
- [x] round-3 Low: `cancel_conversion` cleared `current_child` but left
  `current_pid` set (~one poll interval) — a racing quit could SIGCONT a reaped,
  possibly-recycled PID on macOS. It now clears `current_pid` too (asserted in
  the cancel test).
- [x] `is_running` wedge: `process_queue` now resets `is_running` via an RAII
  `RunningGuard` (fires on an unwinding panic, not just normal return), and
  `run_queue` acquires the lock poison-tolerantly — a crash can no longer leave
  the flag stuck true and permanently block queue starts.
- **Dropped, decisions recorded:** progress-event throttle → [D9](../DECISIONS.md#d9);
  probe_cache eviction → [D10](../DECISIONS.md#d10). Neither is a correctness
  issue; both are policy/perf items outside a mechanical Low batch.

cross-platform-reviewer clean; full Rust suite green; `cargo fmt --check` clean;
no new clippy warnings.

### R7 — queue-watch design items (branch `fix/watch-lifecycle-lows`, PR #83)
Four rust-queue-watch Lows, each a design call taken with the user via
AskUserQuestion and recorded in [DECISIONS.md](../DECISIONS.md) (D11–D14).
Test-first; each behavior change proven fail-capable by neutering.
- [x] **D11 (R7.1) — canonicalization backfill.** New init_db migration
  `backfill_canonical_watch_paths`: rewrites each pre-fix watched_directories
  row to its canonical (dunce) path, dropping a row that would collide with an
  existing canonical one (UNIQUE(path)). Idempotent; non-existent paths pass
  through unchanged. Closes the duplicate-watcher gap on re-add.
- [x] **D12 (R7.2) — nested-watch purge overreach.** Replaced
  `removed_watch_roots` + `purge_pending_under` (drop everything under a removed
  root) with `purge_pending_uncovered` (retain any pending entry a *desired*
  config still covers, via `delay_for_path`). A file under a still-active nested
  watch survives removal of its enclosing watch; also subsumes the old mode-flip
  special-case.
- [x] **D13 (R7.3) — uncancellable in-flight scans.** `enqueue_and_start` now
  runs `filter_watched` (pure core `covered_paths`) first: a background scan
  whose watch was removed mid-scan enqueues nothing from that folder, and the
  reaper is hardened against the same-tick remove/stabilize race. Config re-check
  at the single chokepoint; safe because reconcile populates configs before any
  scan runs.
- [x] **D14 (R7.4) — by-name preset repair breadth.** Left as-is by decision:
  a same-named custom preset can't be reliably told from the seeded bad default
  at init; self-limiting. No code change.

sqlite-migration-reviewer + cross-platform-reviewer clean; full Rust suite green;
`cargo fmt --check` clean; no new clippy warnings.

### R8 — misc lows (branch `fix/r8-misc-lows`, PR #84)
The last two backlog Lows plus the ci-release #5 decision. Test-first; both
behavior changes proven fail-capable by neutering.
- [x] **Silent updater install failure (D15).** The startup auto-updater
  notified only on a successful install; a failed `download_and_install` was
  fully silent, stranding the user on the old version with no signal. Now both
  outcomes notify (extracted `update_install_notification(version, installed)`,
  unit-tested) and failures also `eprintln`. An offline *check* stays quiet by
  design (separate, correct). Consistent with D5's "no invisible updates".
- [x] **Missing probe height → wrong skip verdict.** `parse_scan_media` maps an
  absent `Geometry.Height` to `0`; `should_skip_by_media` read `0` as "no
  downscale benefit" and could skip a file whose true resolution is unknown.
  Added a `source_height <= 0 → never skip` uncertainty guard, mirroring the
  existing unknown-codec policy. Pure `media_skip` change; new table test.
- [x] **ci-release #5 — kept as-is (D16).** No trap added; rationale recorded in
  DECISIONS.md (untestable past `preflight` in CI + marginal, recoverable gap).

No platform-specific code introduced (a notification-body string + a pure i64
guard); full Rust suite green (148), `cargo fmt --check` clean, no new clippy.

## Outcome (2026-07-08)

All batches merged: reports+triage **#75**, R1 **#76**, R3 **#77** (grew two
ride-alongs: the user-applied Stop-hook loop guard and a CLAUDE.md note on
whole-plugin vs JS-half-only removal), R2 **#78** (rebased onto main after R1).

R2 needed two follow-up commits for Windows: the mock-runtime tests die at
load on windows-msvc without a Common-Controls v6 manifest
(STATUS_ENTRYPOINT_NOT_FOUND, tauri-apps/tauri#11028 — caught by the advisory
`rust-windows` job doing exactly its B10 job). `rustc-link-arg-tests` can't
reach lib unit tests and a blanket link-arg would double-manifest the app
binary against tauri-build's bins-only resource, so the Windows CI jobs now
run `cargo test --lib` with RUSTFLAGS embedding
`src-tauri/windows-test-manifest.xml` (rationale comment in build.rs).
