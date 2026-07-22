# Serialized folder intake + clean drop zone — Design

**Date:** 2026-07-22
**Status:** Approved design, ready for implementation plan
**Branch:** `feature/serialized-folder-intake`

## Problem

Dropping several folders in quick succession (repro: the three `2026 SEOA*` folders)
misbehaves. Each drop invokes `DropZone.handlePaths` (`src/components/DropZone.tsx`) with
**no concurrency guard**, so:

1. **Multiple scan/probe processes run at once.** Each drop fires its own
   `classify_paths` walk and, on confirm, its own `confirm_folder_add` (recursive
   `scan_video_files` + per-file HandBrake probe). Several heavy probes race in parallel.
2. **The UI goes inconsistent.** `setPending` **replaces** the whole pending array
   (`DropZone.tsx:19`), so a second folder's confirm list clobbers the first — the first
   folder silently vanishes, never added and never skipped. The shared `status` string
   races between invocations. The single global `AddingIndicator` (`useAddProgress` keeps
   only the *most recent* op) flips between op ids, so its "Checking X of N" jumps around
   with no indication of which folder it belongs to.
3. **The confirm prompt lives inside the drop zone.** The folder-name + Add/Skip buttons
   render *inside* `.drop-zone` (`DropZone.tsx:82-122`), replacing the "Drop files here"
   label, so the drop target is visually consumed by the prompt.

## Goals

- **Serialize the heavy intake work** so only one folder is scanned/probed at a time; a
  new drop **appends** to that pipeline and never interrupts, cancels, or loses the folder
  currently being scanned.
- **Keep the drop zone clean and always droppable** — it never morphs into the folder
  label + buttons.
- **Move confirmation to its own card below the drop zone**, showing one folder at a time,
  and **clear it the instant the user clicks Add/Skip** (decoupled from the scan, which
  continues in the background).
- **Name the scanner** — the progress card shows the folder currently being scanned:
  `"<folder>" · Checking X of N`.

## Non-goals

- No change to the skip-rule engine (`add_files_inner` / `add_files_to_db`), the job queue
  schema, or the ≤5-file auto-add threshold.
- No change to conversion/encode progress (`ActiveJob`).
- No cancellation of an in-flight scan, and no undo of a folder already handed to the
  scanner. "Replace on second drop" from the original ask is **superseded** by the queue
  model — nothing is discarded; new folders line up instead.
- No per-folder *concurrent* scanner stack. One scanner card, one serialized pipeline
  (the explicitly rejected alternative — it reintroduces the concurrency we're removing).

## Approach: a per-drop count feeding one serialized scanner, three UI regions

The Queue page's intake area becomes three stacked regions, top to bottom. None of them is
the drop zone morphing:

```
┌──────────────────────────────────────────────┐
│ DROP ZONE — "Drop video files or folders here" │  always clean, always accepts a drop
├──────────────────────────────────────────────┤
│ CONFIRM CARD — "SEOA 2" · 40 files [Add][Skip] │  only while a big folder awaits confirm;
│                                                │  cleared immediately on Add/Skip
├──────────────────────────────────────────────┤
│ SCANNER — "SEOA" · Checking 12 of 87…          │  persists until THAT folder's scan ends
└──────────────────────────────────────────────┘
```

Two concerns are separated so confirms stay responsive while probing is slow — counting is
lightweight and runs per drop; only the heavy scan/probe is serialized:

- **Counting (lightweight, per drop — not serialized).** `classify_paths` is a metadata-only recursive walk
  that yields loose files + folders-with-counts. It runs per drop to build the confirm card
  promptly, *without* waiting behind the heavy pipeline. Its results are dispatched to:
  - loose files → an auto-add task on the scanner pipeline (generic label),
  - folders with ≤5 files (and >0) → an auto-add task (folder-name label),
  - folders with >5 files → the **confirm queue**.
- **Scanning/probing (heavy, serialized — the scanner).** A single-lane task pipeline
  processes one add task at a time (`add_files` for loose files, `confirm_folder_add` for a
  folder). Only one `add-*` op is ever open from the drop flow, so the named scanner never
  jumps between folders. Confirmed folders and auto-add tasks **append** here; a running
  task is never interrupted by a later drop → **in-flight items are never lost**.

### Confirm queue (one at a time)

At most one `pendingConfirm` is shown. Additional >5-file folders — whether they arrive in
one multi-folder drag or across several drops — sit in a `confirmQueue`. On **Add**: the
folder's `add-folder` task is pushed to the scanner pipeline and `pendingConfirm` advances
to the next queued folder (or clears). On **Skip**: `pendingConfirm` simply advances. Either
way the card clears synchronously on click — it never waits for the scan.

This is the direct fix for the "replace clobbers the first folder" bug: the queue is a ref
so concurrent `classify` results append safely (functional/ref updates, never a wholesale
replace), and nothing is dropped on the floor.

## Frontend

### `DropZone.tsx` — owns intake orchestration + the confirm card

State (queue-like state held in refs so overlapping async handlers read the latest value,
mirroring the existing `pendingRef` pattern at `DropZone.tsx:18`):

- `confirmQueueRef: FolderScanResult[]` + `pendingConfirm` state — the confirm slot.
- `taskQueueRef: AddTask[]` + `runningRef: boolean` — the serialized scanner pipeline.
  `AddTask = { kind: "files"; paths: string[] } | { kind: "folder"; folder: FolderScanResult }`.
- `summary` state — the transient "Added N · M skipped" line, rendered **below** the drop
  box (never inside it), auto-cleared after 4s as today.

Flow:

- `onDragDropEvent` "drop" → `classify_paths(paths)` (per drop, not serialized). On result:
  enqueue a `files` task if any loose files; for each folder, drop 0-count, enqueue a
  `folder` task for ≤5, else push to `confirmQueueRef` and promote `pendingConfirm` if empty.
- `runNext()` drains `taskQueueRef` one task at a time: guard on `runningRef`; pop; `await`
  the invoke (`add_files` or `confirm_folder_add`); on settle, `start_queue()`,
  `onFilesAdded()`, set `summary`, then `runNext()` again. Because each task is awaited before
  the next starts, at most one heavy op is in flight.
- **Add** handler: push a `folder` task, advance `pendingConfirm` from `confirmQueueRef`,
  kick `runNext()`. **Skip** handler: advance `pendingConfirm`. No `startQueue` coupling to
  the card — the queue is kicked when the scanner task completes.

Render: the `.drop-zone` box contains **only** the label / drag-over state (no status, no
buttons). The confirm card and summary line are **siblings below** the box.

### Backend event label → named scanner

`AddOp` gains a label so the scanner can show which folder is being processed. This keeps
the established "App owns add-progress; events carry the data" convention (see the
2026-07-17 spec) rather than having `DropZone` guess the label of the current op — robust
even if a watcher op overlaps.

- `add-started` payload gains `label: String`.
- `AddStarted` (`lib/tauri.ts`) and `AddActivity` gain `label: string`.
- `useAddProgress` carries `label` on `activity` (set from `add-started`; kept on
  `add-progress`).
- `AddingIndicator` renders `"<label>" · Checking X of N` (and `"<label>" · Scanning…`
  before the first tick); falls back to the current "Checking X of N" / "Scanning…" when
  `label` is empty.

Label sources (see Backend section): folder basename for `confirm_folder_add`, empty for
`add_files` (loose files → fallback text), folder/dir basename for the watcher.

## Backend

`src-tauri/src/add_progress.rs`:

- `AddOp::new(app: &AppHandle<R>, label: String) -> AddOp<R>` — `StartedPayload` gains
  `label`, emitted on `add-started`. `report` / `Drop` unchanged.

Call sites (each already constructs an `AddOp` — the only change is passing a label):

- `add_files` (`commands/queue.rs:543`) — `AddOp::new(&app, String::new())`. Loose files
  have no single folder name; the frontend renders "Adding files…".
- `confirm_folder_add` (`commands/queue.rs:589`) — `AddOp::new(&app, <basename of path>)`
  via `Path::new(&path).file_name()`. No new command argument.
- `enqueue_and_start` (`watcher.rs:369`) — pass the scanned folder's basename when readily
  available (parent of the batch), else `String::new()`. Names the watcher's scanner for
  free; empty is an acceptable fallback.

`add_files_inner`, `add_files_to_db`, `resolve_media`, the skip engine, and `classify_paths`
are **untouched**.

## Edge cases

- **New drop while a folder is scanning** → the new folder's `classify` count runs and its
  confirm/auto-add task is appended to the pipeline; the running scan finishes first. The
  in-flight folder is never interrupted or lost.
- **Multiple >5 folders in one drag** → confirmed one at a time via `confirmQueue`; none
  discarded.
- **Add clicked, then another folder dropped** → the confirm card already advanced/cleared on
  the click; the newly dropped folder gets its own confirm without colliding with the scan.
- **Loose files + a big folder in one drop** → loose files auto-add (scanner shows "Adding
  files…"); the big folder shows a confirm card. (Preserves the `DropZone.test.tsx:117`
  regression behavior.)
- **`skip_by_source_media` off** → no probe loop → each task is near-instant → the scanner
  flashes briefly per folder. Honest and acceptable (as in the 2026-07-17 spec).
- **Error in a task** → `AddOp::Drop` still emits `add-finished`; `runNext` continues to the
  next task; the error surfaces in the summary line. One bad folder doesn't stall the queue.
- **Watcher op overlapping a drop op** → both carry labels; `activity` shows the most recent
  (current behavior), now named. Rare; acceptable.

## Testing

- **Frontend (`DropZone.test.tsx`):**
  - Rewrite the simultaneous-multi-confirm tests (the "two Adds resolve out of order" N5
    test at `:208` and any assumption of two Add buttons at once) for **one-at-a-time**
    confirmation: dropping two big folders shows one confirm; Add/Skip advances to the next.
  - **New:** overlapping drops run the heavy adds **sequentially** — with a deferred
    `confirm_folder_add`/`add_files`, assert the second invoke does not fire until the first
    resolves (proves serialization; guards the regression).
  - **New:** the confirm card clears synchronously on Add (before the deferred
    `confirm_folder_add` resolves) — proves the card is decoupled from the scan.
  - Keep: auto-add of loose files + ≤5 folders, the big-folder-with-loose-files prompt, the
    skip-summary line — updated for the below-the-box layout.
- **Frontend (`AddingIndicator.test.tsx` / `useAddProgress.test.ts`):** the label rides
  through `add-started` → `activity.label` → rendered as `"<label>" · Checking X of N`, with
  the empty-label fallback preserved.
- **Rust (`add_progress.rs`, `MockRuntime`):** `add-started` carries the given `label`;
  existing started/finished/report tests updated for the new `AddOp::new` signature.

## Files touched (estimate)

- `src-tauri/src/add_progress.rs` — `label` on `AddOp::new` + `StartedPayload`; tests.
- `src-tauri/src/commands/queue.rs` — pass label at the `add_files` / `confirm_folder_add`
  `AddOp::new` sites (folder basename for confirm).
- `src-tauri/src/watcher.rs` — pass a label (folder basename / empty) at `enqueue_and_start`.
- `src/lib/tauri.ts` — `label` on `AddStarted` and `AddActivity`.
- `src/hooks/useAddProgress.ts` — carry `label` on `activity`.
- `src/components/AddingIndicator.tsx` — render the label prefix.
- `src/components/DropZone.tsx` — serialized task pipeline + confirm queue; confirm card and
  summary rendered **below** the (clean) drop box.
- `src/App.css` — confirm-card styling as a sibling block (reuse existing tokens); drop-zone
  stays label-only.
- Tests alongside the above.

No new Tauri plugin or ACL permission: events are app-emitted and the frontend only adds
`listen`/`invoke` of existing app commands. App commands stay ACL-exempt.
