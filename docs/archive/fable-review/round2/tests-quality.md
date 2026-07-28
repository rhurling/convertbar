# Round 2 — tests-quality (verification pass, 2026-07-08)

Status: in progress — findings appended incrementally by the reviewing subagent.

## Fix verification

Round-1 High/Medium findings from docs/fable-review/tests-quality.md, verified against main @ daf4f8e.

### 1. [Medium] db.rs old-schema ALTER migration only trivially exercised — FIXED
`init_db_upgrades_an_older_jobs_table_missing_the_fingerprint_columns` (src-tauri/src/db.rs:407-470) hand-creates the pre-fingerprint `jobs` schema, runs `init_db` against it, and asserts (a) the ALTER added both columns (the SELECT would error otherwise), (b) the pre-existing row survives with NULL fingerprints, (c) the upgraded table is writable through the new columns (UPDATE backfill + INSERT with fingerprints). This is exactly the requested test. TRIAGE claims it was verified RED by neutering the ALTER loop; the assertion structure supports that (the `SELECT source_size, source_mtime` at db.rs:443 fails hard without the migration). Quality: intent-carrying comments, no path-join comparisons. Passes locally.

### 2. [Medium] handbrake.rs `list_presets` indentation parsing untested — FIXED
`parse_preset_list` extracted (src-tauri/src/handbrake.rs:54-65) and wired into the real call site (`list_presets` at handbrake.rs:46 — no drift risk, single caller). Fixture test `parse_preset_list_keeps_only_four_space_indented_preset_names` (handbrake.rs:287-312) covers col-0 category headers, 4-space presets, 8-space property lines, 4-space trailing-slash nested category, and blank lines; plus an empty-output test (315-317). Each exclusion rule in the fixture would fail the test if the indentation logic regressed. Fixture is hand-written rather than captured raw output, but structurally faithful.

### 3. [High] converter.rs `process_queue` core loop has no coverage — PARTIAL
What landed (B11): `take_pause_after_current` (converter.rs:391-394, consumed at the real call site :788) and `final_run_status` (:398-404, used at :858) extracted and tested (:937-958) — both tests genuinely pin one-shot semantics and the error/idle transition. But these are two micro-decisions out of a ~450-line loop; spawn/progress-streaming, the queued→encoding→done/error DB transitions, notification decisions, kill/error paths, and the in-place handoff remain untested.

**TRIAGE.md's closing claim is false**: B11 says the DB status transitions are "integration-level, exercised by the enforced `e2e-ignored` job (B10)". The only two `#[ignore]` tests in the repo are `add_files_inner_skips_at_target_source_end_to_end` (commands/queue.rs:1651-1739) and `probe_source_reads_real_clip` (probe.rs:252-277). Neither invokes `process_queue` or runs a conversion — they cover add-time skip logic and probing only. Additionally, `e2e-ignored.yml` is weekly + push-to-main and explicitly "not a required PR check" (e2e-ignored.yml:7), so "enforced" overstates it. The largest untested surface in the app identified in round 1 is still the largest untested surface, minus two small decisions. Carried forward as a new finding (severity High → now Medium residual, see New findings N1).

### 4. [Medium] converter.rs pause/resume zero tests — PARTIAL
Flag mechanics are now tested: `take_pause_after_current_consumes_the_flag_exactly_once` (converter.rs:937-950) and `is_pause_after_current_reflects_the_backend_flag` (:961-968, backing the B8 `get_pause_after_current` query). Still untested: `can_pause_process()` platform gating (converter.rs:119), the SIGSTOP/SIGCONT issuance paths in `pause_conversion`/`resume_conversion` (commands/converter.rs:36,63,112,139), and the queue-level pause fallback — commands/converter.rs has no `#[cfg(test)]` module at all. The half of the finding that was grouped under "flag mechanics" is fixed; the platform-gating half is not.

### 5. [Medium] cancel-deadlock test `#[cfg(unix)]` — FIXED
The cfg gate is gone: `cancel_can_kill_child_while_wait_in_progress` (converter.rs:1094-1141) is unconditional, and `spawn_long_running_child` (converter.rs:1075-1089) provides a platform-neutral child (`ping -n 31 127.0.0.1` on Windows, `sleep 30` elsewhere) with a comment naming the intent. Complemented by the new advisory `test-windows.yml` PR job (paths-gated to `src-tauri/**`, per D2), so this test now actually runs on Windows pre-merge when Rust changes. Residual (pre-existing Low, unchanged): the 200ms readiness sleep at converter.rs:1116 can still false-PASS on a loaded runner by not exercising the contended path.

### 6. [Medium] watcher.rs pipeline glue untested — FIXED (as scoped by the round-1 fix)
The recommended fix — extract the reaper's single-iteration body and test one tick — landed precisely: `reap_pending_once` (watcher.rs:284-304), called by the real reaper loop at watcher.rs:314 (no test/prod drift; single call site), with three tests (watcher.rs:619-682) pinning settle-once-and-drop (including the second-tick re-enqueue guard), growing-file-stays-pending, and vanished-file-dropped. Windows-safe: keys are built with `PathBuf::from`/compared via the same `PathBuf`, and the B5 purge tests use `Path::new("removed").join(...)` (watcher.rs:742-743) — the separator lesson held. Residual by design: `build_watcher` notify-event filtering and `enqueue_and_start` remain untested glue (acknowledged in round 1 as the fallback position).

### 7. [Medium] `#[ignore]`d e2e tests never run in CI — FIXED (per D6)
`.github/workflows/e2e-ignored.yml` runs `cargo test -- --ignored` weekly (Mon 06:00 UTC) and on every push to main, installing `handbrake-cli` + `ffmpeg` via apt (e2e-ignored.yml:26-36). Both ignored tests are e2e-shaped and covered by that invocation. Caveat: it is advisory (not a required check) and schedule-failures on a quiet repo only notify watchers — acceptable per D6(a), but "enforced" (TRIAGE B11's word) it is not.

### 8. [High] `fileName()` splits on `/` only; test enshrined the Windows bug — FIXED
src/lib/format.ts:22-26 now splits on `/[/\\]/` with `.filter(Boolean)` and a why-comment; src/lib/format.test.ts:39-49 adds the `C:\\Users\\...\\clip.mp4` regression case (with a comment naming the shipped-bug consequence), a trailing-separator case, and a no-separator passthrough. WatchedFoldersPage's duplicate `basename()` was deleted — it now imports the shared `fileName` (src/pages/WatchedFoldersPage.tsx:3,45), eliminating the round-1 two-conventions conflict.

### 9. [Medium] IPC string contract untested — FIXED
`src/test/ipc-contract.test.ts` (85 lines) extracts `invoke("...")`/`listen("...")` literals from src/ (test files excluded) and checks them against `#[tauri::command]` fn names and `.emit`/`.emit_to`/`.emit_filter` literals in src-tauri/, plus a non-empty-surface guard (>10 commands both sides) against a silently broken scan. It runs inside `npm test` in the required `frontend` job (test.yml:24). Verified against current usage: every frontend invoke is a string literal in src/lib/tauri.ts and matches the extraction regex (greped for non-literal or regex-defeating forms — none exist). Residuals: (a) arg-key casing (`stabilityDelaySecs` ↔ Rust arg names) — part of the round-1 finding text — is still unchecked; (b) the regex has known silent false-negative shapes, see New findings N4.

**Fix-verification tally: 6 FIXED, 3 PARTIAL (items 3, 4; item 9 fixed-with-residual counted as FIXED), 0 NOT FIXED, 0 REGRESSED, 0 N/A-BY-DECISION.**
Counting strictly: items 3 and 4 are PARTIAL; items 1, 2, 5, 6, 7, 8, 9 are FIXED.

## New findings

### N1 [Medium] — TRIAGE B11 closes the process_queue gap on a false premise; the core loop is still ~fully untested
docs/fable-review/TRIAGE.md:132-137 vs src-tauri/src/converter.rs:408-870. B11's checkbox says the queued→encoding→done/error DB transitions are "integration-level, exercised by the enforced `e2e-ignored` job" — but neither `#[ignore]` test (commands/queue.rs:1651, probe.rs:252) calls `process_queue` or runs any conversion; and the job is weekly/main-only advisory, not enforced. Concrete failure scenario: a regression inside the loop (wrong status written after a kill, error path skipping `record_job_error`, notification-settings logic inverted) passes the entire suite AND the e2e job, exactly as in round 1 — while the triage record says otherwise. Fix: either correct the TRIAGE record (accept the residual risk explicitly) or add the real thing: an `#[ignore]`d integration test that drives `process_queue` against a stub encoder script and asserts the DB status trail.

### N2 [Medium] — D3 zero-byte guard is untested, and the `decide_cleanup` matrix still pins the superseded pre-D3 semantics
src-tauri/src/converter.rs:620-635 (guard) vs :1147-1166 (matrix test). The B3/D3 fix ("zero-byte output is never a success") is inline in `process_queue`: `converted_size.unwrap_or(0) == 0` → remove file, `record_job_error`, `continue`. No test covers it. Worse, after the guard, `decide_cleanup` can never receive `conv == 0` from production code — yet the matrix test's rows 4-5 still pin `(1000, 0) → "done"` and `(0, 0) → "skipped"`, i.e. the exact pre-D3 behavior D3 rejected. Concrete failure scenario: delete or invert the guard and the suite stays green while zero-byte outputs are again recorded "done — saved 0B" (the shipped bug D3 exists to prevent); meanwhile the matrix test actively documents that outcome as intended. Fix: extract the guard decision (e.g. `fn output_is_usable(size: Option<u64>) -> bool`) and test it, and re-comment/re-shape the unreachable matrix rows so they stop asserting superseded intent.

### N3 [Medium] — B4 cancel fix (kill → wait → delete partial) has no test; commands/converter.rs has no test module at all
src-tauri/src/commands/converter.rs:~241-262. The B4 Windows-correctness fix (wait for the killed child to release its file handle before deleting partial output) is precisely the kind of ordering logic a refactor silently breaks: reorder `wait()` after `remove_file` and nothing fails on any platform — on Unix the delete works regardless, and on Windows it silently leaves partial files behind (the original bug). The repo already has the pattern to test this (`spawn_long_running_child`, converter.rs:1075). Fix: a test that kills a platform-neutral child holding an open file, asserts the file is deletable only after `wait()` — or at minimum extract the kill/wait/delete sequence into a testable function.

### N4 [Low] — B3 pipe-drain regression test is `#[cfg(unix)]` while the drained code path is cross-platform
src-tauri/src/probe.rs:206-229 (`scan_survives_output_larger_than_the_pipe_buffer` spawns `/bin/sh`). The stdout-drain-on-thread fix it guards runs on Windows too, and the new advisory `test-windows.yml` job will never execute this test. Same blind-spot family round 1 flagged for `wait_with_timeout` (still unix-only, probe.rs:183,231). Acceptable residual, but the one new test written for a cross-platform fix re-entered the known unix-only ghetto.

### N5 [Low] — ipc-contract extraction regexes have silent false-negative shapes
src/test/ipc-contract.test.ts:44,48,57-59. (a) A nested-generic invoke — `invoke<Record<string, string>>("cmd")` — fails the `(?:<[^>]*>)?` group (first `>` ends the class) and the call is silently skipped, not flagged; (b) `import { invoke as call }` aliasing and non-literal command names are invisible; (c) `once("event", ...)` from @tauri-apps/api/event isn't scanned; (d) Rust-side `#[tauri::command]` occurrences inside comments/strings would register phantom command names. Verified none of these shapes exist today (all invokes are simple literals centralized in src/lib/tauri.ts), and the >10-surface guard catches wholesale scan breakage — but a future nested-generic invoke would drift undetected. Also, arg-key casing (round 1's `stabilityDelaySecs` example) remains outside the contract check. Fix idea: fail the test on any `invoke` call whose first argument is NOT a string literal (invert the burden).

### N6 [Low] — cached_preset_metadata tests cannot catch re-introduction of the lock-across-fetch convoy
src-tauri/src/commands/handbrake.rs:38-58, tests :190-214. The B2 fix's actual point — the cache mutex is released across the `get_preset_metadata` shell-out — is only documented in a comment; the fetch isn't injectable, so a regression that re-holds the lock across the subprocess passes both tests (they assert hit/miss semantics only). Low because the hit-path test's bogus-binary trick is genuinely strong for what it covers. Fix idea: make the fetch a closure parameter (probe_cache.rs's `resolve_media` pattern) and assert the lock is free during fetch via a try_lock inside the stub.

### Positive observations (no action)
- New Rust tests are Windows-clean: `Path::join`-built keys in the watcher purge tests (watcher.rs:742-743), `PathBuf`-vs-`PathBuf` comparisons in the reaper tests, opaque path strings elsewhere. No new hardcoded-separator hazards found.
- B1's preset repair is tested in both directions (repairs the bad seeded value, db.rs:239-274; leaves a user choice alone, :277-294).
- The new frontend tests are non-tautological: useQueue's stale-response test defeats the fix if the monotonic counter is removed (deferred-resolve promises, out-of-order resolution); useSettings' optimistic-merge test counts `get_settings` calls rather than trusting the mock; SettingsPage's `.RESOLVED` sentinel proves the suffix preview is backend-computed, not the JS copy; QueueItem's double-click guard counts real `remove_job` calls across an in-flight promise; every new dispatcher rejects unexpected commands (ActiveJob.test.tsx:57, SettingsPage.test.tsx:66).
- `read_suffix_template` (B8 backend-default move) is tested at the shared helper, and both real call sites go through it (commands/settings.rs:160, commands/queue.rs:289) — no test/prod drift.
- `truncate_tray_title` (B4) pins the exact panic input class (multi-byte at the boundary) plus the 20-char no-truncate edge (lib.rs:419-434).

## Summary

All 9 round-1 High/Medium findings were acted on; 7 are genuinely fixed, 2 are partial. The partials share one root: `process_queue` and the process-control command layer (commands/converter.rs) remain untested, and TRIAGE.md papers over that with an incorrect claim that the e2e-ignored job covers the DB transitions (it covers add-time skip logic and probing only). The new tests added across B7-B11 are of high quality — intent-encoded, fail-loud, Windows-separator-clean, and non-tautological; the extracted functions (`parse_preset_list`, `reap_pending_once`, `take_pause_after_current`, `final_run_status`, `read_suffix_template`, `cached_preset_metadata`) are all wired to their single real call sites, so extraction drift is not a live risk today.

Test suites verified locally (2026-07-08): `cargo test` — 127 passed, 0 failed, 2 ignored; `npm test` (vitest) — 15 files, 73 tests, all passed.

## Recommendations

1. Correct TRIAGE.md B11's e2e claim, or better: add an `#[ignore]`d `process_queue` integration test with a stub encoder (runs in the existing e2e-ignored job) asserting the queued→encoding→done/error DB trail (N1).
2. Extract and test the D3 zero-byte guard; retire or re-document the now-unreachable `decide_cleanup` zero-output matrix rows so the suite stops pinning pre-D3 semantics (N2).
3. Add a kill→wait→delete ordering test for cancel using the existing `spawn_long_running_child` helper (N3) — the only B4 behavior a refactor can silently break on Windows alone.
4. Harden ipc-contract: fail on any non-string-literal `invoke` first argument (N5) — turns silent false negatives into loud ones for one extra assertion.
5. Backfill `can_pause_process` gating tests when commands/converter.rs next gets touched (round-1 item 4's open half).
