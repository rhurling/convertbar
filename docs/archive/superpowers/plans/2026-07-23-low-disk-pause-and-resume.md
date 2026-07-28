# Low-Disk Auto-Pause + Queue Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the user a visible way to resume a stopped queue, and automatically pause the queue before starting a file whenever the destination disk lacks enough free space.

**Architecture:** Two related changes. (1) A frontend-only "Resume" button in the Queue page's Pending section, shown when the queue is stopped (`!activeJob`) but jobs remain `queued`; it calls the existing idempotent `start_queue` command. (2) A pre-spawn disk-space gate inside `process_queue`: before starting each job's encode, if a configured threshold is set and the destination filesystem's available space is below `floor + 2×source_size`, the run stops (leaving the job `queued`, exactly like "Pause after this") and emits an event so the UI can explain why. The Resume button from change 1 is the unpause affordance.

**Tech Stack:** Rust (Tauri 2, rusqlite, `fs4` for cross-platform free-space queries), React + TypeScript, Vitest, `cargo test`.

**Branch:** `feature/low-disk-pause-and-resume` (main is protected; PR + admin squash merge per CLAUDE.md).

**Key design decisions (already settled):**
- Headroom is a hardcoded `2×` of the next file's source size — a deliberate safety margin (the encoded output is normally smaller than the source but not guaranteed, and an in-place encode writes its temp output before the source is removed, so source + temp briefly coexist on the same filesystem). It over-reserves in the common case (an in-place/same-disk encode needs only ~1× source of *new* free space; a cross-disk destination also ~1×); that conservatism is intentional and safe, and matches the user's requested `floor + 2×source` formula.
- Threshold unit is GB where 1 GB = 1024³ bytes (matches the app's existing `formatBytes`/`format_bytes_short`). Stored as `low_disk_min_gb` (f64); `0` (or negative/unparseable) = disabled.
- The check runs **before spawning** the next encode, never mid-encode. No `SIGSTOP` of a running process.
- Fail **open**: if the threshold is 0/disabled, or the destination parent can't be resolved, or the free-space query errors, the encode proceeds. Uncertainty must never wedge the queue.
- ~~Surfacing the pause reason is **event-only** (`queue-paused-low-disk`) — no new command, no durable backend state.~~ **[Superseded by the implementation — do not read this as the shipped architecture.]** The shipped code *does* keep durable state and *does* add a command: `ConverterState.low_disk_pause: Mutex<Option<LowDiskPause>>` in `converter.rs`, read via a `get_low_disk_pause` command (`commands/converter.rs`, registered in `lib.rs`), which `QueuePage.tsx` calls on mount to seed the banner. The plan's own concern — that the Queue tab unmounts on tab switch (`App.tsx:43`) and the message can be missed — is exactly why: the event alone was not enough, so the reason was made queryable. The self-healing re-emit on Resume still holds as a secondary path.

---

## File Structure

**Change 1 (Resume button):**
- Modify `src/pages/QueuePage.tsx` — add the Resume button + (change 2) the low-disk banner + event listener.
- Modify `src/App.css` — one additive rule for the button group.
- Modify `src/pages/QueuePage.test.tsx` — restructure the `useQueue` mock to be per-test controllable; add Resume-button + banner tests.

**Change 2 (disk gate):**
- Modify `src-tauri/Cargo.toml` — add `fs4` dependency.
- Modify `src-tauri/src/converter.rs` — pure helpers (`required_free_bytes`, `has_enough_disk`, `gb_to_bytes`), the `destination_available_bytes` wrapper, the `get_low_disk_min_gb` reader, and the gate in `process_queue`; unit + integration tests.
- Modify `src-tauri/src/db.rs` — seed the `low_disk_min_gb` default; test it.
- Modify `src-tauri/src/types.rs` — `Settings.low_disk_min_gb: f64`.
- Modify `src-tauri/src/commands/settings.rs` — parse `low_disk_min_gb` in `get_settings`; add to `ALLOWED_KEYS`.
- Modify `src/lib/tauri.ts` — `AppSettings.low_disk_min_gb: number`.
- Modify `src/hooks/useSettings.ts` — extend `coerceSettingValue` to handle numbers.
- Modify `src/hooks/useSettings.test.ts` + `src/pages/SettingsPage.test.tsx` — add `low_disk_min_gb` to the `makeSettings` factories; add numeric-coerce + input tests.
- Modify `src/pages/SettingsPage.tsx` — the threshold number input.

---

## Task 1: Resume button when the queue is stopped with pending work (Change 1)

Frontend-only, independently shippable. `start_queue` (`src-tauri/src/commands/converter.rs:7`) already no-ops if the queue is running, so showing the button whenever `!activeJob && pendingJobs.length > 0` is safe. `start_queue` is an app-defined command → ACL-exempt, no `default.json` change.

**Files:**
- Modify: `src/pages/QueuePage.tsx`
- Modify: `src/App.css`
- Test: `src/pages/QueuePage.test.tsx`

- [ ] **Step 1: Restructure the QueuePage test mock + add the Resume tests (failing)**

Replace the entire contents of `src/pages/QueuePage.test.tsx` with:

```tsx
import { it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";

// Per-test controllable queue state (reset in beforeEach).
let queueMock: {
  activeJob: unknown;
  pendingJobs: unknown[];
  progress: unknown;
  refresh: () => void;
};

vi.mock("../hooks/useQueue", () => ({ useQueue: () => queueMock }));
vi.mock("../components/DropZone", () => ({ default: () => <div data-testid="dropzone" /> }));
vi.mock("../components/ActiveJob", () => ({ default: () => <div data-testid="active-job" /> }));
vi.mock("../components/QueueItem", () => ({ default: () => <div data-testid="queue-item" /> }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock("../lib/tauri", () => ({
  commands: {
    startQueue: vi.fn(() => Promise.resolve()),
    clearQueue: vi.fn(() => Promise.resolve()),
    reorderQueue: vi.fn(() => Promise.resolve()),
  },
}));

import QueuePage from "./QueuePage";
import { commands } from "../lib/tauri";
import { listen } from "@tauri-apps/api/event";

const intakeStub = { pendingConfirm: null, onAdd: vi.fn(), onSkip: vi.fn(), status: null, isDragOver: false };

beforeEach(() => {
  vi.clearAllMocks();
  // clearAllMocks resets call history but NOT implementations, so restore the default listen
  // stub each test (the banner test overrides it and must not leak into later tests).
  vi.mocked(listen).mockImplementation(() => Promise.resolve(() => {}));
  queueMock = { activeJob: null, pendingJobs: [], progress: null, refresh: vi.fn() };
});

it("suppresses the empty-state while an add is in progress", () => {
  render(
    <QueuePage
      hbStatus={null}
      adding={{ opId: "a", label: "", done: 1, total: 5 }}
      isAdding={true}
      intake={intakeStub}
    />,
  );
  expect(screen.queryByText(/drag video files or folders here to get started/i)).toBeNull();
  expect(screen.getByText(/checking 1 of 5/i)).toBeInTheDocument();
});

it("shows the empty-state when idle", () => {
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  expect(screen.getByText(/drag video files or folders here to get started/i)).toBeInTheDocument();
});

it("suppresses the empty-state while adding even before the first progress tick", () => {
  render(<QueuePage hbStatus={null} adding={null} isAdding={true} intake={intakeStub} />);
  expect(screen.queryByText(/drag video files or folders here to get started/i)).toBeNull();
});

it("shows a Resume button and starts the queue when stopped with pending jobs", async () => {
  queueMock = {
    activeJob: null,
    pendingJobs: [{ id: "j1", source_path: "/m/a.mp4", status: "queued" }],
    progress: null,
    refresh: vi.fn(),
  };
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  const resume = screen.getByRole("button", { name: /^resume$/i });
  fireEvent.click(resume);
  await waitFor(() => expect(commands.startQueue).toHaveBeenCalledTimes(1));
});

it("hides the Resume button while a job is active", () => {
  queueMock = {
    activeJob: { id: "a", source_path: "/m/a.mp4", status: "encoding" },
    pendingJobs: [{ id: "j1", source_path: "/m/b.mp4", status: "queued" }],
    progress: null,
    refresh: vi.fn(),
  };
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  expect(screen.queryByRole("button", { name: /^resume$/i })).toBeNull();
});

it("shows a low-disk banner when the queue-paused-low-disk event fires", async () => {
  let handler: ((e: { payload: unknown }) => void) | undefined;
  vi.mocked(listen).mockImplementation(((name: string, cb: (e: { payload: unknown }) => void) => {
    if (name === "queue-paused-low-disk") handler = cb;
    return Promise.resolve(() => {});
  }) as typeof listen);
  queueMock = {
    activeJob: null,
    pendingJobs: [{ id: "j1", source_path: "/m/a.mp4", status: "queued" }],
    progress: null,
    refresh: vi.fn(),
  };
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  await waitFor(() => expect(handler).toBeDefined());
  act(() =>
    handler!({ payload: { path: "/m/out.mp4", available_bytes: 3_000_000_000, required_bytes: 5_000_000_000 } }),
  );
  expect(screen.getByText(/free on the destination/i)).toBeInTheDocument();
});
```

- [ ] **Step 2: Run the tests to verify the new ones fail**

Run: `npm test -- src/pages/QueuePage.test.tsx`
Expected: the three original tests PASS. "shows a Resume button…" and "shows a low-disk banner…" FAIL (no button / banner yet). "hides the Resume button while a job is active" PASSES **vacuously** (there is no Resume button to find pre-implementation) — that is expected; it gains teeth only after Step 3, where it verifies the `!activeJob` guard actually hides the button. Do not treat its pass here as a checkpoint failure.

- [ ] **Step 3: Implement the Resume button + low-disk banner in QueuePage.tsx**

Replace the entire contents of `src/pages/QueuePage.tsx` with:

```tsx
import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";
import { commands } from "../lib/tauri";
import type { HandbrakeStatus } from "../lib/tauri";
import { formatBytes } from "../lib/format";
import AddingIndicator from "../components/AddingIndicator";
import type { AddActivity } from "../lib/tauri";
import type { FileIntake } from "../hooks/useFileIntake";

interface QueuePageProps {
  hbStatus: HandbrakeStatus | null;
  adding: AddActivity | null;
  isAdding: boolean;
  intake: FileIntake;
}

interface LowDiskPayload {
  path: string;
  available_bytes: number;
  required_bytes: number;
}

export default function QueuePage({ hbStatus, adding, isAdding, intake }: QueuePageProps) {
  const { activeJob, pendingJobs, progress, refresh } = useQueue();
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [lowDiskMsg, setLowDiskMsg] = useState<string | null>(null);

  // The queue stops itself before starting a file when the destination disk is low; surface why.
  useEffect(() => {
    const un = listen<LowDiskPayload>("queue-paused-low-disk", (e) => {
      setLowDiskMsg(
        `Only ${formatBytes(e.payload.available_bytes)} free on the destination — ` +
          `need ${formatBytes(e.payload.required_bytes)} to start the next file. ` +
          `Free up space, then Resume.`,
      );
    });
    return () => {
      un.then((u) => u());
    };
  }, []);

  // A restarted queue (an active job appears) clears the low-disk notice.
  useEffect(() => {
    if (activeJob) setLowDiskMsg(null);
  }, [activeJob]);

  const handleDrop = async (draggedId: string, targetId: string) => {
    setDragOverId(null);
    const ids = pendingJobs.map((j) => j.id);
    const fromIdx = ids.indexOf(draggedId);
    const toIdx = ids.indexOf(targetId);
    if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return;
    ids.splice(fromIdx, 1);
    ids.splice(toIdx, 0, draggedId);
    await commands.reorderQueue(ids);
    refresh();
  };

  return (
    <div className="queue-page">
      {hbStatus && !hbStatus.found && (
        <div className="hb-warning">
          <span className="hb-warning-icon">&#9888;&#65039;</span>
          <div>
            <strong>HandBrakeCLI not found</strong>
            <p>Install via: <code>brew install handbrake</code> or set the path in Settings.</p>
          </div>
        </div>
      )}
      <DropZone
        pendingConfirm={intake.pendingConfirm}
        onAdd={intake.onAdd}
        onSkip={intake.onSkip}
        status={intake.status}
        isDragOver={intake.isDragOver}
      />

      <AddingIndicator activity={adding} />

      {lowDiskMsg && (
        <div className="hb-warning">
          <span className="hb-warning-icon">&#9888;&#65039;</span>
          <div>
            <strong>Queue paused — low disk space</strong>
            <p>{lowDiskMsg}</p>
          </div>
        </div>
      )}

      {activeJob && <ActiveJob job={activeJob} progress={progress} />}

      {pendingJobs.length > 0 && (
        <div className="section">
          <div className="section-header">
            <span>Pending ({pendingJobs.length})</span>
            <div className="section-header-actions">
              {!activeJob && (
                <button
                  className="btn btn-small"
                  onClick={async () => {
                    await commands.startQueue();
                    refresh();
                  }}
                >
                  Resume
                </button>
              )}
              <button
                className="btn btn-small btn-dim"
                onClick={async () => {
                  await commands.clearQueue();
                  refresh();
                }}
              >
                Clear
              </button>
            </div>
          </div>
          <div className="item-list">
            {pendingJobs.map((job) => (
              <QueueItem
                key={job.id}
                job={job}
                onRemoved={refresh}
                onDragStart={() => {}}
                onDragOver={(id) => setDragOverId(id)}
                onDrop={handleDrop}
                isDragOver={dragOverId === job.id}
              />
            ))}
          </div>
        </div>
      )}

      {!isAdding && !activeJob && pendingJobs.length === 0 && (
        <div className="empty-state">
          <span className="empty-state-icon">&#128194;</span>
          <span>Drag video files or folders here to get started</span>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Add the button-group CSS rule**

In `src/App.css`, append this additive rule (does not modify any existing selector):

```css
.section-header-actions {
  display: flex;
  gap: 8px;
}
```

- [ ] **Step 5: Run the QueuePage tests to verify they pass**

Run: `npm test -- src/pages/QueuePage.test.tsx`
Expected: all six tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/pages/QueuePage.tsx src/pages/QueuePage.test.tsx src/App.css
git commit -m "feat: add Resume button when the queue is stopped with pending jobs"
```

---

## Task 2: Cross-platform disk-space helpers (pure) + fs4 dependency

Pure, unit-testable decision logic isolated from the real filesystem, mirroring the codebase's `decide_cleanup` / `final_run_status` style. The real free-space query is a thin wrapper tested indirectly via Task 4.

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/converter.rs`
- Test: `src-tauri/src/converter.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the fs4 dependency**

In `src-tauri/Cargo.toml`, under `[dependencies]` (after the `dunce`/`tauri-plugin-opener` lines), add:

```toml
fs4 = { version = "0.13", features = ["sync"] }
```

`fs4` is a maintained fork of `fs2` (backed by `rustix`, not libc) and works on macOS, Windows, and Linux. It exposes a top-level `available_space(path) -> io::Result<u64>` returning the bytes available to a non-privileged user on the filesystem containing `path`.

- [ ] **Step 2: Write the failing unit tests for the pure helpers**

In `src-tauri/src/converter.rs`, inside the existing `#[cfg(test)] mod tests { ... }` block (e.g. after `final_run_status_is_error_only_when_a_job_failed`), add:

```rust
#[test]
fn required_free_bytes_adds_double_the_source_to_the_floor() {
    // Peak disk during an in-place encode is source + temp ≈ 2× source, on top of the floor.
    assert_eq!(required_free_bytes(1000, 500), 1000 + 1000);
    // Unknown/zero source size degrades to the bare floor.
    assert_eq!(required_free_bytes(1000, 0), 1000);
}

#[test]
fn required_free_bytes_saturates_instead_of_wrapping() {
    assert_eq!(required_free_bytes(u64::MAX, 10), u64::MAX);
    assert_eq!(required_free_bytes(10, u64::MAX), u64::MAX);
}

#[test]
fn has_enough_disk_is_true_only_at_or_above_the_requirement() {
    // floor 1000 + 2*500 = 2000 required.
    assert!(has_enough_disk(2000, 1000, 500));
    assert!(has_enough_disk(2001, 1000, 500));
    assert!(!has_enough_disk(1999, 1000, 500));
}

#[test]
fn gb_to_bytes_converts_and_clamps() {
    assert_eq!(gb_to_bytes(1.0), 1024 * 1024 * 1024);
    // Disabled / nonsense values clamp to 0 (never panic, never wrap).
    assert_eq!(gb_to_bytes(0.0), 0);
    assert_eq!(gb_to_bytes(-5.0), 0);
    // Absurd values saturate rather than panicking (used by the Task 4 integration test).
    assert_eq!(gb_to_bytes(f64::MAX), u64::MAX);
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib required_free_bytes gb_to_bytes has_enough_disk`
Expected: FAIL to compile — `required_free_bytes`, `has_enough_disk`, `gb_to_bytes` not found.

- [ ] **Step 4: Implement the pure helpers + the free-space wrapper**

In `src-tauri/src/converter.rs`, add near the other module-level helpers (e.g. just above `fn get_next_job`):

```rust
/// Peak disk headroom multiplier applied to the next file's source size. An in-place re-encode
/// writes its temp output alongside the still-present source on the same filesystem, so usage
/// peaks at ~2× the source before cleanup removes one.
const LOW_DISK_HEADROOM_FACTOR: u64 = 2;

/// Bytes that must remain free on a job's destination filesystem before its encode may start:
/// the user's configured floor plus headroom for the encode itself. Saturating so an enormous
/// configured floor (or source size) can't wrap.
pub(crate) fn required_free_bytes(reserve_floor: u64, source_size: u64) -> u64 {
    reserve_floor.saturating_add(source_size.saturating_mul(LOW_DISK_HEADROOM_FACTOR))
}

/// Whether `available` free bytes clear the reserve floor plus the encode headroom.
pub(crate) fn has_enough_disk(available: u64, reserve_floor: u64, source_size: u64) -> bool {
    available >= required_free_bytes(reserve_floor, source_size)
}

/// GiB (1024³ bytes), as configured in settings, to bytes. `f64 as u64` saturates at `u64::MAX`
/// and clamps negatives/zero to 0, so a garbage or huge stored value can't panic or wrap.
pub(crate) fn gb_to_bytes(gb: f64) -> u64 {
    if gb <= 0.0 {
        return 0;
    }
    (gb * 1024.0 * 1024.0 * 1024.0) as u64
}

/// Free bytes available to this process on the filesystem holding `output_path`'s parent
/// directory (the output file itself does not exist yet). `None` when the parent can't be
/// resolved or the platform query fails — the caller treats that as "don't block the queue".
fn destination_available_bytes(output_path: &str) -> Option<u64> {
    let parent = std::path::Path::new(output_path).parent()?;
    fs4::available_space(parent).ok()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib required_free_bytes gb_to_bytes has_enough_disk`
Expected: 4 tests PASS. (`destination_available_bytes` is exercised by Task 4.)

Note: `destination_available_bytes` is unused until Task 4 — the compiler will warn `function is never used`. That is expected and resolved in Task 4; do not add `#[allow(dead_code)]`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/converter.rs
git commit -m "feat: add cross-platform disk-space helpers via fs4"
```

---

## Task 3: `low_disk_min_gb` setting plumbing (backend + shared types)

Threads the new setting through the DB default, the `Settings` struct, `get_settings`, `ALLOWED_KEYS`, and the TS `AppSettings` type (plus the two test factories, so the frontend keeps compiling). No UI yet.

**Files:**
- Modify: `src-tauri/src/db.rs`
- Modify: `src-tauri/src/types.rs`
- Modify: `src-tauri/src/commands/settings.rs`
- Modify: `src/lib/tauri.ts`
- Modify: `src/hooks/useSettings.test.ts` and `src/pages/SettingsPage.test.tsx` (factory updates only)
- Test: `src-tauri/src/db.rs`

> **Critical (from review):** `db.rs` has two tests that assert the seeded settings count is **exactly 15** — `init_db_seeds_defaults` (line ~251) and `init_db_is_idempotent_and_preserves_user_changes` (line ~306). Adding a 16th default breaks both. The `init_db_seeds_defaults` count is the designated guard "against accidental additions", so it is also the right home for the new value assertion. Both counts MUST move to 16 in this task.

- [ ] **Step 1: Extend the defaults guard test to expect the new key (failing)**

In `src-tauri/src/db.rs`, in `init_db_seeds_defaults`, change the count assertion from `assert_eq!(count, 15);` to:

```rust
        assert_eq!(count, 16);
```

and add a value assertion alongside the other per-key assertions in that test (e.g. right after the `watch_skip_marker` block):

```rust
        assert_eq!(
            setting(&conn, "low_disk_min_gb").as_deref(),
            Some("0"),
            "low-disk auto-pause is off (0) until the user sets a GB threshold"
        );
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd src-tauri && cargo test --lib init_db_seeds_defaults`
Expected: FAIL — count is 15 (not 16) and `low_disk_min_gb` has no row.

- [ ] **Step 3: Seed the default and fix the idempotency test's count**

In `src-tauri/src/db.rs`, in the `defaults` array (currently ending with `("watch_skip_marker", ".downloading"),`), add a line:

```rust
        ("watch_skip_marker", ".downloading"),
        ("low_disk_min_gb", "0"),
```

Then, in `init_db_is_idempotent_and_preserves_user_changes`, update its count assertion (which now also sees 16 rows) from `assert_eq!(count, 15);` to:

```rust
        assert_eq!(count, 16);
```

- [ ] **Step 4: Run both db tests to verify they pass**

Run: `cd src-tauri && cargo test --lib init_db_seeds_defaults init_db_is_idempotent`
Expected: PASS (both).

- [ ] **Step 5: Add the field to the Settings struct**

In `src-tauri/src/types.rs`, in `pub struct Settings`, add after `pub watch_skip_marker: String,`:

```rust
    pub watch_skip_marker: String,
    pub low_disk_min_gb: f64,
```

- [ ] **Step 6: Parse it in get_settings and allow the key**

In `src-tauri/src/commands/settings.rs`:

Add the local, alongside the other `let mut` declarations near the top of `get_settings`:

```rust
    let mut watch_skip_marker = String::new();
    let mut low_disk_min_gb: f64 = 0.0;
```

Add a match arm in the `for row in rows` loop, after the `"watch_skip_marker" => ...` arm:

```rust
            "watch_skip_marker" => watch_skip_marker = value,
            "low_disk_min_gb" => low_disk_min_gb = value.parse().unwrap_or(0.0),
```

Add the field to the returned `Settings { ... }`, after `watch_skip_marker,`:

```rust
        watch_skip_marker,
        low_disk_min_gb,
```

Add the key to `ALLOWED_KEYS`, after `"watch_skip_marker",`:

```rust
    "watch_skip_marker",
    "low_disk_min_gb",
```

- [ ] **Step 7: Add the field to the TS AppSettings type and the two test factories**

In `src/lib/tauri.ts`, in `export interface AppSettings`, add after `watch_skip_marker: string;`:

```ts
  watch_skip_marker: string;
  low_disk_min_gb: number;
```

In `src/hooks/useSettings.test.ts`, in `makeSettings(...)`, add after `watch_skip_marker: ".downloading",`:

```ts
    watch_skip_marker: ".downloading",
    low_disk_min_gb: 0,
```

In `src/pages/SettingsPage.test.tsx`, in `makeSettings()`, add after `watch_skip_marker: ".downloading",`:

```ts
    watch_skip_marker: ".downloading",
    low_disk_min_gb: 0,
```

- [ ] **Step 8: Verify backend + frontend still build/test green**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS (all existing + new tests).
Run: `npm run build`
Expected: `tsc` passes (the new required field is present in both factories and the type).

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/db.rs src-tauri/src/types.rs src-tauri/src/commands/settings.rs \
        src/lib/tauri.ts src/hooks/useSettings.test.ts src/pages/SettingsPage.test.tsx
git commit -m "feat: add low_disk_min_gb setting (backend plumbing + shared type)"
```

---

## Task 4: Wire the disk gate into process_queue + emit the pause event

The gate runs at the top of the `process_queue` loop, after `get_next_job` picks the job and **before** the job is flipped to `encoding` / spawned. On a low-disk stop it emits `queue-paused-low-disk` and the idle menu-bar update, then **returns** (skipping the "Queue complete" notification — nothing completed) with the job left `queued`.

**Files:**
- Modify: `src-tauri/src/converter.rs`
- Test: `src-tauri/src/converter.rs` (inline tests, using the existing mock-runtime harness)

- [ ] **Step 1: Write the failing integration tests**

In `src-tauri/src/converter.rs`, inside `#[cfg(test)] mod tests`, add (the harness helpers `mock_app`, `test_db`, `set_setting`, `queue_job`, `job_row`, `record_events`, `fake_handbrake_script` already exist in this module):

```rust
#[test]
fn low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued() {
    // An absurd threshold makes required-free exceed any real disk, so the gate always trips —
    // deterministic regardless of the test machine's free space.
    let app = mock_app();
    let db = test_db();
    let converter = ConverterState::new();

    let dir = tempfile::tempdir().unwrap();
    // A real fake HandBrake IS configured: if the gate failed to stop the run, the job would be
    // spawned and end 'error' (empty output). Staying 'queued' proves the gate blocked the spawn.
    let script = fake_handbrake_script(dir.path());
    set_setting(&db, "handbrake_path", script.to_str().unwrap());
    set_setting(&db, "low_disk_min_gb", "1000000000"); // 1e9 GB
    let out = dir.path().join("out.mp4");
    queue_job(&db, "j1", "/nowhere/a.mp4", out.to_str().unwrap(), 1000);

    let paused_events = record_events(&app, "queue-paused-low-disk");
    let status_events = record_events(&app, "job-status-changed");

    process_queue(app.handle(), &db, &converter);

    let (status, _msg) = job_row(&db, "j1");
    assert_eq!(status, "queued", "a low-disk pause leaves the job queued, never encoding/error");
    assert!(!out.exists(), "the encode must never start, so no output is written");
    assert_eq!(paused_events.lock().unwrap().len(), 1, "exactly one low-disk pause event fires");
    assert!(
        paused_events.lock().unwrap()[0].contains("required_bytes"),
        "the event carries the required-free figure for the UI"
    );
    assert!(
        status_events.lock().unwrap().iter().all(|p| !p.contains("\"encoding\"")),
        "the job must never transition to encoding"
    );
    assert!(
        !*converter.is_running.lock().unwrap(),
        "the queue thread must have stopped"
    );
}

#[test]
fn low_disk_check_is_skipped_when_threshold_is_zero() {
    // Threshold 0 = disabled: the gate is a no-op and the job runs through to the encode stage.
    let app = mock_app();
    let db = test_db();
    let converter = ConverterState::new();

    let dir = tempfile::tempdir().unwrap();
    let script = fake_handbrake_script(dir.path()); // exits 0, writes nothing -> empty-output error
    set_setting(&db, "handbrake_path", script.to_str().unwrap());
    set_setting(&db, "low_disk_min_gb", "0");
    let out = dir.path().join("out.mp4");
    queue_job(&db, "j1", "/nowhere/a.mp4", out.to_str().unwrap(), 1000);

    process_queue(app.handle(), &db, &converter);

    let (status, msg) = job_row(&db, "j1");
    assert_eq!(status, "error", "with the check disabled, the job is processed, not held 'queued'");
    assert!(
        msg.unwrap().contains("empty output file"),
        "the job reached the encode stage (fake HandBrake produced no output)"
    );
}
```

- [ ] **Step 2: Run them to verify the first fails**

Run: `cd src-tauri && cargo test --lib low_disk_threshold_pauses low_disk_check_is_skipped`
Expected: `low_disk_check_is_skipped_when_threshold_is_zero` PASSES (current behavior already runs the job); `low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued` FAILS (job becomes `error`, no pause event).

- [ ] **Step 3: Add the settings reader**

In `src-tauri/src/converter.rs`, near `get_cleanup_mode`, add:

```rust
fn get_low_disk_min_gb(db: &Connection) -> f64 {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'low_disk_min_gb'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse::<f64>().ok())
    .unwrap_or(0.0)
}
```

- [ ] **Step 4: Read the threshold and insert the gate in process_queue**

In `process_queue`, in the db-lock block that reads the next job, add the threshold read. Change:

```rust
        let job;
        let handbrake_path_opt;
        let cleanup_mode;
        {
            let db = db.lock().unwrap();
            job = match get_next_job(&db) {
                Some(j) => j,
                None => break,
            };
            handbrake_path_opt = get_handbrake_path(&db);
            cleanup_mode = get_cleanup_mode(&db);

            if handbrake_path_opt.is_some() {
                let _ = db.execute(
                    "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                    params![job.id],
                );
            }
        }
```

to:

```rust
        let job;
        let handbrake_path_opt;
        let cleanup_mode;
        let low_disk_min_gb;
        {
            let db = db.lock().unwrap();
            job = match get_next_job(&db) {
                Some(j) => j,
                None => break,
            };
            handbrake_path_opt = get_handbrake_path(&db);
            cleanup_mode = get_cleanup_mode(&db);
            low_disk_min_gb = get_low_disk_min_gb(&db);
            // The job is flipped to 'encoding' below, AFTER the low-disk gate — a gated job
            // must stay 'queued' so the Resume button can retry it.
        }
```

Then insert the gate **before** the `handbrake_path` match. Locate:

```rust
        let file_name = std::path::Path::new(&job.source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Outside the db-lock scope: record_job_error takes the lock itself, and this
        // failure must count toward had_errors and emit the same events as any other.
        let handbrake_path = match handbrake_path_opt {
```

and insert, between the `file_name` binding and the `// Outside the db-lock scope:` comment:

```rust
        // Low-disk gate: before committing this job to 'encoding', ensure the destination
        // filesystem has room for the floor plus the encode (2× source). On a shortfall, stop
        // the run like "Pause after this" — leave the job 'queued', tell the UI why, and return
        // (nothing completed, so no "Queue complete" notification). Fail open: a 0 threshold, an
        // unresolvable parent, or a failed free-space query all let the encode proceed.
        if low_disk_min_gb > 0.0 {
            if let Some(available) = destination_available_bytes(&job.output_path) {
                let floor = gb_to_bytes(low_disk_min_gb);
                let source_size = job.original_size.unwrap_or(0).max(0) as u64;
                if !has_enough_disk(available, floor, source_size) {
                    let required = required_free_bytes(floor, source_size);
                    let _ = app.emit(
                        "queue-paused-low-disk",
                        serde_json::json!({
                            "path": job.output_path,
                            "available_bytes": available,
                            "required_bytes": required,
                        }),
                    );
                    let _ = app.emit(
                        "menu-bar-update",
                        MenuBarUpdate {
                            // Reuse the end-of-run status so a prior job's failure in this run
                            // still surfaces as "error" in the tray; otherwise a paused queue reads
                            // "idle". (had_errors is in scope at the top of process_queue.)
                            status: final_run_status(had_errors).to_string(),
                            percent: None,
                            file_name: None,
                            eta_seconds: None,
                            queue_count: None,
                            fps: None,
                        },
                    );
                    return;
                }
            }
        }
```

Now move the `encoding` flip to after the `handbrake_path` match (so it still only runs when HandBrake is present, but now also only after the gate passed). Immediately after:

```rust
        let handbrake_path = match handbrake_path_opt {
            Some(p) => p,
            None => {
                had_errors = true;
                record_job_error(app, db, &job.id, &file_name, "HandBrakeCLI not found");
                continue;
            }
        };
```

add:

```rust
        // Claim the job by flipping it to 'encoding' ONLY if it is still 'queued'. The original
        // code did this select+flip under a single db-lock; relocating the flip past the disk
        // gate (whose statvfs can stall on a slow/asleep/network volume) reopened a window where
        // `clear_queue`/`remove_job` — which delete 'queued' rows — could remove this job before
        // the flip. Without the `AND status = 'queued'` guard the loop would then spawn HandBrake
        // on a deleted row and, on success, trash/delete the user's SOURCE file. The conditional
        // claim + row-count check closes that window: 0 rows affected means the job is gone, so
        // skip it.
        let claimed = {
            let db = db.lock().unwrap();
            db.execute(
                "UPDATE jobs SET status = 'encoding' WHERE id = ?1 AND status = 'queued'",
                params![job.id],
            )
            .unwrap_or(0)
        };
        if claimed == 0 {
            continue;
        }
```

(This replaces the original in-lock unconditional `UPDATE ... 'encoding'`. It is gated behind both the disk check and the HandBrake-present check. Existing-test behavior is unchanged: a live 'queued' job is still claimed exactly once, and HandBrake-missing still records an error before the flip is ever reached.)

- [ ] **Step 5: Run the full converter test suite**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS — both new integration tests plus all pre-existing `process_queue` tests (`spawn_failure_surfaces_like_every_other_error`, `quit_mid_encode_leaves_the_job_for_auto_resume`, `zero_byte_output_fails_with_diagnostics_and_the_queue_continues`, etc.) remain green.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/converter.rs
git commit -m "feat: pause the queue before a file when the destination disk is low"
```

---

## Task 5: Settings UI for the threshold + numeric optimistic coerce

Adds the number input and extends the optimistic-merge coercion to keep numeric settings as real numbers (so `type="number"` binding and any `=== number` comparisons work). The low-disk banner + event listener already landed in Task 1.

**Files:**
- Modify: `src/hooks/useSettings.ts`
- Modify: `src/hooks/useSettings.test.ts`
- Modify: `src/pages/SettingsPage.tsx`
- Modify: `src/pages/SettingsPage.test.tsx`

- [ ] **Step 1: Write the failing numeric-coerce test**

In `src/hooks/useSettings.test.ts`, after the `"coerces boolean settings to real booleans…"` test, add:

```ts
it("coerces numeric settings to real numbers in the optimistic merge", async () => {
  const { result } = renderHook(() => useSettings());
  await waitFor(() => expect(result.current.loading).toBe(false));

  await act(async () => {
    await result.current.updateSetting("low_disk_min_gb", "5");
  });

  // A stringly "5" must land as a real number so the number input binds correctly.
  expect(result.current.settings?.low_disk_min_gb).toBe(5);
});
```

The `useSettings.test.ts` invoke mock's `update_setting` arm already returns `Promise.resolve(undefined)` for arbitrary keys, so no mock change is needed.

- [ ] **Step 2: Run it to verify it fails**

Run: `npm test -- src/hooks/useSettings.test.ts`
Expected: FAIL — `low_disk_min_gb` lands as the string `"5"`, not the number `5`.

- [ ] **Step 3: Extend coerceSettingValue for numbers**

In `src/hooks/useSettings.ts`, replace:

```ts
function coerceSettingValue(current: unknown, value: string): string | boolean {
  return typeof current === "boolean" ? value === "true" : value;
}
```

with:

```ts
function coerceSettingValue(current: unknown, value: string): string | boolean | number {
  if (typeof current === "boolean") return value === "true";
  if (typeof current === "number") return Number(value);
  return value;
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `npm test -- src/hooks/useSettings.test.ts`
Expected: PASS (new test + all existing useSettings tests).

The input uses a local draft committed on blur — matching every other text input on this page (`hbDraft`, `markerDraft`, `suffixDraft`). Per-keystroke `updateSetting` on a numeric field snaps the value mid-edit (typing `2.5` briefly becomes `2` when `Number("2.")` resolves) and fires an IPC per character; draft + commit-on-blur avoids both.

- [ ] **Step 5: Write the failing SettingsPage input test**

In `src/pages/SettingsPage.test.tsx`, inside the `describe("SettingsPage", ...)` block, add:

```ts
it("does not write the low-disk threshold per keystroke; commits on blur", async () => {
  render(<SettingsPage />);
  const input = await screen.findByRole("spinbutton"); // the only number input on the page
  fireEvent.change(input, { target: { value: "2" } });
  fireEvent.change(input, { target: { value: "2.5" } });
  expect(updateCallsFor("low_disk_min_gb")).toHaveLength(0);

  fireEvent.blur(input);

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("update_setting", {
      key: "low_disk_min_gb",
      value: "2.5",
    }),
  );
});
```

(`updateCallsFor` already exists in this test file.)

- [ ] **Step 6: Run it to verify it fails**

Run: `npm test -- src/pages/SettingsPage.test.tsx`
Expected: FAIL — `getByRole("spinbutton")` finds no element (no number input yet).

- [ ] **Step 7: Add the draft state, commit handler, and number input to SettingsPage**

In `src/pages/SettingsPage.tsx`:

Add the draft state alongside the other drafts (after `const [markerDraft, setMarkerDraft] = useState(settings?.watch_skip_marker ?? "");`):

```tsx
  const [diskDraft, setDiskDraft] = useState(String(settings?.low_disk_min_gb ?? 0));
```

Add a sync effect after the existing `markerDraft` sync effect (`useEffect(() => { if (settings) setMarkerDraft(settings.watch_skip_marker); }, [settings?.watch_skip_marker]);`):

```tsx
  useEffect(() => {
    if (settings) setDiskDraft(String(settings.low_disk_min_gb));
  }, [settings?.low_disk_min_gb]);
```

Add the commit handler alongside `commitMarker`:

```tsx
  const commitDisk = () => {
    if (diskDraft !== String(settings?.low_disk_min_gb)) {
      updateSetting("low_disk_min_gb", diskDraft);
    }
  };
```

Add a new setting group immediately after the "Skip files already at or below the target" group (its hint ends with "…for device compatibility.") and before the "Watched-folder skip marker" group:

```tsx
      <div className="setting-group">
        <label className="setting-label">Pause when destination free space is low</label>
        <div className="setting-row">
          <input
            className="setting-input"
            type="number"
            min="0"
            step="0.5"
            value={diskDraft}
            onChange={(e) => setDiskDraft(e.target.value)}
            onBlur={commitDisk}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
          />
          <span>GB</span>
        </div>
        <p className="setting-hint">
          Before starting each file, if the destination disk has less free space than this (plus
          room for the encode), the queue pauses instead of converting the next file. Resume it
          from the Queue tab once you&apos;ve freed space. Set to 0 to never pause.
        </p>
      </div>
```

- [ ] **Step 8: Run it to verify it passes**

Run: `npm test -- src/pages/SettingsPage.test.tsx`
Expected: PASS (new test + all existing SettingsPage tests).

- [ ] **Step 9: Full frontend verification**

Run: `npm run build && npm test`
Expected: `tsc` clean; all frontend tests PASS.

- [ ] **Step 10: Commit**

```bash
git add src/hooks/useSettings.ts src/hooks/useSettings.test.ts \
        src/pages/SettingsPage.tsx src/pages/SettingsPage.test.tsx
git commit -m "feat: add low-disk pause threshold setting to the Settings page"
```

---

## Final Verification

- [ ] **Backend:** `cd src-tauri && cargo test --lib` → all pass. `cargo fmt` leaves the tree clean (glance at `git diff --stat`; CI does not gate fmt).
- [ ] **Frontend:** `npm run build && npm test` → tsc clean, all tests pass.
- [ ] **ACL:** No new frontend `core:`/`plugin:` API calls were added (only a new `listen` event name, which reuses the existing event-listen grant; `start_queue`, `get_settings`, `update_setting` are app commands). Optionally run the `acl-auditor` agent to confirm no `capabilities/default.json` drift.
- [ ] **Cross-platform:** `fs4` is cross-platform (no `cfg` gating needed). Optionally run the `cross-platform-reviewer` agent over the `converter.rs` changes.
- [ ] **Manual smoke (macOS):** Set the threshold to just above current destination free space, queue a file → queue pauses, banner explains why, Resume is visible. Set threshold to 0 → conversions run normally. Click "Pause after this", let the job finish → Resume appears and restarts the queue.
- [ ] **fs4 API sanity:** if `fs4::available_space` fails to resolve at build time, confirm the crate's current free-function path on docs.rs (the function returns bytes available to a non-privileged user for the filesystem containing the path) and adjust the single call site in `destination_available_bytes`.

---

## Execution Handoff

Plan complete. Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Before executing: create the branch/worktree (`feature/low-disk-pause-and-resume`) per the using-git-worktrees skill and CLAUDE.md's protected-main workflow.
