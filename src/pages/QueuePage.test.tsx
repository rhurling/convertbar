import { it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";

// Per-test controllable queue state (reset in beforeEach).
let queueMock: {
  activeJob: unknown;
  pendingJobs: unknown[];
  progress: unknown;
  refresh: () => void;
};

vi.mock("../hooks/useQueue", () => ({ useQueue: () => queueMock }));
// Mirrors DropZone's own onPick branch closely enough to prove QueuePage wires it —
// the real branch behavior is DropZone.test.tsx's job, not this file's.
vi.mock("../components/DropZone", () => ({
  default: ({ onPick }: { onPick?: () => void }) =>
    onPick ? (
      <button type="button" onClick={onPick}>
        Add files or folders…
      </button>
    ) : (
      <div data-testid="dropzone" />
    ),
}));
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
    getLowDiskPause: vi.fn(() => Promise.resolve(null)),
  },
}));

import QueuePage from "./QueuePage";
import { commands } from "../lib/tauri";
import { listen } from "@tauri-apps/api/event";

const intakeStub = {
  pendingConfirm: null,
  onAdd: vi.fn(),
  onSkip: vi.fn(),
  status: null,
  isDragOver: false,
  addPaths: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
  // clearAllMocks resets call history but NOT implementations, so restore the default listen
  // stub each test (the banner test overrides it and must not leak into later tests).
  vi.mocked(listen).mockImplementation(() => Promise.resolve(() => {}));
  queueMock = { activeJob: null, pendingJobs: [], progress: null, refresh: vi.fn() };
});

afterEach(() => {
  // Only armed by the server-head test below (stubEnv/resetModules/stubGlobal) — a no-op
  // otherwise.
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

// The server head's ../lib/events opens a real EventSource at module load — jsdom has no such
// global, so loading QueuePage (which listens for queue-paused-low-disk) under
// VITE_HEAD=server throws ReferenceError without this stub. Same shape as events.test.ts's
// MockEventSource, trimmed to what this file needs (no event is ever emitted here).
class StubEventSource {
  addEventListener() {}
  removeEventListener() {}
}

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

it("seeds the low-disk banner from backend state on mount, without the event firing", async () => {
  vi.mocked(commands.getLowDiskPause).mockResolvedValueOnce({
    path: "/m/out.mp4",
    available_bytes: 3_000_000_000,
    required_bytes: 5_000_000_000,
  });
  // A persisted reason implies the queue still holds pending jobs — clear_queue drops the
  // reason, so a Some seed always coincides with a non-empty pending list.
  queueMock = {
    activeJob: null,
    pendingJobs: [{ id: "j1", source_path: "/m/a.mp4", status: "queued" }],
    progress: null,
    refresh: vi.fn(),
  };
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  expect(await screen.findByText(/free on the destination/i)).toBeInTheDocument();
});

it("passes no onPick to DropZone on desktop — there is no picker without a server-side filesystem", () => {
  // Finding 6 of the final-review pass: nothing pinned onPick===undefined on desktop.
  // DropZone's mock here renders the pick button only when onPick is truthy (mirroring
  // DropZone's own behavior, which DropZone.test.tsx pins) and a bare dropzone div otherwise
  // — so the button's absence is exactly what proves QueuePage passed `undefined`.
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} intake={intakeStub} />);
  expect(
    screen.queryByRole("button", { name: /Add files or folders/ }),
  ).not.toBeInTheDocument();
  expect(screen.getByTestId("dropzone")).toBeInTheDocument();
});

it("has no separate intake button on the server head — the drop surface is the picker", async () => {
  vi.stubEnv("VITE_HEAD", "server");
  vi.stubGlobal("EventSource", StubEventSource);
  vi.resetModules();
  const { default: FreshQueuePage } = await import("./QueuePage");

  render(
    <FreshQueuePage
      hbStatus={{ found: true, path: "/usr/bin/HandBrakeCLI", version: "1.9.0" }}
      adding={null}
      isAdding={false}
      intake={intakeStub}
    />,
  );

  expect(await screen.findByRole("button", { name: /Add files or folders/ })).toBeInTheDocument();
  // The old standalone "Add files…" button is gone: two controls for one action was the
  // thing this task removes.
  expect(screen.queryByRole("button", { name: /^Add files…$/ })).not.toBeInTheDocument();
});
