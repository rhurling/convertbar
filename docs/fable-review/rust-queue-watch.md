# Fable Review: rust-queue-watch

## db.rs

Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/db.rs (done)

Overall clean. Migration discipline is good: all five tables are `CREATE TABLE IF NOT EXISTS`, the only column additions since 0.x (`source_size`, `source_mtime` in PR #56) go through idempotent `ALTER TABLE ADD COLUMN`, and the error-row `completed_at` backfill is idempotent and guarded by tests. Verified against git history that no table definition has changed shape without a corresponding ALTER — auto-updating users with an existing convertbar.db are safe.

- **[Low]** db.rs:5-7 — `get_db_path` uses `.expect()` twice; a missing/unwritable data dir panics the app at startup with no user-facing message. Fix: return `Result` and surface a dialog/log from the caller (low priority — practically rare on macOS).
- **[Low]** db.rs:83 — duplicate-column detection matches the error string `"duplicate column name"`. Works today but is coupled to SQLite's English message text. Fix: check `PRAGMA table_info(jobs)` for the column before altering, or match on `rusqlite::ErrorCode` — optional hardening.
- **[Nit]** db.rs:32-33 — `jobs` carries both `original_size` and `source_size`, which read as near-synonyms. If `original_size` is legacy, a one-line comment distinguishing them would prevent future confusion (columns must stay for back-compat regardless).

Tests in this file are meaningful (idempotency, user-value preservation, backfill scoping) and use paths only as opaque strings, so no Windows separator hazard here.

## commands/watch.rs

Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/watch.rs (done)

Well-structured. Every command scopes the `state.db` mutex guard in a block and drops it before calling `watcher::reconcile` / `scan_existing_background`, so there is no lock-held-across-reconcile hazard. The blocking-probe lesson (main-thread freeze) is applied at both entry points (`add_watched_directory`, `set_watched_directory_enabled`) with explanatory comments, and `pick_folder` correctly stays `async` to avoid the main-thread dialog deadlock.

- **[Medium]** watch.rs:45-60 — the watched path is stored verbatim, not canonicalized. `path` UNIQUE only catches byte-identical duplicates: `/Movies` vs `/Movies/` (trailing slash), a symlinked alias, or macOS case-variant (`/movies`) all insert as separate rows, producing two watchers over the same folder and double-enqueue attempts (dedup downstream masks most of it, but wastes probes). Fix: `dunce::canonicalize`/`fs::canonicalize` (or at minimum trim trailing separators) before insert and before the UNIQUE check.
- **[Low]** watch.rs:46 — `dir.is_dir()` in a sync command runs a blocking stat on the main thread. Instant on local disks, but on a dead network mount (SMB/NFS) it can hang the UI for the OS timeout. Fix: acceptable as-is; note it if watched network folders become a supported use case.
- **[Nit]** watch.rs:63 — UNIQUE-violation detection via `e.to_string().contains("UNIQUE")` — same string-matching fragility as db.rs:83; could match on `ErrorCode::ConstraintViolation`.

## watcher.rs

Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/watcher.rs (done)

Solid design: event-kind-agnostic handler + stat-verifying reaper is the right shape for cross-platform notify quirks; the stability timer, temp-extension guard, and skip-marker gating (with the marker-removal rescan) all have real unit tests, and the tests are already separator-normalized for Windows (the `present()` helper). Lock ordering is consistent (event handler: configs -> skip_marker -> pending; reconcile: configs (dropped) -> skip_marker -> watched -> watcher) — no deadlock cycles, and no lock is held across a blocking probe. All heavy work (scans, probes) is off the main thread at every entry point.

- **[Medium]** watcher.rs:440-483 — `reconcile` re-arms watches but never purges `pending` entries under removed/disabled directories. A file mid-stabilization when the user disables or removes its watch is still enqueued and converted by the reaper once it settles — the disable doesn't fully take effect. Fix: after computing `to_unwatch`, `pending.lock().retain(|p, _| !to_unwatch.iter().any(|root| p.starts_with(root)))` (careful with the mode-flip case where the path is re-added).
- **[Low]** watcher.rs:262-276 — the reaper stats every pending file while holding the `pending` mutex. On a slow/network volume one hung `fs::metadata` blocks the notify event-handler thread (which needs `pending` to record new events). Fix: snapshot keys, stat outside the lock, then apply results — only worth it if network watches become supported.
- **[Low]** watcher.rs:267, 407 — non-UTF-8 paths are silently dropped (`path.to_str()` filter) after the file has already settled; the file is removed from `pending` and never enqueued, with no log. Rare on macOS (UTF-8 enforced), possible on Linux. Fix: at minimum `eprintln!` when dropping.
- **[Low]** watcher.rs:96-109, 122-126, 332-353 — with nested/overlapping watched dirs (e.g. `/w` recursive and `/w/sub`), `delay_for_path` and the `has_active_marker` root pick the *first* matching config, and `read_enabled_configs` has no ORDER BY — so the applied delay and the marker-walk boundary are nondeterministic across restarts. Fix: pick the most specific (longest) matching root, or ORDER BY path length.
- **[Low]** watcher.rs:263-278 — settle-then-overwrite window: the reaper removes a path from `pending` and enqueues it; if a new download recycles that filename immediately after, conversion may read a half-written file. Mitigated by the source_size/source_mtime fingerprint (PR #56) and the skip-marker feature; noting as residual known limitation, not a regression.
- **[Nit]** watcher.rs:251, 498 — `expect("failed to create filesystem watcher")` / `.lock().unwrap()` panic at startup if the OS watcher can't be created (e.g. inotify limit on Linux). The app could run degraded (queue still works without watches) instead of dying. Fix: log and continue with watching disabled.
- **[Nit]** watcher.rs:258-259 — the reaper wakes every second forever, even with zero watched dirs and empty pending. Trivial CPU, but for a menu bar app a condvar/park-until-work would be cleaner for energy impact.

## commands/queue.rs

Reviewed: /Users/rhurling/Sites/convertbar/src-tauri/src/commands/queue.rs (done — reviewed in two chunks: core pipeline lines 1-545, commands/history/tests lines 546-1749)

Strengths worth noting: `add_files_to_db` re-derives the skip sets under the final DB lock, so the probe phase (which runs unlocked) can never race a concurrent add into duplicate rows — the TOCTOU between `probe_candidates` and insert is correctly closed. The recycled-filename bug class is well covered by identity-fingerprint tests, the `order_clause` is a whitelist (no SQL injection), all path-string tests go through the `norm()` separator helper (PR #58 lesson applied), and both probe-heavy commands (`add_files`, `confirm_folder_add`) are on `spawn_blocking`.

### Core pipeline (lines 1-545)

- **[High]** queue.rs:335-344, 363-372 — the `preset_cache` mutex is held **across the `handbrake::get_preset_metadata` shell-out** (a blocking HandBrakeCLI subprocess, seconds). `generate_preset_suffix` (commands/handbrake.rs:48) is a *sync* command that runs on the main thread and locks the same mutex — so if the frontend calls it (e.g. user opens preset settings) while a watcher/drag-drop add is resolving metadata in the background, the main thread blocks for the full shell-out and the UI freezes. This is the known main-thread-stall class (MEMORY: "fixed at 4 entry points") arriving via lock convoy instead of a direct probe. Fix: drop the guard before shelling out (check cache -> unlock -> fetch -> relock -> insert), accepting a rare duplicate metadata fetch; also extract the duplicated block at 335-344/363-372 into one helper.
- **[Medium]** queue.rs:163, 175 — `choose_output_path` hardcodes the `.mp4` extension. The Linux default preset is "H.265 MKV 1080p" (db.rs:17) and converter.rs passes no `--format`, so HandBrake uses the preset's MKV container — producing Matroska data in a file named `.mp4` on Linux out of the box. Everything downstream (is_video_file, in-place detection, players) then reasons about the wrong extension. Fix: derive the output extension from the preset metadata/container (mp4 vs mkv) instead of hardcoding.
- **[Medium]** queue.rs:560, 810 — `scan_folder` and `classify_paths` are *sync* commands that run `scan_video_files` (unbounded recursive walk with per-entry stats) on the main thread. A dropped folder with a deep tree or on a network volume freezes the UI — same hazard class as the probe stalls, just I/O-shaped. Fix: make them `async` + `spawn_blocking` like `add_files`/`confirm_folder_add`.
- **[Low]** queue.rs:32-45 — `scan_video_files` follows directory symlinks (`path.is_dir()` traverses), so a symlink cycle inside a scanned/watched folder causes pathological recursion (terminates only via ENAMETOOLONG after a huge wasted walk). Fix: skip entries where `entry.file_type()` is a symlink.
- **[Low]** queue.rs:443-529 — `add_files_to_db` inserts each job in its own implicit transaction; a 500-file folder is 500 fsyncs while the DB mutex is held (blocking every other command, including main-thread ones). Fix: wrap the batch in a single transaction — also makes a batch add atomic on mid-batch failure.
- **[Low]** queue.rs:474-480 — `is_taken` matches `output_path` against *every* job row, including history rows whose output no longer exists on disk. Once a name ever appears in history, all future conversions of that source renumber to `(1)`, `(2)`, ... forever, even into names that are free on disk. Conservative but surprising. Fix: scope the row check to active statuses (queued/encoding/paused) and let the on-disk `exists()` check handle completed outputs.
- **[Low]** queue.rs:261, 444 — `file_identity` is stat'd twice per path (once in `probe_candidates`, again in `add_files_to_db`); if the file changes in between, the probe decision and the stored fingerprint disagree. The insert-side value is authoritative so this is benign, but a single stat passed through would be cheaper and can't diverge.
- **[Nit]** queue.rs:49 — `get_next_queue_order` excludes `'error'`, but `get_queue` displays error rows ordered by `queue_order`; a new job can collide with an errored row's order, making the queue view's relative order of error vs new jobs undefined.
- **[Nit]** queue.rs:640-653 — manual `BEGIN`/`ROLLBACK`/`COMMIT` string SQL; `conn.unchecked_transaction()` gives RAII rollback and can't leave the shared connection stuck in a transaction if an early return is ever added.
- **[Nit]** queue.rs:91-107 — `get_handbrake_path` duplicates the configured-path-or-detect logic in commands/handbrake.rs:55-74. One helper, two callers.

### Commands, history, tests (lines 546-1749)

- **[Low]** queue.rs:702-705, 782-785 — history search interpolates user input into `LIKE` without escaping `%`/`_`, so a search for `100%` matches everything and `_` acts as a wildcard. Cosmetic for a personal history search. Fix: escape `%`, `_`, and the escape char, and add `ESCAPE '\'`.
- **[Nit]** queue.rs:716-764 — the has_search/no-search duplication (two count queries + two data queries differing only in the WHERE clause and param list) could collapse by always binding a pattern (`%` when no search); ~30 lines saved.

Otherwise the command surface (get_queue, remove_job, clear_queue, clear_completed, reorder) is clean: every command takes the DB lock, does pure SQL, and releases — no lock held across I/O or subprocess calls. Test coverage is genuinely good and intent-encoding (recycled-path scenarios, flag matrices, in-place guards).

### Structure

- **[Low]** queue.rs is 1749 lines, of which ~830 are code. It currently owns four concerns: file classification/scanning, skip-decision logic, output naming, and queue+history CRUD commands. The skip logic (`cheap_skip_reason`, `fetch_skip_sets`, `probe_candidates`, `file_identity`) and the history commands (`get_history*`, `get_history_summary*`) are both self-contained and would lift cleanly into `skip.rs` and `history.rs`, taking their tests with them. Not urgent, but the file is at the point where the next feature should trigger the split rather than grow it further.

## Summary

Overall health is good. The three past bug classes this area is known for — main-thread blocking probes, filename-recycling races, and Windows path separators in tests — are all visibly addressed with tests that encode the *why*, and the locking discipline is consistent: DB guards are scoped and dropped before reconcile/probe work, lock ordering has no cycles, and the add pipeline re-validates skip sets under the insert-time lock so unlocked probing can't race a concurrent add.

Themes:
1. The main-thread-stall class isn't fully dead — it survives in two indirect forms: the `preset_cache` mutex held across a HandBrake shell-out (High: freezes the UI via a sync command contending the lock) and sync commands doing unbounded recursive directory walks (`scan_folder`, `classify_paths`).
2. Cross-platform gaps cluster on the less-tested platforms: the hardcoded `.mp4` output extension vs the Linux MKV default preset, and non-UTF-8 path drops.
3. Lifecycle gaps in the watcher: disabling/removing a watch doesn't purge in-flight `pending` entries, so a settling file still gets converted after the user turned the watch off.
4. Small robustness nits repeat: error-string matching (`"UNIQUE"`, `"duplicate column name"`), `.expect()` panics at startup, per-row implicit transactions.

SQLite schema/migration discipline is exemplary — verified against git history that every change since the watched-folders feature has been additive (`CREATE TABLE IF NOT EXISTS` + idempotent `ADD COLUMN`), so auto-updating users are safe.

## Recommendations

Prioritized:

1. **Fix the `preset_cache` lock-across-shell-out** (queue.rs:335-344, 363-372) — release the guard before `get_preset_metadata`, re-lock to insert; extract the duplicated block into a helper. This closes the last known UI-freeze path in the add pipeline. (High)
2. **Derive the output extension from the preset container** in `choose_output_path` instead of hardcoding `.mp4` — the Linux default preset currently produces MKV bytes in `.mp4`-named files. Add a test pinning extension-per-container. (Medium)
3. **Move `scan_folder` and `classify_paths` onto `spawn_blocking`** like their sibling commands. (Medium)
4. **Purge `pending` entries under unwatched roots in `watcher::reconcile`**, so disabling a watch takes effect for files still stabilizing. Add a test: pending file under a removed root is not enqueued. (Medium)
5. **Canonicalize watched-directory paths before insert** in `add_watched_directory` so the UNIQUE constraint actually dedupes aliases/trailing slashes. (Medium)
6. Batch `add_files_to_db` inserts in one transaction; scope `is_taken`'s row check to active statuses; skip symlinked dirs in `scan_video_files`. (Low, quick wins)
7. When the next feature lands in queue.rs, split out `skip.rs` (skip decision logic) and `history.rs` (history commands) rather than growing the file past ~2000 lines. (Low)
