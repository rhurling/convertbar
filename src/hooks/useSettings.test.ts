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
        return Promise.resolve(suffixes[args!.preset!] ?? null);
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

  it("writes the default suffix template when the preset has none stored", async () => {
    suffixes = {}; // no stored suffix for any preset

    const { result } = renderHook(() => useSettings());

    await waitFor(() => expect(result.current.presetSuffix).toBe(DEFAULT_SUFFIX));
    expect(invokeMock).toHaveBeenCalledWith("set_preset_suffix", {
      preset: "Fast 1080p30",
      suffix: DEFAULT_SUFFIX,
    });
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
