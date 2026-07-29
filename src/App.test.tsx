import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import type { LayoutMode } from "./hooks/useLayoutMode";

let layoutMode: LayoutMode = "tabs";
vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => layoutMode }));

vi.mock("./pages/QueuePage", () => ({ default: () => <div data-testid="queue-page" /> }));
vi.mock("./pages/HistoryPage", () => ({ default: () => <div data-testid="history-page" /> }));
vi.mock("./pages/WatchedFoldersPage", () => ({ default: () => <div data-testid="watch-page" /> }));
vi.mock("./pages/SettingsPage", () => ({ default: () => <div data-testid="settings-page" /> }));

// App's own hooks reach for IPC on mount; stub them to inert values.
vi.mock("./hooks/useAddProgress", () => ({ useAddProgress: () => ({ isAdding: false, activity: null }) }));
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
vi.mock("./lib/tauri", () => ({
  commands: {
    validateHandbrake: () => Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI" }),
    hideWindow: vi.fn(),
  },
}));

import App from "./App";

beforeEach(() => {
  vi.clearAllMocks();
  layoutMode = "tabs";
});

describe("App layout", () => {
  it("pins Queue and tabs the rest at two-col", async () => {
    layoutMode = "two-col";
    render(<App />);

    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
    // Queue is always visible, so its tab button is gone.
    expect(screen.queryByRole("button", { name: "Queue" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "History" })).toBeInTheDocument();
    // activeTab defaults to "queue", which is pinned — the derived fallback must land on
    // the first tab still in the bar rather than rendering an empty column.
    expect(screen.getByTestId("history-page")).toBeInTheDocument();
    // The pinned panel names itself; the tabbed one is already named by its tab button.
    expect(screen.getByRole("heading", { name: "Queue" })).toBeInTheDocument();
    // Only Queue is pinned at two-col — Watch and Settings stay unmounted until tabbed to.
    expect(screen.queryByTestId("watch-page")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-page")).not.toBeInTheDocument();
  });

  it("renders every panel and no tab buttons at three-col", async () => {
    layoutMode = "three-col";
    const { container } = render(<App />);

    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
    expect(screen.getByTestId("history-page")).toBeInTheDocument();
    expect(screen.getByTestId("watch-page")).toBeInTheDocument();
    expect(screen.getByTestId("settings-page")).toBeInTheDocument();
    for (const label of ["Queue", "History", "Watch", "Settings"]) {
      expect(screen.queryByRole("button", { name: label })).not.toBeInTheDocument();
    }
    // Each pinned panel names itself, so a four-panel view is self-describing.
    expect(screen.getByRole("heading", { name: "Watch" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Settings" })).toBeInTheDocument();
    // Watch and Settings are grouped into a single column — Settings is by far the longest
    // panel, and pairing it with the shortest balances the row — so three columns total,
    // not four.
    expect(container.querySelectorAll(".app-column")).toHaveLength(3);
    expect(screen.getByTestId("watch-page").closest(".app-column")).toBe(
      screen.getByTestId("settings-page").closest(".app-column"),
    );
    // The tab bar itself must survive an empty `tabs` array: it still carries
    // data-tauri-drag-region and the desktop close button, which have no other home.
    expect(container.querySelector(".tab-bar")).toBeInTheDocument();
    expect(screen.getByTitle("Close")).toBeInTheDocument();
  });

  it("keeps the classic tab bar below the first breakpoint", async () => {
    layoutMode = "tabs";
    render(<App />);

    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Queue" })).toBeInTheDocument();
    expect(screen.queryByTestId("history-page")).not.toBeInTheDocument();
  });

  it("switches the tabbed panel when a tab button is clicked", async () => {
    layoutMode = "tabs";
    render(<App />);

    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    expect(await screen.findByTestId("history-page")).toBeInTheDocument();
    expect(screen.queryByTestId("queue-page")).not.toBeInTheDocument();
  });
});
