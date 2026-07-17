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
- Cover **both** drag-drop and watched-folder scan-existing.
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
| `add-started` | `{ op_id: String }` | An add operation begins, **before** any scan. |
| `add-progress` | `{ op_id: String, done: u32, total: u32 }` | Per file, from the probe loop. |
| `add-finished` | `{ op_id: String }` | The op ends — **including on error / early return**. |

`op_id` is a `uuid` v4 (crate already a dependency). It scopes each operation so overlapping
ops (e.g. a watcher scan during a drag-drop) don't clobber each other.

### `AddOp` RAII guard

A small guard (new module, e.g. `src-tauri/src/add_progress.rs`) centralizes bracketing:

- `AddOp::new(&app) -> AddOp` — generates `op_id`, emits `add-started`.
- `add_op.report(done, total)` — emits `add-progress`.
- `impl Drop for AddOp` — emits `add-finished`. Guarantees the finish fires on **every**
  exit path (success, `?` error, early return), so the spinner can never be pinned on.

### Emit points

Four top-level entry points each create an `AddOp` guard:

1. `add_files` (`commands/queue.rs`) — loose drag-drop files. Guard around `add_files_inner`.
2. `confirm_folder_add` (`commands/queue.rs`) — scan + probe. Guard wraps the whole thing
   so the spinner covers the walk too.
3. `classify_paths` (`commands/queue.rs`) — the drag-drop folder walk that precedes the add.
   Guard covers the scan (indeterminate; emits no `add-progress`). Needs an `AppHandle`
   param added (currently has none).
4. `scan_existing_background` (`watcher.rs`) — watcher scan + probe. Guard around scan +
   `add_files_inner`.

Only `add_files_inner`'s **probe loop** emits `add-progress`. It gains an optional reporter
parameter, e.g. `progress: Option<&dyn Fn(u32, u32)>`. Callers with a UI surface pass a
closure that calls `add_op.report(...)`; tests pass `None`. The reporter wraps the existing
probe callback passed to `resolve_media` — increment a counter per probed file,
`total = candidates_to_probe.len()` — so **`resolve_media` and the pure `add_files_to_db`
stay untouched and testable**. Probing is sequential (verified: no rayon/threads in
`resolve_media`), so counts are naturally ordered; no atomics needed.

Cache hits inside `resolve_media` don't call the probe closure, so on a re-scan where
everything is cached `done` may stay below `total` — harmless, because `add-finished` clears
the indicator regardless. (Optional nicety: emit a final `report(total, total)` before the
guard drops to snap the bar to 100%.)

## Frontend

- **`App.tsx`** holds a global listener maintaining a `Set<op_id>`: add on `add-started`,
  remove on `add-finished`. Passes `isAdding = set.size > 0` to `TabBar`. A Set (not a
  boolean) so overlapping ops don't clear each other early. Listener lives here (above the
  tabs) so it survives tab switches.
- **`TabBar.tsx`** renders the spinner when `isAdding` is true, left of the `×`.
- **Queue page** (`QueuePage.tsx` / a small new component) listens to `add-progress` and
  renders the detailed bar for the op of the **most recent** `add-progress`/`add-started`
  event (latest wins if two overlap): `Scanning…` until the first `add-progress` for that op
  arrives, then `Checking {done} of {total}` + bar; hidden once that op emits `add-finished`
  and no other op is active. The existing `DropZone` status text (summary after add) is
  unchanged.
- **`App.css`** gains a spin `@keyframes` and the indicator styles (none exist today).
- **`lib/tauri.ts`** gains the `AddProgress` / `add-*` event payload types.

## Edge cases

- **`skip_by_source_media` off** → no probe loop → adds are near-instant → only a brief
  spinner, no count. The determinate bar is meaningful precisely when that setting is on
  (which is when adds are actually slow). Acceptable and honest.
- **Folder confirm (>5 files)** → `classify_paths` op finishes, spinner off during the
  user's think-time at the confirm prompt, then `confirm_folder_add` op starts the spinner
  again. Correct — no work happens while waiting on the user.
- **Small-folder auto-add (≤5)** → classify op and add op are back-to-back; spinner may
  blink off for milliseconds between them. Negligible.
- **Error / panic in an add op** → `AddOp::Drop` still emits `add-finished`; spinner clears.
- **Off-Queue watcher progress** → title-bar spinner only (detailed bar is Queue-only, by
  design). Switching to Queue mid-scan shows the detail.

## Testing

- **Rust:** `AddOp` emits `add-started` on `new` and `add-finished` on `Drop`, including
  when the wrapped closure returns `Err` / panics-unwind. The probe reporter fires
  `total` times with monotonic `done` for an all-miss batch, and `add_files_inner` with
  `progress: None` behaves exactly as before (existing skip-rule tests unchanged — the pure
  `add_files_to_db` is not touched).
- **Frontend:** `App` global listener flips `isAdding` true on `add-started` and back to
  false only after **all** open ops emit `add-finished` (overlapping-op test). Queue
  detailed component renders `Scanning…` before first progress, `Checking X of N` after,
  and hides on finish. `TabBar` shows the spinner iff `isAdding`.

## Files touched (estimate)

- `src-tauri/src/add_progress.rs` — new `AddOp` guard + event payloads.
- `src-tauri/src/lib.rs` — register the module.
- `src-tauri/src/commands/queue.rs` — guards in `add_files`, `confirm_folder_add`,
  `classify_paths` (+ `AppHandle`); reporter threaded into `add_files_inner`.
- `src-tauri/src/watcher.rs` — guard in `scan_existing_background`.
- `src/lib/tauri.ts` — event payload types.
- `src/App.tsx` — global op-set listener, pass `isAdding` down.
- `src/components/TabBar.tsx` — spinner.
- `src/pages/QueuePage.tsx` (+ small new indicator component) — detailed bar.
- `src/App.css` — spin keyframes + indicator styles.
- Tests alongside the above.

No new Tauri plugin or ACL permission: the events are app-emitted and the frontend only
adds `listen(...)` calls (no new `core:`/`plugin:` API). App commands stay ACL-exempt.
