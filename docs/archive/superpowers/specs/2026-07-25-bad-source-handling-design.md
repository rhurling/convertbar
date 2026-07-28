# Bad-Source Handling — Design

## Problem

Two distinct gaps, one root cause: ConvertBar never asks *why* a job failed, and never checks whether a job that "succeeded" actually converted the whole file.

**1. Failures are undifferentiated.** Every failure path calls `record_job_error` with a stderr tail and sets `status='error'`. A genuinely corrupt download and a full disk produce the same history row. There is no way to act on "this file is garbage" because nothing knows which failures mean that.

**2. A truncated source is recorded as a success — and its original is trashed.** A partially-downloaded video keeps a valid container header declaring the full duration. HandBrake reads the header, encodes the bytes that are actually present, and **exits 0**. ConvertBar then:

- records `status='done'`,
- computes `space_saved` against the full original size (wildly inflated — the output is short, not smaller),
- and, under the default `cleanup_mode='trash'`, sends the **original to Trash** — `converter.rs:1023` on the distinct-file path, `converter.rs:111` (`TrashSourceThenRename`) for in-place jobs.

The user is left with a truncated output, an inflated savings stat, and the source in the Trash. This is live data loss on ordinary download corruption.

## Evidence

Reproduced locally against HandBrakeCLI 1.11.2 (macOS, Homebrew). A 20 s H.264 clip written with `+faststart`, truncated to 35 % of its bytes:

```
--scan --json  →  Duration: 20s          (container header intact — it lies)
encode         →  EXIT 0, "Encode done!"
output         →  Duration: 5s

stderr, healthy:    sync: got 480 frames, 480 expected   (0 decoder errors)
stderr, truncated:  sync: got 131 frames, 480 expected   (1 decoder errors)
```

### The frame-count marker is a usable signal

| case | container | exit | `sync: got …` | decoder errors |
|---|---|---|---|---|
| healthy CFR | MP4 | 0 | `480 / 480` | 0 |
| healthy VFR | MP4 | 0 | `150 / 150` | 0 |
| healthy | MKV | 0 | `480 / 480` | 0 |
| truncated | MP4 | 0 | `131 / 480` | 1 |
| truncated | MKV | 0 | `155 / 480` | **0** |

- **Variable framerate does not false-positive.** `expected` comes from the sync stage's own frame accounting, not `duration × fps`.
- **Decoder errors must NOT be a required condition.** A cleanly-truncated MKV decodes its available frames without a single error. Requiring `decoder errors > 0` would miss truncation in MKV — the most common download container and the Linux default output format. **Shortfall alone is the signal.**
- **Encoder-independent.** Verified identical under `Fast 480p30` (x264) and `H.265 Apple VideoToolbox 1080p`. The marker is emitted by the decode/sync stage, upstream of the encoder.
- **Already captured.** The marker sits ~1.2 KB from EOF under x264 and ~305 B under VideoToolbox, inside the existing 4096-byte `STDERR_TAIL_BYTES` window.

### Exit codes classify — but only partially

| case | exit |
|---|---|
| source unreadable / unscannable / missing / zero-byte / a directory | 2 |
| valid source, output dir missing | 3 |
| valid source, output dir read-only | 3 |
| `-Z "No Such Preset 9000"` | **2** |

### HandBrake's stderr CANNOT distinguish corrupt from unreadable

This is the finding the entire safety design rests on. Three cases produce **byte-identical** scan-stage output and the same exit code:

```
zero-byte file:        hb_stream_open: open X failed
a directory:           scan: unrecognized file type
GOOD file, chmod 000:  libhb: scan thread found 0 valid title(s)
                       No title found.                          exit = 2
```

A healthy movie on a network mount that hiccuped is indistinguishable from garbage **using HandBrake's output alone**. Any implementation that trusts HandBrake's verdict will eventually destroy good files.

Separately, `Invalid preset` also exits 2. That is a *global config* fault — a HandBrake upgrade renaming a preset would make it fire on **every file in the queue**. A denylist ("destroy anything that isn't a recognized environment fault") would turn one broken dependency into a trashed library.

## Goal

1. **Stop the data loss:** a truncated source must never be recorded as a success and its original must never be trashed. Unconditional — this corrects a wrong answer, not a preference.
2. **Classify failures** into `BadSource` / `Environment` / `Unknown`, conservatively enough that a misclassification cannot destroy a healthy file.
3. **Give the user a deliberate way to clean up** confirmed-bad sources, without the app ever destroying anything on its own.

## Decisions (settled with the user)

- **Scope:** one spec, two phases. Phase 1 = classification + review list. Phase 2 = truncation detection feeding the same pipeline.
- **Truncation outcome:** treat as a **failure** — `status='error'`, discard the short output, source untouched. Not a new `incomplete` status (avoids touching the DB status vocabulary, queue filters, menu-bar roll-up, and every test enumerating statuses). The salvageable partial output is deliberately discarded; the source survives, so nothing is unrecoverable.
- **No automatic destruction, ever.** Classification only *labels*. Destruction happens when the user presses a button in a review list.
- **Setting:** `bad_source_action` = `trash` | `delete`, default `trash`. No `off` value — the review list is harmless until pressed, so gating it behind a setting would only hide corrupt-download information from users who never find the toggle.

## Rejected: an optional ffprobe / second-engine dependency

Considered and measured. **Not adopted.**

`ffprobe -count_packets` detects truncation without decoding, and it is fast:

```
ffprobe -count_packets trunc.mp4 → 132 packets (real 0.01s) vs declared 20s × 24fps = 480 → 27.5%
ffprobe -count_packets trunc.mkv → 155 packets, plus an explicit "File ended prematurely"
```

Three findings sank it:

1. **It adds no accuracy.** 132/480 is the same verdict the free post-encode check already reaches at 131/480. Its only advantage is *earliness*.
2. **HandBrake already provides that earliness.** The default `--scan --json` — the exact command `probe.rs:72` already runs — decodes previews and reports the shortfall. ConvertBar discards it at `probe.rs:81` (`.stderr(Stdio::null())`):

   ```
   healthy:    scan: 10 previews, 320x240, 24.000 fps
   truncated:  Warning: Could not read data for preview 7, skipped
               scan: 6 previews, 320x240, 24.000 fps
   truncated MKV: File ended prematurely (repeated)
   ```

   Healthy files return 10/10 at every duration tested (0.5 s, 1 s, 3 s, 8 s, 120 s).
3. **The wasted-encode argument, the main reason to want pre-flight, is weak.** HandBrake stops at the data boundary rather than encoding the phantom tail — a 2-min clip truncated to 25 % encoded in 1.19 s against 2.82 s healthy. There is no full-length encode to save.

Against that: a configured-optional engine means **two classification paths**, and the non-default one is by construction the under-tested one — plus binary detection, a settings field, docs, and cross-platform validation. Even its one theoretical edge (per-file pre-flight cheaper than a HandBrake scan) is uncertain: `-count_packets` is index-based and near-free on MP4, but must walk clusters on MKV, making it O(file size) I/O — worse than HandBrake's seek-based previews on a NAS.

## Deferred: Phase 3 — pre-flight preview shortfall

> **Tracked as issue #122** (opened 2026-07-28 when this spec was archived), together with
> the accepted path-marker false-negative described further down.

Recorded because the finding above makes it nearly free, **not committed to this spec.**

Parse `scan: N previews` from the scan already run in `probe.rs`, flipping `.stderr(Stdio::null())` to piped. `N` < requested → the source is truncated, detectable before any encode.

Two reasons it is deferred rather than included:

- It only runs when `skip_by_source_media` is on, because that is the only time the scan happens. Making it universal means a HandBrake scan per file at add time — precisely the cost the project already decided must be opt-in (`db.rs:262`: "it shells out to HandBrake per file, so it is opt-in").
- Preview shortfall is a weaker signal than frame shortfall and would be used *pre-flight*, where a false positive rejects a healthy file rather than merely re-checking one. It would need validation against real-world VBR and network-mounted media before it could be authoritative.

If wasted encodes on corrupt downloads prove annoying in practice, this is the follow-up — and it changes nothing in Phases 1–2.

## Non-goals

- No retry / two-strike mechanism, and no in-app "retry this errored job" action. Re-adding the file by hand is the retry path, and it stays permissive for exactly that reason (see Watched folders).
- No backfill of existing history rows. `failure_class` is NULL for pre-existing entries, and NULL never appears in the review list.
- No mid-file corruption detection beyond frame shortfall. A file that decodes end-to-end with artefacts is not detectable this way.
- **Exit 0 with a 0-byte output stays `Unknown`.** Rule 4 below requires exit 2, so the existing empty-output guard will not classify as `BadSource`. Deliberate: the cause of that state is not understood well enough to destroy on it.

---

## Phase 1 — Failure classification

### New module: `src-tauri/src/failure_class.rs`

Pure, no I/O, table-testable. Same shape and rationale as `media_skip.rs`: the caller gathers facts, the module decides.

```rust
pub enum FailureClass { BadSource, Environment, Unknown }

pub struct FailureFacts<'a> {
    pub exit_code: Option<i32>,   // None = killed / signalled
    pub source_readable: bool,    // OUR observation, not HandBrake's
    pub stderr_tail: &'a str,
}

pub fn classify(facts: &FailureFacts) -> FailureClass
```

Rules, first match wins:

1. `!source_readable` → **Environment**.
   The load-bearing rule. It is the *only* thing separating `chmod 000` from a zero-byte file.
2. stderr contains an environment marker → **Environment**.
   `invalid preset`, `no space left`, `permission denied`, `not permitted`, `read-only`, `cannot create`.
   Checked **before** rule 3 so `Invalid preset` (exit 2) can never reach the `BadSource` branch.
3. `exit_code == Some(3)` → **Environment**.
   Exit 3 is `HB_ERROR_INIT` / a libhb work failure — measured for both "output dir missing" and "output dir read-only" with a *valid* source. HandBrake signals bad input with 2, never 3, so 3 is never the file's fault.
4. `exit_code == Some(2)` **and** stderr contains a source marker → **BadSource**.
   `unrecognized file type`, `no title found`, `0 valid title`.
5. Otherwise → **Unknown**.

Matching is lowercase-substring, consistent with the existing `DIAGNOSTIC_MARKERS` handling (`converter.rs:524`).

**Known false-negative: markers match inside file paths.** HandBrake echoes the source path into stderr (`hb_stream_open: open <path> failed`), so a corrupt file named `no space left.mkv` — or `Permission Denied (2015).mkv`, `Read-Only Memories.mkv` — trips rule 2 and is classified `Environment`. Confirmed empirically. The direction is safe (Environment never destroys; the file simply never reaches the review list), so this is accepted rather than fixed. The reverse — a *source* marker in a filename flipping an environment fault to `BadSource` — additionally requires exit 2 and passing rules 1–2, and no construction of it was found. Rule 2 is therefore evaluated against the whole tail; matching only on lines that do not contain the source path is a possible future tightening, not a requirement.

Truncation is deliberately **not** a rule here. By the time the Phase 2 guard fires it has already decided, so its call site passes `BadSource` directly rather than round-tripping a fact through `classify`. `decode_shortfall` / `is_truncated` still live in this module — they are pure parsing and policy — they are just not inputs to `classify`.

### Readability probe

```rust
fn source_is_readable(path: &Path) -> bool
```

`File::open` followed by a 1-byte read. Any `Err` yields `false`.

Note the polarity: `false` routes to **Environment**, which never destroys. So every probe failure — EACCES, EIO, a stalled mount, the file having vanished since — fails *safe*. This is the mirror image of `source_is_confirmed_missing` (`converter.rs:451`), which fails open in the other direction because there the safe answer is "let HandBrake try".

Probed at the failure point, not at spawn time: the question is whether the file is readable *now*, when we are deciding whether to believe HandBrake's verdict.

### Wiring into `converter.rs`

`record_job_error` and `record_job_error_quiet` take a new `class: FailureClass` argument and persist it. The eight call sites:

| line | failure | class |
|---|---|---|
| `:721` | source vanished (`_quiet`) | `Environment` (static) |
| `:768` | HandBrakeCLI not found | `Environment` (static) |
| `:792` | DB claim failed | `Environment` (static) |
| `:865` | spawn failed | `Environment` (static) |
| `:984` | empty output | **classify** |
| `:1052` | in-place apply failed | `Environment` (static) |
| `:1190` | nonzero exit / wait error | **classify** |
| *new* | truncation (Phase 2) | `BadSource` (static) |

Only three sites consult the classifier — precisely the ones where HandBrake's output is the sole evidence. The other five already know structurally what went wrong.

**Exit code capture.** The failure arm is currently `Ok(_) | Err(_)` (`converter.rs:1161`), discarding the status. It becomes a single bound arm extracting `exit_code`:

```rust
other => {
    let exit_code = match &other { Ok(s) => s.code(), Err(_) => None };
    …
}
```

### Persistence

```sql
ALTER TABLE jobs ADD COLUMN failure_class TEXT
```

Additive only, NULL for existing rows — an auto-updating install with an existing `convertbar.db` needs no data migration.

The existing ALTER loop at `db.rs:150` is hardcoded to `INTEGER` (it exists for `source_size`/`source_mtime`), so `failure_class` needs its **own** idempotent ALTER following the same duplicate-column-name pattern, not a new entry in that loop.

**Stored values are pinned strings:**

| variant | stored | in the review list? |
|---|---|---|
| `BadSource` via rule 4 (scan failure) | `'bad_source'` | yes |
| `BadSource` via Phase 2 (truncation) | `'bad_source_truncated'` | yes |
| either, after a successful purge | `'bad_source_purged'` | no |
| a row the purge re-scan proved readable | `'bad_source_recovered'` | no |
| `Environment` | `'environment'` | no |
| `Unknown` | `'unknown'` | no |

Rule-4 and truncation rows are stored **distinctly** because purge must treat them differently: rule-4 rows are re-scanned before destruction, truncation rows must not be (they pass a scan by construction — see the purge ladder). A single `'bad_source'` value would make that distinction unrecoverable at purge time. The review list matches `IN ('bad_source','bad_source_truncated')`.

`'bad_source_recovered'` exists so a row the re-scan just *proved healthy* leaves the list. Without it the row stays listed forever and costs a fresh HandBrake scan — up to the 30 s `PROBE_TIMEOUT` — on every subsequent purge press.

`Unknown` is stored as `'unknown'`, **never NULL**. NULL means exactly one thing — a row written before this feature existed. Collapsing the two would make new unclassified failures indistinguishable from legacy history, and the review list's "NULL never appears" property depends on the distinction.

### Setting

`bad_source_action`, values `trash` | `delete`, default `trash`. Touches the standard six places:

- `db.rs:173` defaults list → the count assertion **16 → 17 in _two_ tests**: `init_db_seeds_defaults` (`db.rs:252`) and `init_db_is_idempotent_and_preserves_user_changes` (`db.rs:312`). Plus a value assertion explaining why `trash` is the default.
- `commands/settings.rs:107` `ALLOWED_KEYS`
- `commands/settings.rs` `get_settings` parse (unrecognized value → `trash`)
- `types.rs` `Settings` struct
- `src/lib/tauri.ts:91` `Settings` type
- `src/pages/SettingsPage.tsx`

### Review list

- **`get_bad_sources()`** → `SELECT … WHERE status='error' AND failure_class IN ('bad_source','bad_source_truncated') ORDER BY completed_at DESC`
- **`purge_bad_sources(ids: Vec<String>)`** → `async` + `spawn_blocking`, returns per-id outcomes. Every outcome except `Purged` means **the file was not touched**:

  1. **`InUse`** — some job in `('queued','encoding','paused')` has this `source_path`. Destroying it mid-run would yank the source out from under a live encode. A failed lookup counts as in-use — uncertainty never destroys.
  2. **`AlreadyGone`** — the path is confirmed absent *and its parent directory is reachable*. Only then is the row stamped `bad_source_purged`, so it leaves the list instead of re-reporting forever.
  3. **`Unverifiable`** — anything that means "could not confirm": a `try_exists` error on the leaf or the parent, an unreachable parent (an unplugged volume), the re-scan being unable to *run*, or the file not being readable by us at destroy time. Never stamps, so the row survives for a later retry.
  4. **`Changed`** — the current `(size, mtime)` does not match the row's `source_size`/`source_mtime`. The path has been re-downloaded or replaced, and a stale verdict must not condemn a new file.
  5. **`Recovered`** — *only for `'bad_source'` rows* — a fresh scan now finds a valid title. Stamps `bad_source_recovered` so the row leaves the list.
  6. **`Failed`** — the row is not an eligible bad source, or the delete/trash call itself failed.
  7. Otherwise destroy per `bad_source_action`.

  **Row eligibility is part of the lookup, not a separate check.** The row SELECT is `WHERE id = ?1 AND status = 'error' AND failure_class IN ('bad_source','bad_source_truncated')`. Every rung verifies *the file*; this is what verifies *the row*. Without it, a caller passing the wrong id collection — the History list's ids rather than the review list's, a one-identifier mistake — reaches a `done` **in-place** conversion whose fingerprint was refreshed after that encode, so the identity check passes exactly, and destroys the user's only remaining copy.

  **`source_readable` gates the destroy, not just the classification.** Before honoring a re-scan's Destroy verdict, purge re-checks that *we* can open the file. HandBrake's stdout and exit code are byte-identical (no title block, exit 2) for a genuinely corrupt file and for a healthy file we merely cannot open — measured, not assumed — so without this gate the re-scan carries the exact blind spot it was built to close, and a healthy file on a flaky mount is destroyed by its own safeguard. This is the same rule-1 evidence the classifier already treats as load-bearing.

  Note the fix could **not** be "require a successful scan exit": real HandBrakeCLI exits 2 on a legitimate no-title scan, so keying on exit status would make every `bad_source` row permanently un-purgeable. Only a *signalled* scan process (`status.code().is_none()` — segfault, abort, OOM-kill) counts as `CouldNotRun`.

  **Identity uses the fingerprint the codebase already has, re-stamped at condemnation time.** `file_identity` returns `probe_cache::FileIdentity { size, mtime }` — existing infrastructure that already guards the *re-encode skip* decision. Reusing it here, where the operation is irreversible, beats a size-only comparison: a replacement of coincidentally identical size passes a size check and fails an mtime check. Rows with NULL fingerprints (pre-feature history, or a source unstattable at add time) have nothing to verify against, so purge **refuses** them (`PurgeOutcome::Changed`) rather than falling back to a weaker `original_size` comparison.

  The fingerprint is re-stamped when a job is condemned `BadSource`/`BadSourceTruncated`, **not** left at its add-time value. Otherwise purge verifies the file *as queued* while the verdict describes the file *as encoded*. The gap is exploitable: a healthy file is queued (fingerprint S,M), truncated in place by a sync tool, condemned `bad_source_truncated` — fingerprint still describing the healthy file — then repaired by that same tool. Because rsync `-t`, Syncthing and `wget -N` all preserve mtime, the repaired file is again exactly (S,M), identity "matches", and a fully repaired file is destroyed. A failed re-stat stores NULL, which purge already refuses.

  **Locking.** The DB mutex is held only for DB work. Filesystem probes and the re-scan run off-lock, so a bad source on a dead SMB/NFS mount cannot freeze the UI or stall the converter thread behind a 30–60 s `stat`. The final eligibility+identity re-check and the destroy itself share one guard, which keeps the check atomic against the queue's job claim (a DB write under that same mutex). The path comparison in the in-use check is canonicalized only in that final phase — by then the mount has been proven responsive — because `canonicalize` is `realpath(3)` and blocks exactly like the calls this structure exists to keep off the lock.

  **Rule-4 rows are re-scanned before destruction.** This is the guard against the design's sharpest failure mode: a healthy file on a network mount that hiccupped during scan produces exit 2 + `No title found.` — indistinguishable from garbage — and, if the mount heals before the readability probe runs, passes rule 1 and lands in the review list as `'bad_source'`. Nothing about the file changed, so the identity check passes too. Without a re-scan, **Delete permanently** destroys a healthy file, which contradicts Goal 2 outright.

  A purge is rare and user-initiated, so one `--scan` per file is affordable — and it uses HandBrakeCLI, requiring no second engine.

  **It must be scoped to `'bad_source'` rows.** Phase 2 truncation rows *pass* a scan by construction: the container header is intact and reports a full title — that is the entire reason truncation is undetectable at scan time. Applying the re-scan to them would clear every truncated file from the list and silently disable Phase 2. Truncation rows are defended by the identity check alone.

  **Three outcomes retire a row** by stamping `failure_class`: a successful destroy and a confirmed-absent file both stamp `bad_source_purged`; a re-scan that proves the file readable stamps `bad_source_recovered`. The list query matches only the two unpurged values, so retired entries drop out while remaining visible in normal history — without this the same rows reappear and every press produces nothing but errors and repeated 30 s scans. Every other outcome deliberately leaves the row listed, because every other outcome means the situation may still change.

### Interaction with existing history clearing — accepted as-is

`clear_completed` (`queue.rs:691`) deletes with `WHERE status = 'error'` (mode `errors`) or `status IN ('done','skipped','error')` (mode `all`). Every `bad_source` row has `status='error'`, so the existing **Clear errors** button wipes the review list and the corrupt files stay on disk unnoticed.

**This is accepted, not fixed.** The bad-sources list is a *view over history*, and history is the record; clearing history emptying the view is the consistent outcome. Carving out an exception would leave rows behind after the user pressed a button that says it clears errors — more surprising than the loss. The spec records the decision so an implementer does not silently pick the other behavior. `remove_history_entry` (`queue.rs:658`) behaves the same way per-row and is likewise unchanged.
### Watched folders must not re-ingest condemned files

The intake dedup ignores error rows, and the watcher rescans every enabled folder on **every launch**. Left alone, one corrupt file in a watched folder produces a fresh `bad_source` row, a wasted failed encode, and a "failed" notification each time the app starts — so the review list fills with duplicates of a single file and the count stops meaning anything.

The **watcher** intake therefore skips paths that already have an unpurged `bad_source`/`bad_source_truncated` row. **Manual drag-and-drop adds stay permissive** — a user re-adding a file by hand is deliberately asking for a retry, and that is the escape hatch from a wrong verdict. The skip query must stay in step with the review-list query; they describe the same membership.

### The panel

`HistoryPage` gains a **Bad sources (N)** panel above the history list, shown only when N > 0 — an always-visible panel rather than a filter the user has to discover, since a corrupt download they never learn about is the failure this feature exists to prevent. Rows show file name + reason, where the reason is the **first line of `error_message`** (already headlined with the diagnostic by `message_with_tail`) — no new field, and no second place to keep in sync with the failure text. The list is scroll-capped so a folder of several hundred corrupt stubs cannot push the button out of a menu-bar popover.

Bulk button worded per setting: **Move N to Trash** / **Delete N permanently**, and it is not rendered at all until settings resolve — a `null` settings object must never front a permanent delete under Trash wording.

**The confirm step applies to both modes**, not only `delete`. Trash is recoverable but still touches user data, and the count is restated at the moment of commitment.

**The armed confirm binds to a snapshot.** Arming captures the id set; confirming purges only that snapshot, intersected with the ids still present. Any change to the set while armed disarms. This matters because the panel live-refreshes on `job-error`: without the snapshot, a job failing between arm and confirm would silently enlarge the batch, and the user would destroy a file they never reviewed — violating the design's own principle that destruction acts on files the user actually saw.

While a purge is in flight both buttons are disabled and a pending indicator shows. Otherwise a second click starts a concurrent batch whose rows are already stamped purged, and the later-resolving note reports "left alone: failed" for files that were in fact just destroyed.

Outcomes are reported in plain language aggregated by kind ("2 are still being converted, 1 changed on disk since it was flagged"), including how many were removed, with a safe fallback for any outcome string the frontend does not recognize.

**Frontend data path.** `get_bad_sources` returns `Vec<JobInfo>` — no new type. That requires adding `failure_class: Option<String>` to `JobInfo` (`types.rs:4`), to its mirror in `src/lib/tauri.ts:4`, and to `row_to_job` (`queue.rs:56`) together with **every** SELECT column list that feeds it — `get_queue`, `get_bad_sources_inner`, and both branches of `get_history_inner` — none of which select it today. Missing one yields a column-count mismatch at runtime, not at compile time. This is not what the `ipc-contract` test covers (it checks `invoke`/`emit` name parity between frontend and backend, not SQL column lists); the equivalent coverage here is `get_bad_sources_inner` itself being exercised end-to-end against the real query in `get_bad_sources_lists_both_bad_classes_and_excludes_everything_else`, plus the frontend mocks kept in sync by hand. The list refreshes on mount and on the existing `job-error` event, reusing whatever `HistoryPage`'s `useHistory` already subscribes to rather than adding a new event.

Both commands are app-defined `#[tauri::command]`s, so they are **ACL-exempt** — `capabilities/default.json` is unchanged. The confirm step is in-app UI rather than the dialog plugin's frontend half, which would require a new grant.

---

## Phase 2 — Truncation detection

### Parsing

```rust
pub fn decode_shortfall(stderr_tail: &str) -> Option<(u64, u64)>  // (got, expected)
pub fn is_truncated(got: u64, expected: u64) -> bool
```

`decode_shortfall` parses `sync: got N frames, M expected`, taking the **last** match in the tail. Absent or unparseable → `None` → no action (uncertainty never destroys).

Last-match is defensive rather than observed: `--multi-pass` and `--subtitle scan` encodes were both checked and emit exactly one such line. But a Phase 2 false positive routes a *healthy* file into the purge list (the guard passes `BadSource` statically, bypassing `classify`), so an extra line from some future configuration must not be allowed to decide the verdict.

```rust
const MIN_DECODED_FRACTION: f64 = 0.90;
```

`is_truncated` → `expected > 0 && (got as f64 / expected as f64) < MIN_DECODED_FRACTION`. Measured margin is wide: healthy cases sit at exactly 1.00, truncated cases at 0.27–0.32. The 10 % slack absorbs container-level frame accounting quirks without approaching either cluster.

### Wiring

Three changes in `converter.rs`:

1. **Hoist the `stderr_tail_thread` join above `match exit_status`** (`:966`). Today it is joined only inside the two failure arms (`:981`, `:1187`), so the success path never reads it. The child has already exited by this point, so the drain thread is at EOF and the join is prompt in every arm. This also removes the current duplicate join.

2. **New guard in the success arm**, immediately after the empty-output guard (`:976`) and **before** `decide_cleanup` (`:1004`) — deliberately the same shape as its neighbour:

   ```rust
   if let Some((got, expected)) = decode_shortfall(&tail) {
       if is_truncated(got, expected) {
           had_errors = true;
           let _ = std::fs::remove_file(&encode_target);
           let pct = (got as f64 / expected as f64 * 100.0).round() as u64;
           record_job_error(app, db, &job.id, &file_name,
               &format!("Source appears truncated: decoded {got} of {expected} frames ({pct}%)"),
               FailureClass::BadSource);
           continue;
       }
   }
   ```

   Because it `continue`s before `decide_cleanup`, the cleanup branch never runs: the source is never trashed and the inflated `space_saved` is never recorded.

   **`encode_target`, never `job.output_path`.** For an in-place job the two are different things — `encode_target` is `in_place_temp_path(&job.source_path)` (`converter.rs:838`) while `output_path` *is* the source. Removing `output_path` here would delete the user's original outright. This mirrors the existing empty-output guard, and it is the same defect class as the previously-fixed in-place auto-resume bug: any new partial-cleanup site must route in-place jobs to the temp path. An in-place truncation case belongs in the integration tests below for exactly this reason.

3. **`STDERR_TAIL_BYTES` 4096 → 8192** (`converter.rs:478`). Measured headroom is 1.2 KB (x264) / 305 B (VideoToolbox), but each additional audio track appends a `mux:` line *after* the marker. Cheap insurance against a multi-track file pushing it out of the window.

---

## Testing

### `failure_class.rs` — table tests over real captured stderr

The stderr from the reproduction above is checked in as `&str` fixtures, following the `SCAN_FIXTURE_*` pattern in `probe.rs:162`.

**The load-bearing test is the `chmod 000` vs zero-byte pair:** identical stderr, identical exit code, **opposite** classification, distinguished only by `source_readable`. It fails the moment anyone "simplifies" rule 1 away — which is exactly the change that would start destroying healthy files. This encodes *why* rule 1 exists, not merely that it is present.

Also covered:
- `Invalid preset` (exit 2) → `Environment`, asserting rule 2 is evaluated before rule 4. Without the ordering this classifies as `BadSource` and a preset rename destroys every file in the queue.
- exit 3 (bad output dir, read-only dir) → `Environment`.
- Unmatched stderr, `exit_code: None` → `Unknown`.

### Phase 2

- `decode_shortfall`: parses the real marker; returns `None` for absent/garbled input.
- `is_truncated`: boundary table around `MIN_DECODED_FRACTION` — `expected == 0` → false; 480/480 → false; 150/150 (VFR) → false; 131/480 and 155/480 → true.
- An MKV truncation case with **0 decoder errors**, pinning that decoder errors are not required. This is a regression guard against the tempting-but-wrong tightening.
- Integration tests on the existing fake-HandBrake harness in `converter.rs` tests (`converter.rs:1650`, `:2631` — the scripts already echo stderr, write an output, and exit 0, on both unix and Windows `.cmd`, so a truncated encode is expressible):
  - a truncated **distinct-file** encode leaves the source **present on disk** and records `status='error'`, not `done`;
  - a truncated **in-place** encode leaves the source present and byte-identical, with only the temp removed. This is the test that fails if someone swaps `encode_target` for `job.output_path`.
- `purge_bad_sources` outcome table: `InUse`, `AlreadyGone`, `Unverifiable`, `Changed` (mtime differs at equal size — the case a size-only guard misses), `Recovered` (rule-4 row that now scans clean), `Failed`, and the destructive path. The `Recovered` test must assert a `'bad_source_truncated'` row is **not** recovered by a re-scan, pinning the scoping — without it, a re-scan applied to all rows silently empties the list of every truncated file.

### Tests that exist because a mutation proved the suite blind

Each of these was added after a specific one-line mutation survived the entire suite. They are listed with the mutation they must fail against, because that is what makes them worth keeping:

- **Trash-vs-delete dispatch.** Making the destroy path ignore the setting and always permanently delete once survived every test — the **default** action was entirely unverified. Pinned via a dispatch seam so both arms are assertable without touching the real Trash, including that the binding itself is not swapped.
- **`source_readable` wiring.** Hardcoding `source_readable: true` at both classification call sites survived the suite; only the pure `classify()` was pinned, not the observation feeding it. Now covered end-to-end by a mode-000 source that must classify `environment`.
- **The in-use status set.** Narrowing `IN ('queued','encoding','paused')` to `IN ('queued')` survived; the test only ever inserted a queued sibling. Now loops all three.
- **The exit-code extraction.** Collapsing it to `None` survived, which silently disables rule 4 at the only site that can ever return `BadSource` — a green suite with the feature permanently inert.
- **A signalled scan process.** A scan killed by a signal was indistinguishable from "scan confirmed the file is bad", so a segfaulting HandBrake destroyed files. Pinned by a `kill -SEGV` stand-in asserting `CouldNotRun`.
- **The armed-confirm snapshot.** No test bound the armed state to the ids the user reviewed, so a file arriving between arm and confirm would have been destroyed unreviewed.
- **StrictMode unmount latching.** RTL's `render()` does not wrap in `StrictMode`, so 138 tests were blind to a guard that latched `false` under React's dev double-invoke and wedged the purge UI mid-flight — files destroyed, nothing reported. One test now renders inside `StrictMode` deliberately.

### Reality tripwire

Every end-to-end truncation test echoes `sync: got N frames, M expected` from a fake script — a string this project does not control. If a HandBrake release reformats that line, the guard silently stops firing and the original data-loss bug returns with the whole suite green.

An `#[ignore]`d test therefore drives the guard with the **real** HandBrakeCLI against a real truncated file (synthesized with `-movflags +faststart`, so truncation leaves the moov atom intact and tests truncation rather than an unreadable file). It asserts the marker was found *separately* from the outcome, so a format change is diagnosed as "HandBrake changed its output" rather than the vaguer "guard didn't fire". `e2e-ignored.yml` already installs HandBrakeCLI and ffmpeg and runs the ignored suite on every push to `main`, so it costs nothing at PR time and runs continuously afterwards.

### Cross-platform

- The `chmod 000` readability test is `#[cfg(unix)]` **and must skip when running as root** — `File::open` succeeds regardless of mode 000 for uid 0, so the test would fail in a rootful container (`act`, docker) while passing on GitHub's non-root ubuntu runner. Guard on an effective-uid check.
- Path separators normalized in assertions — PR CI is ubuntu-only, so a hardcoded `/` only reddens `main` after merge.
- One new platform-gated dependency: `libc` under `[target.'cfg(unix)'.dev-dependencies]`, needed by the root-skip guard above. Production `libc` stays macOS-only (SIGSTOP/SIGCONT) and is gated with `#[cfg(...)]` **attributes**, never the `cfg!()` macro — the macro only skips at runtime and would still force linking on every platform. `trash` and `std::fs` were already in use on all three targets.
