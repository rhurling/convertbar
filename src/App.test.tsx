import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
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
});

it("renders every panel and no tab buttons at three-col", async () => {
  layoutMode = "three-col";
  render(<App />);

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
});

it("keeps the classic tab bar below the first breakpoint", async () => {
  layoutMode = "tabs";
  render(<App />);

  expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Queue" })).toBeInTheDocument();
  expect(screen.queryByTestId("history-page")).not.toBeInTheDocument();
});
});
