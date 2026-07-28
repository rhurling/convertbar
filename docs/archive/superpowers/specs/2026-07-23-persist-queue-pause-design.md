# Persist Queue Pause Across Restart — Design

## Problem

On launch, `lib.rs` auto-starts the queue whenever any `queued` jobs exist (`if has_queued { run_queue(...) }`). So a queue the user deliberately paused (via "Pause" or "Pause after this") starts converting again on the next launch. The pause is an in-memory, one-shot flag (`ConverterState.pause_after_current`) and is lost on restart.

## Goal

Remember a **user-initiated** pause across an app restart: a deliberately-paused queue stays stopped on next launch until the user clicks **Resume** (the button already shown when the queue is stopped with pending jobs). A **low-disk** auto-pause is deliberately *not* remembered — it re-evaluates the disk on restart.

## Decisions (settled with the user)

- **Which pauses persist:** user-initiated only — the macOS **Pause** (SIGSTOP) button and **"Pause after this"**. Low-disk auto-pause does **not** persist.
- **Adding files while paused:** starts the queue (clears the paused state). Matches today's "add files → start" behavior and applies to both drag-drop adds and watched-folder adds.

## Mechanism

A persisted boolean `queue_paused` in the existing `settings` key-value table.

- **Read** with a default of `false` when the row is absent — no seed in `db.rs` defaults, so the settings-count guard test is untouched and existing databases need no migration.
- **Write** via `INSERT INTO settings (key, value) VALUES ('queue_paused', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1` (same upsert shape `update_setting` uses).
- It is backend-managed runtime state, **not** a user preference: it is NOT added to `ALLOWED_KEYS` (so the frontend `update_setting` cannot touch it) and is NOT surfaced in the Settings UI.

Two `pub(crate)` helpers in `converter.rs`:
- `fn set_queue_paused(db: &Connection, paused: bool)` — best-effort upsert (`let _ =`).
- `fn is_queue_paused(db: &Connection) -> bool` — `value == "true"`, else `false`.

And a pure, testable launch predicate:
- `pub(crate) fn should_auto_resume(has_queued: bool, queue_paused: bool) -> bool { has_queued && !queue_paused }`.

## Where the flag is SET (`= true`)

The queue actually entering a user-initiated paused state:

1. **`commands::converter::pause_conversion`** (macOS SIGSTOP branch): after the process is stopped and the job row set to `paused`, `set_queue_paused(&db, true)`.
   - The non-macOS branch of `pause_conversion` only arms `pause_after_current` (a queue-level "pause after this"); it does not stop the queue yet, so it does not set the flag here — the flag is set when that fires (case 2).
2. **`converter::process_queue`**, in the branch where `take_pause_after_current()` returns true (the loop `break`s): `set_queue_paused(&db.lock()…, true)` alongside the existing idle menu-bar emit.

The low-disk gate does NOT set the flag (per the decision).

## Where the flag is CLEARED (`= false`)

Any (re)start of processing or resume of a paused job:

1. **`commands::converter::start_queue`** — before `run_queue`, `set_queue_paused(&db, false)`. Covers the **Resume** button and drag-drop adds (which call `startQueue` after adding).
2. **`commands::converter::resume_conversion`** (SIGCONT / un-freeze) — `set_queue_paused(&db, false)`.
3. **`watcher::enqueue_and_start`** — before its `run_queue`, `set_queue_paused(&db, false)`. Covers watched-folder file arrivals (adding files → start).
4. **`commands::queue::clear_queue`** — `set_queue_paused(&db, false)` alongside the existing `low_disk_pause` clear (an empty queue has nothing to stay paused for).
5. **`commands::converter::cancel_conversion`** — `set_queue_paused(&db, false)`. Cancelling the current job does not stop the queue (the loop continues with the next job), so a pause remembered from an earlier macOS SIGSTOP must be dropped, or the next launch would wrongly stay paused for a queue that was actively running.

## Launch guard (`lib.rs`)

In the setup hook, after `recover_interrupted_jobs(&db)` and computing `has_queued`, read the flag and gate the auto-start:

```rust
let queue_paused = crate::converter::is_queue_paused(&db);   // inside the existing db-lock scope
// … lock released …
if crate::converter::should_auto_resume(has_queued, queue_paused) {
    converter::run_queue(app_handle, db_arc, conv_arc);
}
```

When paused, the queue thread is not started; jobs stay `queued`; the frontend's existing Resume button (shown when `!activeJob && pendingJobs.length > 0`) is the affordance to continue, and clicking it clears the flag and runs.

## Interactions

- **Low-disk + user pause:** if the queue is user-paused AND the disk is low, launch stays paused (flag). Clicking Resume → `start_queue` clears the flag → `run_queue` → the low-disk gate re-checks and re-pauses if still low (that re-pause does not set `queue_paused`). Correct.
- **`recover_interrupted_jobs`** still resets `encoding`/`paused` rows to `queued` first; a SIGSTOP-paused job therefore becomes `queued`, and `queue_paused = true` keeps it from auto-starting. Correct.
- The `pause_after_current` *arm* itself is not persisted (out of scope): quitting mid-job while merely armed resumes normally on next launch, as today.

## No frontend change

The Resume button and the stopped-with-pending-jobs rendering already exist. A persisted pause simply lands the user in that state on launch. (No "Paused" label is added — YAGNI; the Resume button is the signal.)

## Testing

- `should_auto_resume` truth table: `(true,false)→true`; `(true,true)→false`; `(false,_)→false`.
- `set_queue_paused` / `is_queue_paused` round-trip against an in-memory DB, including the default-`false`-when-absent case.
- `process_queue`: when `pause_after_current` is armed, after the job completes the queue breaks AND `is_queue_paused(&db)` is `true` (extend/adjust an existing pause-after-current integration test).
- `start_queue` clears the flag (mock-app style, like the existing `cancel_conversion` test): set `queue_paused=true`, insert a queued job, call `start_queue`, assert the flag is cleared. (`resume_conversion`, `enqueue_and_start`, `clear_queue` clears follow the same shape; at least `start_queue` and `clear_queue` get direct tests.)
- A launch-behavior check is covered by the `should_auto_resume` predicate test plus the set/clear tests (the `lib.rs` setup hook itself is not unit-tested; the predicate isolates its only new decision).

## Out of scope

- Persisting the "Pause after this" *arm* (pre-pause intent).
- Any new Settings UI or "Paused" badge.
- Changing low-disk pause behavior.
