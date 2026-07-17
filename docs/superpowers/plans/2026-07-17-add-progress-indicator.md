# File-Intake Progress Indicator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a title-bar spinner whenever the app is processing files for the queue, plus a determinate "Checking X of N" bar on the Queue page during the per-file probe.

**Architecture:** A Rust RAII guard (`AddOp`) brackets each file-intake operation with `add-started`/`add-finished` events and emits `add-progress` per probed file. Three backend entry points (`add_files`, `confirm_folder_add`, watcher `enqueue_and_start`) create the guard; the probe loop inside `add_files_inner` reports counts. The frontend listens via a `useAddProgress` hook in `App.tsx`, driving a `TabBar` spinner (any op active) and a Queue-page `AddingIndicator` (most-recent op detail).

**Tech Stack:** Rust + Tauri 2 (`Emitter`, `uuid` v4, `MockRuntime` tests), React + TypeScript, Vitest + Testing Library, plain CSS.

**Design spec:** `docs/superpowers/specs/2026-07-17-add-progress-indicator-design.md`

**Conventions to respect:**
- Events are emitted with **string literal** names (`app.emit("add-started", …)`) — `src/test/ipc-contract.test.ts:66` scans Rust for `.emit("literal")` and fails if a frontend `listen("x")` has no matching backend emit. This test is free verification of the seam.
- Testable emitters are generic over the runtime: `AppHandle<R: tauri::Runtime>` (mirrors `converter.rs`), so `MockRuntime` unit tests can drive them.
- Rust is fmt-clean; run from repo root. Frontend tests: `npm test -- <path>`. Type-check: `npm run build`. Rust tests: `cd src-tauri && cargo test --lib <name>`.

---

## File Structure

**Backend (Rust):**
- `src-tauri/src/add_progress.rs` — **new.** The `AddOp<R>` guard, its three `#[derive(Serialize)]` payload structs, and unit tests. One responsibility: bracket + report intake progress.
- `src-tauri/src/lib.rs` — register `mod add_progress;`.
- `src-tauri/src/commands/queue.rs` — `add_files_inner` gains a `progress` param; the probe closure reports counts; `add_files` + `confirm_folder_add` create guards inside their `spawn_blocking` closures; two test call sites pass `None`.
- `src-tauri/src/watcher.rs` — `enqueue_and_start` creates one guard (covers startup + background + reaper).

**Frontend (React/TS):**
- `src/lib/tauri.ts` — event payload types + `AddActivity`.
- `src/hooks/useAddProgress.ts` — **new.** Owns the op-set (spinner) + latest activity (detail). One responsibility: turn the three events into `{ isAdding, activity }`.
- `src/components/AddingIndicator.tsx` — **new.** Pure presentational: renders the detail bar from an `activity` prop.
- `src/components/TabBar.tsx` — accept `isAdding`, render spinner.
- `src/App.tsx` — call the hook, wire props down.
- `src/pages/QueuePage.tsx` — accept `adding` prop, render the indicator, suppress empty-state while adding.
- `src/App.css` — spin keyframes + spinner + indicator styles.

**Ordering rationale:** Backend first (Tasks 1–2) so the events exist before any frontend `listen`, keeping `ipc-contract.test.ts` green at every commit. Then frontend units (3–5), wiring (6), CSS (7), full verification (8).

---

## Task 1: `AddOp` guard module (Rust)

**Files:**
- Create: `src-tauri/src/add_progress.rs`
- Modify: `src-tauri/src/lib.rs:1-9` (add `mod add_progress;`)
- Test: inline `#[cfg(test)]` in `src-tauri/src/add_progress.rs`

- [ ] **Step 1: Register the module**

In `src-tauri/src/lib.rs`, the module list is lines 1–9. Add `add_progress` in alphabetical position (after `mod` line 1 `commands;`... keep it sorted — insert after line 1):

```rust
mod add_progress;
mod commands;
mod converter;
mod db;
mod handbrake;
mod media_skip;
mod probe;
mod probe_cache;
mod types;
mod watcher;
```

- [ ] **Step 2: Write the failing test**

Create `src-tauri/src/add_progress.rs` with ONLY the test module first (the types/impl come in Step 4). This lets the test fail to compile → then pass:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    fn record(app: &tauri::App<tauri::test::MockRuntime>, name: &str) -> Arc<Mutex<Vec<String>>> {
        let store = Arc::new(Mutex::new(Vec::new()));
        let sink = store.clone();
        app.listen_any(name.to_string(), move |e| {
            sink.lock().unwrap().push(e.payload().to_string());
        });
        store
    }

    #[test]
    fn emits_started_on_new_and_finished_on_drop() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let finished = record(&app, "add-finished");
        {
            let _op = AddOp::new(app.handle());
            assert_eq!(started.lock().unwrap().len(), 1, "started fires immediately");
            assert_eq!(finished.lock().unwrap().len(), 0, "not finished yet");
        }
        assert_eq!(finished.lock().unwrap().len(), 1, "finished fires on drop");
    }

    #[test]
    fn finished_fires_even_on_early_return() {
        // Simulates the enqueue_and_start Err arm: the guard is in scope, an early
        // return drops it, and add-finished must still fire so the spinner clears.
        fn guarded(app: &tauri::AppHandle<tauri::test::MockRuntime>, bail: bool) {
            let _op = AddOp::new(app);
            if bail {
                return;
            }
        }
        let app = mock_app();
        let finished = record(&app, "add-finished");
        guarded(app.handle(), true);
        assert_eq!(finished.lock().unwrap().len(), 1);
    }

    #[test]
    fn report_emits_progress_with_the_same_op_id() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let progress = record(&app, "add-progress");
        let op = AddOp::new(app.handle());
        op.report(1, 3);
        op.report(2, 3);

        let started = started.lock().unwrap();
        let op_id: serde_json::Value = serde_json::from_str(&started[0]).unwrap();
        let op_id = op_id["op_id"].as_str().unwrap();

        let progress = progress.lock().unwrap();
        assert_eq!(progress.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&progress[0]).unwrap();
        assert_eq!(first["op_id"].as_str().unwrap(), op_id, "progress carries the op's id");
        assert_eq!(first["done"].as_u64().unwrap(), 1);
        assert_eq!(first["total"].as_u64().unwrap(), 3);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd src-tauri && cargo test --lib add_progress`
Expected: FAIL — compile error `cannot find type/value AddOp in this scope` (types not written yet).

- [ ] **Step 4: Write the implementation**

Prepend the implementation above the test module in `src-tauri/src/add_progress.rs`:

```rust
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use uuid::Uuid;

#[derive(Clone, Serialize)]
struct StartedPayload {
    op_id: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    op_id: String,
    done: u32,
    total: u32,
}

#[derive(Clone, Serialize)]
struct FinishedPayload {
    op_id: String,
}

/// RAII guard bracketing a file-intake operation (scan + duplicate/format checks) with
/// `add-started` / `add-finished` events, and emitting `add-progress` per probed file.
/// The finish fires in `Drop`, so the UI spinner always clears — even on an early return,
/// `?`-propagated error, or a panic-unwind (the app has no `panic = "abort"` profile).
///
/// Generic over the runtime so it can be driven by `MockRuntime` in unit tests, matching
/// the other emitters in this codebase (see `converter.rs`).
pub struct AddOp<R: Runtime> {
    app: AppHandle<R>,
    op_id: String,
}

impl<R: Runtime> AddOp<R> {
    pub fn new(app: &AppHandle<R>) -> Self {
        let op_id = Uuid::new_v4().to_string();
        let _ = app.emit("add-started", StartedPayload { op_id: op_id.clone() });
        Self {
            app: app.clone(),
            op_id,
        }
    }

    /// Emit one per-file progress tick. `done` counts probed files so far; `total` is the
    /// probe-candidate count for this batch.
    pub fn report(&self, done: u32, total: u32) {
        let _ = self.app.emit(
            "add-progress",
            ProgressPayload {
                op_id: self.op_id.clone(),
                done,
                total,
            },
        );
    }
}

impl<R: Runtime> Drop for AddOp<R> {
    fn drop(&mut self) {
        let _ = self.app.emit(
            "add-finished",
            FinishedPayload {
                op_id: self.op_id.clone(),
            },
        );
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd src-tauri && cargo test --lib add_progress`
Expected: PASS — 3 tests (`emits_started_on_new_and_finished_on_drop`, `finished_fires_even_on_early_return`, `report_emits_progress_with_the_same_op_id`).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/add_progress.rs src-tauri/src/lib.rs
git commit -m "feat: add AddOp guard emitting file-intake progress events"
```

---

## Task 2: Thread the reporter through `add_files_inner` and its callers (Rust)

**Files:**
- Modify: `src-tauri/src/commands/queue.rs:276` (signature), `:361` (probe closure), `:527-530` (`add_files`), `:572-580` (`confirm_folder_add`), `:1767` & `:1785` (test calls)
- Modify: `src-tauri/src/watcher.rs:358-382` (`enqueue_and_start`)

This task is a signature change plus wiring; its correctness is that **all existing Rust tests still pass** (the probe count can't be unit-tested without a real HandBrake probe — it's exercised in Task 8's end-to-end drive). No new Rust test here.

- [ ] **Step 1: Change the `add_files_inner` signature**

`src-tauri/src/commands/queue.rs:276` currently:

```rust
pub(crate) fn add_files_inner(state: &AppState, paths: &[String]) -> Result<AddResult, String> {
```

Change to:

```rust
pub(crate) fn add_files_inner(
    state: &AppState,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
```

- [ ] **Step 2: Report progress from the probe closure**

`src-tauri/src/commands/queue.rs:361-372` is the `resolve_media` call. It currently passes the probe closure as `|p| crate::probe::probe_source(hb, p),`. Replace that call so a `Cell` counter ticks per probe and forwards to `progress`. The full replacement for the `let probed = crate::probe_cache::resolve_media( … );` block:

```rust
            let total = candidates_to_probe.len() as u32;
            let probe_count = std::cell::Cell::new(0u32);
            let probed = crate::probe_cache::resolve_media(
                &with_identity,
                |ids| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::lookup_batch(&conn, ids)
                },
                |p| {
                    let media = crate::probe::probe_source(hb, p);
                    let done = probe_count.get() + 1;
                    probe_count.set(done);
                    if let Some(report) = progress {
                        report(done, total);
                    }
                    media
                },
                |items| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::store_batch(&conn, items);
                },
            );
```

`Cell<u32>` (not a plain `mut`) because `resolve_media` requires `P: Fn`, not `FnMut` (`probe_cache.rs:88`). Probing is sequential, so no atomics are needed. Cache hits never call this closure, so on an all-cached re-scan `done` may stay below `total` — harmless; `add-finished` clears the bar regardless.

- [ ] **Step 3: Create the guard in `add_files`**

`src-tauri/src/commands/queue.rs:527-530`. Replace the `spawn_blocking` closure body:

```rust
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let op = crate::add_progress::AddOp::new(&app);
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(&state, &paths, Some(&reporter as &dyn Fn(u32, u32)))
    })
    .await
    .map_err(|e| e.to_string())?
```

The `as &dyn Fn(u32, u32)` cast is required: `&closure` does not auto-coerce to `&dyn Fn` inside `Some(...)`. `op` drops at the end of the closure block (after the returned value is computed), firing `add-finished`.

- [ ] **Step 4: Create the guard in `confirm_folder_add` (before the scan)**

`src-tauri/src/commands/queue.rs:572-580`. Replace the `spawn_blocking` closure body so the guard brackets the folder walk too:

```rust
    tauri::async_runtime::spawn_blocking(move || {
        let op = crate::add_progress::AddOp::new(&app);
        let files = scan_video_files(Path::new(&path));
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect();
        let state = app.state::<AppState>();
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(&state, &paths, Some(&reporter as &dyn Fn(u32, u32)))
    })
    .await
    .map_err(|e| e.to_string())?
```

- [ ] **Step 5: Create the guard in the watcher's `enqueue_and_start`**

`src-tauri/src/watcher.rs:358-382`. This one function is the single choke point for all three watcher paths. Scope the guard to just the `add_files_inner` call (the conversion that follows has its own progress). Replace the `let result = match … ;` statement (lines 367-374) with:

```rust
    let app_state = app.state::<AppState>();
    let result = {
        let op = crate::add_progress::AddOp::new(app);
        let reporter = |done: u32, total: u32| op.report(done, total);
        match queue::add_files_inner(&app_state, &paths, Some(&reporter as &dyn Fn(u32, u32))) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("watcher: failed to enqueue {paths:?}: {err}");
                return;
            }
        }
        // `op` drops here → add-finished (also on the early return above).
    };
```

Leave the rest of the function (the `result.added.is_empty()` check, `run_queue`, `queue-updated` emit) unchanged.

- [ ] **Step 6: Update the two Rust test call sites**

`src-tauri/src/commands/queue.rs:1767` and `:1785` currently call `add_files_inner(&state, &inputs)`. Add the `None` argument:

```rust
        let result = add_files_inner(&state, &inputs, None).unwrap();
```
```rust
        let again = add_files_inner(&state, &inputs, None).unwrap();
```

- [ ] **Step 7: Build and run the full Rust suite**

Run: `cd src-tauri && cargo build && cargo test --lib`
Expected: PASS — the whole library suite (≈169 tests) green, no warnings about unused `progress`. If the compiler complains that `&reporter` doesn't coerce, confirm the `as &dyn Fn(u32, u32)` cast is present at all three call sites.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/commands/queue.rs src-tauri/src/watcher.rs
git commit -m "feat: report per-file intake progress from add_files_inner and its callers"
```

---

## Task 3: Frontend event types + `useAddProgress` hook

**Files:**
- Modify: `src/lib/tauri.ts` (add types near the other interfaces, e.g. after `ConversionProgress` at line 49)
- Create: `src/hooks/useAddProgress.ts`
- Test: `src/hooks/useAddProgress.test.ts`

- [ ] **Step 1: Add the payload + activity types**

In `src/lib/tauri.ts`, after the `ConversionProgress` interface (ends line 49), add:

```ts
export interface AddStarted {
  op_id: string;
}

export interface AddProgress {
  op_id: string;
  done: number;
  total: number;
}

export interface AddFinished {
  op_id: string;
}

// Frontend view of the current add operation. `done`/`total` are null during the
// indeterminate scan phase (before the first per-file probe tick).
export interface AddActivity {
  opId: string;
  done: number | null;
  total: number | null;
}
```

- [ ] **Step 2: Write the failing test**

Create `src/hooks/useAddProgress.test.ts` (mirrors the `listen` mock in `src/hooks/useQueue.test.ts`):

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { listen } from "@tauri-apps/api/event";
import { useAddProgress } from "./useAddProgress";

const listenMock = vi.mocked(listen);
const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();

function emit(event: string, payload: unknown) {
  act(() => {
    listeners.get(event)?.forEach((cb) => cb({ payload }));
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
});

it("goes adding on start and clears on finish", () => {
  const { result } = renderHook(() => useAddProgress());
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();

  emit("add-started", { op_id: "a" });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "a", done: null, total: null });

  emit("add-progress", { op_id: "a", done: 3, total: 10 });
  expect(result.current.activity).toEqual({ opId: "a", done: 3, total: 10 });

  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();
});

it("stays adding until every overlapping op finishes", () => {
  const { result } = renderHook(() => useAddProgress());
  emit("add-started", { op_id: "a" });
  emit("add-started", { op_id: "b" });
  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(true); // b still open
  emit("add-finished", { op_id: "b" });
  expect(result.current.isAdding).toBe(false);
});

it("tolerates progress for an op whose start it missed", () => {
  // A watcher scan can emit add-started before the webview attaches listeners.
  const { result } = renderHook(() => useAddProgress());
  emit("add-progress", { op_id: "x", done: 1, total: 4 });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "x", done: 1, total: 4 });
});

it("ignores a stray finish for an unseen op", () => {
  const { result } = renderHook(() => useAddProgress());
  emit("add-finished", { op_id: "ghost" });
  expect(result.current.isAdding).toBe(false);
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `npm test -- src/hooks/useAddProgress.test.ts`
Expected: FAIL — `Cannot find module './useAddProgress'`.

- [ ] **Step 4: Write the hook**

Create `src/hooks/useAddProgress.ts`:

```ts
import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { AddStarted, AddProgress, AddFinished, AddActivity } from "../lib/tauri";

/**
 * Turns the backend `add-*` events into UI state: `isAdding` (any operation in flight,
 * drives the title-bar spinner) and `activity` (the most-recent operation's detail,
 * drives the Queue-page bar). Owned by App.tsx so tab switches never lose it.
 *
 * Uses a Set of open op ids, not a counter: a watcher scan can emit `add-started` before
 * these listeners attach, so a later `add-finished` for an unseen op must be a harmless
 * no-op rather than driving a counter negative.
 */
export function useAddProgress() {
  const [openOps, setOpenOps] = useState<Set<string>>(new Set());
  const [activity, setActivity] = useState<AddActivity | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    const add = (id: string) =>
      setOpenOps((prev) => {
        if (prev.has(id)) return prev;
        const next = new Set(prev);
        next.add(id);
        return next;
      });

    const unlisteners = [
      listen<AddStarted>("add-started", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id);
        setActivity({ opId: payload.op_id, done: null, total: null });
      }),
      listen<AddProgress>("add-progress", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id); // covers a start we never saw
        setActivity({ opId: payload.op_id, done: payload.done, total: payload.total });
      }),
      listen<AddFinished>("add-finished", ({ payload }) => {
        if (!mounted.current) return;
        setOpenOps((prev) => {
          if (!prev.has(payload.op_id)) return prev;
          const next = new Set(prev);
          next.delete(payload.op_id);
          return next;
        });
        setActivity((cur) => (cur?.opId === payload.op_id ? null : cur));
      }),
    ];

    return () => {
      mounted.current = false;
      unlisteners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);

  return { isAdding: openOps.size > 0, activity };
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `npm test -- src/hooks/useAddProgress.test.ts`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/lib/tauri.ts src/hooks/useAddProgress.ts src/hooks/useAddProgress.test.ts
git commit -m "feat: useAddProgress hook tracking file-intake events"
```

---

## Task 4: `AddingIndicator` component

**Files:**
- Create: `src/components/AddingIndicator.tsx`
- Test: `src/components/AddingIndicator.test.tsx`

- [ ] **Step 1: Write the failing test**

Create `src/components/AddingIndicator.test.tsx`:

```tsx
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import AddingIndicator from "./AddingIndicator";

it("renders nothing when idle", () => {
  const { container } = render(<AddingIndicator activity={null} />);
  expect(container.firstChild).toBeNull();
});

it("shows an indeterminate scanning label before the first count", () => {
  render(<AddingIndicator activity={{ opId: "a", done: null, total: null }} />);
  expect(screen.getByText(/scanning/i)).toBeInTheDocument();
});

it("shows the count and a filled bar during probing", () => {
  render(<AddingIndicator activity={{ opId: "a", done: 3, total: 12 }} />);
  expect(screen.getByText(/checking 3 of 12/i)).toBeInTheDocument();
  const fill = document.querySelector(".progress-bar-fill") as HTMLElement;
  expect(fill.style.width).toBe("25%");
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npm test -- src/components/AddingIndicator.test.tsx`
Expected: FAIL — `Cannot find module './AddingIndicator'`.

- [ ] **Step 3: Write the component**

Create `src/components/AddingIndicator.tsx`:

```tsx
import type { AddActivity } from "../lib/tauri";

interface AddingIndicatorProps {
  activity: AddActivity | null;
}

export default function AddingIndicator({ activity }: AddingIndicatorProps) {
  if (!activity) return null;

  const determinate = activity.total !== null && activity.total > 0;
  const percent = determinate
    ? Math.min(100, Math.round((activity.done! / activity.total!) * 100))
    : 0;

  return (
    <div className="adding-indicator">
      <div className="adding-indicator-label">
        <span className="spinner" aria-hidden="true" />
        <span>
          {determinate ? `Checking ${activity.done} of ${activity.total}…` : "Scanning…"}
        </span>
      </div>
      {determinate && (
        <div className="progress-bar-track">
          <div className="progress-bar-fill" style={{ width: `${percent}%` }} />
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npm test -- src/components/AddingIndicator.test.tsx`
Expected: PASS — 3 tests. (Styling for `.adding-indicator`/`.spinner` lands in Task 7; the tests assert structure, not appearance.)

- [ ] **Step 5: Commit**

```bash
git add src/components/AddingIndicator.tsx src/components/AddingIndicator.test.tsx
git commit -m "feat: AddingIndicator component for Queue-page intake progress"
```

---

## Task 5: `TabBar` spinner

**Files:**
- Modify: `src/components/TabBar.tsx`
- Test: `src/components/TabBar.test.tsx`

- [ ] **Step 1: Write the failing test**

Create `src/components/TabBar.test.tsx`:

```tsx
import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";

vi.mock("../lib/tauri", () => ({ commands: { hideWindow: vi.fn() } }));

import TabBar from "./TabBar";

const noop = () => {};

it("shows the spinner only while adding", () => {
  const { container, rerender } = render(
    <TabBar activeTab="queue" onTabChange={noop} isAdding={false} />,
  );
  expect(container.querySelector(".tab-spinner")).toBeNull();

  rerender(<TabBar activeTab="queue" onTabChange={noop} isAdding={true} />);
  expect(container.querySelector(".tab-spinner")).not.toBeNull();
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npm test -- src/components/TabBar.test.tsx`
Expected: FAIL — TypeScript/prop error (`isAdding` not on props) or the spinner element is missing.

- [ ] **Step 3: Add the `isAdding` prop and spinner**

In `src/components/TabBar.tsx`, extend the props interface and render a spinner in the spacer area, before the close button. The full updated file:

```tsx
import { commands } from "../lib/tauri";

type Tab = "queue" | "history" | "watch" | "settings";

interface TabBarProps {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
  isAdding: boolean;
}

const tabs: { id: Tab; label: string }[] = [
  { id: "queue", label: "Queue" },
  { id: "history", label: "History" },
  { id: "watch", label: "Watch" },
  { id: "settings", label: "Settings" },
];

export default function TabBar({ activeTab, onTabChange, isAdding }: TabBarProps) {
  return (
    <div className="tab-bar" data-tauri-drag-region>
      {tabs.map((tab) => (
        <button
          key={tab.id}
          className={`tab-btn ${activeTab === tab.id ? "active" : ""}`}
          onClick={() => onTabChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
      <div className="tab-spacer" data-tauri-drag-region />
      {isAdding && (
        <span className="tab-spinner" title="Adding files to the queue…" aria-label="Adding files" />
      )}
      <button className="tab-btn close-tab-btn" onClick={() => commands.hideWindow()} title="Close">
        &times;
      </button>
    </div>
  );
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npm test -- src/components/TabBar.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/TabBar.tsx src/components/TabBar.test.tsx
git commit -m "feat: title-bar spinner in TabBar while adding files"
```

---

## Task 6: Wire into `App.tsx` and `QueuePage.tsx`

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/pages/QueuePage.tsx`
- Test: `src/pages/QueuePage.test.tsx`

- [ ] **Step 1: Wire the hook into `App.tsx`**

In `src/App.tsx`: import the hook, call it, and pass props down. Add the import after line 7 (`import { commands, … }`):

```tsx
import { useAddProgress } from "./hooks/useAddProgress";
```

Inside `App()`, after `const [hbStatus, …]` (line 14), add:

```tsx
  const { isAdding, activity } = useAddProgress();
```

Update the `TabBar` usage (line 37) and the `QueuePage` usage (line 39):

```tsx
      <TabBar activeTab={activeTab} onTabChange={setActiveTab} isAdding={isAdding} />
      <div className="page">
        {activeTab === "queue" && <QueuePage hbStatus={hbStatus} adding={activity} />}
```

(Leave the other three page lines unchanged.)

- [ ] **Step 2: Write the failing `QueuePage` test**

Create `src/pages/QueuePage.test.tsx`. It mocks `useQueue` (empty queue) and the child components so we can assert the empty-state / indicator logic in isolation:

```tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../hooks/useQueue", () => ({
  useQueue: () => ({ activeJob: null, pendingJobs: [], progress: null, refresh: vi.fn() }),
}));
vi.mock("../components/DropZone", () => ({ default: () => <div data-testid="dropzone" /> }));

import QueuePage from "./QueuePage";

it("suppresses the empty-state while an add is in progress", () => {
  render(<QueuePage hbStatus={null} adding={{ opId: "a", done: 1, total: 5 }} />);
  expect(screen.queryByText(/drag video files or folders here to get started/i)).toBeNull();
  expect(screen.getByText(/checking 1 of 5/i)).toBeInTheDocument();
});

it("shows the empty-state when idle", () => {
  render(<QueuePage hbStatus={null} adding={null} />);
  expect(screen.getByText(/drag video files or folders here to get started/i)).toBeInTheDocument();
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `npm test -- src/pages/QueuePage.test.tsx`
Expected: FAIL — `adding` is not a prop yet, and `AddingIndicator` isn't rendered, so "checking 1 of 5" is absent and the empty-state still shows.

- [ ] **Step 4: Update `QueuePage.tsx`**

In `src/pages/QueuePage.tsx`: import the indicator and the type, add the `adding` prop, render the indicator between `DropZone` and `ActiveJob`, and gate the empty-state on `!adding`.

Add imports after line 7 (`import type { HandbrakeStatus } …`):

```tsx
import AddingIndicator from "../components/AddingIndicator";
import type { AddActivity } from "../lib/tauri";
```

Change the props interface (lines 9-11):

```tsx
interface QueuePageProps {
  hbStatus: HandbrakeStatus | null;
  adding: AddActivity | null;
}
```

Change the component signature (line 13):

```tsx
export default function QueuePage({ hbStatus, adding }: QueuePageProps) {
```

Render the indicator right after `<DropZone … />` (line 41):

```tsx
      <DropZone onFilesAdded={refresh} />

      <AddingIndicator activity={adding} />
```

Gate the empty-state (line 70) so it hides while adding:

```tsx
      {!adding && !activeJob && pendingJobs.length === 0 && (
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `npm test -- src/pages/QueuePage.test.tsx`
Expected: PASS — 2 tests.

- [ ] **Step 6: Type-check the whole frontend**

Run: `npm run build`
Expected: PASS — `tsc` clean (confirms `App.tsx`, `TabBar`, `QueuePage` prop wiring all type-check) and Vite build succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/App.tsx src/pages/QueuePage.tsx src/pages/QueuePage.test.tsx
git commit -m "feat: wire intake progress into App and Queue page"
```

---

## Task 7: Styles — spin keyframes, title-bar spinner, indicator

**Files:**
- Modify: `src/App.css`

No unit test (CSS); verified visually in Task 8.

- [ ] **Step 1: Add the keyframes and classes**

Append to `src/App.css` (end of file). Colors reuse existing CSS variables already used elsewhere in the file (`--accent`, `--border`, `--text-secondary`, `--bg-secondary`):

```css
/* File-intake progress: title-bar spinner + Queue-page indicator */
@keyframes cb-spin {
  to {
    transform: rotate(360deg);
  }
}

.tab-spinner {
  align-self: center;
  width: 12px;
  height: 12px;
  margin-right: 8px;
  border: 2px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: cb-spin 0.7s linear infinite;
}

.adding-indicator {
  margin: 0 12px 8px;
  padding: 8px 12px;
  background: var(--bg-secondary);
  border-radius: var(--radius);
}

.adding-indicator-label {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
  font-size: 12px;
  color: var(--text-secondary);
}

.adding-indicator .spinner {
  width: 12px;
  height: 12px;
  border: 2px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: cb-spin 0.7s linear infinite;
}
```

- [ ] **Step 2: Verify the stylesheet still compiles into the build**

Run: `npm run build`
Expected: PASS — Vite bundles the CSS with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/App.css
git commit -m "style: spinner keyframes and intake-progress indicator"
```

---

## Task 8: Full verification (automated + end-to-end drive)

**Files:** none (verification only).

- [ ] **Step 1: Full frontend suite (incl. the IPC contract)**

Run: `npm test`
Expected: PASS — all suites green. Confirm `src/test/ipc-contract.test.ts` passes: it proves each new `listen("add-started" | "add-progress" | "add-finished")` has a matching backend `.emit(...)`. If it reports "listened but never emitted", the Rust emit literals in `add_progress.rs` don't match the frontend strings — reconcile them.

- [ ] **Step 2: Full Rust suite + build**

Run: `cd src-tauri && cargo build && cargo test --lib`
Expected: PASS — including the three `add_progress` tests and all pre-existing tests.

- [ ] **Step 3: Type-check / production build**

Run: `npm run build`
Expected: PASS.

- [ ] **Step 4: Drive the real app (this is where the probe-count path is exercised)**

The `add-progress` counting can only be observed with a real HandBrake probe, so verify it live. Use the `/run` skill (or `npm run tauri dev`). Then:
1. In Settings, enable **Skip by source media** (this turns on the per-file probe — the slow path the bar is for).
2. Drag a folder of several video files onto the Queue page.
3. Confirm: the **title-bar spinner** appears (left of ×) during processing; the Queue page shows **Scanning…** then **Checking X of N** with the bar advancing; both clear when done; the queue populates.
4. Switch to the **History** tab mid-add and confirm the title-bar spinner is still visible there.
5. (Watcher) Add a watched folder that already contains videos; confirm the title-bar spinner appears while it scans/probes existing files in the background.

Expected: all indicators appear and clear as described. If the bar never shows counts, re-check that *Skip by source media* is on (without it, adds are near-instant by design and only the spinner blinks).

- [ ] **Step 5: Final commit (if any doc/cleanup remains)**

Nothing to commit if Tasks 1–7 committed cleanly. Otherwise:

```bash
git status   # confirm clean
```

---

## Notes for the implementer

- **Do not** touch `resolve_media` (`probe_cache.rs`) or `add_files_to_db` (`queue.rs`) — the reporter wraps the probe closure *outside* them so their existing tests stay valid.
- **Do not** add a permission to `capabilities/default.json`: the events are app-emitted and the frontend only adds `listen(...)` (no new `core:`/`plugin:` API). App-defined commands and app-emitted events are ACL-exempt.
- Keep event-name strings **identical** across Rust and TS (`add-started`, `add-progress`, `add-finished`) — the IPC-contract test enforces it, but a mismatch would otherwise ship a silently dead listener.
- This is feature work on `feature/add-progress-indicator`. `main` is protected; open a PR when done (see CLAUDE.md "Merging a PR").
