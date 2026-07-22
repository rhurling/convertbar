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
   (defined `DropZone.tsx:19`, called with a fresh `toConfirm` at `:46`), so a second
   folder's confirm list clobbers the first — the first folder silently vanishes, never
   added and never skipped. The shared `status` string
   races between invocations. The single global `AddingIndicator` (`useAddProgress` keeps
   only the *most recent* op) flips between op ids, so its "Checking X of N" jumps around
   with no indication of which folder it belongs to.
3. **Confirm is coupled to the scan.** The Add handler `await`s the entire
   `confirm_folder_add` (recursive walk + per-file probe) *before* removing the confirmed
   row (`DropZone.tsx:90-103`), so the folder-name + Add/Skip prompt lingers inside
   `.drop-zone` for the whole scan instead of reverting to the droppable label immediately.

## Goals

- **Serialize the heavy drop-initiated intake work** so only one drop-triggered folder is
  scanned/probed at a time; a new drop **appends** to that pipeline and never interrupts,
  cancels, or loses the folder currently being scanned. (Scope: the drop flow. An independent
  watcher scan can still run concurrently — see edge cases — but the drop flow no longer races
  itself.)
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

- **Counting (per drop — not serialized).** `classify_paths` is a metadata-only recursive
  walk (still a full `scan_video_files` tree walk, so not free on deep/network trees). It runs
  per drop to build the confirm card promptly, *without* waiting behind the heavy pipeline. Its
  results are dispatched to:
  - loose files → an `add_files` task on the scanner pipeline,
  - folders with ≤5 files (and >0) → a `confirm_folder_add` task,
  - folders with >5 files → the **confirm queue**.
  (The scanner's label comes from the backend `add-*` events, not the frontend task — see the
  label subsection — so `AddTask` carries no label field.)
- **Scanning/probing (heavy, serialized — the scanner).** A single-lane task pipeline
  processes one add task at a time (`add_files` for loose files, `confirm_folder_add` for a
  folder). Because the drop flow awaits each op before starting the next, at most one
  drop-initiated `add-*` op is open, so the named scanner doesn't jump between the drop flow's
  folders. (A concurrent watcher op can still interleave — rare; see edge cases.) Confirmed
  folders and auto-add tasks **append** here; a running task is never interrupted by a later
  drop → **in-flight items are never lost within a session** (the frontend task queue is
  in-memory; an app quit mid-pipeline loses only not-yet-started queued tasks).

### Confirm queue (one at a time)

`confirmQueueRef` (a ref) is the **single source of truth** for what awaits confirmation; the
displayed prompt is always its head, `confirmQueueRef.current[0]`. A render-trigger state
(a nonce, or a mirrored `pendingConfirm = head`) is updated only *after* the ref mutates, so
what renders is derived from the ref, never set independently.

This is essential, not cosmetic: if `pendingConfirm` were its own React state promoted with
"set it if currently empty," two drops whose `classify_paths` resolve in adjacent microtasks
would both read the stale render-closure `pendingConfirm === null` and the second
`setPendingConfirm` would overwrite the first — silently dropping a folder, i.e. the exact
`DropZone.tsx:46` clobber in new clothes. Head-of-ref avoids it: each `classify` result
**pushes** to the ref (never replaces), and only synchronous ref reads decide the head.

- **Add**: push the head's `confirm_folder_add` task to the scanner pipeline, `shift` the ref,
  bump the render trigger. **Skip**: `shift` the ref, bump. Either way the card advances to the
  next queued folder (or clears) **synchronously on click** — it never waits for the scan.

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

- `confirmQueueRef: FolderScanResult[]` — source of truth for confirmations; the rendered
  prompt is its head (see the confirm-queue section). A render-trigger nonce reflects it.
- `taskQueueRef: AddTask[]` + `runningRef: boolean` — the serialized scanner pipeline.
  `AddTask = { kind: "files"; paths: string[] } | { kind: "folder"; folder: FolderScanResult }`.
- `status` state + `statusTimerRef` — the transient "Adding…" / "Added N · M skipped" /
  "Error: …" line, shown inside the drop zone (`.drop-zone-status`). The timer ref is cleared
  and re-armed on every `status` set, so a later task's summary can't be wiped by an earlier
  task's stale 4s timer (a bug the current `DropZone.tsx:53` would show once tasks run
  sequentially).
- `isDragOver` state — for the Queue tab's drag-over highlight.

The single window-level `onDragDropEvent` is registered **once** — the effect uses stable
refs (empty dep array), not `handlePaths` in its deps as `DropZone.tsx:79` does today — so it
never re-registers, and it survives React StrictMode's double-mount via the existing
promise-unlisten cleanup shape.

Flow:

- `onDragDropEvent` "drop" → `switchToQueue()`, set `status` to an immediate "Adding…" (so a
  slow `classify_paths` walk isn't dead air), then `await classify_paths(paths)` inside a
  try/catch (an error sets `status: "Error: …"`, matching `DropZone.tsx:55-58` today). On
  result: enqueue a `files` task if any loose files; for each folder, drop 0-count, enqueue a
  `folder` task for ≤5, else **push to `confirmQueueRef`** and bump the render trigger.
  ("over"/"enter"/"leave" only toggle `isDragOver`; they never switch tabs — only an actual
  drop does.)
- `runNext()` drains `taskQueueRef` one task at a time: if `runningRef` set, return; else set
  it; pop; `await` the invoke (`add_files` or `confirm_folder_add`); on settle,
  `start_queue()`, set `status`; then **clear `runningRef` and immediately re-check the queue
  in the same synchronous tick** (no await between clear and re-check) and `runNext()` again.
  Because each task is awaited before the next starts, at most one drop-initiated heavy op is
  in flight.
- **Add** handler: push the head's `folder` task, `shift` `confirmQueueRef`, bump the render
  trigger, kick `runNext()`. **Skip** handler: `shift` `confirmQueueRef`, bump. No `startQueue`
  coupling to the card — the queue is kicked by each scanner task on completion, so **Skip no
  longer calls `start_queue`** (behavior change from `DropZone.tsx:115`; harmless because
  any auto-add/confirmed task kicks the queue itself).

The hook returns `{ pendingConfirm, onAdd, onSkip, status, isDragOver }` (where
`pendingConfirm` is the derived head); `App` threads them
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
2026-07-17 spec) rather than having the frontend hook guess the label of the current op —
robust even if a watcher op overlaps.

- **Both** `add-started` **and** `add-progress` payloads gain `label: String`. Carrying it on
  `add-progress` too is deliberate: `useAddProgress.ts:38` replaces `activity` wholesale on
  each progress tick, so if the label rode only on `add-started` an interleaved watcher op
  would strip it. With it on both events, the rendered label always matches the op whose
  progress is shown.
- `AddStarted`, `AddProgress` (`lib/tauri.ts`) and `AddActivity` gain `label: string`.
- `useAddProgress` sets `activity.label` from whichever event it receives.
- `AddingIndicator` renders `"<label>" · Checking X of N` (and `"<label>" · Scanning…` before
  the first tick) when `label` is non-empty; when empty it renders the current unprefixed
  "Checking X of N" / "Scanning…" (`AddingIndicator.tsx:20`) — **no new copy**.

Label sources (see Backend section): folder basename for `confirm_folder_add`, **empty** for
`add_files` (loose files render the unprefixed text), folder basename for the watcher when its
batch is a single directory, else empty.

## Backend

`src-tauri/src/add_progress.rs`:

- `AddOp::new(app: &AppHandle<R>, label: String) -> AddOp<R>` — the op stores `label`;
  `StartedPayload` gains `label` (emitted on `add-started`). `ProgressPayload` also gains
  `label`, and `report` includes `self.label` on every `add-progress` (so the label survives
  `useAddProgress`'s wholesale `activity` replacement — see the label subsection). `Drop`
  unchanged.

Call sites (each already constructs an `AddOp` — the only change is passing a label):

- `add_files` (`commands/queue.rs:543`) — `AddOp::new(&app, String::new())`. Loose files have
  no single folder name; the frontend renders the unprefixed text.
- `confirm_folder_add` (`commands/queue.rs:589`) — `AddOp::new(&app, <basename of path>)` via
  `Path::new(&path).file_name()` (native `\`/`/` handling, precedent at
  `scan_folder_inner`, `queue.rs:567-571`). No new command argument.
- `enqueue_and_start` (`watcher.rs:369`) — pass the batch's single-directory basename when
  there is one (the `scan_existing` path, `watcher.rs:472-476`), else `String::new()`. Reaper
  batches (`watcher.rs:306`) can span multiple dirs under a recursive watch, so empty is the
  common case there — an acceptable fallback; naming the watcher scanner is a bonus, not a goal.

`add_files` and `confirm_folder_add` also `emit("queue-updated", ())` on the **Ok** path after
the add completes (inside/after the `spawn_blocking` await, since these are async commands),
mirroring `enqueue_and_start` (`watcher.rs:386`). Unconditional-on-Ok is fine — a redundant
refresh with zero added rows is a harmless no-op, deduped by `useQueue.ts:11-17`'s request-id
guard. This lets `useQueue` refresh reactively; the frontend `onFilesAdded` callback is dropped.

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
- **Loose files + a big folder in one drop** → loose files auto-add (scanner shows the
  unprefixed text); the big folder shows a confirm card. (Preserves the `DropZone.test.tsx:117`
  regression behavior.)
- **Skip the (only) pending folder** → the card clears; the queue is *not* explicitly kicked
  by Skip (behavior change from `DropZone.tsx:115`). If anything else in the drop auto-added,
  that task already called `start_queue`; if nothing did, there's nothing to start. Net
  outcome unchanged.
- **`skip_by_source_media` off** → no probe loop → each task is near-instant → the scanner
  flashes briefly per folder. Honest and acceptable (as in the 2026-07-17 spec).
- **Error in a task** → `AddOp::Drop` still emits `add-finished`; `runNext` continues to the
  next task; the error surfaces in the `status` line. One bad folder doesn't stall the queue.
- **Watcher op overlapping a drop op** → both ops carry their own label on every event, so the
  scanner shows a *consistent* label+progress pair for whichever op ticked last; it can flip
  between the drop folder and the watcher folder. Rare (needs a watched-folder scan concurrent
  with a manual drop) and acceptable — the label is never mismatched to the wrong progress.
- **Drop while on another tab** → the `App`-level listener catches it, switches to the Queue
  tab, and processes it; the confirm prompt (if any) appears on the now-active Queue tab.
- **Dragging over a non-Queue tab** → no drag-over highlight (the `.drop-zone` isn't mounted
  there); the drop still lands via the window-level listener and switches to Queue. Accepted:
  only the actual drop switches tabs, not hover, to avoid yanking the user off a tab mid-drag.
- **Switch tabs mid-confirm / mid-scan** → confirm queue and scanner state live in the
  `App`-owned hook, so nothing is lost; returning to Queue shows the current state.

## Testing

The intake behavior moves to the hook, so the behavioral tests move with it; `DropZone` tests
become presentational. (The `dragBus` webviewWindow mock at `DropZone.test.tsx:8-21` moves to
the hook test.)

- **Frontend (`useFileIntake` test — new, owns the drop/invoke mocks):**
  - **One-at-a-time confirm:** dropping two big folders shows one confirm at a time; Add/Skip
    advances to the next (replaces the deleted simultaneous-multi-confirm N5 test at
    `DropZone.test.tsx:208`).
  - **Serialization (the key invariant):** with a deferred `confirm_folder_add`/`add_files`,
    the second heavy invoke does not fire until the first resolves.
  - **Promotion race:** two drops whose `classify_paths` resolve back-to-back both surface
    their folders (neither is dropped) — locks the head-of-`confirmQueueRef` fix.
  - **Synchronous card clear:** the confirm advances on Add *before* the deferred
    `confirm_folder_add` resolves — proves the card is decoupled from the scan.
  - **Cross-tab drop:** a "drop" calls the switch callback (`onDrop`) and still processes the
    paths. (State surviving a tab switch is *architectural* — the hook lives in always-mounted
    `App`, not `QueuePage` — so it's guaranteed by construction rather than unit-tested; the
    manual smoke covers it.)
  - Carry over: auto-add of loose files + ≤5 folders, the big-folder-with-loose-files prompt
    (`:117`), the skip-summary line.
  - **Not unit-tested (by construction):** the status auto-clear race — the fix is a single
    timer ref cleared-and-rearmed on every `status` set, so a stale timer cannot exist; a
    fake-timer test would be brittle for no added assurance.
- **Frontend (`DropZone.test.tsx` — now presentational):** given props it renders the confirm
  prompt / status / label three-way switch, and Add/Skip call the passed handlers. No invoke
  or drag mocks.
- **Frontend (`AddingIndicator.test.tsx` / `useAddProgress.test.ts`):** the label rides
  through **both** `add-started` and `add-progress` → `activity.label` → rendered as
  `"<label>" · Checking X of N`; the empty-label unprefixed fallback is preserved; a progress
  tick keeps the label (not stripped).
- **Rust (`add_progress.rs`, `MockRuntime`):** `add-started` **and** `add-progress` carry the
  given `label`; existing started/finished/report tests updated for the new `AddOp::new`
  signature.
- **`queue-updated` emit — not unit-tested:** the async `#[tauri::command]` wrappers need a
  fully-managed `AppState` + async runtime to exercise, which the codebase deliberately avoids
  (only sync `_inner` fns are unit-tested); the emit is a one-line mirror of the untested
  watcher emit (`watcher.rs:386`) and is verified by the frontend integration (queue refreshes
  without `onFilesAdded`) + manual smoke.

## Files touched (estimate)

- `src-tauri/src/add_progress.rs` — `label` on `AddOp::new`, `StartedPayload`, **and**
  `ProgressPayload` (`report` emits it); tests.
- `src-tauri/src/commands/queue.rs` — pass label at the `add_files` / `confirm_folder_add`
  `AddOp::new` sites (folder basename for confirm); emit `queue-updated` on the Ok path of each
  add so `useQueue` refreshes reactively (removes the `onFilesAdded` wiring).
- `src-tauri/src/watcher.rs` — pass a label (single-dir basename / empty) at `enqueue_and_start`.
- `src/lib/tauri.ts` — `label` on `AddStarted`, `AddProgress`, and `AddActivity`.
- `src/hooks/useAddProgress.ts` — set `label` on `activity` from both `add-started` and
  `add-progress`.
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
