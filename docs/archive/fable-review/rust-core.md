# Fable Review: rust-core

## types.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/types.rs (done)

File is clean. Plain serde DTOs, well-documented skip-reason enum with snake_case wire format.

- **[Nit]** types.rs:9 — `JobInfo.status` (and `Settings.cleanup_mode`) are stringly-typed while `SkipReason` is a proper enum. Consistent with SQLite storage and existing convention, so acceptable; only worth changing if status values ever grow.

## probe.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/probe.rs (done)

- **[Medium]** probe.rs:73-84 — stdout is only read *after* the child exits, but the child holds a piped stdout while `wait_with_timeout` polls. If the `JSON Title Set` (plus scan `Progress:` JSON blocks, which also go to stdout with `--json`) exceeds the OS pipe buffer (~64KB on macOS/Linux), HandBrake blocks on write, never exits, and gets killed at the 30s deadline. Multi-title sources or files with many audio/subtitle tracks can plausibly hit this. Failure mode is safe (None => queue, don't skip) but costs a silent 30s stall per such file during folder scans. Fix: drain stdout on a spawned thread while polling (or take the stdout handle, read_to_string in a thread, then join after wait).
- **[Low]** probe.rs:103 — `wait_with_timeout` returns `None` on `try_wait()` `Err(_)` without killing/reaping, so a child could be leaked on that (rare) path. Fix: mirror the timeout branch — `kill()` + `wait()` before returning.
- **[Nit]** probe.rs:57 — `serde_json::from_str` on everything after the marker requires the Title Set JSON to be the final stdout content; any trailing log line makes the whole probe return None. `serde_json::Deserializer::from_str(...).into_iter()` (or `StreamDeserializer`) would parse the leading JSON value and ignore trailers. Current behavior degrades safely, so low priority.

Positives: timeout semantics are well-reasoned and tested (kill-on-overrun test asserts the child is reaped); codec normalization table is documented with real observed decoder names; None-means-queue policy is stated at every layer.
## media_skip.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/media_skip.rs (done)

Essentially clean — pure, no I/O, exhaustively table-tested with "why" strings on every case (exactly the intent-encoding tests the project asks for). Margin constant is documented with real-world justification (1082/1088 encoder drift).

- **[Low]** media_skip.rs:44-56 (interacts with probe.rs:59) — unknown *codec* is treated as uncertainty (never skip), but unknown *height* is not: `parse_scan_media` maps a missing `Geometry.Height` to `0`, and `should_skip_by_media` reads `source_height=0` as "below target, no downscale benefit" -> skip when codecs match. A 4K file whose height failed to parse would be wrongly skipped. Unlikely in practice (Height is always present in a valid title), but inconsistent with the stated uncertainty policy. Fix: have `parse_scan_media` return `None` when Height is missing/non-positive, or treat `source_height <= 0` as "resolution could help" in the policy.
- **[Nit]** media_skip.rs:2-3 — module doc says the HandBrake shell-out producing `SourceMedia` "lives in `handbrake.rs`", but it lives in `probe.rs` (probe.rs's own header says so). Stale pointer; update the comment.
## probe_cache.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/probe_cache.rs (done)

Cache correctness is solid: identity is (size AND mtime), stale rows are upserted, failed probes are never cached (so uncertainty is re-evaluated), identity-less files are probed but never stored, and the pure-hit path provably never takes the DB write lock. `resolve_media`'s dependency-injected design keeps the memoization logic unit-testable and keeps probes outside any DB lock — good, not overengineered.

- **[Low]** probe_cache.rs:58-67 — no eviction: rows for deleted/moved files accumulate forever (one row per path ever probed). Rows are tiny so this is slow-burn, but a periodic prune (e.g. delete rows older than N months on startup) would bound it.
- **[Nit]** probe_cache.rs:58-67 — `store_batch` issues one autocommit INSERT per file; a large folder scan pays a per-row fsync. Wrap the loop in a single transaction (`conn.unchecked_transaction()` or cached statement) if scans of hundreds of files ever feel slow.
- **[Nit]** probe_cache.rs:20-33 — `lookup_batch` runs one SELECT per candidate. Fine at current scale; a single `WHERE path IN (...)` query is the fix if it ever shows up in profiles.
## handbrake.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/handbrake.rs (done)

- **[Medium]** handbrake.rs:21,40,65 (callers: src/commands/handbrake.rs:29,36,76,103-106) — every function here blocks on `Command::output()`, and they are invoked from *sync* `#[tauri::command]` fns (`detect_handbrake`, `list_handbrake_presets`, `generate_preset_suffix`, `validate_handbrake`), which run on the main thread. This is the same bug class as the fixed main-thread probe hazard, just with shorter shell-outs: `HandBrakeCLI --preset-list` / `--preset-export` / `--version` each cost a full CLI startup (hundreds of ms, worse on cold cache), stuttering the UI. The queue add path was correctly fixed with `spawn_blocking` (commands/queue.rs:551,588); these four were not. Fix: make those commands `async` and wrap the shell-outs in `tauri::async_runtime::spawn_blocking`, mirroring `add_files`.
- **[Low]** handbrake.rs:65-83 — `get_preset_metadata` never checks `output.status`. For an unknown/misspelled preset HandBrake exits non-zero with empty stdout, and the user sees "Failed to parse preset JSON: EOF while parsing... Output: " instead of the real cause. Fix: on `!output.status.success()`, return an error carrying stderr.
- **[Low]** handbrake.rs:79 — `&stdout[..stdout.len().min(200)]` slices by byte offset and panics if byte 200 lands inside a multi-byte UTF-8 char (possible with localized HandBrake output or non-ASCII preset names). Fix: truncate on a char boundary (`stdout.char_indices().nth(...)` or `floor_char_boundary` pattern).
- **[Low]** handbrake.rs:39-59 — `list_presets` screen-scrapes `--preset-list` stderr by indentation depth (4-vs-8 spaces), doesn't check exit status, and has no unit test on a captured fixture. If a HandBrake release reformats the listing, this silently returns an empty preset list with `Ok`. Fix: check status, treat an empty parse as an error, and add a fixture test like the ones probe.rs has.
- **[Nit]** handbrake.rs:100-121 vs probe.rs:19-49 — two hand-maintained codec-normalization tables ("same vocabulary" by convention only): `classify_preset` lacks mpeg2/mpeg4/vc1 (maps them to "unknown", confirmed by its own test). Safe today because unknown never skips, but the tables can drift. Fix: share one normalizer or add a test asserting both emit the same slug set.
- **[Nit]** handbrake.rs:212-263 — `resolve_suffix_template` removes an empty variable with `replacen(..., 1)`, so a placeholder repeated in the template leaves a literal `{codec}` in the output filename. Edge-casey; document or use a loop.

Positives: `classify_preset` split out pure for table tests (and well covered); `slugify` is simple and tested; Windows `where` multi-line output handled.
## converter.rs
Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/converter.rs (done)

Lifecycle design is sound: the queue runs on a dedicated thread (`run_queue` guards `is_running` under one lock before spawning); `wait_for_active_child` polls `try_wait` without holding `current_child` across a blocking wait (pinned by a dedicated deadlock regression test); cancel (commands/converter.rs) writes status *before* killing so the queue's error branch can't race a spurious "failed" notification; stderr is drained on a thread so HandBrake can't deadlock on a full pipe; the in-place pipeline is decomposed into pure, table-tested functions with an explicit fatal-vs-benign cleanup-failure rule. cfg-gating verified correct: `libc` is only under `[target.'cfg(target_os = "macos")'.dependencies]` in Cargo.toml, every `libc::kill` sits inside a `#[cfg(target_os = "macos")]` block, non-macOS falls back to queue-level pause via `pause_after_current`, and cancel uses handle-based `Child::kill()` on all platforms.

- **[Medium]** converter.rs:412-423, 714 — the stderr drain thread throws HandBrake's output away, so every real failure is recorded as the string literal `'Conversion failed'` with no diagnostic. Users (and bug reports) get nothing actionable. Fix: keep a bounded tail in the drain thread (e.g. a ring of the last ~20 lines / 4KB), hand it back via a channel or shared buffer, and put it in `error_message`.
- **[Low]** converter.rs:142-162 + 659-664 — `decide_cleanup(orig, 0)` (successful exit, zero/missing output) returns status `"done"`, so the job is recorded as done with `kept_file = "original"` and the notification reads "converted — saved 0B" even though nothing usable was produced. The matrix test pins this as intentional behavior-preservation, but it misreports a failure as success. Fix: treat `converted_size == 0` after a successful exit as an error (or at least `skipped`) and say so in the notification.
- **[Low]** converter.rs:791 (whole `process_queue`) — every `.lock().unwrap()` means a poisoned mutex (panic in any other holder) panics the queue thread mid-job; `is_running` then stays `true` forever and `run_queue` silently no-ops until app restart, with a live HandBrake child left in `current_child`. Fix: a drop guard that resets `is_running` (and reaps `current_child`) even on unwind.
- **[Low]** converter.rs:446-470 — the progress thread emits two Tauri events per parsed line with no throttling; HandBrake refreshes the `Encoding:` line many times per second, so the webview gets a steady event flood for hours-long encodes. Fix: rate-limit (e.g. emit on >=0.1% change or at most every 250ms).
- **[Low]** (adjacent scope) commands/converter.rs:63,139,250 — pause/resume/cancel signal by *raw pid* (`libc::kill(pid, SIGSTOP/SIGCONT)`) read from `current_pid`; if the encode exits between the read and the kill, the signal can hit a recycled pid (SIGSTOP freezing an unrelated process). Window is ~100ms (the wait poll) and pid reuse that fast is unlikely, so Low. Fix: send the signal while holding `current_child` and re-checking `try_wait` first.
- **[Nit]** converter.rs:572-581, 637-652, 733-742 — three near-identical inline "read a boolean setting from the settings table" blocks. Extract a `read_bool_setting(&db, key, default)` helper.
- **[Nit]** converter.rs:697-698 — a user-initiated cancel takes the `Ok(_) | Err(_)` arm and sets `had_errors = true`, so the tray ends in the "error" state after a deliberate cancel. Consider distinguishing status `'error'`-by-cancel when computing `final_status`.
- **[Nit]** converter.rs:183-221 — `parse_progress` is anchored on "Encoding:", has a percent-only fallback, and handles `\r`-delimited updates; solid. Only note: it depends on HandBrake's plain-text progress format — if it ever breaks, progress silently freezes at 0% while the encode still works. `--json` progress would be structurally parseable, but that's a bigger change; not worth it today.

## Summary

Overall health: good. This is a carefully engineered conversion core with unusually strong tests — pure decision logic (cleanup, in-place actions, skip policy, cache memoization) is consistently extracted from side effects and table-tested with intent-explaining assertions. The historic bug classes this codebase has memory of are genuinely fixed here: no blocking probe reaches the main thread via the queue add path (spawn_blocking at commands/queue.rs:551,588), the child-wait/cancel deadlock has a regression test, and libc/SIGSTOP is correctly confined to macOS with a queue-level fallback elsewhere.

Themes:
1. Error *surfacing* is the weakest area: HandBrake stderr is discarded ("Conversion failed" tells the user nothing), `get_preset_metadata` ignores exit status, and a zero-byte "successful" encode is reported as done.
2. The main-thread-blocking bug class still has one uncovered corner: the four sync commands in commands/handbrake.rs shell out to HandBrakeCLI on the main thread (short calls, so stutter rather than freeze — but the exact pattern that bit before).
3. Pipe handling is inconsistent: converter.rs drains stderr on a thread specifically to avoid the full-pipe deadlock, but probe.rs waits before reading stdout and relies on the output being "small enough".
4. Duplicated normalization/settings-reading code in a few places is drift risk, not a bug.

## Recommendations

1. (Medium) Capture a bounded tail of HandBrake stderr and store it in `error_message` instead of the literal "Conversion failed" — converter.rs:412-423,714. Highest user-visible payoff.
2. (Medium) Make `detect_handbrake`, `list_handbrake_presets`, `generate_preset_suffix`, `validate_handbrake` async + `spawn_blocking`, mirroring `add_files` — commands/handbrake.rs. Closes the last known instance of the main-thread shell-out class.
3. (Medium) Drain probe stdout concurrently in `probe_source` so an oversized title set can't stall a scan 30s per file — probe.rs:73-84.
4. (Low) Check `output.status` in `get_preset_metadata` / `list_presets` and fix the UTF-8-unsafe byte slice in the error message — handbrake.rs:65-83,39-59.
5. (Low) Treat zero-size output after a successful exit as an error, not "done" — converter.rs:142-162.
6. (Low) Treat missing probe height as uncertainty (never skip) to match the unknown-codec policy — probe.rs:59 / media_skip.rs:44-56.
7. (Low) Add an unwind-safe guard for `is_running` in `process_queue`, and consider throttling progress events — converter.rs.
8. (Nit backlog) Unify the two codec-normalization tables; extract a bool-setting reader; prune probe_cache periodically; fixture-test `list_presets`.

## Verification pass (2026-07-07)
- **Confirmed** — probe stdout only read after exit; oversized output stalls scan 30s (probe.rs:73-84): stdout is piped (probe.rs:75) but first read at probe.rs:82-83, strictly after `wait_with_timeout` returns; the poll loop (probe.rs:90-105) never drains the pipe, so a child blocked writing >~64KB can never exit and is killed at the 30s deadline (probe.rs:96-99), returning None. The in-code comment at probe.rs:71-72 assumes only the small title set reaches stdout, but `--json` also routes scan `Progress:` blocks and the full (audio/subtitle/chapter-laden) title set there; failure is edge-case but the mechanism is exactly as described.
- **Confirmed** — four sync commands shell out on the main thread (handbrake.rs:21,40,65 via commands/handbrake.rs): `detect_handbrake`, `list_handbrake_presets`, `generate_preset_suffix`, `validate_handbrake` are all non-async `#[tauri::command]` fns (commands/handbrake.rs:10,33,42,88) registered in lib.rs:53-55,75 and invoked from the frontend (src/lib/tauri.ts:137-148); each reaches a blocking `Command::output()` (`which`/`--preset-list`/`--preset-export`/`--version`). Tauri 2 runs sync commands on the main thread — the same class as the fixed probe hazard — and the contrast fix genuinely exists only on the add path (`async` + `spawn_blocking` at commands/queue.rs:547-551,581-588). Minor nuance: `detect_handbrake` only spawns `which` (ms-scale), so its stutter is far milder than the three HandBrakeCLI startups; `generate_preset_suffix` also hits an in-memory cache after first call.
- **Confirmed** — stderr drain discards HandBrake diagnostics; failures recorded as literal 'Conversion failed' (converter.rs:412-423,714): the drain thread reads into a 4KB buffer and drops every byte (converter.rs:413-423, no accumulation, nothing returned), and the error branch hardcodes `error_message = 'Conversion failed'` in the UPDATE (converter.rs:714) and the `job-error` emit (converter.rs:721); stdout is consumed solely for progress parsing (converter.rs:432-477), so no diagnostic text survives anywhere.

**Tally:** 3 confirmed, 0 partial, 0 refuted (of 3).
