# Fable Review: tests-quality

## db.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/db.rs (done)

Overall strong: assertions carry intent messages (e.g. why `skip_by_source_media` defaults OFF), idempotency test simulates a real restart-after-user-change scenario, and the error-row backfill test covers all three cases (backfilled, already-set, non-error untouched). Path strings are opaque DB values, never compared to `Path::join` output — Windows-safe.

- **[Medium]** db.rs:81-87 — The `ALTER TABLE jobs ADD COLUMN source_size/source_mtime` migration path is only exercised trivially (fresh DB already has the columns, so every test run just hits the "duplicate column" ignore branch). No test creates an *old-schema* jobs table (without those columns) and verifies init_db adds them and that inserts/reads work afterward. This is exactly the auto-update-from-old-version compat risk the project cares about. Fix: add a test that hand-creates the pre-0.x jobs schema, runs `init_db`, and asserts the new columns exist and are writable.
- **[Low]** db.rs:167 — `assert_eq!(count, 15)` hardcodes the settings count in two tests; the comment justifies it as drift protection, but it forces a two-place edit on every new setting and fails without naming the missing/extra key. Fix: compare the actual key set against the `defaults` slice (or at least emit the diff in the failure message).
- **[Nit]** db.rs:203-223 — Idempotency test only re-checks `cleanup_mode`; the preset_suffix INSERT OR IGNORE is not covered for user-modified suffix preservation. Minor, same mechanism.

## media_skip.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/media_skip.rs (done)

Clean — this is the model test file in the repo. Table-driven cases each carry a "why" string that encodes business intent ("av1 1080p -> h265 1080p is pure waste", "never upscale"), the 5% resolution margin has explicit boundary tests at 1134/1135, and the uncertainty rules (unknown codec on either side, `None` media) are asserted with rationale. Paths are opaque strings, never joined — Windows-safe. No findings.

## probe_cache.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/probe_cache.rs (done)

Clean. `resolve_media` is generic over its three side effects (lookup/probe/store), so the memoization contract is tested directly: cache hits cost zero probes (asserted via a call log, plus a `panic!("a pure cache hit must never probe")` probe stub in the steady-state test), failed probes and identity-less files are never cached, store is skipped entirely when nothing needs caching (encoding the "don't take the DB write lock" intent), and both size- and mtime-change invalidation are covered with comments naming the real scenarios (file replaced at path / our own in-place re-encode). Upsert keeps one row per path. Paths are opaque strings — Windows-safe. No findings.

## probe.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/probe.rs (done)

Good: codec normalization is table-tested against real observed HandBrake decoder strings (with comments like "AV1 reports the dav1d DECODER name"), the parse fixture reproduces actual `--scan --json` output including the `Version:` preamble, and there is an `#[ignore]`d real-HandBrake integration test with run instructions. The timeout tests assert intent explicitly ("must return shortly after the deadline, not wait out the child"; "killed, not leaked").

- **[Low]** probe.rs:166-198 — Both `wait_with_timeout` tests are `#[cfg(unix)]` (they spawn `sleep`/`true`), so the poll-kill-on-timeout path has zero test coverage on Windows — and Windows CI only runs on pushes to main anyway. Fix: use platform-neutral helpers, e.g. spawn `cmd /C timeout` on Windows or, simpler, a `std::process::Command::new(env!("CARGO_BIN_EXE_..."))`-free approach like spawning the current test binary; at minimum keep it in mind as a Windows-sensitive blind spot.
- **[Low]** probe.rs:135-155 — `parse_scan_media` fixtures always end at the JSON blob. `serde_json::from_str` rejects trailing characters, so if HandBrake ever prints anything after `JSON Title Set: {...}` the parse silently returns `None` (file re-queued forever). No test pins the trailing-output behavior either way. Fix: add a fixture with trailing log lines and decide/assert the intended behavior (likely: should still parse — would require a streaming/`Deserializer::from_str` parse).
- **[Nit]** probe.rs:168-187 — Timing-based test (200ms deadline, <5s bound) is generously margined; flake risk is low but nonzero on a heavily loaded CI runner. Acceptable as-is.

## handbrake.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/handbrake.rs (done)

`classify_preset` and `resolve_suffix_template` are well table-tested with realistic HandBrake encoder strings and preset names; the "Super HQ must win over HQ" ordering intent is stated in a comment.

- **[Medium]** handbrake.rs:39-59 — `list_presets` parses HandBrake's `--preset-list` stderr by indentation depth (4 spaces = preset, 8 = description) and has zero tests. This is exactly the kind of brittle text-format parsing that breaks silently on a HandBrake version bump, and it's trivially extractable (a `parse_preset_list(stderr: &str)` fn) for a fixture test. Fix: extract and table-test against captured `--preset-list` output.
- **[Low]** handbrake.rs:212-264 — `resolve_suffix_template` tests only cover one-empty-var cases on the default template. The gnarly separator-removal logic (three separators, leading-dot special case, var-then-sep vs sep-then-var branches) has untested paths: multiple empty vars in one template, `_` separator, non-dot-leading templates, and an unknown/unsupported placeholder left in the template. Fix: convert to a table test covering those.
- **[Nit]** handbrake.rs:328 — `("mpeg2", "unknown")` pins that a *target* mpeg2 encoder classifies as unknown (so media-skip never skips), while `probe.rs::normalize_source_codec` maps mpeg2 sources to `"mpeg2"`. The asymmetry is presumably intentional (no HandBrake preset targets mpeg2) but the test states no "why". Fix: add a one-line comment.

## converter.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/converter.rs (done)

The extracted pure helpers are tested with strong intent: `decide_cleanup_matrix` pins every cell of an irreversible-delete decision ("this decision drives an irreversible delete, so every cell is pinned"), the `//`/`/.` normalization regression test explains the source-destroying failure mode it guards, `apply_rename_surfaces_failure_when_temp_missing` encodes "error, not false success", and the cancel-freeze deadlock has a real two-thread regression test with mpsc timeouts. Path comparisons use `Path` equality (component-wise), not string compare — safe on Windows despite forward-slash literals.

- **[High]** converter.rs:282-794 — `process_queue` (the ~500-line core loop: spawn HandBrake, stream progress, pause gate at loop top, `pause_after_current` handoff, notification decisions, DB status transitions, error paths) has no test coverage at all. Everything tested is a leaf helper. A logic change inside the loop (e.g. wrong status written on kill, pause flag not reset) would pass the suite. Fix: extract the per-job state machine (status transitions + pause/cancel decision points) into a testable function, or add an `#[ignore]`d integration test driving a fake `HandBrakeCLI` shell script.
- **[Medium]** converter.rs:101-140 — Pause/resume has zero tests: `can_pause_process()` platform gating, SIGSTOP/SIGCONT issuance on macOS, queue-level pause fallback, and the `pause_after_current` reset at line 681-682 are all untested. Given pause/resume is a headline behavior with per-platform semantics (per CLAUDE.md), at least the flag mechanics deserve unit tests. Fix: test the pause flag state machine with a stub child.
- **[Medium]** converter.rs:922 — The cancel-deadlock regression test is `#[cfg(unix)]`, but cancel via `Child::kill()` is the one process-control path that runs on ALL platforms. Windows gets no coverage, and Windows CI only runs on main pushes. Fix: replace `sleep 30` with a platform-neutral long-running child (`cmd /C ping -n 60 127.0.0.1` on Windows, or spawn a `powershell -c Start-Sleep`) and drop the cfg gate.
- **[Low]** converter.rs:857-879 — `parse_progress` fixtures cover full line, percent-only, and non-encoding lines, but not malformed near-matches (e.g. `ETA 00h00m00s`, >100%, comma decimal locales). HandBrake locale output is a known real-world parse hazard. Fix: add an ETA-zero and a garbled-numbers case.
- **[Low]** converter.rs:947 — 200ms sleep to "let Thread A enter the wait" is a race window on a loaded runner; the 5s recv_timeouts make false failures unlikely but a false PASS is possible if Thread A hasn't entered the wait yet (the test then never exercises the contended path). Fix: have the waiter signal readiness via a channel before the cancel thread starts.

## watcher.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/watcher.rs (done)

Very good. The stability debounce (`PendingEntry::observe`) is tested with an injected clock — no real sleeps, zero flake risk — and covers timer resets on both size and mtime change (rewrite-with-same-size). Download-marker logic (`has_active_marker` / `marker_removed_dir`) is tested through injected `exists` closures with a `present()` helper that explicitly normalizes `\` to `/` and documents why (the past Windows CI break) — the separator lesson was learned. Boundary intent is encoded: marker search "stops at watched root", non-recursive watches never own subfolder markers, `valid_marker` rejects separator values because `dir.join(marker)` "would silently pass every file".

- **[Medium]** watcher.rs:209-330 — The event pipeline glue is untested: `build_watcher`'s notify-event filtering, `spawn_reaper`'s poll loop (stat → observe → enqueue on stable), and `enqueue_and_start`. The pure pieces are covered, but nothing verifies they are wired together correctly (e.g. that a stable file is actually removed from `pending` and enqueued exactly once — the file-recycling race lives at this seam). Fix: extract the reaper's single-iteration body into a function taking stat/enqueue closures and test one tick: growing file stays pending, stable file enqueues once and is removed.
- **[Low]** watcher.rs:518-531 — `is_temp_file` covers the common download extensions; missing: uppercase variants of extensions other than `.CRDOWNLOAD` (one case-insensitive sample is probably fine) and no test that a *directory* named `movie.mp4.part` is irrelevant (delay_for_path presumably filters dirs upstream, untested).
- **[Nit]** watcher.rs:842-850 — `valid_marker` rejection tests cover `/` but not `\`; on Windows `Path::file_name` treats `\` as a separator too, so a `"sub\\.downloading"` case (cfg(windows)) would pin that. Very minor.

## commands/queue.rs (Rust)
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/queue.rs (done)

The strongest module in the suite, and it targets exactly the app's riskiest behaviors. The recycled-filename bug has layered regression coverage: unit (`cheap_skip_reason` identity-vs-name matrix incl. stat-failure uncertainty), integration (`fetch_skip_sets` flag/fingerprint semantics incl. the legacy in-place `//` path-equality row), and end-to-end against real tempdir files ("the earlier conversion's output must never be clobbered" is asserted on actual bytes). Output renumbering covers taken-name, batch-dedupe, and the never-renumber-in-place invariant with its rationale. Separator handling uses a documented `norm()` helper and filename-only `ends_with` — the Windows lesson is applied. `probe_candidates` pins the "don't pay for probes on cheap-skipped files" intent.

- **[Medium]** queue.rs:1660-1748 — Both end-to-end tests here and in probe.rs are `#[ignore]`d (need ffmpeg + HandBrakeCLI), so CI never runs them and nothing enforces they still compile against reality. Fix: consider a scheduled/main-only CI job that installs HandBrakeCLI+ffmpeg (both are in homebrew/apt/choco) and runs `cargo test -- --ignored`.
- **[Low]** queue.rs:1443-1462 — `reorder_queue_inner` is only tested for the happy path (all queued IDs listed). Untested: IDs missing from the list, unknown IDs, and jobs in `converting` status — the behaviors a drag-and-drop bug would hit. Fix: add a partial-list case pinning intended semantics.
- **[Low]** queue.rs:1494-1539 — History sort tests cover `space_saved` and `source_path` but not an unrecognized/malicious sort key. If `get_history_inner` interpolates the sort column into SQL, the whitelist fallback deserves a pinned test (`Some("evil; DROP")` -> default order, no error). Fix: add one rejection case.
- **[Nit]** queue.rs:949-971 — Comment says renumbering happens when there is "no completed-job fingerprint to prove it belongs to this source"; there is no companion test where the existing output DOES belong to the source *and* the source is re-added with flag off (should it reuse the same output path or renumber?). Behavior may be intentional either way; a test would document it.

## src/test/ (setup.ts, smoke.test.ts, release-script.test.ts)
Reviewed: /Users/rhurling/Sites/convertbar/src/test/setup.ts, smoke.test.ts, release-script.test.ts (done)

setup.ts correctly registers Testing Library cleanup (with a why-comment about Vitest globals). smoke.test.ts is a trivial harness canary — harmless. release-script.test.ts is a genuinely good idea: it executes the real `scripts/release.sh` against the real repo and pins the three *deterministic, offline* exits (bad `--notes`, not-newer version, `--dry-run`), asserting `git status --porcelain` is unchanged and no branch/tag was created. I verified the script's control flow: `--dry-run` exits before preflight and the version check is preflight step 1 (before `gh auth`/`git fetch`), so these tests need no network or clean tree.

- **[Low]** src/test/release-script.test.ts:41-51 — The "not newer" test implicitly depends on preflight ordering (version check before `gh auth status` / `git fetch`). If someone reorders preflight, the test starts requiring network/gh auth and fails confusingly in sandboxed CI. The script comments "checked first — deterministic", but the test file doesn't state this dependency. Fix: add a comment in the test naming the ordering contract.
- **[Nit]** src/test/release-script.test.ts:10 — `execFileSync("bash", ...)` and relative path `scripts/release.sh` assume a POSIX shell and repo-root cwd; fine on ubuntu CI (`npm test` runs on ubuntu-latest only) but the suite won't run on a Windows dev box. Acceptable; note only.

## src/lib/format.test.ts + addSummary.test.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/lib/format.test.ts, /Users/rhurling/Sites/convertbar/src/lib/addSummary.test.ts (done)

addSummary tests are excellent — they pin the stable REASON_ORDER merge semantics with a why-comment and cover empty/added-only/skips-only/merged cases. formatBytes/formatEta/formatPercent tests name the guarded failure modes (Math.log(0), division by zero, zero-padding).

- **[High]** src/lib/format.test.ts:36-43 + src/lib/format.ts:22-24 — `fileName` splits ONLY on `/`, and the test enshrines that: on Windows, Tauri delivers `C:\Users\...\clip.mp4`, so every queue item, notification, and history row will display the full path instead of the file name. This is the third incarnation of the hardcoded-separator class that already broke Windows CI twice — but this one lives in shipped frontend code and no CI leg can catch it (vitest runs on ubuntu only). Fix: make `fileName` split on `/[\\/]/` and add a `fileName("C:\\Movies\\clip.mp4") === "clip.mp4"` test.

## Frontend hook tests (useQueue, useSettings, useWatchedDirectories, useHistory)
Reviewed: /Users/rhurling/Sites/convertbar/src/hooks/*.test.ts (done)

Not over-mocked, despite mocking the whole Tauri IPC layer: each file builds a small *stateful fake backend* (mutable `queueData`/`pages`/`suffixes` the dispatcher reads) plus a recorded event bus, so what's under test is the hooks' real responsibilities — derived state (activeJob/pendingJobs incl. paused-counts-as-active), event-driven refetch (with a call-count proving exactly one refetch), default-suffix write-back, "don't reload preset metadata for non-preset changes" (a real perf intent, asserted via call counting), pagination offset/hasMore math, and error surfacing. Crucially, every dispatcher rejects unexpected commands (`unexpected invoke: ...`) — fail-loud. The heavy logic these mocks hide is tested on the Rust side, so the split is sound.

- **[Medium]** all four hook test files — The frontend/backend *contract* is the untested seam: command names (`"get_queue"`, `"set_preset_suffix"`), camelCase arg keys (`stabilityDelaySecs`), and event names (`"job-status-changed"`, `"conversion-progress"`) are asserted only against strings the tests themselves define. Renaming a Rust `#[tauri::command]` or an emitted event breaks the app while both test suites stay green. Fix: a small contract test (or CI grep) that extracts every `invoke("...")`/`listen("...")` literal from src/ and checks it against `#[tauri::command] fn` names and `emit("...")` literals in src-tauri/.
- **[Low]** useQueue.test.ts — No test for an empty-queue/idle state or for the unlisten cleanup on unmount (the listener Map supports it — `renderHook` unmount + emit would prove no setState-after-unmount). Minor.

## Frontend component/page tests (DropZone, QueueItem, HistoryPage, WatchedFoldersPage)
Reviewed: /Users/rhurling/Sites/convertbar/src/components/*.test.tsx, /Users/rhurling/Sites/convertbar/src/pages/*.test.tsx (done)

DropZone is the standout: it captures the real `onDragDropEvent` registration, then drives drop → classify → auto-add vs confirm-prompt threshold → confirm/skip → start_queue → per-reason summary as user-visible flows with proper negative assertions ("does not start the queue", "no confirmation prompt"). HistoryPage tests encode UX intent in comments (errors-only history still gets Clear; savings hidden when meaningless). QueueItem pins the In-place badge both ways.

- **[Low]** DropZone.test.tsx:60-99 — The folder auto-add threshold is tested at 3 and 12 but never at the boundary (5 vs 6). The prompt text says "5 or fewer"; an off-by-one (`>` vs `>=`) would pass. Fix: add file_count 5 (auto) and 6 (prompt) cases.
- **[Low]** WatchedFoldersPage.test.tsx:31-39 — Unlike every other test file, the dispatcher's default case is `Promise.resolve(undefined)`, so an unexpected/renamed command silently succeeds instead of failing loud. Fix: reject in the default arm like the sibling files.
- **[Low]** Coverage map gaps: ActiveJob.tsx (progress display, pause/resume/cancel buttons — the UI face of the riskiest backend feature), HistoryItem.tsx, QueuePage.tsx, SettingsPage.tsx, TabBar.tsx have no tests. ActiveJob is the most valuable of these (asserting cancel/pause invoke the right commands in the right states).
- **[Nit]** WatchedFoldersPage.tsx:6 correctly splits on `/[/\\]/` for basenames while format.ts `fileName` splits on `/` only — two conflicting conventions in the same codebase; the WatchedFoldersPage one is correct (see High finding above).

## Summary

Overall test health: strong — well above typical for a project this size. The suite's defining quality is that tests encode intent: nearly every non-trivial assertion carries a "why" message or comment naming the business consequence ("this decision drives an irreversible delete", "the earlier conversion's output must never be clobbered", "a pure cache hit must never probe"). The known-risky behaviors have real regression coverage: the recycled-filename/fingerprint race is tested at three layers in commands/queue.rs; download-marker gating and the stability debounce are tested purely with injected clocks/closures (zero sleeps, zero flake); probe caching pins its zero-reprobe and no-write-lock-on-steady-state contracts; output renumbering covers dedup-within-batch and the never-renumber-in-place invariant; the cancel-freeze deadlock has a genuine two-thread regression test.

Separator hygiene: the Windows lessons were absorbed on the Rust side (documented `norm()` helpers, Path-equality comparisons, filename-only `ends_with`) — no regressions found. The one live separator bug is in shipped frontend code: `fileName` in src/lib/format.ts splits on `/` only, and its test enshrines it (High).

Windows-sensitive blind spots (Windows CI runs only on pushes to main): the `#[cfg(unix)]` cancel-deadlock test (converter.rs) and both `wait_with_timeout` tests (probe.rs) — meaning the cross-platform `Child::kill()` cancel path and the probe timeout have no Windows coverage anywhere.

Frontend mocking is done right: stateful fake backends + fail-loud dispatchers keep the hooks' orchestration genuinely under test; the untested seam is the string-typed IPC contract (command/event names, arg key casing).

Coverage map:
- Rust, well covered: db.rs (seed/idempotency/backfill), media_skip.rs (exemplary), probe_cache.rs, probe.rs (parsing/timeout), watcher.rs (pure logic), commands/queue.rs (skip/renumber/history), handbrake.rs (classify/suffix).
- Rust, uncovered: converter.rs `process_queue` core loop + pause/resume mechanics (biggest gap); watcher event pipeline glue (build_watcher/reaper/enqueue); handbrake `list_presets` stderr parsing; thin command wrappers (commands/converter.rs, settings.rs, watch.rs — acceptable); SQLite old-schema ALTER migration path (only trivially exercised).
- Frontend, well covered: all four hooks, DropZone, HistoryPage, WatchedFoldersPage, QueueItem, addSummary, format, release.sh preflight.
- Frontend, uncovered: ActiveJob (pause/resume/cancel UI), QueuePage, SettingsPage, HistoryItem, TabBar.
- E2E: two valuable `#[ignore]`d ffmpeg+HandBrake tests exist but never run in CI.

## Recommendations

Prioritized, most valuable first:

1. Fix `fileName` to split on `/[\\/]/` and add a backslash test (src/lib/format.ts) — a shipped Windows UI bug the current test actively protects. Two-line change.
2. De-`cfg(unix)` the cancel-deadlock test (converter.rs:922) with a platform-neutral long-running child — cancel is the one process-control path shared by all platforms, and Windows only reddens main post-merge today.
3. Add a pause/resume state-machine test for converter.rs (`pause_after_current` set/reset, `can_pause_process` gating) and extract the per-job status-transition logic out of `process_queue` for unit testing — the core loop is the largest untested surface in the app.
4. Add an old-schema migration test in db.rs: create a pre-fingerprint jobs table by hand, run `init_db`, assert `source_size`/`source_mtime` exist and are writable — this is the exact auto-update path the sqlite-migration-reviewer agent exists to protect, and it currently has no executable guard.
5. Add an IPC contract check (test or CI grep): every `invoke("x")`/`listen("y")` literal in src/ must match a `#[tauri::command] fn x` / `emit("y")` in src-tauri/. Closes the only meaningful hole the frontend mocking leaves.
6. Run the two `#[ignore]`d end-to-end tests (queue.rs, probe.rs) in a scheduled or main-only CI job with HandBrakeCLI+ffmpeg installed — they are the only tests that exercise real HandBrake output, and right now nothing prevents them from silently rotting.
7. Extract and fixture-test `list_presets`' indentation-based stderr parsing (handbrake.rs:39) — most fragile untested parser in the codebase.
8. Test the watcher reaper's single tick via extracted closure-taking body (stable file enqueues exactly once and leaves `pending`) — the seam where the file-recycling race actually lives.
9. Add DropZone threshold boundary cases (5 auto-adds, 6 prompts) and an ActiveJob test asserting pause/resume/cancel invoke the right commands per state.


## Verification pass (2026-07-07)

- **Confirmed** — [Medium] db.rs:81-87 old-schema ALTER migration only trivially exercised: all five db.rs tests open a fresh in-memory DB and call `init_db` first (db.rs:157-334); no test in the repo hand-creates a jobs table lacking `source_size`/`source_mtime` (grep for `CREATE TABLE jobs` outside init_db: no hits), so the ALTER branch only ever hits the duplicate-column ignore path.
- **Confirmed** — [Medium] handbrake.rs:39-59 `list_presets` indentation parsing untested: the only references to `list_presets` are its definition (handbrake.rs:39) and the command wrapper (commands/handbrake.rs:36); no `#[cfg(test)]` module anywhere mentions it or `--preset-list`, and the 4-vs-8-space stderr logic (handbrake.rs:50) is inline, not extracted.
- **Confirmed** — [High] converter.rs:282-794 `process_queue` has zero coverage: the only non-doc references are the definition (converter.rs:282) and the spawn call site (converter.rs:806); the test module (converter.rs:852+) tests only leaf helpers (parse_progress, decide_cleanup, in-place apply, record_source_identity, cancel deadlock) — nothing drives the loop or its status transitions.
- **Confirmed** — [Medium] converter.rs:101-140 pause/resume zero tests: repo-wide grep for `can_pause_process`/`pause_after_current`/`SIGSTOP`/`SIGCONT` finds no test references anywhere; `can_pause_process` is converter.rs:119 and the flag reset is converter.rs:681-682 as stated. One attribution note: the SIGSTOP issuance itself lives in commands/converter.rs (pause_conversion, ~line 63), which has no `#[cfg(test)]` module either — the gap holds.
- **Confirmed** — [Medium] converter.rs:922 cancel-deadlock test is unix-only: `#[cfg(unix)]` sits directly on `cancel_can_kill_child_while_wait_in_progress` (converter.rs:922-923), which spawns `sleep 30`; and test.yml:34 confirms the rust matrix is ubuntu-only on PRs and only includes windows-latest on pushes to main, so the cross-platform `Child::kill()` path has no Windows coverage anywhere.
- **Confirmed** — [Medium] watcher.rs:209-330 pipeline glue untested: `build_watcher` (watcher.rs:209), `spawn_reaper` (watcher.rs:257), and `enqueue_and_start` (watcher.rs:308) appear nowhere in the test module (watcher.rs:510+) — grep of the module for those names returns zero hits; only `PendingEntry::observe` and the marker/temp-file pure helpers are covered.
- **Confirmed** — [Medium] queue.rs:1660-1748 ignored e2e tests never run in CI: exactly two `#[ignore]` tests exist (commands/queue.rs:1661, probe.rs:211), and the only CI test invocation is a plain `cargo test --manifest-path src-tauri/Cargo.toml` (test.yml:47) with no `--ignored` / `--include-ignored` anywhere in .github/workflows.
- **Partial** — [High] format.ts fileName splits only on `/`: implementation confirmed (format.ts:22-24 `path.split("/")`), test enshrines forward-slash-only inputs (format.test.ts:36-43), and on Windows the backend does emit backslash paths (`source_path` originates from native drag-drop/watcher `PathBuf`s and `Path::join`, which serializes with `\`), so QueueItem.tsx:47, HistoryItem.tsx:29, and ActiveJob.tsx:42 would all render the full path. Correction: notifications are NOT affected — they are built backend-side from `Path::new(&job.source_path).file_name()` (converter.rs:330-333, used at converter.rs:587), which is platform-correct; no frontend code sends notifications.
- **Confirmed** — [Medium] hook tests leave the IPC string contract untested: all four hook test files mock `@tauri-apps/api/core`/`event` outright (e.g. useQueue.test.ts:4-5) and assert against command/event strings defined inside the tests (useQueue.test.ts:54, :86); no contract test, script, or CI step compares `invoke(...)`/`listen(...)` literals in src/ against `#[tauri::command]`/`emit` names in src-tauri/ (scripts/ contains only release.sh; test.yml has no such check).

**Tally:** 8 confirmed, 1 partial, 0 refuted (of 9).
