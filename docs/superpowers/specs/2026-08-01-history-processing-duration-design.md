# History Processing Duration

**Date:** 2026-08-01
**Status:** Approved for planning

## Problem

History entries say how much space a conversion saved but not how long it took. On a
slow host — the Docker/web head especially, but also older desktops — encode time is the
number that tells the user whether a preset is worth running. There is currently no way
to see it, and the database does not record it.

## Scope

Add an opt-out setting that shows each history entry's encode duration under its
status badge. One implementation serves both heads: the desktop shell and the server
head render the same React history list.

Out of scope: aggregate timing stats, per-job speed (fps is already a live-only menu bar
readout), and any explicit "this run was paused" marker — see Known Gaps.

## What the duration measures

Wall-clock time from the moment HandBrake was launched for that file to the moment the
job reached its terminal state. Not queue wait: a watched folder that ingests fifty
files at once would otherwise make file fifty report hours for a four-minute encode.

The `jobs` table has `created_at` (queued) and `completed_at` (finished) but nothing for
"encoding began", so this needs a new timestamp.

### Pause

Pause is the one case where wall clock can lie, and it is narrower than it looks:

- **Low-disk auto-pause cannot inflate it.** The gate runs *before* the job is claimed
  (`converter.rs:867`; the comment at `:831` states the ordering, and
  `low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued` pins it). The job
  stays `queued`, so it has no start timestamp to inflate.
- **Windows cannot inflate it.** `pause_conversion` falls back to queue-level pause when
  `can_pause_process()` is false (`control.rs:48`), which never freezes a running encode.
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
`failure_class` (`db.rs:171`), and added to the `CREATE TABLE` in `init_db` so fresh
databases get it directly. No backfill: rows written before this change keep `NULL` and
render no duration.

`JobInfo` gains `started_at: Option<String>`. The SELECTs that build `JobInfo`
(`converter.rs:436`, `queue_ops.rs:72/1190/1293/1413/1430`) each take the new column.

### Write sites

There are exactly two, both amendments to statements that already exist:

**Stamp** — in `claim_job` (`converter.rs:660`), the atomic claim, so the timestamp
cannot drift from "encoding actually began":

```sql
UPDATE jobs SET status = 'encoding', started_at = ?2 WHERE id = ?1 AND status = 'queued'
```

**Clear** — in `pause_conversion` (`control.rs:84`), which already runs a targeted UPDATE
in a branch reached only after a real SIGSTOP was delivered to a live PID with a current
job:

```sql
UPDATE jobs SET status = 'paused', started_at = NULL WHERE id = ?1
```

`resume_conversion` deliberately does *not* re-stamp — once cleared, the attempt reports
no duration.

Because `recover_interrupted_jobs` returns an interrupted job to `queued`
(`converter.rs:58`), the re-claim re-stamps. The duration therefore describes the attempt
that actually finished, not an abandoned partial.

## UI

The duration is right-aligned on the existing bottom row (`margin-left: auto`), placing
it directly under the status badge:

```
big-movie.mkv                        [ Saved ]
1.2 GB → 640 MB  -47%                 12m 34s
```

Error rows get it too — an encode that died forty minutes in is worth knowing. The error
message div becomes a flex row so the message keeps its ellipsis while the duration holds
the right edge; the entry stays two lines.

Rows that never encoded (`skipped`, or an error recorded before the claim) have a NULL
`started_at` and render nothing. So do rows whose encode was paused, and pre-upgrade rows.

`formatDuration` is new in `format.ts`, kept separate from `formatEta` so the menu bar's
ETA format is untouched:

| Elapsed | Rendered |
|---|---|
| < 60 s | `45s` |
| < 1 h | `12m 34s` |
| >= 1 h | `1h 05m` |

Computed client-side from the two RFC3339 timestamps, consistent with how sizes are
formatted. A non-positive delta — a clock jump or NTP correction between the two
stamps — renders nothing rather than `0s` or a negative.

## Setting

Key `history_show_duration`, boolean, **default `true`**.

It lives in a new "History" group in `SettingsPage.tsx` that is *not* wrapped in
`!isServerHead`: the Docker web UI is the primary audience, unlike the menu bar and
notification groups.

`HistoryPage` already calls `useSettings()`, so it passes
`showDuration={settings?.history_show_duration === true}` to `HistoryItem`. Defaulting to
`=== true` means a still-loading settings object shows no duration rather than flashing
one.

Touch list: `db.rs` defaults (the settings count guard goes 18 → 19, two assertions), the
`settings_ops` allowlist and parser, `Settings` in `types.rs`, `AppSettings` in
`transport/types.ts`, `SettingsPage.tsx`, and the settings fixtures in four test files
(`App.layoutTransition.test.tsx`, `useSettings.test.ts`, `HistoryPage.test.tsx`,
`SettingsPage.test.tsx`).
`routes.json` needs no change — it maps routes, not fields, and `get_settings` serializes
the struct whole.

## Testing

**Rust**

- `claim_job` writes a `started_at`; a claim that finds the row no longer queued writes
  neither status nor timestamp.
- A recovered interrupted job re-stamps on its second claim, replacing the first value.
- A mid-encode pause clears `started_at`, and resume does not restore it.
- Queue-level pause (the non-unix fallback) leaves `started_at` intact.
- `init_db` adds the column to a pre-existing old-schema DB and is idempotent across two
  runs — the existing migration test pattern.
- `update_setting` accepts `history_show_duration` and `get_settings` reports it.

**Frontend**

- `formatDuration` at each boundary: 59 s, 60 s, 3599 s, 3600 s.
- `HistoryItem` renders the duration when the toggle is on and both timestamps exist;
  renders nothing when the toggle is off, when `started_at` is NULL, and when the delta is
  non-positive.
- An error row renders both the message and the duration.

Each test states why the behavior matters, not just what it does — a NULL-renders-nothing
test exists because a paused or pre-upgrade row must not show a fabricated time.

## Known Gaps

- **No "was paused" indicator.** A paused run and a pre-upgrade row are both blank, and a
  user cannot tell which. Distinguishing them is a separate, smaller change (a boolean
  column and a marker glyph) and is deliberately deferred.
- **Existing history stays blank.** With the setting defaulting on, users see the column
  appear empty for all prior conversions until new ones run. Self-healing, but briefly
  odd.
- **Clock changes are not defended against beyond the non-positive guard.** A backwards
  jump mid-encode shortens the reported time; a forwards jump lengthens it. Both are
  rare enough not to justify a monotonic clock plumbed through the DB.
