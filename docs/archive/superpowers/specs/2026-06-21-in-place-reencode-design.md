# True in-place re-encode + skip-reason feedback

**Status:** Design approved, not yet implemented
**Date:** 2026-06-21
**Source issue:** `docs/OPEN_ISSUES.md` → "True in-place re-encode (temp file + atomic rename)"

## Problem

The output extension is always forced to `.mp4` (`format!("{}{}.mp4", stem, suffix)`,
`src-tauri/src/commands/queue.rs:199`) and HandBrake writes straight to the final path
(`src-tauri/src/converter.rs:281`) — there is no temp+rename.

Two consequences:

1. **No in-place re-encode.** When the source is already `.mp4` and the suffix is empty,
   the resolved output path equals the source path. The pre-existing
   `if output_path.exists() { continue; }` guard (`queue.rs:203`) silently drops the file
   from the queue — no error, no corruption, but no feedback either, and no way to
   re-encode an mp4 onto itself.
2. **Silent skips.** That same guard (and the other `continue` points in `add_files_to_db`)
   drop files with zero user feedback whenever an output already exists, the file is already
   queued, etc.

## Scope decisions (settled during brainstorming)

- **Both** halves are in scope: the in-place re-encode path **and** per-reason skip feedback.
- **Trigger:** empty suffix implies in-place — but only the `.mp4`-source case actually needs
  new behavior. The forced `.mp4` stays; the suffix template never contains the extension. So
  the resolved output path equals the source path in exactly one situation: `.mp4` source +
  empty suffix. Everything non-mp4 already produces a distinct `.mp4` and flows through the
  normal path untouched. In-place is therefore **derived from `source_path == output_path`**,
  not a new mode.
- **Size guard:** if the in-place re-encode comes out larger than or equal to the original,
  keep the original and record it as the existing **"kept original"** outcome (status
  `skipped`, `kept_file = "original"`). `decide_cleanup`'s decision is reused verbatim; only the
  physical action changes.
- **Replace mode:** respect the existing `cleanup_mode` setting. In `trash` mode the original
  is moved to Trash before the re-encode takes its place (preserving recoverability,
  consistent with how distinct-file conversions treat the discarded file). In `delete` mode the
  re-encode atomically overwrites the original.
- **Skip feedback** is shown at add time, grouped by reason with counts, and is **never written
  to history** (skipped-at-add files get no DB row today, and that does not change).

## Part A — In-place re-encode

### A1. Detection (add time, `add_files_to_db`)

After building `output_path`, branch on equality instead of the blanket existence check:

- `output_path == source_path` → **queue an in-place job** (today's silently-skipped case).
- else if `output_path.exists()` → skip with reason `OutputExists`.

This only fires for `.mp4` + empty suffix; the forced `.mp4` makes every other case a distinct
file. The empty-suffix case also bypasses the `stem.ends_with(suffix)` fast-skip (that check is
gated on `!suffix.is_empty()`), which is expected and surfaced in the UI (Part C).

**No schema change.** `output_path` is stored equal to `source_path` for in-place jobs, and
"in-place" is *derived* from that equality everywhere it matters (converter + frontend badge).

> Alternative considered: an explicit `in_place` boolean column on `jobs`. Rejected — it adds a
> migration and a redundant flag when path equality already encodes the same fact.

### A2. Encode (`converter.rs`), when `source_path == output_path`

1. Derive a **same-directory** temp path with a recognizable marker, e.g.
   `.{stem}.convertbar-tmp.mp4`. Same directory ⇒ same filesystem ⇒ the final rename is atomic.
   The marker lets `scan_video_files` and the add-time video check skip it, so a folder scan or
   a **watched folder never enqueues an in-flight temp**. The `.mp4` extension is kept so
   HandBrake's container behavior is identical to the existing distinct-file path.
2. Remove any stale temp left by a previous crash, then run HandBrake with `-o <temp>`.
3. On success, compute `converted_size = stat(temp)` and `original_size = fresh stat(source)`,
   then `decide_cleanup(original_size, converted_size)` — **reused verbatim**:
   - `KeptFile::Converted` (converted smaller): respect `cleanup_mode` —
     - `trash`: `trash::delete(source)`, then `fs::rename(temp → source_path)`.
     - `delete`: `fs::rename(temp → source_path)` (atomic replace).
   - `KeptFile::Original` / `KeptFile::Neither` (≥ original, or zero output): `fs::remove_file(temp)`,
     leave the source untouched → recorded as the existing **"kept original"** outcome
     (status `skipped`, `kept_file = "original"`).
4. On failure or cancel: `fs::remove_file(temp)` only — **never** the source.

Non-in-place jobs are completely unchanged: HandBrake writes straight to the distinct `.mp4`,
and the existing cleanup runs.

### A3. Latent bug fixed

The failure path currently runs `std::fs::remove_file(&job.output_path)` (`converter.rs:524`).
For an in-place job `output_path == source_path`, so a failed in-place encode would delete the
user's original. Routing in-place output through a temp file and only ever deleting the temp on
failure removes this hazard.

### A4. Cross-platform note

`std::fs::rename` replaces an existing destination atomically on both Unix and Windows
(`MOVEFILE_REPLACE_EXISTING`). Keeping the temp in the same directory guarantees a same-filesystem
move. Flag this for the `cross-platform-reviewer` agent during implementation.

## Part B — Skip-reason feedback

### B1. Backend

The add core returns a struct instead of a bare vector:

```rust
struct AddResult { added: Vec<JobInfo>, skipped: Vec<SkipCount> }
struct SkipCount { reason: SkipReason, count: u32 }
enum SkipReason { NotVideo, AlreadyQueued, AlreadyConverted, OutputExists }
```

Reasons map to the existing `continue` points in `add_files_to_db`:

- `NotVideo` — `!is_video_file(path)`.
- `AlreadyQueued` — `existing_paths.contains(path_str)` from the queued/encoding/paused set.
- `AlreadyConverted` — `stem.ends_with(suffix)` **or** a match via the `skip_already_converted`
  history UNION.
- `OutputExists` — a *distinct* output already exists on disk.

The `output_path == source_path` branch no longer produces a skip; it becomes an in-place job.

`add_files` and `confirm_folder_add` return `AddResult`. `watcher.rs:212` uses `.added` and
ignores (or logs) `.skipped`.

> Alternative considered: emit a separate `files-skipped` event. Rejected — the add commands
> already return synchronously to `DropZone`, so a return field is simpler and avoids event
> plumbing/timing.

### B2. Frontend (`DropZone`)

`handlePaths` currently discards the returned jobs. Instead it aggregates `skipped` counts across
the file-add call and each folder-add call, then renders a per-reason summary in the existing
`status` line, e.g.:

```
Added 4 · 2 skipped (output exists) · 1 skipped (already converted)
```

Auto-clear after a few seconds, matching the existing error-status behavior. Nothing is written
to history.

## Part C — UI indications

- **`QueueItem`**: derive `isInPlace = job.source_path === job.output_path` and render an
  "In place" badge alongside the existing "Queued" badge.
- **`SettingsPage`** (when the output suffix template is empty): an info note under the field
  covering both consequences:
  > "Empty suffix: mp4 files are re-encoded in place, replacing the original. The fast
  > 'already converted' skip-by-suffix is also disabled."

## Testing

### Rust (`src-tauri`)

- `add_files_to_db`:
  - mp4 + empty suffix where output == source now **queues an in-place job** (regression of the
    old silent skip).
  - a *distinct* pre-existing output still skips with `OutputExists`.
  - per-reason counts are returned for `NotVideo`, `AlreadyQueued`, `AlreadyConverted`,
    `OutputExists`.
- In-place decision→action mapping (the new logic): table-test that
  - converted smaller + `delete` → temp renamed over source;
  - converted smaller + `trash` → source trashed, temp renamed into place;
  - converted ≥ original → temp removed, source kept, outcome "kept original";
  - failure/cancel → temp removed, **source still present**.
- `decide_cleanup` itself is unchanged; its existing matrix test stays green.

### Frontend (`src`)

- `DropZone` renders the aggregated per-reason skip summary.
- `QueueItem` shows the "In place" badge when `source_path === output_path`.
- `SettingsPage` shows the empty-suffix info note.

## Out of scope

- Keeping non-mp4 source extensions (mkv/webm stay-in-container). The forced `.mp4` is retained;
  only mp4 sources get a true in-place path.
- Any change to `decide_cleanup`'s decision logic.
- An explicit "in-place" settings toggle — empty suffix is the trigger.
