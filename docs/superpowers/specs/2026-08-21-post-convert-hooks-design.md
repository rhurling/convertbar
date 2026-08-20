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
| `queue-drained` | once when the queue empties after a run | `converter.rs:1524`, the existing "queue done" block |

Both fire on **every** terminal outcome. There is no configurable outcome filter.

`post-convert` has **two** fire points, because `process_queue` books completions and failures
through entirely separate code paths:

- `converter.rs:1387`, the completion booking, for `done` and `skipped`.
- `converter::record_job_error_quiet` (`converter.rs:734`), for `error`. This is the single
  choke point for all nine error bookings in `process_queue` — one direct call at :869 and
  eight through the notifying wrapper `record_job_error` at :921, :951, :1029, :1172, :1199,
  :1309, :1341, and :1512. Firing at the wrapper instead would silently miss the :869 path.

To keep the two sites from drifting, **neither builds the payload**. Both call one
`hooks::fire_post_convert(ctx, &job_id, &mut run_state)` which re-reads the freshly booked
`jobs` row and constructs the payload from it. The hook therefore always reports exactly what
History shows, and a future status field is picked up by both paths at once.

Each trigger point supports two independent mechanisms — a webhook and a command — which may be
configured together, separately, or not at all. An unset URL or command means that half is off.

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

`control::cancel_conversion` books `status = 'error', error_message = 'Cancelled by user'`
directly (`control.rs:260`) and then kills the child. It does **not** call
`record_job_error_quiet`, so it does not itself fire a hook. What follows differs by state:

- **Cancelled while encoding:** the killed child makes `wait_for_active_child` return a
  non-success status, `process_queue` takes a failure arm, and the hook fires once from
  `record_job_error_quiet` with `status: "error"`. Correct: the job did terminate inside the
  engine, and a receiver watching for terminal outcomes should hear about it.
- **Cancelled while queued or paused:** no encode was ever running, `process_queue` never sees
  the row, and no hook fires. Also correct: nothing was converted.

The asymmetry is a consequence of where the row is booked, not a policy choice, and is spelled
out here so it is not later "fixed" into firing twice for an encoding cancel.

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

`run_status` is `converter::final_run_status(had_errors)` — `"idle"` or `"error"`.
`output_dirs` is the deduplicated, path-mapped list of directories touched by the run, in
first-seen order, derived from each job's `result_path` and therefore naming only directories
that contain a file that exists. Jobs with `status: "error"` contribute nothing to it, since
they have no `result_path`. It exists because it is exactly what a library rescan wants as its
argument, and a rescan of a path that was deleted is at best wasted work.

`completed` counts `done` and `skipped` jobs; `errors` counts `error` jobs. `space_saved` is the
sum over all jobs and, like the per-job field, can be negative.

Jobs are accumulated in a `Vec` local to `process_queue` for the duration of the run.

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
  pre-formed valid JSON and are inserted **raw**.

An unknown placeholder is left untouched rather than replaced with empty — a silent empty string
would send a malformed request that looks well-formed to the receiver.

Worked example, the driving case, on `queue-drained`:

```
URL      http://stash:9999/graphql
Headers  ApiKey: <key>
Body     {"query":"mutation { metadataScan(input: {paths: {{output_dirs_json}}}) }"}
```

### Headers

One `Name: value` per line. Blank lines ignored. `Content-Type: application/json` is sent by
default and may be overridden by an explicit line.

A line with no `:` is a configuration error. The hook **fails loud** — it does not fire with the
bad line silently dropped. Validation happens at fire time (read side), matching the crate's
existing convention that `update_setting` validates the key and nothing else.

Headers are stored in plaintext in `convertbar.db` and are readable by any authenticated web-UI
user, since the UI must display them for editing. This is the same posture as the server's auth
token and is called out in the README rather than engineered around.

## Mechanism: command

Config per trigger point: a command line. Empty disables it.

The payload is passed as environment variables, screaming-snake-cased with a `CONVERTBAR_`
prefix: `CONVERTBAR_EVENT`, `CONVERTBAR_STATUS`, `CONVERTBAR_SOURCE_PATH`,
`CONVERTBAR_OUTPUT_PATH`, `CONVERTBAR_OUTPUT_DIR`, `CONVERTBAR_SPACE_SAVED`, and so on. A `null`
field is passed as an empty string.

`CONVERTBAR_PAYLOAD` always carries the entire JSON payload, which is how a `queue-drained`
command reads the `jobs` array and how any consumer reads a field this list forgot.

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

Mapping applies to `source_path`, `output_path`, `output_dir`, and `output_dirs`.

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

### Command settings — deliberately not remotely configurable

`post_convert_command` and `queue_drained_command` are arbitrary code execution. On the server
head they must not be reachable through the HTTP API. Two structural facts enforce this, neither
of which is a filter someone can forget to apply:

- **Write:** the keys are absent from `ALLOWED_KEYS`, so `settings_ops::update_setting` rejects
  them, and `PUT /api/settings/{key}` is the only write path.
- **Read:** `settings_ops::get_settings` populates a typed `Settings` struct via an explicit
  `match` with a `_ => {}` arm. A key with no field in `Settings` is dropped on read. Adding no
  field is what keeps it off `GET /api/settings`.

Resolution order at fire time: the environment variable wins; otherwise the stored settings row;
otherwise the hook is off.

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
- A hook failure — non-2xx, transport error, timeout, malformed header line, non-zero exit
  status, command not found — **never** changes the job's status and never sets `had_errors`.

Failures surface three ways:

- `ctx.events.notify("ConvertBar", "Post-convert hook failed: <reason>")`
- `ctx.events.emit_t("hook-failed", { "event": …, "reason": … })` so the web UI can toast
- a line on stderr

**Notification is suppressed after the first failure of a queue run.** A broken receiver on a
200-file queue would otherwise produce 200 notifications. A `bool` local to `process_queue`,
alongside the existing `had_errors`, carries this; subsequent failures still log and still emit.
The `queue-drained` hook fires once and so is unaffected.

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
17 `Ctx::new` call sites; the compiler forces each to declare its runner. This is the same reason
`handbrake` is injected rather than defaulted — a test that reaches the hook layer without
declaring its world should fail to compile rather than make a real network call.

Implementations:

| Type | Used by | Behaviour |
|---|---|---|
| `HttpHookRunner` | both heads | real HTTP and real process spawn |
| `RecordingHookRunner` | test fixture default | records requests, returns `Ok`, touches nothing |
| `FailingHookRunner` | tests | returns `Err` — drives the failure-surfacing tests |
| `SlowHookRunner` | tests | blocks past the timeout — drives the timeout tests |

Payload construction, templating, header parsing, and path mapping are **pure functions** in
`hooks.rs`, tested directly without any runner. The runner trait covers only the I/O edge.

### HTTP client

`ureq`. Blocking, small, native timeout support, no async runtime. `reqwest` would pull tokio
into the desktop app for the sake of a handful of requests per queue run. `ca-certificates` is
already present in the runtime image, so TLS to an external receiver works unchanged.

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
  `record_job_error` wrapper and thereby missing the :869 path.
- A job cancelled while encoding fires once with `status: "error"`; a job cancelled while
  queued fires nothing.
- No path fires `post-convert` twice for one job.
- The in-place + `keep` case fires nothing.
- An empty URL and empty command fire nothing.
- A run fires exactly one `queue-drained` carrying every job of the run.
- `FailingHookRunner`: job status stays `done`, `had_errors` stays false, `hook-failed` is
  emitted, and the second failure of a run does not notify again.
- `SlowHookRunner`: the hook is abandoned at the timeout and the queue proceeds.
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
  block. No schema change to `jobs`, so nothing to review for backward compatibility.
- README and `docker-compose.example.yml` gain the hook environment variables and a worked Stash
  example. `unraid-template.xml` gains the two command variables.
