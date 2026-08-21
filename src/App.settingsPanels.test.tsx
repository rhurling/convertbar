import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { LayoutMode } from "./hooks/useLayoutMode";
import type { AppSettings, HistoryPage as HistoryPageData, HistorySummary, JobInfo } from "./lib/tauri";

// Three-col is the only layout that mounts History and Settings at the same time (App.tsx
// PINNED), so it is the only place their two useSettings() instances can either duplicate the
// load or drift apart. Both pages stay real here: the subject is what the *pair* does, which no
// single-page or hook-level test can observe.
let layoutMode: LayoutMode = "three-col";
vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => layoutMode }));

// Queue and Watch have no useSettings instance and would only drag their own IPC into this file.
vi.mock("./pages/QueuePage", () => ({ default: () => <div data-testid="queue-page" /> }));
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

// isServerHead is left at its build-time default (false) — neither behaviour under test is
// head-dependent, and the desktop branch needs no getAppInfo/http stubbing. The extra "Move
// original to Trash" radio it renders is unrelated to the cleanup_mode write driven below.

const SUFFIX_TEMPLATE = ".{resolution}-{codec}";

// The one setting both panels read: Settings writes it, History's savings label keys on it.
let cleanupMode: AppSettings["cleanup_mode"];

function makeSettings(): AppSettings {
  return {
    preset: "Fast 1080p30",
    cleanup_mode: cleanupMode,
    launch_at_login: false,
    handbrake_path: "",
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
    history_show_duration: false,
    encode_priority: "normal",
    post_convert_webhook_url: "",
    post_convert_webhook_headers: "",
    post_convert_webhook_body: "",
    queue_drained_webhook_url: "",
    queue_drained_webhook_headers: "",
    queue_drained_webhook_body: "",
    hook_path_map: "",
    hook_timeout_seconds: "30",
  };
}

const DONE_JOB: JobInfo = {
  id: "j1",
  source_path: "/in/clip.mp4",
  output_path: "/out/clip.mkv",
  preset: "Fast 1080p30",
  status: "done",
  original_size: 1000,
  converted_size: 500,
  kept_file: "converted",
  space_saved: 500,
  error_message: null,
  failure_class: null,
  queue_order: 0,
  created_at: "",
  started_at: null,
  completed_at: "2026-08-01",
};

const HISTORY: HistoryPageData = { jobs: [DONE_JOB], total: 1 };
const SUMMARY: HistorySummary = { total_files: 1, total_saved_bytes: 500 };

const callsTo = (cmd: string) => invokeMock.mock.calls.filter((c) => c[0] === cmd).length;

beforeEach(() => {
  vi.clearAllMocks();
  layoutMode = "three-col";
  cleanupMode = "trash";

  invokeMock.mockImplementation(((cmd: string, args?: { key?: string; value?: string }) => {
    switch (cmd) {
      case "validate_handbrake":
        return Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI", version: "1.9.0" });
      case "get_settings":
        return Promise.resolve(makeSettings());
      case "list_handbrake_presets":
        return Promise.resolve(["Fast 1080p30"]);
      case "get_preset_suffix":
        return Promise.resolve(SUFFIX_TEMPLATE);
      case "generate_preset_suffix":
        return Promise.resolve({
          codec: "h265",
          resolution: "1080p",
          quality: "hq",
          preset: "Fast 1080p30",
          device: "apple",
        });
      case "resolve_suffix_template":
        return Promise.resolve(".1080p-h265");
      case "update_setting":
        if (args!.key === "cleanup_mode") {
          cleanupMode = args!.value as AppSettings["cleanup_mode"];
        }
        return Promise.resolve(undefined);
      case "get_history":
        return Promise.resolve(HISTORY);
      case "get_history_summary":
        return Promise.resolve(SUMMARY);
      case "get_bad_sources":
        return Promise.resolve([]);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

/** Both panels loaded: Settings' suffix draft has synced (the last value useSettings resolves,
 *  so it anchors the whole load) and History's summary row is on screen. */
async function renderThreeColAndSettle() {
  render(<App />);
  await waitFor(() =>
    expect(screen.getByPlaceholderText(SUFFIX_TEMPLATE)).toHaveValue(SUFFIX_TEMPLATE),
  );
  await screen.findByText(/Total saved:/);
}

describe("History + Settings mounted together (three-col)", () => {
  it("loads the preset pipeline once, not once per mounted panel", async () => {
    // list_handbrake_presets shells out to HandBrakeCLI on every call — the server route has no
    // cache (crates/convertbar-server/src/routes/handbrake.rs), unlike generate_preset_suffix.
    // With History dragging the whole preset pipeline along for a `settings` value it is the only
    // thing it uses, opening the app spawned two concurrent CLI processes.
    await renderThreeColAndSettle();

    expect(callsTo("list_handbrake_presets")).toBe(1);
    expect(callsTo("generate_preset_suffix")).toBe(1);
    expect(callsTo("get_preset_suffix")).toBe(1);
  });

  it("flips History's savings label when Settings writes cleanup_mode", async () => {
    // useSettings' write registry is pinned at the hook level, but the user-visible stake is two
    // panels visible at once disagreeing: under `keep` nothing is deleted, so the total is only a
    // *potential* saving until the user removes the originals by hand. Selecting Keep in the
    // Settings column must relabel the History column that is already on screen beside it.
    await renderThreeColAndSettle();
    expect(screen.getByText(/^Total saved:/)).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Keep both files"));

    expect(await screen.findByText(/^Potential savings:/)).toBeInTheDocument();
    expect(screen.queryByText(/^Total saved:/)).not.toBeInTheDocument();
  });
});
