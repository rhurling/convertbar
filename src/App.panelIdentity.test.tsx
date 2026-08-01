import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { LayoutMode } from "./hooks/useLayoutMode";

// App.layoutTransition.test.tsx covers what a *doomed* panel must save on its way out (Settings'
// commit-on-unmount). This file covers the panel that should never have been unmounted at all:
// Queue renders in all three layouts, so crossing 900px or 1300px must not destroy its state.
// The user-visible stake is the file picker — an open FileBrowserModal, and the cross-folder
// selection the modal exists to gather, both vanish when QueuePage is remounted underneath it.
let layoutMode: LayoutMode = "three-col";
vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => layoutMode }));

// The picker is server-head only (a browser tab has no native file dialog), which is also the
// only head that has these layouts at all — desktop is a fixed 400x500 window.
vi.mock("./lib/head", () => ({ isServerHead: true }));

// Queue stays real (with its DropZone and FileBrowserModal); the other three panels are not the
// subject and would only drag their own IPC into this file.
vi.mock("./pages/HistoryPage", () => ({ default: () => <div data-testid="history-page" /> }));
vi.mock("./pages/WatchedFoldersPage", () => ({ default: () => <div data-testid="watch-page" /> }));
vi.mock("./pages/SettingsPage", () => ({ default: () => <div data-testid="settings-page" /> }));

vi.mock("./hooks/useAddProgress", () => ({
  useAddProgress: () => ({ isAdding: false, activity: null }),
}));
vi.mock("./hooks/useUpdate", () => ({ useUpdate: () => ({ state: null }) }));
vi.mock("./hooks/useFileIntake", () => ({
  useFileIntake: () => ({
    pendingConfirm: null,
    onAdd: vi.fn(),
    onSkip: vi.fn(),
    status: null,
    isDragOver: false,
    addPaths: vi.fn(),
  }),
}));

const fsListMock = vi.fn();
vi.mock("./lib/transport/http", () => ({
  httpCommands: {
    fsList: (path: string) => fsListMock(path),
    getAppInfo: () =>
      Promise.resolve({
        version: "1.0.0",
        head: "server",
        can_pause_process: false,
        auth_required: false,
        browse_roots: ["/"],
      }),
  },
}));

// Mocked at the IPC boundary, not at lib/tauri, so the real command layer is exercised.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { invoke } from "@tauri-apps/api/core";
import App from "./App";

const invokeMock = vi.mocked(invoke);

beforeEach(() => {
  vi.clearAllMocks();
  layoutMode = "three-col";
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "validate_handbrake":
        return Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI" });
      case "get_queue":
        return Promise.resolve([]);
      case "get_low_disk_pause":
        return Promise.resolve(null);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
  fsListMock.mockResolvedValue({
    path: "/",
    entries: [{ name: "clip.mp4", path: "/clip.mp4", is_dir: false, size: 1000 }],
  });
});

/** Opens Queue's picker and selects one file, leaving the modal open mid-task. */
async function openPickerWithOneFileSelected() {
  fireEvent.click(await screen.findByRole("button", { name: "Add files or folders…" }));
  fireEvent.click(await screen.findByText("clip.mp4"));
  expect(await screen.findByRole("button", { name: "Add 1 item" })).toBeInTheDocument();
}

/** The open picker, still open, still holding the selection it was gathering. */
function expectPickerIntact() {
  expect(screen.getByRole("dialog", { name: "Add files" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Add 1 item" })).toBeInTheDocument();
}

describe("panel identity across layout crossings", () => {
  it("keeps Queue's open file picker when the 1300px crossing changes the layout", async () => {
    const { rerender } = render(<App />);
    await openPickerWithOneFileSelected();

    // A zoom step (Cmd -) is enough to cross this without touching the window.
    layoutMode = "two-col";
    rerender(<App />);

    expectPickerIntact();
  });

  it("keeps Queue's open file picker when the 900px crossing changes the layout", async () => {
    layoutMode = "tabs";
    const { rerender } = render(<App />);
    await openPickerWithOneFileSelected();

    // Queue moves from the tabbed column into its own pinned column here — a move, not a
    // teardown, as far as the user is concerned.
    layoutMode = "two-col";
    rerender(<App />);

    expectPickerIntact();
  });

  it("does not refetch the queue on a crossing, because Queue is never remounted", async () => {
    const { rerender } = render(<App />);
    const queueFetches = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "get_queue").length;
    await waitFor(() => expect(queueFetches()).toBeGreaterThan(0));
    const before = queueFetches();

    layoutMode = "two-col";
    rerender(<App />);
    layoutMode = "tabs";
    rerender(<App />);

    // A remount would re-run useQueue's mount effect; a moved-but-live instance would not.
    expect(queueFetches()).toBe(before);
  });
});
