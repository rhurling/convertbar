# History Processing Duration

**Date:** 2026-08-01
**Status:** Approved for planning (revised after adversarial review)

## Problem

History entries say how much space a conversion saved but not how long it took. On a
slow host — the Docker/web head especially, but also older desktops — encode time is the
number that tells the user whether a preset is worth running. There is currently no way
to see it, and the database does not record it.

## Scope

Add a setting, defaulting on, that shows each history entry's encode duration under its
status badge. One implementation serves both heads: the desktop shell and the server head
render the same React history list.

Only history rows are affected. History is `status IN ('done', 'error', 'skipped')`
(`queue_ops.rs:1402`), so in-flight jobs are never history rows; the queue view is
untouched and no live elapsed-time counter is in scope.

Out of scope: aggregate timing stats, per-job encode speed (fps is already a live-only
menu bar readout), sorting by duration, and any explicit "this run was paused" marker.
See Known Gaps.

## What the duration measures

Wall-clock time from the moment HandBrake was launched for that file to the moment the
job reached its terminal state. Not queue wait: a watched folder that ingests fifty
files at once would otherwise make file fifty report hours for a four-minute encode.

The `jobs` table has `created_at` (queued) and `completed_at` (finished) but nothing for
"encoding began", so this needs a new timestamp.

Per status:

| Status | Shows |
|---|---|
| `done` | The encode's duration. |
| `skipped` | The encode's duration. **`skipped` is a post-encode state** — `decide_cleanup` (`converter.rs:311-331`) assigns it when the encode ran to completion but the output was no smaller than the source. The wasted time is real and is exactly what the user wants to see. Pre-encode skips never create a row at all; they are `AddResult.skipped` counts. |
| `error` | The duration up to the failure, when the job had actually been claimed. An encode that died forty minutes in is worth knowing. Errors recorded before the claim show nothing. |
| Cancelled | Cancel writes `status='error'` with a `completed_at` (`control.rs:256-263`), so a cancelled job shows the elapsed time until cancel. |
| Paused mid-encode | Nothing — see below. |

### Pause

Pause is the one case where wall clock can lie, and it is narrower than it looks:

- **Low-disk auto-pause cannot inflate it.** The gate runs *before* the job is claimed
  (`converter.rs:867`, claim at `:924`; the comment at `:831` states the ordering, and
  `low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued` pins it). The job
  stays `queued`, so it has no start timestamp to inflate. No wait state exists between
  claim and spawn.
- **Windows cannot inflate it.** `pause_conversion` falls back to queue-level pause when
  `can_pause_process()` is false (`control.rs:48`), which never freezes a running encode;
  the encode finishes and reports a correct duration.
- **The updater cannot inflate it.** Its drain uses `pause_after_current`
  (`updater.rs:978, 996`) and never sends SIGSTOP.
- **A mid-encode SIGSTOP on macOS/Linux would inflate it** — an encode paused overnight
  would report the overnight.

That last case is handled by *discarding* the measurement rather than correcting it: the
pause clears the start timestamp, so the finished job shows no duration at all. Honest
absence beats a wrong number, and it reuses the same empty state as pre-upgrade rows.

Rejected alternative: a `paused_ms` accumulator with write points in pause, resume, and
completion. Three write sites and three tests to salvage a number for a rare, deliberate
user action.

## Data model

One additive column:

```sql
ALTER TABLE jobs ADD COLUMN started_at TEXT
```

Applied with the idempotent `duplicate column name` guard already used for
`failure_class` (`db.rs:171`), and added to the `CREATE TABLE` in `init_db` (`db.rs:83`)
so fresh databases get it directly. No backfill: rows written before this change keep
`NULL` and render no duration.

### The invariant

> `started_at` is stamped when a job is claimed into `encoding`, and cleared by **any
> non-terminal transition out of `encoding`**. Terminal transitions (`done`, `skipped`,
> `error`) leave it alone.

State the rule this way rather than as a list of sites, so a future transition added to
the state machine is obliged to answer the question. There are two non-terminal exits
today, and both must clear:

**Stamp — the claim** (`converter.rs:660`), the atomic claim, so the timestamp cannot
drift from "encoding actually began":

```sql
UPDATE jobs SET status = 'encoding', started_at = ?2 WHERE id = ?1 AND status = 'queued'
```

**Clear — mid-encode pause** (`control.rs:84`), amending a targeted UPDATE that already
exists in a branch reached only after a SIGSTOP to a live PID with a current job:

```sql
UPDATE jobs SET status = 'paused', started_at = NULL WHERE id = ?1
```

`resume_conversion` (`control.rs:169`) is the only non-claim path back to `encoding` and
deliberately does *not* re-stamp — once cleared, that attempt reports no duration.

**Clear — crash recovery** (`converter.rs:57-60`):

```sql
UPDATE jobs SET status = 'queued', started_at = NULL WHERE id = ?1
```

This third site is load-bearing and was missed in the first draft. `recover_interrupted_jobs`
returns interrupted jobs to `queued`, and the "re-claim re-stamps" argument only holds for
jobs that reach the claim. Three error paths in `process_queue` write `completed_at`
*before* claiming — the vanished-source gate (`:851`), HandBrake-not-found (`:901`), and
`ClaimOutcome::Failed` (`:933`), all via `record_job_error{,_quiet}`. Without this clause:
an encode stamped Monday 22:00, a crash, a source the user then deletes, a relaunch, and
a Tuesday 10:00 vanished-source error yields a **12-hour "encode duration"** on an error
row.

### Type plumbing

`JobInfo` gains `started_at: Option<String>` in `types.rs`. Sites:

- The shared row mapper `row_to_job` (`queue_ops.rs:62-79`) and the SELECT lists feeding
  it (`queue_ops.rs:1187/1291/1413/1430`) plus `converter.rs:436`.
- The `JobInfo` struct literal in the add path (`queue_ops.rs:1098`) — compiler-enforced,
  but a plan needs a task for it.
- The **frontend** `JobInfo` interface (`src/lib/transport/types.ts:3-17`), hand-written
  with no codegen, plus any `JobInfo` literal fixtures in frontend tests. `tsc` catches
  this, but it is the one edit that makes the field reachable in either head.

No transport work beyond that: `routes.json` maps commands to method and path with no
field enumeration, and `get_history`/`get_settings` serialize the structs whole. Live
updates need nothing either — `useHistory` refetches the full page on `job-completed` and
`job-error` (`useHistory.ts:62-67`) rather than reading fields off the event payload, and
the server head heals missed events on SSE reconnect via the same refetch.

## UI

The duration is right-aligned on the entry's bottom row (`margin-left: auto`, plus
`white-space: nowrap` so the value cannot wrap mid-token), placing it directly under the
status badge:

```
big-movie.mkv                        [ Saved ]
1.2 GB → 640 MB  -47%                 12m 34s
```

It carries `title="Encode time"`. The saved-percent beside it has no tooltip, but a bare
duration is less self-describing than a percentage.

### Error rows need a markup change, not just a flex container

Error rows show the duration too, but the obvious implementation does not work.
`.history-item-error-msg` (`App.css:346-353`) is a div with
`overflow: hidden; text-overflow: ellipsis; white-space: nowrap` wrapping a bare text
node (`HistoryItem.tsx:56-60`). Making that div `display: flex` turns its text into an
**anonymous flex item**, and `text-overflow: ellipsis` no longer applies — it only
ellipsizes a block container's own text. The anonymous item's `min-width: auto` then holds
it at full min-content width and pushes the duration past the right edge, where the div's
`overflow: hidden` clips it. Since `message_with_tail` promotes HandBrake's diagnostic to
the headline (`converter.rs:696`), these messages are long essentially always, so the
duration would be invisible at every window width.

Required instead: wrap the message in a child `<span>` that carries `min-width: 0`, the
ellipsis rules, and the existing `title`; make the parent the flex row; the duration is
its sibling.

### When there is no bottom row

A non-error row renders its sizes row only when `original_size !== null`
(`HistoryItem.tsx:40`), and an error row renders its message div only when
`error_message` is truthy. Both are rare, but a row can have a `started_at` and no bottom
row. Rule: **if the bottom row would otherwise be absent and a duration exists, render
the bottom row containing only the duration.**

### Format

Two pure functions in `format.ts`, kept separate from `formatEta` so the menu bar's ETA
format is untouched:

- `durationSeconds(startedAt: string | null, completedAt: string | null): number | null`
  — parses both, returns `null` if either is absent or unparseable, or if the delta is
  non-positive (a clock jump or NTP correction between the stamps).
- `formatDuration(seconds: number): string` — rounds the total to the nearest second,
  then decomposes.

Splitting them keeps the guard in one testable place and lets the boundary tests pass
plain numbers.

| Input (s) | Output | Note |
|---|---|---|
| 0.3 | `<1s` | Positive but rounds to zero. An instant failure must not read as `0s`, and must stay distinguishable from "no data". |
| 1 | `1s` | |
| 59 | `59s` | |
| 60 | `1m 00s` | Seconds always two digits in this form. |
| 754 | `12m 34s` | |
| 3599 | `59m 59s` | |
| 3600 | `1h 00m` | Minutes always two digits in this form; seconds dropped. |
| 90000 | `25h 00m` | Hours do not roll over into days. |

## Setting

Key `history_show_duration`, boolean, **default `true`**.

The toggle exists because the duration is noise for users who only care about space
saved, and the history row is deliberately terse — two short lines. It defaults on so the
Docker users who motivated the feature get it without hunting through settings.

It lives in a new "History" group in `SettingsPage.tsx` that is *not* wrapped in
`!isServerHead`: the Docker web UI is the primary audience, unlike the menu bar and
notification groups.

`HistoryPage` already calls `useSettings()`, so it passes
`showDuration={settings?.history_show_duration === true}` to `HistoryItem`. The `=== true`
means a still-loading settings object shows no duration rather than flashing one.

The parser in `settings_ops` must initialize this key's fallback to **`true`**, matching
the seeded default. The surrounding initializers are a mix of `true` and `false`
(`settings_ops.rs:110-117`), so the correct value has to be stated rather than inferred
from a neighbouring line; getting it wrong silently inverts the default for any database
missing the row, and is masked in tests because `init_db` seeds it.

Touch list: `db.rs` defaults (the settings count guard goes 18 → 19, two assertions at
`db.rs:273` and `db.rs:334`), the `settings_ops` allowlist and parser
(`settings_ops.rs:39-58, 137-181`), `Settings` in `types.rs`, `AppSettings` in
`transport/types.ts`, `SettingsPage.tsx`, and the settings fixtures in four test files
(`App.layoutTransition.test.tsx`, `useSettings.test.ts`, `HistoryPage.test.tsx`,
`SettingsPage.test.tsx`).

## Testing

Each test must fail if the behavior it names is removed. Two traps to avoid, both caught
in review of the first draft:

- A suite that only exercises `formatDuration` and `HistoryItem` with a hand-passed prop
  stays green when `HistoryPage` never wires the setting up — the feature ships dead, or
  hardcoded on, with everything passing.
- "Queue-level pause leaves `started_at` intact" is **not** a useful test. The non-unix
  branch (`control.rs:48-54`) contains no SQL at all, so the assertion tests the absence
  of code and cannot fail; it is also unreachable on the Linux CI that gates merges. It is
  deliberately not in this list.

**Rust**

- `claim_job` writes a `started_at`; a claim that finds the row no longer queued writes
  neither status nor timestamp.
- Crash recovery clears `started_at`, and a job that then fails on a pre-claim error path
  (vanished source) reports no duration — the 12-hour-error scenario above, as a
  regression test.
- A recovered job that *does* reach the claim re-stamps, replacing the first value.
- A mid-encode pause clears `started_at`; resume does not restore it.
- A fresh DB reports `history_show_duration == true` from `get_settings` — pins the
  default, which the Setting section's whole argument rests on.
- `update_setting` accepts `history_show_duration`.
- `init_db` adds the column to a pre-existing old-schema DB and is idempotent across two
  runs — the existing migration test pattern.

Tests that reach HandBrake resolution must declare their locator world explicitly
(`AbsentLocator`/`StubLocator`), per the fixture rule in CLAUDE.md.

**Frontend**

- `formatDuration` at every row of the format table above, asserting the exact strings —
  including the sub-second `<1s` case and the two padding forms.
- `durationSeconds` returns `null` for a missing stamp, an unparseable stamp, and a
  non-positive delta.
- At least one test feeds a **verbatim backend-produced timestamp** — chrono's
  `to_rfc3339()` emits up to nine fractional digits and a `+00:00` offset, e.g.
  `2026-08-01T10:00:00.123456789+00:00` — not a hand-typed `...Z` fixture. Nothing in
  `src/` parses timestamps today, so this assumption is currently unproven.
- `HistoryItem` renders the duration when the toggle is on and both stamps exist; renders
  nothing when the toggle is off, when `started_at` is null, and when the delta is
  non-positive.
- A `skipped` row renders its duration — the wasted-encode case, which the first draft got
  backwards.
- An error row renders both the message and the duration, and the message still
  ellipsizes.
- A row with no sizes row and no error message renders a bottom row containing only the
  duration.
- `HistoryPage` passes the setting through: with `history_show_duration: false` and a row
  carrying both stamps, no duration renders; with `true`, it does. Without this the
  feature can ship unwired.

## Known Gaps

- **No "was paused" indicator.** A paused run and a pre-upgrade row are both blank, and a
  user cannot tell which. Distinguishing them is a separate, smaller change (a boolean
  column and a marker glyph) and is deliberately deferred.
- **No sort-by-duration.** The Problem section frames a comparison task, and the history
  sort bar (`HistoryPage.tsx:303-308` → `queue_ops.rs:1388-1393`) gains no duration key.
  Sorting would need a computed `ORDER BY` over two TEXT timestamps. Deferred, but
  recorded as a decision rather than left silent.
- **Pause racing child exit blanks a good duration.** `current_pid`/`current_job_id` are
  cleared only after `wait_for_active_child` returns (`converter.rs:1105`), so a pause
  landing in that window SIGSTOPs a dead PID and still runs the clearing UPDATE. The
  status-flip half of this race is pre-existing; the new consequence is a blank duration
  on a job that completed normally. Not worth a guard.
- **The clearing UPDATE stays unguarded.** It has no `AND status = 'encoding'`, matching
  the statement it amends. This would NOT close the pause-races-child-exit gap above:
  `current_job_id` is cleared (`converter.rs:1119`) right after `wait_for_active_child`
  returns, but the completion UPDATE that writes `done`/`skipped` runs later
  (`converter.rs:1370`), so throughout that window the row is still `encoding` and the
  guard would let the clearing UPDATE through regardless. What it would narrow instead is
  a separate, simultaneous cancel-and-pause race: `cancel_conversion` writes `error` while
  `current_job_id` is still set, so a pause landing in that window could otherwise rewrite
  an already-`error` row. That changes pause semantics and belongs in its own change with
  its own tests. Inherited knowingly.
- **`started_at` clearing depends on `current_job_id` being `Some`.** SIGSTOP is sent
  whenever `current_pid` is `Some`, while the DB write sits inside a nested
  `if let Some(ref job_id)`. They are set together at spawn, so a desync is believed
  unreachable — but the "discard the measurement" guarantee rests on it.
- **Existing history stays blank.** With the setting on by default, prior conversions show
  no duration until new ones run. Nothing looks broken — those rows render exactly as they
  do today — but the feature appears to do nothing at first.
- **Clock changes are not defended against beyond the non-positive guard.** A backwards
  jump mid-encode shortens the reported time; a forwards jump lengthens it. Rare enough
  not to justify a monotonic clock plumbed through the DB.
