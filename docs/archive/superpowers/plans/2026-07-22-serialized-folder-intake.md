# Serialized folder intake + cross-tab drops — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serialize drop-initiated folder scanning into one pipeline (new drops append, never interrupt), keep the confirm prompt in the drop zone but revert it instantly on Add/Skip, name the scanner with the folder being scanned, and accept drops on any tab.

**Architecture:** Move the drag-drop listener + intake orchestration out of `DropZone` (which unmounts on tab switch) into an always-mounted `useFileIntake` hook in `App`. The hook keeps a `confirmQueueRef` (head = the one shown prompt) and a serialized task pipeline that awaits each `add_files`/`confirm_folder_add` before the next. The backend `AddOp` gains a `label` (emitted on both `add-started` and `add-progress`) so the scanner card shows the folder name; `add_files`/`confirm_folder_add` emit `queue-updated` so `useQueue` refreshes reactively. `DropZone` becomes presentational.

**Tech Stack:** Rust (Tauri 2, `MockRuntime` tests), React + TypeScript, Vitest + @testing-library.

**Spec:** `docs/superpowers/specs/2026-07-22-serialized-folder-intake-design.md`

**Commands:**
- Frontend, one file: `npx vitest run <path>`
- Frontend, all: `npm test`
- Rust, one test: `cargo test --manifest-path src-tauri/Cargo.toml <name>`
- Rust, all: `cargo test --manifest-path src-tauri/Cargo.toml`
- Type-check + build: `npm run build`

## File structure

| File | Responsibility | Task |
| --- | --- | --- |
| `src-tauri/src/add_progress.rs` | `AddOp<R>` guard; `label` on started + progress payloads | 1 |
| `src-tauri/src/commands/queue.rs` | pass label at `AddOp::new`; emit `queue-updated` after add | 1, 2 |
| `src-tauri/src/watcher.rs` | `batch_label` helper; pass label at `enqueue_and_start` | 1 |
| `src/lib/tauri.ts` | `label` on `AddStarted`, `AddProgress`, `AddActivity` | 3 |
| `src/hooks/useAddProgress.ts` | carry `label` on `activity` from both events | 3 |
| `src/components/AddingIndicator.tsx` | render the label prefix | 3 |
| `src/hooks/useFileIntake.ts` (new) | drop listener, classify, confirm queue, serialized pipeline | 4 |
| `src/components/DropZone.tsx` | presentational: props-driven confirm/status/label | 5 |
| `src/App.tsx` | mount `useFileIntake`, `switchToQueue`, thread `intake` | 5 |
| `src/pages/QueuePage.tsx` | pass `intake` to `DropZone`; drop `onFilesAdded` | 5 |
| `src/pages/QueuePage.test.tsx` | `label: ""` on the `adding` literal (T3); stub `intake` prop (T5) | 3, 5 |

---

## Task 1: Backend — `label` on `AddOp` (both events) + call sites

**Files:**
- Modify: `src-tauri/src/add_progress.rs` (payload structs, `AddOp`, tests)
- Modify: `src-tauri/src/commands/queue.rs:543` (add_files), `:589` (confirm_folder_add)
- Modify: `src-tauri/src/watcher.rs:358-387` (`enqueue_and_start` + new `batch_label`)

- [ ] **Step 1: Update the `add_progress.rs` tests to expect `label` on both events**

In `src-tauri/src/add_progress.rs`, replace the three existing tests' `AddOp::new(...)` calls and add label assertions. Replace the whole `#[cfg(test)] mod tests { ... }` body's three test fns with:

```rust
    #[test]
    fn emits_started_with_label_on_new_and_finished_on_drop() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let finished = record(&app, "add-finished");
        {
            let _op = AddOp::new(app.handle(), "My Folder".to_string());
            assert_eq!(started.lock().unwrap().len(), 1, "started fires immediately");
            let payload: serde_json::Value =
                serde_json::from_str(&started.lock().unwrap()[0]).unwrap();
            assert_eq!(payload["label"].as_str().unwrap(), "My Folder");
            assert_eq!(finished.lock().unwrap().len(), 0, "not finished yet");
        }
        assert_eq!(finished.lock().unwrap().len(), 1, "finished fires on drop");
    }

    #[test]
    fn finished_fires_even_on_early_return() {
        fn guarded(app: &tauri::AppHandle<tauri::test::MockRuntime>, bail: bool) {
            let _op = AddOp::new(app, String::new());
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
    fn report_emits_progress_with_op_id_and_label() {
        let app = mock_app();
        let started = record(&app, "add-started");
        let progress = record(&app, "add-progress");
        let op = AddOp::new(app.handle(), "Clips".to_string());
        op.report(1, 3);

        let started = started.lock().unwrap();
        let started_val: serde_json::Value = serde_json::from_str(&started[0]).unwrap();
        let op_id = started_val["op_id"].as_str().unwrap();

        let progress = progress.lock().unwrap();
        assert_eq!(progress.len(), 1);
        let first: serde_json::Value = serde_json::from_str(&progress[0]).unwrap();
        assert_eq!(first["op_id"].as_str().unwrap(), op_id, "progress carries the op's id");
        assert_eq!(first["label"].as_str().unwrap(), "Clips", "progress carries the label");
        assert_eq!(first["done"].as_u64().unwrap(), 1);
        assert_eq!(first["total"].as_u64().unwrap(), 3);
    }
```

- [ ] **Step 2: Run the Rust tests to verify they fail to compile**

Run: `cargo test --manifest-path src-tauri/Cargo.toml add_progress`
Expected: FAIL — compile error, `AddOp::new` takes 1 argument but 2 were supplied (signature not yet changed).

- [ ] **Step 3: Add `label` to the payloads and `AddOp`**

In `src-tauri/src/add_progress.rs`, change the two payload structs and the guard:

```rust
#[derive(Clone, Serialize)]
struct StartedPayload {
    op_id: String,
    label: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    op_id: String,
    label: String,
    done: u32,
    total: u32,
}
```

```rust
pub struct AddOp<R: Runtime> {
    app: AppHandle<R>,
    op_id: String,
    label: String,
}

impl<R: Runtime> AddOp<R> {
    pub fn new(app: &AppHandle<R>, label: String) -> Self {
        let op_id = Uuid::new_v4().to_string();
        let _ = app.emit(
            "add-started",
            StartedPayload {
                op_id: op_id.clone(),
                label: label.clone(),
            },
        );
        Self {
            app: app.clone(),
            op_id,
            label,
        }
    }

    /// Emit one per-file progress tick. `done` counts probed files so far; `total` is the
    /// probe-candidate count for this batch.
    pub fn report(&self, done: u32, total: u32) {
        let _ = self.app.emit(
            "add-progress",
            ProgressPayload {
                op_id: self.op_id.clone(),
                label: self.label.clone(),
                done,
                total,
            },
        );
    }
}
```

Leave the doc-comment on `AddOp` and the `Drop` impl unchanged.

- [ ] **Step 4: Update the three call sites**

In `src-tauri/src/commands/queue.rs`, `add_files` (~line 543):

```rust
        let op = crate::add_progress::AddOp::new(&app, String::new());
```

In `src-tauri/src/commands/queue.rs`, `confirm_folder_add` (~line 589) — derive the folder basename from `path` (native `\`/`/` handling, mirrors `scan_folder_inner`):

```rust
        let label = Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let op = crate::add_progress::AddOp::new(&app, label);
```

In `src-tauri/src/watcher.rs`, `enqueue_and_start` (~line 369):

```rust
        let op = crate::add_progress::AddOp::new(app, batch_label(&paths));
```

- [ ] **Step 5: Add the `batch_label` helper + its test in `watcher.rs`**

Add this function just above `enqueue_and_start` in `src-tauri/src/watcher.rs` (ensure `use std::path::Path;` is present in the file — it is used elsewhere in the module):

```rust
/// The basename of the single directory a batch of paths shares, or empty when the batch spans
/// multiple directories (e.g. a recursive reaper batch). Used only to name the intake scanner in
/// the UI, so an empty fallback is harmless.
fn batch_label(paths: &[String]) -> String {
    let mut parents = paths.iter().map(|p| Path::new(p).parent());
    let first = match parents.next() {
        Some(Some(p)) => p,
        _ => return String::new(),
    };
    if parents.all(|p| p == Some(first)) {
        first
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}
```

Add a test in `watcher.rs`'s `#[cfg(test)] mod tests` (create the section if the module's tests live elsewhere — search for `mod tests` in the file and append there):

```rust
    #[test]
    fn batch_label_names_a_single_dir_batch_and_empties_a_mixed_one() {
        assert_eq!(
            super::batch_label(&["/movies/SEOA/a.mp4".into(), "/movies/SEOA/b.mp4".into()]),
            "SEOA",
            "all-same-parent batch takes the parent's basename"
        );
        assert_eq!(
            super::batch_label(&["/movies/SEOA/a.mp4".into(), "/movies/Other/b.mp4".into()]),
            "",
            "a multi-directory batch has no single name"
        );
        assert_eq!(super::batch_label(&[]), "", "empty batch → empty label");
    }
```

- [ ] **Step 6: Run the Rust tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests, including `add_progress` and `batch_label_*`.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/add_progress.rs src-tauri/src/commands/queue.rs src-tauri/src/watcher.rs
git commit -m "feat: thread a label through AddOp add-started/add-progress events"
```

---

## Task 2: Backend — emit `queue-updated` after a drop-initiated add

**Files:**
- Modify: `src-tauri/src/commands/queue.rs` — `use` line, `add_files`, `confirm_folder_add`

**Note on verification:** the emit is a one-line mirror of the watcher's `enqueue_and_start` (`watcher.rs:386`), which is itself not unit-tested — the async `#[tauri::command]` wrappers need a fully-managed `AppState` + async runtime to exercise, which the codebase deliberately avoids (only the sync `_inner` fns are unit-tested). This emit is verified by the frontend integration (Task 5: the queue refreshes without `onFilesAdded`) and the manual smoke test in Task 6. No brittle command-wrapper unit test is added.

- [ ] **Step 1: Import the `Emitter` trait**

In `src-tauri/src/commands/queue.rs`, change the Tauri import:

```rust
use tauri::{AppHandle, Emitter, Manager, State};
```

- [ ] **Step 2: Emit `queue-updated` on the Ok path of `add_files`**

Rewrite `add_files` (~line 537) so it emits after a successful add (clone `app` before it moves into the closure):

```rust
#[tauri::command]
pub async fn add_files(app: AppHandle, paths: Vec<String>) -> Result<AddResult, String> {
    // add_files_inner runs a blocking HandBrakeCLI probe per file (source-media skip), so a large
    // drop would freeze the main-thread event loop. Offload to a blocking thread; the AddResult
    // still returns to the awaiting frontend. Same hazard the watcher avoids via scan_existing_background.
    let app_for_emit = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let op = crate::add_progress::AddOp::new(&app, String::new());
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(&state, &paths, Some(&reporter as &dyn Fn(u32, u32)))
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        // Mirror enqueue_and_start (watcher.rs) so useQueue refreshes without a frontend callback.
        let _ = app_for_emit.emit("queue-updated", ());
    }
    result
}
```

- [ ] **Step 3: Emit `queue-updated` on the Ok path of `confirm_folder_add`**

Rewrite `confirm_folder_add` (~line 581) the same way (keeping the label from Task 1):

```rust
#[tauri::command]
pub async fn confirm_folder_add(app: AppHandle, path: String) -> Result<AddResult, String> {
    if !Path::new(&path).is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // Both the recursive scan and the per-file probe block; run them off the main thread so
    // confirming a large folder doesn't freeze the UI (same hazard as add_files).
    let app_for_emit = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let label = Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let op = crate::add_progress::AddOp::new(&app, label);
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
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        let _ = app_for_emit.emit("queue-updated", ());
    }
    result
}
```

- [ ] **Step 4: Verify it still compiles and all Rust tests pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (no new tests; this confirms the `Emitter` import and the closure/clone changes compile).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/queue.rs
git commit -m "feat: emit queue-updated after add_files/confirm_folder_add so the queue refreshes reactively"
```

---

## Task 3: Frontend — `label` on the event types, `useAddProgress`, and `AddingIndicator`

These are one cohesive change: making `AddActivity.label` required means every constructor of it
(the hook and the `AddingIndicator` test literals) must change together, or an intermediate commit
would fail `tsc`. So types + hook + component ship in one task.

**Files:**
- Modify: `src/lib/tauri.ts` (`AddStarted`, `AddProgress`, `AddActivity`)
- Modify: `src/hooks/useAddProgress.ts`
- Test: `src/hooks/useAddProgress.test.ts`
- Modify: `src/components/AddingIndicator.tsx`
- Test: `src/components/AddingIndicator.test.tsx`

- [ ] **Step 1: Update both test files (failing)**

In `src/hooks/useAddProgress.test.ts`, replace the first three tests' bodies (the emit payloads gain `label`, and `activity` assertions gain `label`):

```ts
it("goes adding on start and clears on finish", () => {
  const { result } = renderHook(() => useAddProgress());
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();

  emit("add-started", { op_id: "a", label: "Folder" });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "a", label: "Folder", done: null, total: null });

  emit("add-progress", { op_id: "a", label: "Folder", done: 3, total: 10 });
  expect(result.current.activity).toEqual({ opId: "a", label: "Folder", done: 3, total: 10 });

  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();
});

it("stays adding until every overlapping op finishes", () => {
  const { result } = renderHook(() => useAddProgress());
  emit("add-started", { op_id: "a", label: "A" });
  emit("add-started", { op_id: "b", label: "B" });
  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(true); // b still open
  emit("add-finished", { op_id: "b" });
  expect(result.current.isAdding).toBe(false);
});

it("tolerates progress for an op whose start it missed, keeping its label", () => {
  // A watcher scan can emit add-started before the webview attaches listeners.
  const { result } = renderHook(() => useAddProgress());
  emit("add-progress", { op_id: "x", label: "Watched", done: 1, total: 4 });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "x", label: "Watched", done: 1, total: 4 });
});
```

Leave the fourth test ("ignores a stray finish for an unseen op") unchanged.

Then replace the entire contents of `src/components/AddingIndicator.test.tsx` with:

```tsx
import { it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import AddingIndicator from "./AddingIndicator";

it("renders nothing when idle", () => {
  const { container } = render(<AddingIndicator activity={null} />);
  expect(container.firstChild).toBeNull();
});

it("shows an indeterminate scanning label before the first count", () => {
  render(<AddingIndicator activity={{ opId: "a", label: "", done: null, total: null }} />);
  expect(screen.getByText(/scanning/i)).toBeInTheDocument();
});

it("shows the count and a filled bar during probing", () => {
  render(<AddingIndicator activity={{ opId: "a", label: "", done: 3, total: 12 }} />);
  expect(screen.getByText(/checking 3 of 12/i)).toBeInTheDocument();
  const fill = document.querySelector(".progress-bar-fill") as HTMLElement;
  expect(fill.style.width).toBe("25%");
});

it("prefixes the folder label when present", () => {
  render(<AddingIndicator activity={{ opId: "a", label: "SEOA", done: 3, total: 12 }} />);
  expect(screen.getByText(/SEOA · Checking 3 of 12/i)).toBeInTheDocument();
});
```

Finally, `src/pages/QueuePage.test.tsx` constructs an `AddActivity` literal that will fail `tsc`
once `label` is required (it is type-checked — `tsconfig.json` includes `src`, and CI's required
`frontend` check runs `npm run build`). Add `label: ""` to the `adding` prop on line 12:

```tsx
  render(<QueuePage hbStatus={null} adding={{ opId: "a", label: "", done: 1, total: 5 }} isAdding={true} />);
```

- [ ] **Step 2: Run both test files to verify they fail**

Run: `npx vitest run src/hooks/useAddProgress.test.ts src/components/AddingIndicator.test.tsx`
Expected: FAIL — `activity` lacks `label`, no label prefix rendered (and TS: `AddActivity` has no `label`).

- [ ] **Step 3: Add `label` to the event/activity types**

In `src/lib/tauri.ts`, update three interfaces:

```ts
export interface AddStarted {
  op_id: string;
  label: string;
}

export interface AddProgress {
  op_id: string;
  label: string;
  done: number;
  total: number;
}
```

```ts
// Frontend view of the current add operation. `done`/`total` are null during the
// indeterminate scan phase (before the first per-file probe tick). `label` is the folder
// name (empty for loose-file adds).
export interface AddActivity {
  opId: string;
  label: string;
  done: number | null;
  total: number | null;
}
```

- [ ] **Step 4: Carry `label` on `activity` in `useAddProgress`**

In `src/hooks/useAddProgress.ts`, update the two `setActivity` calls:

```ts
      listen<AddStarted>("add-started", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id);
        setActivity({ opId: payload.op_id, label: payload.label, done: null, total: null });
      }),
      listen<AddProgress>("add-progress", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id); // covers a start we never saw
        setActivity({ opId: payload.op_id, label: payload.label, done: payload.done, total: payload.total });
      }),
```

- [ ] **Step 5: Render the label prefix in `AddingIndicator`**

In `src/components/AddingIndicator.tsx`, replace everything **after** the `if (!activity) return null;`
guard (the `const determinate` line through the closing `);`) with:

```tsx
  const determinate = activity.total !== null && activity.total > 0;
  const percent = determinate
    ? Math.min(100, Math.round((activity.done! / activity.total!) * 100))
    : 0;
  const prefix = activity.label ? `${activity.label} · ` : "";

  return (
    <div className="adding-indicator">
      <div className="adding-indicator-label">
        <span className="spinner" aria-hidden="true" />
        <span>
          {prefix}
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
```

- [ ] **Step 6: Run both test files to verify they pass**

Run: `npx vitest run src/hooks/useAddProgress.test.ts src/components/AddingIndicator.test.tsx`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/lib/tauri.ts src/hooks/useAddProgress.ts src/hooks/useAddProgress.test.ts src/components/AddingIndicator.tsx src/components/AddingIndicator.test.tsx src/pages/QueuePage.test.tsx
git commit -m "feat: carry the intake label through add events and show it in the indicator"
```

> The `npm run build` (tsc over `src`) is not run until Task 5, but the `QueuePage.test.tsx`
> label fix above keeps this commit type-clean if it is — the required CI `frontend` check runs it.

---

## Task 4: Frontend — the `useFileIntake` hook (drop listener, confirm queue, serialized pipeline)

**Files:**
- Create: `src/hooks/useFileIntake.ts`
- Test: `src/hooks/useFileIntake.test.tsx`

This hook is created and tested in isolation; it is not wired into `App` until Task 5, so the app keeps building and behaving as before at this task's commit.

- [ ] **Step 1: Write the failing tests**

Create `src/hooks/useFileIntake.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// Capture the drag-drop callback the hook registers so tests can fire a "drop".
const dragBus = vi.hoisted(() => ({
  handler: null as null | ((e: { payload: { type: string; paths?: string[] } }) => void),
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({
    onDragDropEvent: (cb: (e: { payload: { type: string; paths?: string[] } }) => void) => {
      dragBus.handler = cb;
      return Promise.resolve(() => {
        dragBus.handler = null;
      });
    },
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import { useFileIntake } from "./useFileIntake";
import type { ClassifiedPaths, AddResult } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
let classified: ClassifiedPaths = { files: [], folders: [] };

function fireDrop(paths: string[]) {
  act(() => {
    dragBus.handler?.({ payload: { type: "drop", paths } });
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  dragBus.handler = null;
  classified = { files: [], folders: [] };
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "classify_paths":
        return Promise.resolve(classified);
      case "add_files":
        return Promise.resolve({ added: [], skipped: [] });
      case "confirm_folder_add":
        return Promise.resolve({ added: [], skipped: [] });
      case "start_queue":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("useFileIntake", () => {
  it("auto-adds loose files and ≤5-file folders, starts the queue, and switches to Queue on drop", async () => {
    classified = {
      files: ["/m/a.mp4"],
      folders: [{ file_count: 3, folder_name: "Clips", folder_path: "/clips" }],
    };
    const onDrop = vi.fn();
    renderHook(() => useFileIntake({ onDrop }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/clips"]);

    expect(onDrop).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/m/a.mp4"] }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/clips" }));
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
  });

  it("prompts for >5-file folders one at a time", async () => {
    classified = {
      files: [],
      folders: [
        { file_count: 12, folder_name: "A", folder_path: "/a" },
        { file_count: 20, folder_name: "B", folder_path: "/b" },
      ],
    };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a", "/b"]);

    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/a" });

    act(() => result.current.onAdd());
    // A advances to B synchronously; A's task is enqueued.
    expect(result.current.pendingConfirm?.folder_path).toBe("/b");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/a" }));

    act(() => result.current.onSkip());
    expect(result.current.pendingConfirm).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/b" });
  });

  it("runs the heavy adds sequentially — the second waits for the first to resolve", async () => {
    classified = {
      files: [],
      folders: [
        { file_count: 12, folder_name: "A", folder_path: "/a" },
        { file_count: 12, folder_name: "B", folder_path: "/b" },
      ],
    };
    const resolvers: Array<{ path: string; resolve: (v: AddResult) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: { path?: string }) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return new Promise<AddResult>((resolve) => resolvers.push({ path: args!.path!, resolve }));
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a", "/b"]);
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    act(() => result.current.onAdd()); // enqueue A, show B
    act(() => result.current.onAdd()); // enqueue B

    await waitFor(() => expect(resolvers).toHaveLength(1)); // only A started
    expect(resolvers[0].path).toBe("/a");

    await act(async () => {
      resolvers[0].resolve({ added: [], skipped: [] });
    });
    await waitFor(() => expect(resolvers).toHaveLength(2)); // B starts only after A resolves
    expect(resolvers[1].path).toBe("/b");
  });

  it("does not drop a folder when two separate drops' classify resolve back-to-back", async () => {
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    classified = { files: [], folders: [{ file_count: 12, folder_name: "A", folder_path: "/a" }] };
    fireDrop(["/a"]);
    classified = { files: [], folders: [{ file_count: 12, folder_name: "B", folder_path: "/b" }] };
    fireDrop(["/b"]);

    // Both folders are queued for confirmation; head is A, B waits behind it.
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    act(() => result.current.onSkip());
    expect(result.current.pendingConfirm?.folder_path).toBe("/b");
  });

  it("shows a per-reason skip summary after an add", async () => {
    classified = { files: ["/m/a.mp4", "/m/b.txt"], folders: [] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "add_files":
          return Promise.resolve({ added: [{ id: "1" }], skipped: [{ reason: "not_video", count: 1 }] });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/m/b.txt"]);

    await waitFor(() => expect(result.current.status).toBe("Added 1 · 1 skipped (not a video)"));
  });

  it("auto-adds loose files while a >5-file folder in the same drop still prompts", async () => {
    // Regression guard for the mixed-drop bug (old DropZone.test.tsx:117): the loose file must
    // auto-add and the big folder must still show a confirm — neither swallows the other.
    classified = {
      files: ["/m/a.mp4"],
      folders: [{ file_count: 12, folder_name: "Big", folder_path: "/big" }],
    };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/big"]);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/m/a.mp4"] }));
    expect(result.current.pendingConfirm?.folder_path).toBe("/big");
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
  });

  it("surfaces an error in the status line when a folder add fails", async () => {
    // Regression guard for old DropZone.test.tsx:178 — a failing confirm must not vanish silently.
    classified = { files: [], folders: [{ file_count: 12, folder_name: "Big", folder_path: "/big" }] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return Promise.reject(new Error("scan failed"));
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/big"));
    act(() => result.current.onAdd());

    await waitFor(() => expect(result.current.status).toMatch(/Error:.*scan failed/));
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/hooks/useFileIntake.test.tsx`
Expected: FAIL — `Cannot find module './useFileIntake'`.

- [ ] **Step 3: Implement the hook**

Create `src/hooks/useFileIntake.ts`:

```ts
import { useState, useEffect, useRef, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands, type FolderScanResult, type AddResult } from "../lib/tauri";
import { summarizeAdds } from "../lib/addSummary";

/** Folders with this many files or fewer are added without a confirm prompt. */
const AUTO_ADD_MAX = 5;

type AddTask =
  | { kind: "files"; paths: string[] }
  | { kind: "folder"; folder: FolderScanResult };

export interface FileIntake {
  pendingConfirm: FolderScanResult | null;
  onAdd: () => void;
  onSkip: () => void;
  status: string | null;
  isDragOver: boolean;
}

/**
 * Owns the whole drag-drop intake pipeline. Mounted in always-mounted `App` so drops work on
 * any tab (a drop calls `onDrop` to switch to Queue) and confirm/scan state survives tab
 * switches. The heavy scan/probe work runs through a single serialized pipeline: each
 * `add_files`/`confirm_folder_add` is awaited before the next, so a new drop appends and never
 * interrupts the folder currently being scanned. `confirmQueueRef` is the source of truth for
 * what awaits confirmation; the shown prompt is always its head.
 */
export function useFileIntake(opts: { onDrop: () => void }): FileIntake {
  const [isDragOver, setIsDragOver] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [, forceRender] = useState(0);
  const bump = useCallback(() => forceRender((n) => n + 1), []);

  const confirmQueueRef = useRef<FolderScanResult[]>([]);
  const taskQueueRef = useRef<AddTask[]>([]);
  const runningRef = useRef(false);
  const statusTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Keep the switch callback in a ref so the drag-drop effect can register exactly once.
  const onDropRef = useRef(opts.onDrop);
  onDropRef.current = opts.onDrop;

  // Always clear the prior auto-clear timer before setting a new status, so a later task's
  // summary can never be wiped by an earlier task's stale 4s timer.
  const setStatusMsg = useCallback((text: string | null, autoClear = false) => {
    if (statusTimerRef.current) {
      clearTimeout(statusTimerRef.current);
      statusTimerRef.current = null;
    }
    setStatus(text);
    if (autoClear && text) {
      statusTimerRef.current = setTimeout(() => setStatus(null), 4000);
    }
  }, []);

  // Drains the task queue one at a time. The while loop re-reads the live ref each iteration,
  // and runningRef is cleared synchronously (no await between the last check and the clear),
  // so a task pushed by enqueue() is always either picked up by the running drain or starts a
  // fresh one — never stranded.
  const runNext = useCallback(async () => {
    if (runningRef.current) return;
    runningRef.current = true;
    try {
      while (taskQueueRef.current.length > 0) {
        const task = taskQueueRef.current.shift()!;
        try {
          const res: AddResult =
            task.kind === "files"
              ? await commands.addFiles(task.paths)
              : await commands.confirmFolderAdd(task.folder.folder_path);
          await commands.startQueue();
          // summarizeAdds returns string | null; a null status renders nothing.
          setStatusMsg(summarizeAdds([res]), true);
        } catch (e) {
          setStatusMsg(`Error: ${e}`, true);
        }
      }
    } finally {
      runningRef.current = false;
    }
  }, [setStatusMsg]);

  const enqueue = useCallback(
    (task: AddTask) => {
      taskQueueRef.current.push(task);
      void runNext();
    },
    [runNext],
  );

  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatusMsg("Adding…"); // immediate feedback so a slow classify walk isn't dead air
      let classified;
      try {
        classified = await commands.classifyPaths(paths);
      } catch (e) {
        setStatusMsg(`Error: ${e}`, true);
        return;
      }
      setStatusMsg(null); // clear the placeholder; tasks report via the scanner + summaries
      if (classified.files.length > 0) {
        enqueue({ kind: "files", paths: classified.files });
      }
      for (const folder of classified.folders) {
        if (folder.file_count === 0) continue;
        if (folder.file_count <= AUTO_ADD_MAX) {
          enqueue({ kind: "folder", folder });
        } else {
          confirmQueueRef.current.push(folder); // push, never replace — the anti-clobber invariant
          bump();
        }
      }
    },
    [enqueue, setStatusMsg, bump],
  );

  const handlePathsRef = useRef(handlePaths);
  handlePathsRef.current = handlePaths;

  // Register the single window-level listener once (empty deps + refs), StrictMode-safe.
  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const unlisten = appWindow.onDragDropEvent((event) => {
      if (event.payload.type === "over" || event.payload.type === "enter") {
        setIsDragOver(true);
      } else if (event.payload.type === "drop") {
        setIsDragOver(false);
        onDropRef.current();
        void handlePathsRef.current(event.payload.paths);
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const onAdd = useCallback(() => {
    const folder = confirmQueueRef.current[0];
    if (!folder) return;
    confirmQueueRef.current.shift();
    bump();
    enqueue({ kind: "folder", folder });
  }, [enqueue, bump]);

  const onSkip = useCallback(() => {
    if (confirmQueueRef.current.length === 0) return;
    confirmQueueRef.current.shift();
    bump();
  }, [bump]);

  const pendingConfirm = confirmQueueRef.current[0] ?? null;

  return { pendingConfirm, onAdd, onSkip, status, isDragOver };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npx vitest run src/hooks/useFileIntake.test.tsx`
Expected: PASS (all 7 tests).

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useFileIntake.ts src/hooks/useFileIntake.test.tsx
git commit -m "feat: add useFileIntake hook (serialized pipeline + confirm queue + cross-tab drop)"
```

---

## Task 5: Frontend — wire the hook into App/QueuePage; make DropZone presentational

**Files:**
- Modify: `src/components/DropZone.tsx` (rewrite as presentational)
- Modify: `src/pages/QueuePage.tsx` (pass `intake` to `DropZone`, drop `onFilesAdded`)
- Modify: `src/App.tsx` (mount `useFileIntake`, pass `switchToQueue`, thread `intake`)
- Test: `src/components/DropZone.test.tsx` (rewrite as presentational)
- Test: `src/pages/QueuePage.test.tsx` (pass a stub `intake` prop — now required)

- [ ] **Step 1: Rewrite `DropZone.test.tsx` as presentational (failing)**

Replace the entire contents of `src/components/DropZone.test.tsx` with:

```tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import DropZone from "./DropZone";

const noop = () => {};

describe("DropZone (presentational)", () => {
  it("shows the drop label when idle", () => {
    render(<DropZone pendingConfirm={null} status={null} isDragOver={false} onAdd={noop} onSkip={noop} />);
    expect(screen.getByText(/drop video files or folders here/i)).toBeInTheDocument();
  });

  it("shows the status line when set and nothing is pending", () => {
    render(<DropZone pendingConfirm={null} status={"Added 1"} isDragOver={false} onAdd={noop} onSkip={noop} />);
    expect(screen.getByText("Added 1")).toBeInTheDocument();
    expect(screen.queryByText(/drop video files/i)).not.toBeInTheDocument();
  });

  it("renders the confirm prompt and wires Add/Skip to the handlers", async () => {
    const onAdd = vi.fn();
    const onSkip = vi.fn();
    render(
      <DropZone
        pendingConfirm={{ file_count: 12, folder_name: "Big", folder_path: "/big" }}
        status={null}
        isDragOver={false}
        onAdd={onAdd}
        onSkip={onSkip}
      />,
    );
    expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(onAdd).toHaveBeenCalledTimes(1);
    expect(onSkip).toHaveBeenCalledTimes(1);
  });

  it("shows the confirm prompt even when a status is also set", () => {
    // Guards the render-gating bug (old DropZone.test.tsx:117): the confirm and a live status
    // can coexist, so the confirm must not be hidden behind the status branch.
    render(
      <DropZone
        pendingConfirm={{ file_count: 12, folder_name: "Big", folder_path: "/big" }}
        status={"Added 1"}
        isDragOver={false}
        onAdd={noop}
        onSkip={noop}
      />,
    );
    expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument();
    expect(screen.getByText("Added 1")).toBeInTheDocument();
  });

  it("applies drag-over styling", () => {
    const { container } = render(
      <DropZone pendingConfirm={null} status={null} isDragOver={true} onAdd={noop} onSkip={noop} />,
    );
    expect(container.querySelector(".drop-zone.drag-over")).not.toBeNull();
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npx vitest run src/components/DropZone.test.tsx`
Expected: FAIL — TS/runtime error: `DropZone` still expects `onFilesAdded`, not the new props.

- [ ] **Step 3: Rewrite `DropZone.tsx` as presentational**

Replace the entire contents of `src/components/DropZone.tsx` with:

```tsx
import { FolderScanResult } from "../lib/tauri";

interface DropZoneProps {
  pendingConfirm: FolderScanResult | null;
  onAdd: () => void;
  onSkip: () => void;
  status: string | null;
  isDragOver: boolean;
}

/**
 * Presentational drop surface. All intake orchestration lives in `useFileIntake` (App-owned);
 * this component just renders the confirm prompt / transient status / label three-way switch
 * and calls the passed handlers. The window-level drag-drop listener lives in the hook, so this
 * component is never the reason a drop is or isn't captured.
 */
export default function DropZone({ pendingConfirm, onAdd, onSkip, status, isDragOver }: DropZoneProps) {
  return (
    <div className={`drop-zone ${isDragOver ? "drag-over" : ""}`}>
      {pendingConfirm ? (
        <div className="folder-confirm">
          {status && <span className="drop-zone-status">{status}</span>}
          <div className="folder-confirm-item">
            <span>
              Add {pendingConfirm.file_count} files from &quot;{pendingConfirm.folder_name}&quot;?
            </span>
            <div className="folder-confirm-actions">
              <button className="btn btn-small" onClick={onAdd}>
                Add
              </button>
              <button className="btn btn-small btn-dim" onClick={onSkip}>
                Skip
              </button>
            </div>
          </div>
        </div>
      ) : status ? (
        <span className="drop-zone-status">{status}</span>
      ) : (
        <span className="drop-zone-label">Drop video files or folders here</span>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Wire `useFileIntake` into `App.tsx`**

In `src/App.tsx`, add **only** the `useFileIntake` import (`useAddProgress` is already imported —
do not duplicate it):

```tsx
import { useFileIntake } from "./hooks/useFileIntake";
```

Inside `App`, after the existing `useAddProgress` line:

```tsx
  const { isAdding, activity } = useAddProgress();
  const intake = useFileIntake({ onDrop: () => setActiveTab("queue") });
```

And update the Queue tab render:

```tsx
        {activeTab === "queue" && (
          <QueuePage hbStatus={hbStatus} adding={activity} isAdding={isAdding} intake={intake} />
        )}
```

- [ ] **Step 5: Thread `intake` through `QueuePage.tsx`**

In `src/pages/QueuePage.tsx`, add **only** the `FileIntake` import (`HandbrakeStatus` and
`AddActivity` are already imported at the top of the file — do not duplicate them):

```tsx
import type { FileIntake } from "../hooks/useFileIntake";
```

Extend the props interface and destructuring:

```tsx
interface QueuePageProps {
  hbStatus: HandbrakeStatus | null;
  adding: AddActivity | null;
  isAdding: boolean;
  intake: FileIntake;
}

export default function QueuePage({ hbStatus, adding, isAdding, intake }: QueuePageProps) {
```

Replace the `<DropZone .../>` usage (currently `<DropZone onFilesAdded={refresh} />`) with:

```tsx
      <DropZone
        pendingConfirm={intake.pendingConfirm}
        onAdd={intake.onAdd}
        onSkip={intake.onSkip}
        status={intake.status}
        isDragOver={intake.isDragOver}
      />
```

Leave `useQueue`'s `refresh` in place — it is still used by `handleDrop` (reorder) and the Clear button, and it now also refreshes reactively via the backend `queue-updated` event.

- [ ] **Step 5b: Give `QueuePage.test.tsx` the now-required `intake` prop**

`QueuePage` now requires `intake`, so its three renders in `src/pages/QueuePage.test.tsx` fail to
type-check and throw at runtime (the JSX evaluates `intake.pendingConfirm`). Add a shared stub at
the top of the file and pass it to all three renders:

```tsx
const intakeStub = { pendingConfirm: null, onAdd: vi.fn(), onSkip: vi.fn(), status: null, isDragOver: false };
```

Then add `intake={intakeStub}` to each `<QueuePage ... />` render, e.g.:

```tsx
  render(<QueuePage hbStatus={null} adding={{ opId: "a", label: "", done: 1, total: 5 }} isAdding={true} intake={intakeStub} />);
```

(The `DropZone` mock already stubs the child, so the stub is only consumed by `QueuePage`'s prop wiring.)

- [ ] **Step 6: Run the full frontend suite + type-check**

Run: `npm test`
Expected: PASS — all frontend tests (DropZone presentational, useFileIntake, useAddProgress, AddingIndicator, and untouched suites).

Run: `npm run build`
Expected: PASS — `tsc` type-check clean (no `onFilesAdded`/prop mismatches) and vite build succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/components/DropZone.tsx src/components/DropZone.test.tsx src/pages/QueuePage.tsx src/pages/QueuePage.test.tsx src/App.tsx
git commit -m "feat: wire useFileIntake into App; make DropZone presentational; drop onFilesAdded"
```

---

## Task 6: Full verification + manual smoke

**Files:** none (verification only)

- [ ] **Step 1: Run the entire test suite (frontend + Rust)**

Run: `npm test`
Expected: PASS.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

Run: `npm run build`
Expected: PASS.

- [ ] **Step 2: Manual smoke (drives the actual feature)**

Run: `npm run tauri dev`

Verify, using three sizeable folders:
1. Drop folder 1 → its confirm prompt appears in the drop zone (`"<name>" · N files [Add][Skip]`).
2. Click **Add** → the prompt reverts to "Drop video files or folders here" immediately; the scanner card below shows `"<folder>" · Checking X of N`.
3. While folder 1 is still scanning, drop folder 2 → folder 1's scan is NOT interrupted; folder 2's confirm appears; on Add it queues behind folder 1.
4. Drop several folders quickly → confirms appear one at a time; none are lost.
5. Switch to the History/Watch tab and drop a folder → the app auto-switches to Queue and processes it.
6. Confirm the queue list (Pending N) updates after each add without a manual refresh.

- [ ] **Step 3: Confirm the diff is clean and formatted**

Run: `git diff --stat main`
Expected: only the files listed in this plan are touched.

Run: `git status`
Expected: clean working tree (all commits made).

---

## Notes for the implementer

- **Windows path separators:** the folder-label derivation uses `Path::file_name()` (native separator handling), and `batch_label` compares `Path::parent()` values — no hardcoded `/`. Do not introduce string-split path logic.
- **ACL:** no new permission — the events are app-emitted and the frontend only `listen`/`invoke`s existing app commands; `core:event` listen/unlisten is already granted.
- **Serialization invariant:** never insert an `await` between the `while` loop exiting and `runningRef.current = false` in `runNext` — that gap must stay synchronous, or a concurrently-enqueued task can be stranded.
- **Do not** re-add an `onDragDropEvent` listener anywhere but `useFileIntake` — a second registration would double-process every drop.
- **Intended behavior change (do not "fix"):** the old `DropZone` batched loose files + all ≤5
  folders into one `add_files`/`confirm_folder_add` sweep with a single combined summary and one
  `start_queue`. The serialized pipeline runs each as its own task, so the status line shows
  per-task summaries (the last one wins) and `start_queue` is called once per task. This is
  inherent to serialization and matches the spec — the status line is single-line by design, and
  a new drop's "Adding…"→null can transiently overwrite a prior task's summary. Acceptable.
