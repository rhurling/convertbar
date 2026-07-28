import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock("../components/UpdatePanel", () => ({ default: () => null }));

import { invoke } from "@tauri-apps/api/core";
import SettingsPage from "./SettingsPage";
import type { AppSettings, PresetMetadata } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);

function makeSettings(): AppSettings {
  return {
    preset: "Fast 1080p30",
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
  };
}

const META: PresetMetadata = {
  codec: "h265",
  resolution: "1080p",
  quality: "hq",
  preset: "Fast 1080p30",
  device: "apple",
};

beforeEach(() => {
  vi.clearAllMocks();
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve(makeSettings());
      case "list_handbrake_presets":
        return Promise.resolve(["Fast 1080p30"]);
      case "get_preset_suffix":
        return Promise.resolve(".{resolution}-{codec}");
      case "generate_preset_suffix":
        return Promise.resolve(META);
      case "resolve_suffix_template":
        return Promise.resolve(".RESOLVED"); // sentinel proving the preview is backend-computed
      case "update_setting":
      case "set_preset_suffix":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

function updateCallsFor(key: string) {
  return invokeMock.mock.calls.filter(
    (c) => c[0] === "update_setting" && (c[1] as { key: string }).key === key,
  );
}

describe("SettingsPage", () => {
  it("does not write the HandBrakeCLI path per edit; commits on blur", async () => {
    render(<SettingsPage onHbPathChanged={() => {}} />);
    const input = await screen.findByPlaceholderText(
      "/usr/local/bin/HandBrakeCLI",
    );

    // Two edits reflect immediately (draft) with no write yet.
    fireEvent.change(input, { target: { value: "/new" } });
    fireEvent.change(input, { target: { value: "/new/hb" } });
    expect(input).toHaveValue("/new/hb");
    expect(updateCallsFor("handbrake_path")).toHaveLength(0);

    fireEvent.blur(input);

    await waitFor(() =>
      expect(updateCallsFor("handbrake_path")).toHaveLength(1),
    );
    expect(invokeMock).toHaveBeenCalledWith("update_setting", {
      key: "handbrake_path",
      value: "/new/hb",
    });
  });

  it("does not write the skip marker per edit; commits on blur", async () => {
    render(<SettingsPage />);
    const input = await screen.findByPlaceholderText(".downloading");

    fireEvent.change(input, { target: { value: ".pa" } });
    fireEvent.change(input, { target: { value: ".part" } });
    expect(updateCallsFor("watch_skip_marker")).toHaveLength(0);

    fireEvent.blur(input);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "watch_skip_marker",
        value: ".part",
      }),
    );
  });

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

  it("does not persist the suffix per edit; commits on blur", async () => {
    render(<SettingsPage />);
    const input = await screen.findByPlaceholderText(".{resolution}-{codec}");

    fireEvent.change(input, { target: { value: ".h" } });
    fireEvent.change(input, { target: { value: ".hevc" } });
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "set_preset_suffix"),
    ).toHaveLength(0);

    fireEvent.blur(input);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_preset_suffix", {
        preset: "Fast 1080p30",
        suffix: ".hevc",
      }),
    );
  });

  it("switches the bad-source action to permanent deletion", async () => {
    render(<SettingsPage />);
    const radio = await screen.findByLabelText(/delete bad source files permanently/i);
    fireEvent.click(radio);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "bad_source_action",
        value: "delete",
      }),
    );
  });

  it("renders the suffix preview from the backend resolver, not a JS copy", async () => {
    render(<SettingsPage />);
    // The JS copy would compute ".1080p-h265"; the backend sentinel proves delegation.
    await waitFor(() =>
      expect(screen.getByText("vacation.RESOLVED.mp4")).toBeInTheDocument(),
    );
  });

  it("ignores a stale suffix-preview resolve that lands after a newer edit", async () => {
    // N1: the debounce cancels the pending *timer*, but an already-fired resolve invoke is
    // not generation-checked — a slow older resolve must not overwrite a newer draft's preview.
    const resolvers: Array<{ template: string; resolve: (v: string) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: { template?: string }) => {
      switch (cmd) {
        case "get_settings":
          return Promise.resolve(makeSettings());
        case "list_handbrake_presets":
          return Promise.resolve(["Fast 1080p30"]);
        case "get_preset_suffix":
          return Promise.resolve(".{resolution}-{codec}");
        case "generate_preset_suffix":
          return Promise.resolve(META);
        case "resolve_suffix_template":
          return new Promise<string>((resolve) =>
            resolvers.push({ template: args!.template!, resolve }),
          );
        case "set_preset_suffix":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    render(<SettingsPage />);
    const input = await screen.findByPlaceholderText(".{resolution}-{codec}");

    // Settle the initial debounced preview so its resolve can't confuse the assertions.
    await waitFor(() =>
      expect(resolvers.some((r) => r.template === ".{resolution}-{codec}")).toBe(true),
    );
    await act(async () => {
      resolvers.find((r) => r.template === ".{resolution}-{codec}")!.resolve(".INITIAL");
    });
    await screen.findByText("vacation.INITIAL.mp4");

    // Edit to A, let its debounced resolve fire (left pending).
    fireEvent.change(input, { target: { value: ".AAA" } });
    await waitFor(() => expect(resolvers.some((r) => r.template === ".AAA")).toBe(true));
    // Edit to B, let its debounced resolve fire (left pending).
    fireEvent.change(input, { target: { value: ".BBB" } });
    await waitFor(() => expect(resolvers.some((r) => r.template === ".BBB")).toBe(true));

    // Newer (B) resolves first, older (A) resolves late — A must not clobber the preview.
    await act(async () => {
      resolvers.find((r) => r.template === ".BBB")!.resolve(".RESOLVED_B");
    });
    await act(async () => {
      resolvers.find((r) => r.template === ".AAA")!.resolve(".RESOLVED_A");
    });

    expect(screen.getByText("vacation.RESOLVED_B.mp4")).toBeInTheDocument();
    expect(screen.queryByText("vacation.RESOLVED_A.mp4")).not.toBeInTheDocument();
  });
});
