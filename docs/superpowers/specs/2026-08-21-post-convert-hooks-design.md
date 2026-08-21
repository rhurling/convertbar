# Post-Convert Hooks — Design

**Date:** 2026-08-21
**Status:** Approved, ready for implementation planning

## Motivation

ConvertBar finishes a conversion and the world outside does not find out. Downstream tools —
a media library that needs to rescan, a dedupe pass that wants to run over the new file — have
no signal to react to. Today the only way to notice is to poll the filesystem.

The driving case: a Docker/Unraid deployment where `stashapp/stash` should rescan and refresh
metadata for the file ConvertBar just produced, and where a `stashdupe` CLI living on the
Unraid *host* may also want to run.

### The constraint that shapes the design

The server head's container is `debian:bookworm-slim` with `handbrake-cli` and
`ca-certificates`, on `bridge` networking. It has `bash` but no `curl`. Two consequences:

1. A command hook running inside the container can only run things *inside* the container.
   Anything on the Unraid host is reachable only over the network — so a webhook is not the
   weaker mechanism, it is the only one that escapes the container unaided.
2. A bind-mounted script is still useful, because `bash` can open a raw socket
   (`exec 3<>/dev/tcp/host/port`) without `curl`. So a mounted script can fan out to several
   receivers, which is why one webhook per trigger point is enough (see "Rejected alternatives").

## Goals

- Notify an external system after each conversion, and once after a queue run finishes.
- Carry every fact ConvertBar already knows about the job: status, paths, sizes, timing, error.
- Work in the Docker server head with no custom image and no extra packages.
- Work in the desktop head, where running a local script is the natural idiom.
- Translate paths, because the path ConvertBar sees is rarely the path the receiver sees.

## Non-goals

- Retries, backoff, or a delivery queue. A hook is a notification, not a transaction.
- Multiple webhook receivers per trigger point. The receiver multiplexes.
- Filtering by outcome in config. The payload carries `status`; the receiver decides.
- Hook failure affecting job status. The encode succeeded; a broken receiver is not a bad encode.

## Trigger points

Two, both on the queue thread in `converter::process_queue`:

| Hook | Fires | Site |
|---|---|---|
| `post-convert` | after each job reaches a terminal state (`done`, `skipped`, or `error`) | two sites, see below |
| `queue-drained` | once when the queue genuinely drains | the post-loop queue-done block, guarded — see below |

Both fire on **every** terminal outcome. There is no configurable outcome filter.

`post-convert` has **two** fire points, because `process_queue` books completions and failures
through entirely separate code paths:

- `process_queue`'s success arm, where a job's `done`/`skipped` completion is booked — look for
  the `// The completion fire point (done/skipped).` comment immediately after the booking
  `UPDATE`.
- `converter::record_job_error_quiet`, for `error`. This is the single choke point for all nine
  error bookings in `process_queue` — one direct call in the vanished-source gate (the comment
  reads `// Vanished-source gate: ...`) and eight through the notifying wrapper
  `record_job_error` (`grep -n 'record_job_error(' converter.rs` finds the current call sites).
  Firing at the wrapper instead would silently miss the vanished-source gate's direct call — line
  numbers for all of this drift as the surrounding code changes, so cite the comment/function
  name, not the line, when this needs re-verifying.

To keep the two sites from drifting, **neither builds the payload**. Both call one
`hooks::fire_post_convert(ctx, &job_id)` which re-reads the freshly booked `jobs` row and
constructs the payload from it. It takes only `&Ctx`, which is what lets the error fire point sit
inside a free function without any signature churn. The hook therefore always reports exactly what
History shows, and a future status field is picked up by both paths at once.

Each trigger point supports two independent mechanisms — a webhook and a command — which may be
configured together, separately, or not at all. An unset URL or command means that half is off.

### `queue-drained` is not simply "the queue-done block was reached"

The existing queue-done block (the `// No more jobs — queue done notification` block at the
bottom of `process_queue`'s loop, immediately followed by the `queue-drained` fire) is **not** a
drain signal. Two `break`s reach it:

- `get_next_job` returns `None` — a true drain.
- `take_pause_after_current` fires (the `if take_pause_after_current(&ctx.converter) { ...
  break; }` arm inside the per-job loop) — "pause after this job", **and every pause on
  Windows**, since `pause_conversion` falls back to `pause_after_current` when
  `can_pause_process()` is false (`control.rs:46-53`).

And two paths never reach it at all: the low-disk pause and the shutdown path both `return`
early. So the naive placement would fire mid-run on every Windows pause, fire again after
resume, and silently drop every job completed before a low-disk stop.

Two changes fix this:

1. **Fire only on a true drain.** `process_queue` tracks a `drained` flag set only on the
   `get_next_job` → `None` break. The pause break leaves it false and fires nothing.
2. **Derive the job list from a persisted watermark, not an in-memory `Vec`.** A run-local
   accumulator cannot survive a pause, a low-disk stop, a quit, or a crash — exactly the
   interruptions a long Unraid queue actually hits. Instead a `settings` row holds the
   `completed_at` of the newest job already reported. On a true drain the hook selects `jobs`
   rows with `completed_at > watermark`, ordered by `completed_at`, fires, and only then
   advances the watermark to the newest `completed_at` in that set.
3. **One watermark per delivery mechanism**, `last_queue_drained_at_webhook` and
   `last_queue_drained_at_command`, each with its own batch loop. A single shared watermark
   advances only when *every* configured mechanism delivers, so with both configured a webhook
   that works is pinned behind a command that fails: it re-receives the identical oldest batch
   on every drain forever, and once the unreported backlog exceeds `QUEUE_DRAINED_BATCH` it
   never sees the jobs behind that batch at all. At-least-once delivery covers a retry, not an
   infinite replay. A mechanism that is not configured has no watermark to advance and is not
   treated as a failure.

This makes the payload correct across every interruption path: work completed before a pause is
reported by the drain that eventually follows it. It also means **a drain with nothing new fires
nothing at all** — an idle queue does not emit empty `queue-drained` payloads.

A mechanism's watermark advances only after a successful fire **to that mechanism**, so a failed
hook re-reports the same jobs to it on the next drain rather than losing them — and leaves the
other mechanism's watermark alone. "Successful" is `dispatch_mechanism` returning
`DispatchOutcome::Delivered` — an explicit outcome, deliberately not a failure counter's delta.
The counter could not represent the third case: a shutdown return is neither a success nor a
failure, and read as "no failure seen" it advanced the watermark past jobs nothing was ever sent
for, silently and irrecoverably (`completed_at > watermark` means they never come back). Only
`Delivered` advances; `Failed`, `Skipped` and `NotConfigured` all stop the loop. A receiver must therefore tolerate a repeat; for a library
rescan that is harmless, and it is the right trade against silent loss. Clearing History drops
rows that were never reported.

**The payload is batched, `QUEUE_DRAINED_BATCH = 100` jobs per payload.** `fire_queue_drained`
loops: each pass selects up to 100 rows since the watermark, dispatches one payload, and — only
after that dispatch succeeds — advances the watermark to the newest `completed_at` actually sent,
then loops again if the batch was full. A whole backlog therefore drains inside one call to
`fire_queue_drained`, not one batch per queue run; a 250-job backlog sends three payloads
(`[100, 100, 50]` job counts before the boundary rule below trims full batches). This exists
because `CONVERTBAR_PAYLOAD` puts the entire payload into a single command-hook environment
variable, and an unbounded payload would eventually fail `spawn()` with `E2BIG` — a failure that
correctly refuses to advance the watermark, so the *next* drain would build an even larger
payload: a wedge that never self-heals. At roughly 500 bytes per job object a full batch is
~50 KB, comfortably under both the Linux `MAX_ARG_STRLEN` (128 KiB) and macOS's combined
args+env cap (256 KiB).

**A full batch is cut at a `completed_at` timestamp boundary**, so a tie group (several jobs
sharing one `completed_at`) that straddles the cut is never split silently: the next pass asks for
`completed_at > watermark`, so any tied rows left on the far side of a mid-tie cut would be
silently *skipped*, not merely delayed. `fire_queue_drained` instead drops the trailing tied rows
from a full batch before dispatching, so they come back whole together with their siblings on the
next pass — a 250-job backlog with no ties therefore sends `[99, 99, 52]`, not `[100, 100, 50]`,
because the row that would have split a tie group is deferred to the following batch. **When a
single tie group is larger than one whole batch it cannot be split this way**, so it is sent
whole (a batch larger than `QUEUE_DRAINED_BATCH` by exactly the excess), and any further jobs
sharing that exact `completed_at` beyond the batch are skipped — logged loudly to stderr rather
than silently dropped, since silent loss here is the one outcome the watermark design exists to
prevent.

### Lock discipline

The hook runs **after** the `ctx.db` guard is dropped, exactly like `emit_t`. Holding `ctx.db`
across a hook would reproduce the deadlock documented in CLAUDE.md ("Emitting Events Under the
DB Lock"): the desktop tray listener re-locks `ctx.db` synchronously on the same thread and
`std::sync::Mutex` is not reentrant. A hook is strictly slower than an emit, so the window is
wider, not narrower. All settings the hook needs are read into owned values and the guard
released before the hook is invoked.

## Payload

### `post-convert`

```json
{
  "event": "post-convert",
  "job_id": "…",
  "status": "done",
  "source_path": "/data/movies/x.mkv",
  "output_path": "/data/movies/x.1080p-h265.mkv",
  "output_dir": "/data/movies",
  "result_path": "/data/movies/x.1080p-h265.mkv",
  "in_place": false,
  "preset": "…",
  "kept_file": "converted",
  "original_size": 4160749568,
  "converted_size": 1073741824,
  "space_saved": 3087007744,
  "duration_seconds": 412,
  "error_message": null,
  "failure_class": null,
  "started_at": "2026-08-21T10:00:00+00:00",
  "completed_at": "2026-08-21T10:06:52+00:00"
}
```

Field sources: all from the `jobs` row as booked, plus `output_dir` (parent of `result_path`),
`in_place` (`converter::is_in_place(source_path, output_path)`), and `duration_seconds`
(`completed_at - started_at`, `null` when `started_at` is absent).

**`result_path` is the field a receiver should act on.** It names the file that actually exists
on disk now: `output_path` when `kept_file` is `"converted"`, and `source_path` when it is
`"original"` — the `skipped` case and the cleanup-failure case, where the converted file was
discarded and the original survived. `output_path` alone is a trap: on a `skipped` job it points
at a file that was deleted. It is `null` on `status: "error"`, where no file is guaranteed.
`output_dir` is the parent of `result_path` for the same reason, which is what makes
`output_dirs` safe to hand to a library rescan.

### Status values

`status` is one of three, not two. `converter::decide_cleanup` returns **`"skipped"`** when the
encode produced a file that is not smaller than the original — the job succeeded, the original
was kept, and the converted file was discarded. A receiver that treats `skipped` as `done` would
scan a path that does not exist.

`kept_file` is `"converted"` or `"original"` as stored. On `status: "error"` the size and timing
fields may be `null`; `error_message` and `failure_class` are populated.

`space_saved` is the optimization delta `original_size - converted_size` and **can be negative**
— on a `skipped` job the encode was larger. It is deliberately not floored at zero.

The in-place + `keep` case deletes its job row and never books a completion (see CLAUDE.md,
"Cleanup Modes and the In-Place Rule"). It therefore fires **no** hook — there is no conversion
to report. This is deliberate and gets a test.

### Cancellation

**A cancelled job fires no `post-convert` hook, in any state.**

`control::cancel_conversion` books `status = 'error', error_message = 'Cancelled by user'`
directly (`control.rs:260`) and *then* kills the child. It does not call
`record_job_error_quiet`. When the killed child makes `wait_for_active_child` return non-success,
`process_queue`'s failure arm reads the row back and guards on it — the
`if current_status.as_deref() != Some("error") { ... }` check that wraps the failure arm's
booking — so `record_job_error` is skipped precisely because cancel already wrote the row.
Neither fire point is reached.

This is the correct outcome and is left as-is: a cancellation is a user action, not a conversion
result, and a receiver asked to rescan a file the user just abandoned is being told a lie. Do not
"fix" this by adding a third fire point inside the already-error branch.

A cancelled job **does** still appear in the next `queue-drained` payload, as an `error` row.
That is not an inconsistency with the above: `queue-drained` reports what History records since
the watermark, and History records the cancellation. The per-file hook is a live notification
about a conversion that happened; the drain payload is a ledger of what the queue did. A
cancelled job belongs in the ledger and not in the live notification.

One consequence must still be handled rather than inherited: `had_errors = true` is set in that
same failure arm, *before* that guard runs, and the low-disk and shutdown paths can set it for a run
whose rows fall outside the reported window. `run_status` in the payload is therefore derived
from the payload's **own** job set (`errors > 0`), never from `had_errors`, so
`{"run_status": "error", "errors": 0}` is unrepresentable. This can differ from the tray's
status; that is intended, and the tray is not a hook consumer.

### `queue-drained`

```json
{
  "event": "queue-drained",
  "run_status": "idle",
  "completed": 12,
  "errors": 1,
  "space_saved": 39284756123,
  "output_dirs": ["/data/movies", "/data/tv"],
  "jobs": [ /* array of post-convert objects, minus the "event" field */ ]
}
```

`run_status` is `"error"` when the reported job set contains an `error` row and `"idle"`
otherwise. It is deliberately **not** `converter::final_run_status(had_errors)` — see
"Cancellation" for why that would report `"error"` alongside `"errors": 0`.
`output_dirs` is the deduplicated, path-mapped list of directories, in first-seen order,
derived from each job's `result_path` and therefore naming only directories that contain a file
that exists. Jobs with `status: "error"` — including cancelled ones — contribute nothing to it,
since they have no `result_path`. It exists because it is exactly what a library rescan wants as its
argument, and a rescan of a path that was deleted is at best wasted work.

`completed` counts `done` and `skipped` jobs; `errors` counts `error` jobs. `space_saved` is the
sum over all jobs and, like the per-job field, can be negative.

The job set comes from the watermark query described under "Trigger points" — the *receiving
mechanism's* watermark, so the two can be at different points in History — not from an in-memory
accumulator, so it survives pauses, low-disk stops, and restarts.
It arrives in batches of up to `QUEUE_DRAINED_BATCH` (100) jobs, not as one payload per drain —
see the batching rule under "Trigger points". **The first drain after a hook is configured
reports the entire completed History**, batched, because there is no watermark yet.

## Mechanism: webhook

Config per trigger point: URL, headers, body template. Method is always `POST` — no case in
scope needs otherwise, and it is trivial to add later.

An empty URL disables the webhook.

### Body templating

An empty body sends the payload JSON above verbatim with `Content-Type: application/json`.

A non-empty body is a template. `{{placeholder}}` is substituted from the payload:

- **Scalar placeholders** (`{{output_path}}`, `{{status}}`, `{{space_saved}}`, …) are always
  **JSON-string-escaped** on substitution. A path containing `"` or `\` cannot break the
  surrounding JSON. They are substituted *without* surrounding quotes, so the template supplies
  them: `"path": "{{output_path}}"`.
- **`_json` placeholders** (`{{output_dirs_json}}`, `{{jobs_json}}`, `{{payload_json}}`) render
  pre-formed valid JSON and are inserted **raw**. Raw means they belong at a JSON *value*
  position, never inside a string literal: `"paths": {{output_dirs_json}}` is right, and
  `"query": "... {{output_dirs_json}} ..."` would splice unescaped quotes into that string and
  produce invalid JSON. This is why the worked example below passes the array as a GraphQL
  **variable** rather than interpolating it into the query text — the query stays a constant
  string and the data sits where JSON data belongs. The alternative, making substitution
  context-sensitive by tracking whether the placeholder falls inside an open string, was
  rejected: it makes one placeholder syntax mean two different things depending on surrounding
  text, and the template scanner would then have to model escaped quotes correctly to stay
  right.

A **`null`** field substitutes as the empty string, so `"path": "{{result_path}}"` on an error
job yields `"path": ""` — valid JSON that the receiver can test. It does not render the bare token
`null`, which would produce `"null"` inside quotes and read as a real path.

An **unknown** placeholder is left untouched rather than replaced with empty — a silent empty
string would send a malformed request that looks well-formed to the receiver.

Worked example, the driving case, on `queue-drained`:

```
URL      http://stash:9999/graphql
Headers  ApiKey: <key>
Body     {"query":"mutation($input: ScanMetadataInput!) { metadataScan(input: $input) }","variables":{"input":{"paths":{{output_dirs_json}}}}}
```

### Headers

One `Name: value` per line. Blank lines ignored. `Content-Type: application/json` is sent by
default and may be overridden by an explicit line.

A line with no `:` is a configuration error. The hook **fails loud** — it does not fire with the
bad line silently dropped. Validation happens at fire time (read side), matching the crate's
existing convention that `update_setting` validates the key and nothing else.

Headers are stored in plaintext in `convertbar.db` and are readable by any authenticated web-UI
user, since the UI must display them for editing. Relatedly, an authenticated user can aim the
webhook — arbitrary URL, headers, and body — at any address the container can reach, including
internal ones, which is a request-forgery primitive. Both are the same trust class as the auth
token itself: holding it already implies control of what ConvertBar converts and where output
lands. Called out in the README rather than engineered around, but called out.

## Mechanism: command

Config per trigger point: a command line. Empty disables it.

The payload is passed as environment variables, screaming-snake-cased with a `CONVERTBAR_`
prefix: `CONVERTBAR_EVENT`, `CONVERTBAR_STATUS`, `CONVERTBAR_SOURCE_PATH`,
`CONVERTBAR_OUTPUT_PATH`, `CONVERTBAR_OUTPUT_DIR`, `CONVERTBAR_SPACE_SAVED`, and so on. A `null`
field is passed as an empty string.

Non-scalar fields get no variable of their own: `queue-drained` exposes no
`CONVERTBAR_JOBS`, and `CONVERTBAR_OUTPUT_DIRS` carries the directory list as a **JSON array**,
not a shell-ambiguous space- or newline-joined string — a path containing a space would otherwise
be unrecoverable.

`CONVERTBAR_PAYLOAD` always carries the entire JSON payload, which is how a `queue-drained`
command reads the `jobs` array and how any consumer reads a field this list forgot.

**Each individual environment value is capped at `MAX_COMMAND_ENV_VALUE_BYTES` (96 KiB)**,
checked immediately before `spawn()`. 96 KiB sits under both the Linux `MAX_ARG_STRLEN` (128 KiB)
and macOS's combined args+env cap (256 KiB), with room for the rest of the process environment.
An oversized value fails with a message naming the variable and its size rather than a bare
`E2BIG` from the OS. `QUEUE_DRAINED_BATCH` is the primary defence against this ever being hit;
the cap is the backstop for when a batch's per-job size estimate is wrong (very long paths, for
instance). **The webhook path carries no such cap** — it is the escape hatch for a payload too
large for a command hook's environment.

The command line is split into a program and arguments on whitespace, with single- or
double-quoted segments kept intact as one argument. There are no escape sequences, no variable
expansion, and no globbing — a literal `$HOME` or `*` reaches the program unchanged. It is
executed **without a shell**.
There is no `sh -c`. A user who wants shell semantics points the hook at a script — which is the
intended Docker usage anyway (`/config/hooks/post-convert.sh` from a `./hooks:/config/hooks`
volume, executable, with a shebang).

Paths reaching a command hook are **not** path-mapped — neither the individual variables nor
the paths inside `CONVERTBAR_PAYLOAD`. See "Path mapping" below.

## Path mapping

One setting, `hook_path_map`, one rule per line:

```
/media => /data
```

Whitespace around `=>` is trimmed. Rules apply longest-`from`-first, so a more specific prefix
wins regardless of line order. The first matching rule applies; rewriting is not chained. A rule
matches only on a path-segment boundary, so `/media` does not match `/mediafoo`.

A trailing separator is stripped from **both** sides before storage, so `/media/ => /data/` and
`/media => /data` are equivalent. Without this, a trailing slash consumed into `from` would leave
`apply`'s segment-boundary check no separator to match on for any real subpath, so a rule written
with a trailing slash on `from` would silently never rewrite anything; a trailing slash left on
`to` would double up into `/data//...` instead.

Mapping applies to **every path-valued field**: `source_path`, `output_path`, `result_path`,
`output_dir`, and `output_dirs` — and recursively to the same fields inside each element of
`queue-drained`'s `jobs` array. `result_path` in particular must be mapped: the spec tells
receivers to act on it, and shipping it unmapped beside a mapped `output_path` would be the
worst of both.

**Mapping applies to webhook payloads only.** A command hook receives raw container paths,
because a shell script can rewrite them itself in one parameter expansion, and a second mapping
table in config to express what `${VAR/a/b}` already expresses is ceremony. This is a deliberate
asymmetry and is documented in the UI help text, not left to be discovered.

Mapping never touches what is stored in the database. It is a presentation concern of the hook
payload and nothing else.

## Configuration surface

### Webhook settings — normal, editable in both heads

Added to `settings_ops::ALLOWED_KEYS` and to the `Settings` struct, seeded in `db.rs` defaults:

| Key | Default |
|---|---|
| `post_convert_webhook_url` | `""` |
| `post_convert_webhook_headers` | `""` |
| `post_convert_webhook_body` | `""` |
| `queue_drained_webhook_url` | `""` |
| `queue_drained_webhook_headers` | `""` |
| `queue_drained_webhook_body` | `""` |
| `hook_path_map` | `""` |
| `hook_timeout_seconds` | `"30"` |

`hook_timeout_seconds` is parsed on read and clamped to `1..=300`; an unparseable value reads as
the default 30.

### Internal state, not user-editable

`last_queue_drained_at_webhook` and `last_queue_drained_at_command` hold the watermarks (one per
mechanism, see "Trigger points"). They are written by the engine, never by a user, so both are
**absent from `ALLOWED_KEYS` and from the `Settings` struct** — the same treatment the three
updater keys already get (`update_skipped_version`, `update_notified_version`,
`update_installed`), and the existing test that pins those exclusions is the model. An absent or
unparseable value reads as "no watermark", meaning the first drain reports every completed job in
History. That is a one-time burst on upgrade; the alternative — seeding it at migration time to
`now` — is a silent behaviour difference between a fresh install and an upgrade, so the burst is
accepted and noted in the release notes.

### Command settings — deliberately not remotely configurable

`post_convert_command` and `queue_drained_command` are arbitrary code execution. On the server
head they must not be reachable through the HTTP API. Two structural facts enforce this, neither
of which is a filter someone can forget to apply:

- **Write:** the keys are absent from `ALLOWED_KEYS`, so `settings_ops::update_setting` rejects
  them, and `PUT /api/settings/{key}` is the only write path.
- **Read:** `settings_ops::get_settings` populates a typed `Settings` struct via an explicit
  `match` with a `_ => {}` arm. A key with no field in `Settings` is dropped on read. Adding no
  field is what keeps it off `GET /api/settings`.

Resolution at fire time depends on the head, and the server head **never reads the settings
row at all**:

- **Server head:** environment variable only. If it is unset, the command hook is off.
- **Desktop head:** environment variable if set, otherwise the settings row.

"Environment wins, then fall back to the row" applied uniformly would be a live hazard, not a
convenience: a `convertbar.db` copied or migrated from a desktop install carries a
`post_convert_command` row, and the container would execute it. (Copying a live database into a
head has already caused one incident in this project.) The head therefore supplies the resolution
policy — `Ctx` carries whether the settings row is an accepted source — rather than core guessing.
A test asserts the server policy ignores a populated row.

| Trigger | Environment variable | Settings key (desktop only) |
|---|---|---|
| `post-convert` | `CONVERTBAR_POST_CONVERT_COMMAND` | `post_convert_command` |
| `queue-drained` | `CONVERTBAR_QUEUE_DRAINED_COMMAND` | `queue_drained_command` |

The server head is configured by environment variable, set in the Unraid template or compose
file. The desktop head reads and writes the settings row through dedicated `#[tauri::command]`
functions, which are local-only and ACL-exempt (CLAUDE.md, "Permissions (ACL)").

Two tests pin the boundary, because these two absences *are* the security control and both are
one line each:

1. `ALLOWED_KEYS` contains neither command key.
2. `get_settings` on a database where both command rows are set returns a `Settings` that
   serializes without them.

## Execution and failure semantics

**Blocking, on the queue thread, with a hard timeout.** The next job waits. Ordering is
guaranteed and a hung receiver cannot wedge the app.

- Timeout: `hook_timeout_seconds`, default 30. For a webhook it bounds connect and read. For a
  command, `process_queue` waits on a channel with `recv_timeout` from a waiter thread and calls
  `Child::kill()` when it expires — `std::process` has no native wait-with-timeout.
- **No retries.** A retry multiplies the worst-case stall and risks duplicating a side effect the
  receiver already performed before timing out.
- **Shutdown skips hooks.** `ConverterState::is_shutting_down` is checked immediately before
  firing, not only at the loop head. Without this, quitting the app blocks the queue thread for up
  to the timeout — up to 300s at the maximum setting — and a command hook's child could outlive
  the app. A hook already in flight at shutdown is abandoned, and a command child already
  spawned is NOT killed — `run_command` kills only when its own timeout expires, so on quit the
  child is simply orphaned. The `is_shutting_down` check prevents starting new ones; it does not
  reach the one already running.
- **Worst case is a multiple of the timeout.** With both a webhook and a command configured on
  `post-convert`, a dead receiver costs `2 ×` the timeout per job. The 30s default is the
  per-hook bound, not the per-job bound; the UI help text says so next to the field.
- **Cancelling during a hook does nothing.** `current_job_id` is already cleared by then — reset,
  along with `current_pid`/`current_child`, immediately once `wait_for_active_child` returns and
  shared by every arm of the match on its result — so cancel has no target. The hook runs to
  completion or timeout.
- A hook failure — non-2xx, transport error, timeout, malformed header line, non-zero exit
  status, command not found — **never** changes the job's status and never sets `had_errors`.

Failures surface three ways:

- `ctx.events.notify("ConvertBar", "<event> hook failed — <reason>")`, e.g.
  `post-convert hook failed — webhook: ...`
- `ctx.events.emit_t("hook-failed", { "event": …, "reason": … })`, so a head can surface it.
  No UI consumer ships in this change — the event exists so one can be added without touching
  the engine, and a test asserts it is emitted.
- a line on stderr

Ordering note for the error fire point: `record_job_error` calls `record_job_error_quiet` first
and *then* re-locks the db to send its "X failed" notification. A hook firing inside `quiet`
therefore delays that notification by up to the hook timeout, and a hook-failure notification
arrives before the job-failure one. Accepted: moving the fire after the notification would miss
the direct `record_job_error_quiet` call in the vanished-source gate, which never goes through
`record_job_error` at all.

**Notification is suppressed after the first failure of a queue run.** A broken receiver on a
200-file queue would otherwise produce 200 notifications. Subsequent failures still log and still
emit `hook-failed`.

This flag must **not** be a `process_queue` local. One of the two fire points is inside
`record_job_error_quiet`, a free function called from nine places via a wrapper; threading
`&mut` state into it would mean changing both function signatures and all nine call sites, and
the obvious shortcut — hoisting the hook call out to the nine sites instead — recreates exactly
the drift the two-fire-point design exists to prevent. Instead the flag is a field on
`ConverterState` (already an `Arc` reachable as `ctx.converter` from both fire points), cleared
when `process_queue` starts a run.

Hook-failure notifications ignore `notifications_per_file` and `notifications_errors_only`. Those
settings describe conversion outcomes; a misconfigured hook is a different condition and
suppressing it under a notifications preference would hide a config error indefinitely.

## Module structure

New `crates/convertbar-core/src/hooks.rs`, following the `FileDisposer` / `HandbrakeLocator`
injection pattern already established in the crate:

```rust
pub trait HookRunner: Send + Sync {
    fn run_webhook(&self, req: &WebhookRequest) -> Result<(), String>;
    fn run_command(&self, req: &CommandRequest) -> Result<(), String>;
}
```

`Ctx` gains `pub hooks: Arc<dyn HookRunner>` and `Ctx::new` gains a fifth parameter. There are
16 `Ctx::new` call sites (a 17th `grep` hit is a doc comment at `settings_ops.rs:266`); the
compiler forces each to declare its runner. This is the same reason
`handbrake` is injected rather than defaulted — a test that reaches the hook layer without
declaring its world should fail to compile rather than make a real network call.

Implementations:

| Type | Used by | Behaviour |
|---|---|---|
| `HttpHookRunner` | both heads | real HTTP and real process spawn |
| `RecordingHookRunner` | test fixture default | records requests, returns `Ok`, touches nothing |
| `FailingHookRunner` | tests | returns `Err` — drives the failure-surfacing tests |

Payload construction, templating, header parsing, and path mapping are **pure functions** in
`hooks.rs`, tested directly without any runner. The runner trait covers only the I/O edge.

### HTTP client

**`ureq` 3**, pinned to the major version. Blocking, small, no async runtime. `reqwest` would
pull tokio into the desktop app for the sake of a handful of requests per queue run.

Two details that are easy to get wrong:

- **TLS roots.** ureq 3's default features use rustls with **bundled webpki-roots**, which do
  *not* consult the container's `ca-certificates` store. Public-CA receivers would work, but a
  receiver behind a private CA or a self-signed homelab reverse proxy — a likely deployment for
  this feature — would fail with no obvious cause. The `platform-verifier` feature is therefore
  selected explicitly so the OS trust store is used.
- **Timeouts** in ureq 3 are configured on the agent (`Agent::config_builder()`), not per
  request. One agent is built with the configured timeout and reused.

`convertbar-core` currently has **zero** network dependencies. The `HookRunner` trait and all
pure logic live in `hooks.rs` in core; `HttpHookRunner` lives there too and core takes the ureq
dependency. Both heads need it, and duplicating the implementation per head to keep core
network-free would be worse. Flagged because it grows every consumer's build and the shared
`rust-tests` CI cache key (CLAUDE.md, cache topology).

## UI

Desktop Settings and the web UI both gain a "Hooks" section: per trigger point a URL field, a
headers textarea, and a body textarea; plus the shared path-map textarea and timeout field.

The command hook appears **only in the desktop UI**, as a path field with a file picker. The web
UI does not render it at all — not disabled, not shown read-only — because the server cannot
serve its value and a field that always renders empty invites a bug report. The web UI shows a
short note that the command hook is set by environment variable on the server head.

Help text states the two asymmetries that would otherwise be surprising: path mapping applies to
webhooks only, and the command runs without a shell.

## Testing

Pure functions, no runner needed:

- Templating: scalar substitution is JSON-escaped; a path with `"` and `\` produces parseable
  JSON; `_json` placeholders insert raw; an unknown placeholder is left untouched.
- Header parsing: multi-line parse, blank lines ignored, default `Content-Type` present and
  overridable, a line without `:` is an error.
- Path mapping: longest-prefix wins over line order; segment-boundary matching rejects
  `/mediafoo`; no rule means identity; rewriting does not chain.
- Timeout parsing: clamping at both ends, unparseable reads as 30.
- Payload construction: field-for-field against a booked `jobs` row; `duration_seconds` is `null`
  without `started_at`; an error job carries `error_message`/`failure_class`.
- `result_path`: equals `output_path` when `kept_file` is `"converted"`, equals `source_path`
  when it is `"original"`, and is `null` on an error row. This is the field most likely to be
  "simplified" into `output_path`, so it gets a test per branch.
- `queue-drained` aggregation: counts, summed `space_saved` (including a negative total),
  `output_dirs` deduped in first-seen order and path-mapped, and error jobs contributing no
  directory.

Through `process_queue`, with an injected runner:

- A `done` job fires `post-convert` exactly once with the right payload.
- A **`skipped`** job fires once, with `status: "skipped"` and `result_path == source_path`.
- An `error` job fires once — driven through a real failure arm, not by calling
  `record_job_error_quiet` directly, so the test would catch the hook being attached to the
  `record_job_error` wrapper and thereby missing the vanished-source gate's direct call.
- A cancelled job fires nothing, cancelled while encoding **and** while queued. The
  encoding case must drive a real cancel through `control::cancel_conversion` so it exercises
  the `!= Some("error")` guard in `process_queue`'s failure arm, not a hand-written error row.
- No path fires `post-convert` twice for one job.
- The hook-failure notification fires once per run and not once per file, and the flag resets on
  the next run — it lives on `ConverterState`, so a stale flag would silence a later run.
- The in-place + `keep` case fires nothing.
- An empty URL and empty command fire nothing.
- A true drain fires exactly one `queue-drained` carrying every job since the watermark, and
  advances the watermark.
- **`pause_after_current` fires no `queue-drained`.** This is the Windows-pause path
  (`control.rs:46-53`), so on Windows every pause would otherwise emit a spurious drain. The
  test drives the pause flag, not the platform.
- Jobs completed before a pause appear in the `queue-drained` that follows the eventual true
  drain — the regression test for the in-memory accumulator this design rejects.
- A drain with nothing new since the watermark fires nothing.
- A failed `queue-drained` hook does not advance the watermark, so the next drain re-reports the
  same jobs rather than losing them.
- **A drain during shutdown sends nothing and does not advance the watermark either** — the
  `Skipped` outcome. Paired with a positive control on the identical fixture without shutdown, so
  "the watermark stayed put" cannot pass by way of an arrangement that could never have advanced.
- The command-hook environment variables (`CONVERTBAR_POST_CONVERT_COMMAND`,
  `CONVERTBAR_QUEUE_DRAINED_COMMAND`) are honoured and beat a stored settings row. In its own
  integration-test binary (`tests/hook_command_env.rs`) because `set_var` is process-global and
  would otherwise hand a command hook to every lib test running concurrently.
- `run_status` is `"idle"` when the job set has no errors even though a cancelled job set
  `had_errors`.
- `FailingHookRunner`: job status stays `done`, `had_errors` stays false, `hook-failed` is
  emitted, and the second failure of a run does not notify again.
- Timeout enforcement is tested against a **real** child process that outlives its timeout,
  not against a slow test double. A double would only prove `dispatch` blocks for however long
  the double sleeps: the timeout lives inside `HttpHookRunner` (the ureq agent config for
  webhooks, `recv_timeout` for commands), so a double bypasses the thing under test entirely.
- A `LockProbeSink`-style check that `ctx.db` is not held while the hook runs — the existing
  probe in `control.rs` is the model, and this is the invariant most likely to regress.

Boundary tests: the two `ALLOWED_KEYS` / `Settings` assertions described above.

## Rejected alternatives

**A list of webhooks per trigger point.** Two named consumers (Stash rescan, `stashdupe`) argue
for it, but it means a CRUD table, routes, and list UI instead of two flat config blocks. A
single mounted script fans out — including to the Unraid host, since `bash` can open
`/dev/tcp/host/port` without `curl`. The receiver multiplexes; ConvertBar does not.

**Command hook editable in the web UI.** It would make the server's auth token the only thing
between the network and remote code execution as the container user. Environment-variable
configuration costs a container restart to change and removes the escalation entirely.

**Fire-and-forget hooks.** Never slows encoding, but hooks for different files overlap and land
out of order, and failures are only discoverable in the log. Ordering and visible failure were
judged worth a bounded stall.

**A configurable outcome filter.** `status` is in the payload and one `if` in a script or one
receiver-side check covers it. A checkbox per outcome per trigger point is four settings that
express what the payload already carries.

## Open items for the implementation plan

- Migration is additive only: new `settings` rows via the existing `INSERT OR IGNORE` defaults
  block, plus the engine-written watermarks. No schema change to `jobs`, so nothing to review for
  backward compatibility. `db::init_db` also seeds both per-mechanism watermarks from the
  pre-split `last_queue_drained_at` where that row exists, with `INSERT OR IGNORE` so a boot
  after one has advanced cannot drag it back; the pre-split row is left in place so a rollback
  still finds its watermark instead of replaying History.
- Release notes must mention the first-drain burst described under "Internal state".
- README and `docker-compose.example.yml` gain the hook environment variables and a worked Stash
  example. `unraid-template.xml` gains the two command variables.
