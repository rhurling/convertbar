# Serialized folder intake + cross-tab drops — Design

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
3. **Confirm is coupled to the scan.** The Add handler `await`s the entire
   `confirm_folder_add` (recursive walk + per-file probe) *before* removing the confirmed
   row (`DropZone.tsx:90-103`), so the folder-name + Add/Skip prompt lingers inside
   `.drop-zone` for the whole scan instead of reverting to the droppable label immediately.

## Goals

- **Serialize the heavy intake work** so only one folder is scanned/probed at a time; a
  new drop **appends** to that pipeline and never interrupts, cancels, or loses the folder
  currently being scanned.
- **Decouple confirm from the scan.** The confirm prompt stays where it is (inside the drop
  zone — the whole window stays droppable via the window-level `onDragDropEvent`, so its
  placement costs no droppability), showing **one folder at a time**, and reverts to the
  "Drop files here" label **the instant the user clicks Add/Skip** — never lingering through
  the scan, which continues in the background scanner.
- **Name the scanner** — the progress card shows the folder currently being scanned:
  `"<folder>" · Checking X of N`.
- **Accept drops on any tab** — a folder dropped while on History/Watch/Settings
  auto-switches to the Queue tab and is processed there, instead of being ignored.

## Non-goals

- No change to the skip-rule engine (`add_files_inner` / `add_files_to_db`), the job queue
  schema, or the ≤5-file auto-add threshold.
- No change to conversion/encode progress (`ActiveJob`).
- No cancellation of an in-flight scan, and no undo of a folder already handed to the
  scanner. "Replace on second drop" from the original ask is **superseded** by the queue
  model — nothing is discarded; new folders line up instead.
- No per-folder *concurrent* scanner stack. One scanner card, one serialized pipeline
  (the explicitly rejected alternative — it reintroduces the concurrency we're removing).

## Approach: a per-drop count feeding one serialized scanner, two UI regions

The Queue page's intake area is two stacked regions. The drop zone doubles as the confirm
surface (kept in place to minimize churn); the whole window stays droppable throughout:

```
┌──────────────────────────────────────────────┐
│ DROP ZONE — "Drop video files or folders here" │  morphs to "SEOA 2" · 40 files [Add][Skip]
│                                                │  while a big folder awaits confirm;
│                                                │  reverts to the label on Add/Skip click
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

### Intake ownership moves to App (`useFileIntake` hook)

The drop listener and the whole intake pipeline move out of `DropZone` into a new
`useFileIntake` hook mounted in **always-mounted `App`** (the same convention as
`useAddProgress`). Two payoffs:

1. **Drops work on any tab.** `DropZone` only mounts on the Queue tab, so today its listener
   is gone on History/Watch/Settings. A hook in `App` keeps one persistent window-level
   `onDragDropEvent`; a "drop" auto-switches to the Queue tab (`setActiveTab("queue")`, passed
   into the hook as a callback) and then processes the paths.
2. **State survives tab switches.** The confirm queue and in-flight scanner state live in the
   hook, so switching away and back mid-confirm/mid-scan no longer loses them (today a
   `DropZone` unmount would).

Hook state (queue-like state held in refs so overlapping async handlers read the latest
value, mirroring the existing `pendingRef` pattern at `DropZone.tsx:18`):

- `confirmQueueRef: FolderScanResult[]` + `pendingConfirm` state — the confirm slot.
- `taskQueueRef: AddTask[]` + `runningRef: boolean` — the serialized scanner pipeline.
  `AddTask = { kind: "files"; paths: string[] } | { kind: "folder"; folder: FolderScanResult }`.
- `status` state — the transient "Added N · M skipped" line, shown inside the drop zone as
  today (`.drop-zone-status`), auto-cleared after 4s.
- `isDragOver` state — for the Queue tab's drag-over highlight.

Flow:

- `onDragDropEvent` "drop" → `switchToQueue()`, then `classify_paths(paths)` (per drop, not
  serialized). On result: enqueue a `files` task if any loose files; for each folder, drop
  0-count, enqueue a `folder` task for ≤5, else push to `confirmQueueRef` and promote
  `pendingConfirm` if empty. ("over"/"enter"/"leave" only toggle `isDragOver`; they never
  switch tabs — only an actual drop does.)
- `runNext()` drains `taskQueueRef` one task at a time: guard on `runningRef`; pop; `await`
  the invoke (`add_files` or `confirm_folder_add`); on settle, `start_queue()`, set `status`,
  then `runNext()` again. Because each task is awaited before the next starts, at most one
  heavy op is in flight.
- **Add** handler: push a `folder` task, advance `pendingConfirm` from `confirmQueueRef`,
  kick `runNext()`. **Skip** handler: advance `pendingConfirm`. No `startQueue` coupling to
  the card — the queue is kicked when the scanner task completes.

The hook returns `{ pendingConfirm, onAdd, onSkip, status, isDragOver }`; `App` threads them
through `QueuePage` to `DropZone`. **Queue refresh needs no cross-component wiring:**
`add_files` and `confirm_folder_add` emit `queue-updated` on completion (mirroring the
watcher's `enqueue_and_start`, `watcher.rs:386`), which `useQueue` already listens for
(`useQueue.ts:40`) and refreshes reactively. The current `DropZone(onFilesAdded)` →
`useQueue.refresh` prop wiring is removed.

### `DropZone.tsx` — presentational

`DropZone` becomes a pure presentational component: it takes `pendingConfirm`, `onAdd`,
`onSkip`, `status`, `isDragOver` as props and renders the `.drop-zone` box with its current
three-way switch — confirm prompt / transient status / label. The only markup change is
rendering a **single** `pendingConfirm` (one Add/Skip pair) instead of mapping an array; the
Add/Skip buttons just call the prop handlers. No `onDragDropEvent`, no `invoke`, no intake
state of its own.

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

`add_files` and `confirm_folder_add` also `emit("queue-updated", ())` after the add completes
(mirroring `enqueue_and_start`, `watcher.rs:386`), so `useQueue` refreshes reactively and the
frontend `onFilesAdded` callback is dropped.

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
  next task; the error surfaces in the `status` line. One bad folder doesn't stall the queue.
- **Watcher op overlapping a drop op** → both carry labels; `activity` shows the most recent
  (current behavior), now named. Rare; acceptable.
- **Drop while on another tab** → the `App`-level listener catches it, switches to the Queue
  tab, and processes it; the confirm prompt (if any) appears on the now-active Queue tab.
- **Switch tabs mid-confirm / mid-scan** → confirm queue and scanner state live in the
  `App`-owned hook, so nothing is lost; returning to Queue shows the current state.

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
    skip-summary line — confirm prompt stays inside the drop zone, so these assertions hold
    with only the single-vs-array markup change. (These now render `DropZone` via the hook,
    or drive the hook directly.)
- **Frontend (`useFileIntake` / `App`):** a "drop" fired while `activeTab !== "queue"` calls
  the switch callback (activeTab becomes "queue") and still processes the paths; confirm/scan
  state is retained across a simulated tab switch.
- **Frontend (`AddingIndicator.test.tsx` / `useAddProgress.test.ts`):** the label rides
  through `add-started` → `activity.label` → rendered as `"<label>" · Checking X of N`, with
  the empty-label fallback preserved.
- **Rust (`add_progress.rs`, `MockRuntime`):** `add-started` carries the given `label`;
  existing started/finished/report tests updated for the new `AddOp::new` signature.

## Files touched (estimate)

- `src-tauri/src/add_progress.rs` — `label` on `AddOp::new` + `StartedPayload`; tests.
- `src-tauri/src/commands/queue.rs` — pass label at the `add_files` / `confirm_folder_add`
  `AddOp::new` sites (folder basename for confirm); emit `queue-updated` after each add so
  `useQueue` refreshes reactively (removes the `onFilesAdded` wiring).
- `src-tauri/src/watcher.rs` — pass a label (folder basename / empty) at `enqueue_and_start`.
- `src/lib/tauri.ts` — `label` on `AddStarted` and `AddActivity`.
- `src/hooks/useAddProgress.ts` — carry `label` on `activity`.
- `src/components/AddingIndicator.tsx` — render the label prefix.
- `src/hooks/useFileIntake.ts` — **new.** Window-level drop listener; per-drop `classify`;
  confirm queue; serialized `add_files` / `confirm_folder_add` pipeline; `status`,
  `isDragOver`, `pendingConfirm`; takes a `switchToQueue` callback.
- `src/App.tsx` — mount `useFileIntake`, pass `switchToQueue = () => setActiveTab("queue")`,
  thread `{ pendingConfirm, onAdd, onSkip, status, isDragOver }` to `QueuePage`.
- `src/pages/QueuePage.tsx` — receive the intake props and pass them to `DropZone`; drop the
  `onFilesAdded` wiring (refresh now rides the `queue-updated` event `useQueue` handles).
- `src/components/DropZone.tsx` — becomes presentational: props-driven
  (`pendingConfirm`/`onAdd`/`onSkip`/`status`/`isDragOver`), single `pendingConfirm` markup,
  no listener/invoke/state of its own.
- `src/App.css` — no layout change expected (existing `.drop-zone` / `.folder-confirm` /
  `.adding-indicator` styles reused as-is); tweak only if the scanner label prefix needs it.
- Tests alongside the above (`useFileIntake` gains its own test for the serialization,
  confirm-queue, and cross-tab-drop behavior; `DropZone` tests become prop-driven).

No new Tauri plugin or ACL permission: events are app-emitted and the frontend only adds
`listen`/`invoke` of existing app commands. App commands stay ACL-exempt.
