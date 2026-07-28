# Round 2 — rust-core (verification pass, 2026-07-08)

Status: in progress — findings appended incrementally by the reviewing subagent.

## Fix verification

Round-1 rust-core.md had 3 Medium findings (no Highs). One Low (zero-byte output) was promoted into B3 via D3, so it is verified here too. Verified against main @ daf4f8e; diff base eb44d4b.

### 1. Probe stdout only read after exit → 30s stall per oversized scan (Medium, B3)
**Verdict: FIXED.**
- `probe_source` now delegates to `scan_with` (src-tauri/src/probe.rs:70-101), which takes `child.stdout` and drains it to EOF on a spawned thread (probe.rs:89-95) *concurrently* with `wait_with_timeout` (probe.rs:97). Join happens after the wait returns (probe.rs:99), and on the timeout path the child is killed first (probe.rs:113-116), so the reader hits EOF and the join cannot hang.
- Ordering is correct: `status?` is checked *after* joining the drain thread (probe.rs:99-100), so no thread is leaked on timeout.
- Regression test `scan_survives_output_larger_than_the_pipe_buffer` (probe.rs:206-228) floods 200KB (>~64KB pipe buffer) through a scan-shaped `/bin/sh` process and asserts completion in <10s with the trailing title set parsed. Unix-only (`#[cfg(unix)]`), acceptable for a pipe-buffer mechanism test.

### 2. Four sync commands shell out on the main thread (Medium, B2)
**Verdict: FIXED.**
- All four are now `async` + `tauri::async_runtime::spawn_blocking`: `detect_handbrake` (src-tauri/src/commands/handbrake.rs:65), `list_handbrake_presets` (:75), `generate_preset_suffix` (:88), `validate_handbrake` (:111). Registered in lib.rs:61-64,85; frontend call sites unchanged (src/lib/tauri.ts:138-151).
- The triage rider "give validate a timeout like probe.rs" is honored: `handbrake_version` (commands/handbrake.rs:144-162) spawns `--version` and bounds it with `crate::probe::wait_with_timeout(…, VERSION_CHECK_TIMEOUT)` (10s, :10). Reading stderr *after* exit is fine here — `--version` output is tiny (comment at :141-143 states the rationale).
- The related B2 item (preset_cache mutex held across the shell-out) is fixed via the extracted `cached_preset_metadata` helper (commands/handbrake.rs:36-58): check → unlock → fetch → relock → insert, with the lock-convoy rationale documented. `queue.rs:329,348` now call this helper instead of duplicating the block.
- New sync command `resolve_suffix_template` (commands/handbrake.rs:138) is pure string substitution — no subprocess, no DB — so sync is correct.

### 3. Stderr drain discards HandBrake diagnostics; failures recorded as literal 'Conversion failed' (Medium, B3)
**Verdict: FIXED.**
- Drain thread now returns a bounded tail: `read_bounded_tail` (src-tauri/src/converter.rs:314-327) keeps the last 4KB while still draining to EOF (pipe can never fill; memory bounded). Wired at converter.rs:541-544.
- Error branch joins the tail thread (converter.rs:821-823 — safe: child already reaped, so EOF guarantees a prompt join) and stores `error_message_from_tail(&tail)` (converter.rs:333-341: last 20 non-empty lines, `"Conversion failed:\n…"` prefix, generic fallback for empty tail) via the shared `record_job_error` (converter.rs:344-388).
- Tests pin the bounded-flood behavior (converter.rs:971-981), the empty-tail fallback (:984-989), and the 20-line window (:992-1006).
- The success path drops the JoinHandle without joining — harmless (thread ends at EOF after child exit; detached, bounded).

### 4. Zero-byte output after exit 0 recorded as "done — saved 0B" (Low in rust-core.md, promoted via D3 → B3)
**Verdict: FIXED (per D3 option a).**
- Guard at converter.rs:620-635: after a successful exit, `converted_size.unwrap_or(0) == 0` (covers both missing and empty output) sets `had_errors`, removes the empty/partial output (for in-place jobs this is the temp — the source is untouched), and records "Conversion produced an empty output file" via `record_job_error`, then `continue`s. Cleanup can no longer trash a source in favor of a 0-byte file.
- State hygiene on the `continue` path is correct: `current_pid`/`current_child`/`current_job_id` are cleared before the match (converter.rs:610-612), so nothing stale leaks into the next iteration.
- Residual doc drift noted in New findings (decide_cleanup's zero-size matrix row is now dead at runtime).

**Tally: 4/4 FIXED** (3 Medium + 1 promoted Low). 0 partial, 0 not fixed, 0 regressed, 0 N/A-by-decision.

## New findings

### N1 — [Medium] Quit-mid-queue can still orphan a HandBrake encoder: `kill_active_child` has no queue-shutdown signal
- converter.rs:135-152 (`kill_active_child`), lib.rs:401-410 (`RunEvent::ExitRequested` handler), converter.rs:408ff (`process_queue` loop head has no shutdown check).
- The B4 fix kills and reaps the *current* child on exit, but nothing tells the queue thread to stop. After the kill, the queue thread's `try_wait` sees the exit, runs the error branch (a few DB writes/emits), loops, picks the **next queued job, and spawns a fresh HandBrakeCLI** (converter.rs:497-507) — racing the main thread's post-handler teardown. `std::process::exit` does not kill child processes, so if the queue thread wins (its path to the next spawn is milliseconds; Tauri teardown after the handler is not instant), the app exits leaving a brand-new orphaned encoder running for hours against the next file — exactly the failure mode B4 set out to fix, now confined to the multi-job case.
- Smaller window, same class: if ExitRequested fires between `Command::spawn` (converter.rs:497) and the handle being stored in `current_child` (converter.rs:547), `kill_active_child` finds `None` and the *current* encode is orphaned.
- Fix shape: a shutdown `AtomicBool` on `ConverterState`, set in the ExitRequested handler *before* the kill, checked at the top of the `process_queue` loop (and ideally between spawn and store). One flag closes both windows to near-zero.

### N2 — [Low] Two failure paths bypass `record_job_error`: tray ends "idle" and no notification despite failed jobs
- converter.rs:420-435 (HandBrakeCLI-not-found) and converter.rs:509-530 (spawn failure) mark the job `error` in the DB and emit `job-error`, but do **not** set `had_errors = true`, do not emit `job-status-changed`, and send no failure notification — unlike every other failure path, which now goes through `record_job_error` (converter.rs:344-388).
- Scenario: HandBrakeCLI is removed/moved while jobs are queued → every job errors, yet `final_run_status` (converter.rs:858) returns "idle", the tray shows a clean finish, and the user gets only a "Queue complete" notification. Pre-existing behavior, but the B3 refactor extracted the shared helper and stopped one step short of these two branches; converting both to `record_job_error` + `had_errors = true` is a five-line fix.

### N3 — [Low] Zero-byte failure discards the stderr tail it already holds
- converter.rs:624-635: the D3 guard records the fixed string "Conversion produced an empty output file" while `stderr_tail_thread` is alive and joinable in scope — HandBrake's stderr (e.g. "No title found", muxer errors) almost certainly says *why* the output was empty. B3's whole point was surfacing that diagnostic; this new failure path drops it. Join the tail and append it (reusing `error_message_from_tail`'s tail-window) to the message.

### N4 — [Low] `decide_cleanup`'s zero-size row and `KeptFile::Neither` are now dead code pinned by a misleading test
- The guard at converter.rs:624 makes `conv_size == 0` unreachable at the `decide_cleanup` call site (converter.rs:645-647), so the `KeptFile::Neither` arm (converter.rs:164-165,177-178) can no longer occur at runtime, yet the matrix test still pins `(1000, 0) → Neither/"done"` with the comment "No/zero output … done" (converter.rs:1155-1156) — the exact semantics D3 rejected. A future refactor that trusts the matrix could reintroduce the bug. Update the row to document that the call site guards zero upstream (or make `decide_cleanup` itself return an error-ish status and delete the guard's duplication).

### Carried-over round-1 Lows (unbatched by triage policy; re-confirmed still open, unchanged severity)
- **wait_with_timeout `Err(_)` leak** — probe.rs:120 still returns `None` without kill/reap; now also reachable from `handbrake_version` (commands/handbrake.rs:154), so the (rare) leaked-child path spreads to validate.
- **`get_preset_metadata`: no exit-status check + UTF-8-unsafe slice** — handbrake.rs:71-89; `&stdout[..min(200)]` (handbrake.rs:85) still panics if byte 200 splits a codepoint. Mitigation since B2: the call now runs inside `spawn_blocking`, so the panic surfaces as a JoinError string to the frontend instead of taking down the main thread.
- **`list_presets` silently returns `Ok(vec![])` on CLI failure** — handbrake.rs:39-49 still ignores `output.status`; the new `parse_preset_list` fixture tests (handbrake.rs:287-317) are a real improvement but the empty-output test pins Ok-empty as acceptable.
- **Missing probe height → wrong skip** — probe.rs `parse_scan_media` still maps absent `Geometry.Height` to 0 and media_skip.rs:44-56 still reads 0 as "no downscale benefit" (skip), contradicting the unknown-means-uncertainty policy. media_skip.rs/probe parsing unchanged since baseline.
- **Progress event flood** (converter.rs:553-599, no throttle), **poisoned-mutex `is_running` wedge** (converter.rs:871, `run_queue` converter.rs:876-888 has no unwind guard), **probe_cache no eviction** (probe_cache.rs unchanged) — all as described in round 1.

## Summary

All four tracked findings in scope (3 Medium + the D3-promoted zero-byte Low) are genuinely fixed, with regression tests that encode the failure mechanism, not just the behavior: the probe pipe-buffer test floods past 64KB, the stderr-tail tests pin boundedness and the 20-line window, and the cancel-deadlock test now runs on Windows too. The B2 async conversion is complete and correct (all four commands, timeout on validate, lock-convoy fix via `cached_preset_metadata`), and the new `resolve_suffix_template` command is legitimately sync. probe_cache.rs, media_skip.rs, and types.rs are untouched since baseline, consistent with their Low/Nit-only round-1 status.

The one real new concern is N1: the B4 quit-kill fix reaps the current child but leaves the queue thread free to spawn the next one during teardown, so the orphaned-encoder bug it targets survives in the multi-job quit case. Everything else new is Low: two pre-existing failure paths left out of the `record_job_error` consolidation, a dropped diagnostic on the new zero-byte path, and test/doc drift around the now-dead `KeptFile::Neither`.

## Recommendations

1. (Medium) Add a shutdown flag to `ConverterState`, set it in the `ExitRequested` handler before `kill_active_child`, check it at the top of the `process_queue` loop — closes N1's respawn race; also re-check it between spawn and the `current_child` store.
2. (Low, small) Route the not-found and spawn-failure branches through `record_job_error` + `had_errors = true` (N2), and join/append the stderr tail in the zero-byte branch (N3).
3. (Low, doc) Fix the `decide_cleanup` matrix row and `KeptFile::Neither` docs to reflect the upstream zero-byte guard (N4).
4. (Backlog, unchanged) The carried-over Lows above — the `&stdout[..200]` slice and `list_presets` status check are the cheapest to ride along with the next handbrake.rs change.
