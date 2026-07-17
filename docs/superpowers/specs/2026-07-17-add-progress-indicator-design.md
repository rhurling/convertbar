# File-intake progress indicator — Design

**Date:** 2026-07-17
**Status:** Approved design, ready for implementation plan
**Branch:** `feature/add-progress-indicator`

## Problem

When files are added to the queue, the app runs duplicate checks and a per-file
HandBrakeCLI probe (the "format check", gated on the `skip_by_source_media` setting) plus
a recursive folder walk. Today the only feedback is a static `"Adding files..."` string in
the drop zone (`DropZone.tsx`), which looks frozen during a large batch and gives no sense
of scale or progress. There is **no animation anywhere** in the app (no spin keyframes in
`App.css`).

Two add paths run the same slow core (`add_files_inner`, `src-tauri/src/commands/queue.rs`)
but have different visibility:

- **Manual drag-drop** — has a visible surface (the drop zone on the Queue page).
- **Watched-folder scan-existing** (`scan_existing_background`, `src-tauri/src/watcher.rs`) —
  runs on a background thread when a watched folder is added/enabled. The user is typically
  on the **Watch** page then, with no drop-zone surface to show anything.

## Goals

- Show **determinate** `Checking X of N` progress during the per-file probe ("format check").
- Cover **both** drag-drop and **all** watched-folder add paths (startup scan, add/enable
  scan, and reaper stabilized-download adds).
- Keep detailed progress in **one spot** (Queue page); show a persistent **spinner in the
  title bar** for at-a-glance "still working", visible from any tab.

## Non-goals

- No progress for the conversion/encode itself (already handled by `ActiveJob` +
  `conversion-progress`).
- No per-file granularity for the **folder walk** — its total is unknown until the walk
  finishes, so that stretch is inherently indeterminate ("Scanning…").
- No cancellation of an in-flight add.

## Approach: two tiers, one backend event stream

### Tier 1 — Global spinner in the title bar

A small indeterminate spinner in `TabBar`, left of the `×` close button. Visible on every
tab. Appears whenever **any** add operation is in flight; disappears when all finish. This
is the only surface the watcher path can use.

### Tier 2 — Detailed bar on the Queue page

Placed **between the drop zone and the encoding block**:
`DropZone → [adding indicator] → ActiveJob → Pending list`. Rationale: the indicator is
feedback for the drop just performed, so it reads top-to-bottom as intake → encoding →
queue; and during adding there is usually no active encoding block yet (the queue only
starts *after* the add finishes).

Shows `Scanning…` (indeterminate) while a folder walk runs, then upgrades to
`Checking X of N` with a bar once probing starts. Reuses the existing
`.progress-bar-track` / `.progress-bar-fill` styles for consistency with encoding.

## Backend — three events

Mirrors the existing `conversion-progress` emit pattern (`app.emit(name, payload)`).

| Event | Payload | When |
| --- | --- | --- |
| `add-started` | `{ op_id: String }` | An add operation begins (before its probe loop; for a folder drop, before the scan too). |
| `add-progress` | `{ op_id: String, done: u32, total: u32 }` | Per file, from the probe loop. |
| `add-finished` | `{ op_id: String }` | The op ends — **including on error / early return**. |

`op_id` is a `uuid` v4 (crate already a dependency). It scopes each operation so overlapping
ops (e.g. a watcher scan during a drag-drop) don't clobber each other.

### `AddOp` RAII guard

A small guard (new module, e.g. `src-tauri/src/add_progress.rs`) centralizes bracketing.
It is **generic over the Tauri runtime** — `AddOp<R: tauri::Runtime>` holding
`AppHandle<R>` — matching every other testable emitter in this codebase
(`converter.rs:431/507/997`); a concrete `AppHandle` would make the guard's own
`MockRuntime` unit tests impossible.

- `AddOp::new(app: &AppHandle<R>) -> AddOp<R>` — generates `op_id`, emits `add-started`.
- `add_op.report(done, total)` — emits `add-progress`.
- `impl<R> Drop for AddOp<R>` — emits `add-finished`. Guarantees the finish fires on
  **every** exit path (success, `?` error, unwinding panic — the app has no
  `panic = "abort"` profile, and `spawn_blocking` catches the panic), so the spinner can
  never be pinned on.

### Emit points

Three top-level entry points each create an `AddOp` guard. All three already have an
`AppHandle` in scope — **no frontend-facing command signature changes**:

1. `add_files` (`commands/queue.rs:523`) — loose drag-drop files. Guard created **inside the
   `spawn_blocking` closure** (so started/finished bracket the work on the blocking thread),
   reporter passed into `add_files_inner`.
2. `confirm_folder_add` (`commands/queue.rs:565`) — scan + probe. Guard created inside the
   `spawn_blocking` closure, **before** `scan_video_files`, so the indeterminate indicator
   covers the folder walk too; reporter into `add_files_inner`.
3. `enqueue_and_start` (`watcher.rs:358`) — the watcher's single choke point into
   `add_files_inner` (`watcher.rs:368`). One guard here covers **all three** watcher paths:
   startup scan (`scan_all_enabled → scan_existing`), command-triggered add/enable
   (`scan_existing_background → scan_existing`), and reaper stabilized-download adds
   (`watcher.rs:306`). It brackets the probe only (the walk happens upstream in
   `scan_existing`, `watcher.rs:446`) — acceptable, since the watcher is a background surface
   and the probe is the slow part. Reporter passed in too.

**`classify_paths` is deliberately NOT instrumented.** Drag-drop only registers on the Queue
tab (`DropZone.tsx:63` attaches `onDragDropEvent` only while mounted, and `DropZone` mounts
only in `QueuePage`), where `DropZone`'s synchronous `"Adding files..."` status
(`DropZone.tsx:26`) already covers the classify walk. Instrumenting it would duplicate that
feedback, require the one command-signature change, and flash the title-bar spinner on every
trivial loose-file drop. Cut.

Only `add_files_inner`'s **probe loop** emits `add-progress`. It gains an optional reporter
parameter `progress: Option<&dyn Fn(u32, u32)>`. Callers with a UI surface pass a closure
that calls `add_op.report(...)`; existing tests pass `None` (so the pure `add_files_to_db`
skip-rule tests are untouched). The reporter wraps the probe closure that is already
constructed inside `add_files_inner` and handed to `resolve_media` (`queue.rs:361`) —
incrementing a counter per probed file, `total = candidates_to_probe.len()`. `resolve_media`
takes `P: Fn` (not `FnMut`, `probe_cache.rs:88`), so the counter is a `Cell<u32>` (probing is
sequential — verified, no rayon/threads in `resolve_media` — so no atomics needed). This
leaves **`resolve_media` and `add_files_to_db` completely untouched**.

Cache hits inside `resolve_media` don't call the probe closure, so on a re-scan where
everything is cached `done` may stay below `total` — harmless, because `add-finished` clears
the indicator regardless.

## Frontend

**`App.tsx` owns *all* add-progress state** and never unmounts (it wraps the tabs), so
nothing is lost when the user switches tabs mid-scan. It holds one `add-*` listener set:

- A `Set<op_id>`: add on `add-started`, remove on `add-finished`. `isAdding = set.size > 0`
  drives the spinner. A Set (not a counter) because the startup watcher scan can emit
  `add-started` before the webview's listeners attach; a later `add-finished` for an op the
  Set never saw is a harmless `delete` no-op, whereas a counter would go negative.
- The latest `AddProgress` payload (plus the started/finished bookkeeping) for the detailed
  bar, so the detail survives tab switches too.

Wiring:

- **`TabBar.tsx`** — renders the spinner when `isAdding` is true, left of the `×`.
- **`QueuePage.tsx`** — receives the current add state as props (no listener of its own) and
  renders a small new indicator component between `DropZone` and `ActiveJob`. It shows the
  **most recent** op's state (latest wins if two overlap): `Scanning…` from `add-started`
  until that op's first `add-progress`, then `Checking {done} of {total}` + bar; hidden once
  it emits `add-finished` and no other op is active. Because state lives in `App.tsx`, a
  QueuePage mounted **mid-scan** still renders correctly from the retained state (it does not
  depend on having personally witnessed `add-started`).
- **Queue empty-state** (`QueuePage.tsx:70`) is suppressed while an add is in progress, so
  the "Drag files here" placeholder and the "Checking X of N" indicator don't show at once
  during a first add on an empty queue.
- **`DropZone`** keeps its existing status line unchanged — it still shows `"Adding files..."`
  (which uniquely covers the un-instrumented `classify_paths` walk) and the final summary.
  Brief visual overlap with the indicator's `Scanning…` during a folder add is accepted;
  both mean the same thing and clear together.
- **`App.css`** gains a spin `@keyframes` and the indicator styles (none exist today).
- **`lib/tauri.ts`** gains the `AddProgress` / `add-*` event payload types.

## Edge cases

- **`skip_by_source_media` off** → no probe loop → adds are near-instant → only a brief
  spinner, no count. The determinate bar is meaningful precisely when that setting is on
  (which is when adds are actually slow). Acceptable and honest.
- **Folder confirm (>5 files)** → the confirm prompt sits between the (un-instrumented)
  classify walk and the `confirm_folder_add` op, so the spinner is correctly **off** during
  the user's think-time, then on once they click Add. No work happens while waiting.
- **Error / panic in an add op** → `AddOp::Drop` still emits `add-finished`; spinner clears.
- **Startup watcher scan before the webview is ready** → `add-started`/`add-finished` may
  fire before React attaches listeners; the Set handles it gracefully (unseen op → no-op) and
  the spinner simply doesn't show for that pre-UI window. Inherent, acceptable.
- **Off-Queue watcher progress** → title-bar spinner only (detailed bar is Queue-only, by
  design). Because add-state lives in `App.tsx`, switching to Queue mid-op shows the current
  detail from retained state.

## Testing

- **Rust (`MockRuntime`, as the other emitter tests do):** `AddOp<R>` emits `add-started` on
  `new` and `add-finished` on `Drop`, including when the guarded scope returns `Err` (drop
  still fires). The probe reporter fires `total` times with monotonic `done` for an all-miss
  batch, and `add_files_inner` with `progress: None` behaves exactly as before (existing
  skip-rule tests unchanged — the pure `add_files_to_db` is not touched).
- **Frontend:** `App` global listener flips `isAdding` true on `add-started` and back to
  false only after **all** open ops emit `add-finished` (overlapping-op test) and tolerates a
  stray `add-finished` for an unseen op. The Queue indicator renders `Scanning…` before first
  progress, `Checking X of N` after, and hides on finish. `TabBar` shows the spinner iff
  `isAdding`. Empty-state is suppressed while `isAdding`.

## Files touched (estimate)

- `src-tauri/src/add_progress.rs` — new generic `AddOp<R>` guard + event payload structs.
- `src-tauri/src/lib.rs` — register the module.
- `src-tauri/src/commands/queue.rs` — guards inside the `spawn_blocking` closures of
  `add_files` and `confirm_folder_add`; `progress: Option<&dyn Fn(u32, u32)>` param on
  `add_files_inner` with the reporter wrapping the existing probe closure. **No signature
  change to `classify_paths`.**
- `src-tauri/src/watcher.rs` — one guard in `enqueue_and_start` (covers startup, background,
  and reaper), reporter into `add_files_inner`.
- `src/lib/tauri.ts` — event payload types.
- `src/App.tsx` — global `add-*` listener owning op-set + latest progress; pass `isAdding`
  to `TabBar` and add-state to `QueuePage`.
- `src/components/TabBar.tsx` — spinner.
- `src/pages/QueuePage.tsx` (+ small new indicator component) — detailed bar; suppress
  empty-state while adding.
- `src/App.css` — spin keyframes + indicator styles.
- Tests alongside the above.

No new Tauri plugin or ACL permission: the events are app-emitted and the frontend only
adds `listen(...)` calls (no new `core:`/`plugin:` API). App commands stay ACL-exempt.
