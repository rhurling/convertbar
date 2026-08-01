import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { LayoutMode } from "./hooks/useLayoutMode";
import type { AppSettings } from "./lib/tauri";

// App.test.tsx mocks every page, which makes a layout change invisible: the panels it swaps have
// no state to lose. This file keeps SettingsPage *real* — the only panel that holds uncommitted
// user input — and drives the transition App.tsx cannot survive: crossing 1300px swaps between
// two JSX trees whose child slots don't reconcile, so all four panels unmount.
let layoutMode: LayoutMode = "three-col";
vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => layoutMode }));

vi.mock("./pages/QueuePage", () => ({ default: () => <div data-testid="queue-page" /> }));
vi.mock("./pages/HistoryPage", () => ({ default: () => <div data-testid="history-page" /> }));
vi.mock("./pages/WatchedFoldersPage", () => ({ default: () => <div data-testid="watch-page" /> }));
vi.mock("./components/UpdatePanel", () => ({ default: () => null }));

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

// Mocked at the IPC boundary, not at lib/tauri, so the real command layer is exercised.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { invoke } from "@tauri-apps/api/core";
import App from "./App";

const invokeMock = vi.mocked(invoke);

function makeSettings(): AppSettings {
  return {
    preset: "Fast 1080p30",
    cleanup_mode: "trash",
    launch_at_login: false,
    handbrake_path: "/usr/bin/HandBrakeCLI",
    menubar_show_percent: true,
    menubar_show_eta: true,
    menubar_show_queue: true,
    menubar_show_filename: true,
    menubar_show_fps: false,
    notifications_per_file: false,
    notifications_errors_only: false,
    notifications_queue_done: false,
    skip_already_converted: false,
    skip_by_source_media: true,
    watch_skip_marker: ".downloading",
    low_disk_min_gb: 0,
    bad_source_action: "trash",
    update_mode: "automatic",
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  layoutMode = "three-col";
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "validate_handbrake":
        return Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI" });
      case "get_settings":
        return Promise.resolve(makeSettings());
      case "list_handbrake_presets":
        return Promise.resolve(["Fast 1080p30"]);
      case "get_preset_suffix":
        return Promise.resolve(".{resolution}-{codec}");
      case "generate_preset_suffix":
        return Promise.resolve({
          codec: "h265",
          resolution: "1080p",
          quality: "hq",
          preset: "Fast 1080p30",
          device: "apple",
        });
      case "resolve_suffix_template":
        return Promise.resolve(".RESOLVED");
      case "update_setting":
      case "set_preset_suffix":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("App layout transitions", () => {
  it("persists a typed-but-unblurred suffix when the 1300px crossing unmounts Settings", async () => {
    const { rerender } = render(<App />);
    const input = await screen.findByPlaceholderText(".{resolution}-{codec}");
    // Same load-sync race as SettingsPage's unmount test: the input mounts when get_settings
    // lands, but presetSuffix arrives later and its effect re-syncs suffixDraft, silently
    // overwriting a value typed in between. Anchor on the loaded template before editing.
    await waitFor(() => expect(input).toHaveValue(".{resolution}-{codec}"));
    fireEvent.change(input, { target: { value: ".hevc" } });

    // The crossing itself: a zoom step or window resize, with the edit still unblurred.
    layoutMode = "two-col";
    rerender(<App />);

    // Settings is not among two-col's panels, so it is gone rather than re-rendered...
    expect(screen.queryByPlaceholderText(".{resolution}-{codec}")).not.toBeInTheDocument();
    // ...and the edit must have reached the backend on its way out.
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preset_suffix", {
        preset: "Fast 1080p30",
        suffix: ".hevc",
      }),
    );
  });

  it("keeps Settings live when the 900px crossing only moves it to another column", async () => {
    // Settings is the tabbed panel at tabs, and still reachable at two-col — it just shifts one
    // column to the right, behind Queue's new pinned column. Columns are keyed by the panels
    // they hold precisely so that shift is a reorder: index-keyed columns would match Settings
    // against Queue and rebuild it, which is a remount the user never asked for.
    layoutMode = "tabs";
    const { rerender } = render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

    const input = await screen.findByPlaceholderText(".{resolution}-{codec}");
    await waitFor(() => expect(input).toHaveValue(".{resolution}-{codec}"));
    fireEvent.change(input, { target: { value: ".hevc" } });

    layoutMode = "two-col";
    rerender(<App />);

    // The draft is still a draft: same instance, so it is neither reset from the stored
    // template nor force-committed by an unmount the user did not cause.
    expect(screen.getByPlaceholderText(".{resolution}-{codec}")).toHaveValue(".hevc");
    expect(invokeMock).not.toHaveBeenCalledWith("set_preset_suffix", expect.anything());
  });
});
