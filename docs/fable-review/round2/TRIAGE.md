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

- **claude-automation #3 — `stop_hook_active` guard in check-version-sync.sh:**
  the edit was blocked by the permission classifier (hook self-modification);
  needs the user to apply. Patch: after the `command -v jq` line, exit 0 when
  the Stop-hook stdin JSON has `.stop_hook_active == true`.
- **ci-release #5** (trap-based restore covering `bump_manifests` itself):
  bigger refactor, unlikely failure — accepted residue.
- **frontend 5 Lows** (in-flight response races, `updatePresetSuffix` blur-retry
  asymmetry, DropZone stale-snapshot filter): ride along with the next frontend
  batch, per round-2 README.
- **rust-queue-watch 4 Lows** (no canonicalization backfill for pre-fix watch
  rows, nested-watch purge overreach, uncancellable in-flight scans, preset
  repair matching user presets by name): each needs a design decision, none is
  user-visible damage today.
- **rust-app-shell 2 Lows** (quit-vs-queue race can mark a mid-encode job
  'error' instead of leaving it to auto-resume; silent updater install
  failure) and the rust-core carried-over round-1 Lows: backlog.

## Sequence / PR order

R1 → R2 are stacked (R2 builds on R1's converter changes): merge R1 first,
then rebase R2 onto main. R3 is independent. All three branches exist locally;
pushing is manual (git push is deny-listed for Claude).
