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
    low_disk_min_gb: 0,
    bad_source_action: "trash",
    update_mode: "automatic",
    history_show_duration: true,
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

function makeMeta(preset: string): PresetMetadata {
  return { codec: "h265", resolution: "1080p", quality: "hq", preset, device: "apple" };
}

// Backend state the dispatcher reads from; tests mutate these before rendering.
let currentPreset: string;
let presetList: string[];
let presetsShouldThrow: boolean;
// What list_handbrake_presets rejects with, shaped as the desktop backend rejects: the
// serialized `CommandError`. A test may swap in the panic variant.
let presetsFailure: unknown;
let suffixes: Record<string, string | null>;
let detectResult: string | null;

type Args = { key?: string; value?: string; preset?: string; suffix?: string };

beforeEach(() => {
  vi.clearAllMocks();
  currentPreset = "Fast 1080p30";
  presetList = ["Fast 1080p30", "HQ 1080p30"];
  presetsShouldThrow = false;
  presetsFailure = { error: "no cli" };
  suffixes = { "Fast 1080p30": ".fast", "HQ 1080p30": ".hq" };
  detectResult = null;

  invokeMock.mockImplementation(((cmd: string, args?: Args) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve(makeSettings(currentPreset));
      case "list_handbrake_presets":
        return presetsShouldThrow
          ? Promise.reject(presetsFailure)
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

  it("coerces numeric settings to real numbers in the optimistic merge", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.updateSetting("low_disk_min_gb", "5");
    });

    // A stringly "5" must land as a real number so the number input binds correctly.
    expect(result.current.settings?.low_disk_min_gb).toBe(5);
  });

  it("surfaces an error and restores the value when a write fails", async () => {
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    invokeMock.mockImplementationOnce(((cmd: string) =>
      cmd === "update_setting"
        ? Promise.reject({ error: "db locked" })
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

  it("does not blame the HandBrakeCLI install for a panic", async () => {
    // This site swallowed the error and showed fixed copy, so it survived the String(e) sweep
    // and kept reproducing item 16's own bug: a crash inside list_handbrake_presets sent the
    // user off to check an install that was fine. The discriminator only helps if the copy
    // branches on it.
    presetsShouldThrow = true;
    presetsFailure = { error: "task panicked: boom", kind: "panic" };

    const { result } = renderHook(() => useSettings());

    await waitFor(() =>
      expect(result.current.presetsError).toBe(
        "Internal error (this is a bug): task panicked: boom",
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

  it("restores only the failed key on write failure, leaving a concurrent edit intact", async () => {
    // N2: the failure path must not re-fetch the whole settings object — an out-of-order
    // get_settings can resolve mid-flight and clobber a *different* key's optimistic edit.
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    // A concurrent optimistic edit the backend's get_settings does not (yet) reflect.
    await act(async () => {
      await result.current.updateSetting("skip_already_converted", "true");
    });
    expect(result.current.settings?.skip_already_converted).toBe(true);

    const readsBefore = invokeMock.mock.calls.filter((c) => c[0] === "get_settings").length;

    // The next write fails.
    invokeMock.mockImplementationOnce(((cmd: string) =>
      cmd === "update_setting"
        ? Promise.reject({ error: "db locked" })
        : Promise.reject(new Error(`unexpected: ${cmd}`))) as typeof invoke);

    await act(async () => {
      await result.current.updateSetting("handbrake_path", "/bad/path");
    });

    expect(result.current.error).not.toBeNull();
    // The failed key reverts to its stored truth...
    expect(result.current.settings?.handbrake_path).toBe("");
    // ...without a whole-object refetch (which would reset skip_already_converted to its
    // stored false) and without touching get_settings at all.
    expect(result.current.settings?.skip_already_converted).toBe(true);
    const readsAfter = invokeMock.mock.calls.filter((c) => c[0] === "get_settings").length;
    expect(readsAfter).toBe(readsBefore);
  });

  it("restores the previous suffix when a suffix save fails, so a blur-retry can fire", async () => {
    // N3: updatePresetSuffix must mirror updateSetting and roll back on failure, or the
    // SettingsPage `suffixDraft !== presetSuffix` commit guard treats the unsaved value as
    // committed and a re-blur becomes a silent no-op.
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.presetSuffix).toBe(".fast"));

    invokeMock.mockImplementationOnce(((cmd: string) =>
      cmd === "set_preset_suffix"
        ? Promise.reject({ error: "db locked" })
        : Promise.reject(new Error(`unexpected: ${cmd}`))) as typeof invoke);

    await act(async () => {
      await result.current.updatePresetSuffix(".broken");
    });

    expect(result.current.error).not.toBeNull();
    expect(result.current.presetSuffix).toBe(".fast");
  });

  it("converges a second mounted instance after one instance writes", async () => {
    // Three-col layout mounts History and Settings simultaneously and permanently, each with
    // its own useSettings() instance. Without convergence, flipping cleanup_mode in Settings
    // would leave History's copy stale indefinitely — two visible panels disagreeing about
    // destructive behavior. Fix: a module-level registry shares the confirmed write instead of
    // each instance needing its own backend event or refetch.
    const a = renderHook(() => useSettings());
    const b = renderHook(() => useSettings());
    await waitFor(() => expect(a.result.current.loading).toBe(false));
    await waitFor(() => expect(b.result.current.loading).toBe(false));
    expect(b.result.current.settings?.cleanup_mode).toBe("trash");

    const readsBefore = invokeMock.mock.calls.filter((c) => c[0] === "get_settings").length;

    await act(async () => {
      await a.result.current.updateSetting("cleanup_mode", "keep");
    });

    expect(a.result.current.settings?.cleanup_mode).toBe("keep");
    expect(b.result.current.settings?.cleanup_mode).toBe("keep");
    // Convergence must come from sharing the value the write already confirmed, not from a
    // second instance polling get_settings again (a refetch storm as more panels mount).
    const readsAfter = invokeMock.mock.calls.filter((c) => c[0] === "get_settings").length;
    expect(readsAfter).toBe(readsBefore);
  });

  it("ignores stale suffix/metadata when rapid preset switches resolve out of order", async () => {
    // N4: two quick preset changes (A then B) can interleave so A's suffix/metadata resolve
    // last, leaving state for A while settings.preset is B — the same race useQueue guards.
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.loading).toBe(false));

    // Make the preset-scoped suffix read controllable; metadata resolves immediately so the
    // test is agnostic to whether the loads run sequentially or in parallel.
    const suffixResolvers: Array<{ preset: string; resolve: (v: string) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: Args) => {
      switch (cmd) {
        case "update_setting":
          return Promise.resolve(undefined);
        case "get_preset_suffix":
          return new Promise<string>((resolve) =>
            suffixResolvers.push({ preset: args!.preset!, resolve }),
          );
        case "generate_preset_suffix":
          return Promise.resolve(makeMeta(args!.preset!));
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    act(() => {
      void result.current.updateSetting("preset", "A");
    });
    await waitFor(() => expect(suffixResolvers.some((r) => r.preset === "A")).toBe(true));
    act(() => {
      void result.current.updateSetting("preset", "B");
    });
    await waitFor(() => expect(suffixResolvers.some((r) => r.preset === "B")).toBe(true));

    // Newer request (B) resolves first, older (A) resolves late.
    await act(async () => {
      suffixResolvers.find((r) => r.preset === "B")!.resolve(".bbb");
    });
    await act(async () => {
      suffixResolvers.find((r) => r.preset === "A")!.resolve(".aaa");
    });

    expect(result.current.settings?.preset).toBe("B");
    expect(result.current.presetSuffix).toBe(".bbb");
    expect(result.current.presetMetadata?.preset).toBe("B");
  });
});
