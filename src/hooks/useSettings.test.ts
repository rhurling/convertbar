import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "./useSettings";
import type { AppSettings, PresetMetadata } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
const DEFAULT_SUFFIX = ".{resolution}-{codec}";

function makeSettings(preset: string): AppSettings {
  return {
    preset,
    cleanup_mode: "trash",
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
  };
}

function makeMeta(preset: string): PresetMetadata {
  return { codec: "h265", resolution: "1080p", quality: "hq", preset, device: "apple" };
}

// Backend state the dispatcher reads from; tests mutate these before rendering.
let currentPreset: string;
let presetList: string[];
let presetsShouldThrow: boolean;
let suffixes: Record<string, string | null>;
let detectResult: string | null;

type Args = { key?: string; value?: string; preset?: string; suffix?: string };

beforeEach(() => {
  vi.clearAllMocks();
  currentPreset = "Fast 1080p30";
  presetList = ["Fast 1080p30", "HQ 1080p30"];
  presetsShouldThrow = false;
  suffixes = { "Fast 1080p30": ".fast", "HQ 1080p30": ".hq" };
  detectResult = null;

  invokeMock.mockImplementation(((cmd: string, args?: Args) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve(makeSettings(currentPreset));
      case "list_handbrake_presets":
        return presetsShouldThrow
          ? Promise.reject(new Error("no cli"))
          : Promise.resolve(presetList);
      case "get_preset_suffix":
        // Backend supplies the default template when a preset has no stored row,
        // so the frontend never has to write one from a read path.
        return Promise.resolve(suffixes[args!.preset!] ?? DEFAULT_SUFFIX);
      case "set_preset_suffix":
        suffixes[args!.preset!] = args!.suffix!;
        return Promise.resolve(undefined);
      case "generate_preset_suffix":
        return Promise.resolve(makeMeta(args!.preset!));
      case "update_setting":
        if (args!.key === "preset") currentPreset = args!.value!;
        return Promise.resolve(undefined);
      case "detect_handbrake":
        return Promise.resolve(detectResult);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("useSettings", () => {
  it("loads settings, presets, the stored suffix, and metadata on mount", async () => {
    const { result } = renderHook(() => useSettings());

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.settings?.preset).toBe("Fast 1080p30");
    expect(result.current.presets).toEqual(["Fast 1080p30", "HQ 1080p30"]);
    expect(result.current.presetSuffix).toBe(".fast");
    expect(result.current.presetMetadata?.preset).toBe("Fast 1080p30");
    expect(result.current.presetsError).toBeNull();
  });

  it("shows the backend-provided default suffix without writing from the read path", async () => {
    suffixes = {}; // no stored suffix for any preset; backend returns the default

    const { result } = renderHook(() => useSettings());

    await waitFor(() => expect(result.current.presetSuffix).toBe(DEFAULT_SUFFIX));
    // The default now lives in the backend; a read must never persist it (StrictMode
    // would double-write, and it belongs in Rust, not a frontend read path).
    expect(invokeMock).not.toHaveBeenCalledWith(
      "set_preset_suffix",
      expect.anything(),
    );
  });

  it("updates a setting optimistically without a full get_settings refetch", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const settingsReadsBefore = invokeMock.mock.calls.filter(
      (c) => c[0] === "get_settings",
    ).length;

    await act(async () => {
      await result.current.updateSetting("handbrake_path", "/opt/HandBrakeCLI");
    });

    expect(result.current.settings?.handbrake_path).toBe("/opt/HandBrakeCLI");
    const settingsReadsAfter = invokeMock.mock.calls.filter(
      (c) => c[0] === "get_settings",
    ).length;
    expect(settingsReadsAfter).toBe(settingsReadsBefore);
  });

  it("coerces boolean settings to real booleans in the optimistic merge", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.updateSetting("skip_already_converted", "true");
    });

    // A stringly "true" must land as a real boolean so `checked={setting === true}` works.
    expect(result.current.settings?.skip_already_converted).toBe(true);
  });

  it("surfaces an error and restores the value when a write fails", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    invokeMock.mockImplementationOnce(((cmd: string) =>
      cmd === "update_setting"
        ? Promise.reject(new Error("db locked"))
        : Promise.reject(new Error(`unexpected: ${cmd}`))) as typeof invoke);

    await act(async () => {
      await result.current.updateSetting("handbrake_path", "/bad/path");
    });

    expect(result.current.error).not.toBeNull();
    // Optimistic value was not persisted, so state must fall back to the stored truth.
    expect(result.current.settings?.handbrake_path).toBe("");
  });

  it("surfaces an error and empties the preset list when listing fails", async () => {
    presetsShouldThrow = true;

    const { result } = renderHook(() => useSettings());

    await waitFor(() =>
      expect(result.current.presetsError).toBe(
        "Could not load presets. Is HandBrakeCLI installed?",
      ),
    );
    expect(result.current.presets).toEqual([]);
  });

  it("reloads suffix and metadata when the preset setting changes", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.presetSuffix).toBe(".fast"));

    await act(async () => {
      await result.current.updateSetting("preset", "HQ 1080p30");
    });

    expect(result.current.settings?.preset).toBe("HQ 1080p30");
    expect(result.current.presetSuffix).toBe(".hq");
    expect(result.current.presetMetadata?.preset).toBe("HQ 1080p30");
  });

  it("does not reload preset metadata for a non-preset setting change", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const metaCallsBefore = invokeMock.mock.calls.filter(
      (c) => c[0] === "generate_preset_suffix",
    ).length;

    await act(async () => {
      await result.current.updateSetting("cleanup_mode", "keep");
    });

    const metaCallsAfter = invokeMock.mock.calls.filter(
      (c) => c[0] === "generate_preset_suffix",
    ).length;
    expect(metaCallsAfter).toBe(metaCallsBefore);
  });

  it("persists a new suffix via updatePresetSuffix", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.updatePresetSuffix(".custom");
    });

    expect(result.current.presetSuffix).toBe(".custom");
    expect(invokeMock).toHaveBeenCalledWith("set_preset_suffix", {
      preset: "Fast 1080p30",
      suffix: ".custom",
    });
  });

  it("stores the detected HandBrake path when detection succeeds", async () => {
    detectResult = "/opt/homebrew/bin/HandBrakeCLI";
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let returned: string | null = null;
    await act(async () => {
      returned = await result.current.detectHandbrake();
    });

    expect(returned).toBe("/opt/homebrew/bin/HandBrakeCLI");
    expect(invokeMock).toHaveBeenCalledWith("update_setting", {
      key: "handbrake_path",
      value: "/opt/homebrew/bin/HandBrakeCLI",
    });
  });
});
