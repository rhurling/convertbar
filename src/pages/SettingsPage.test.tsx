import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
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

function makeSettings(overrides: Partial<AppSettings> = {}): AppSettings {
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
    update_mode: "automatic",
    ...overrides,
  };
}

const META: PresetMetadata = {
  codec: "h265",
  resolution: "1080p",
  quality: "hq",
  preset: "Fast 1080p30",
  device: "apple",
};

afterEach(() => {
  // Only ever armed by the server-head version test below (stubEnv/stubGlobal/resetModules) —
  // a no-op otherwise, so it is safe to run unconditionally after every test in this file.
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

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
  // Desktop's own "Updates" version label was replaced by `<UpdatePanel />` (mocked to null
  // above), which sources the version from useUpdate()/getUpdateState() instead — that path is
  // pinned by UpdatePanel.test.tsx, not here. getAppInfo() still drives SettingsPage's OWN
  // version display, but only on the server head (no UpdatePanel there), so that's what this
  // pins now. Same resetModules/stubEnv approach as events.test.ts's server-head suite: isServerHead
  // is a module-level const, so the env must be stubbed and the module graph reloaded fresh.
  it("renders the app version from getAppInfo() on the server head (its only version display, since there's no UpdatePanel there)", async () => {
    vi.stubEnv("VITE_HEAD", "server");
    const fetchMock = vi.fn((path: string) => {
      if (path === "/api/info") {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({
              version: "1.2.3",
              head: "server",
              can_pause_process: true,
              auth_required: false,
              browse_roots: [],
            }),
        });
      }
      if (path === "/api/settings") {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(makeSettings()) });
      }
      if (path === "/api/handbrake/presets") {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(["Fast 1080p30"]) });
      }
      if (path.includes("/suffix/generate")) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(META) });
      }
      if (path.includes("/suffix")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve(".{resolution}-{codec}"),
        });
      }
      return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "not mocked" }) });
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.resetModules();

    const { default: FreshSettingsPage } = await import("./SettingsPage");
    render(<FreshSettingsPage />);

    expect(await screen.findByText("v1.2.3")).toBeInTheDocument();
  });

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

  it("offers three cleanup modes on desktop", async () => {
    render(<SettingsPage />);

    expect(await screen.findByLabelText("Move original to Trash")).toBeInTheDocument();
    expect(screen.getByLabelText("Delete original permanently")).toBeInTheDocument();
    expect(screen.getByLabelText("Keep both files")).toBeInTheDocument();
  });

  it("writes cleanup_mode=keep when Keep is chosen", async () => {
    render(<SettingsPage />);

    fireEvent.click(await screen.findByLabelText("Keep both files"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "cleanup_mode",
        value: "keep",
      }),
    );
  });

  it("hides the Trash option on the server head", async () => {
    // A headless deployment has no Trash, and the `trash` crate litters .Trash-<uid>
    // directories on the NAS mounts these servers run against.
    vi.stubEnv("VITE_HEAD", "server");
    const fetchMock = vi.fn((path: string) => {
      if (path === "/api/settings") {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(makeSettings()) });
      }
      if (path === "/api/handbrake/presets") {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(["Fast 1080p30"]) });
      }
      if (path === "/api/info") {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({
              version: "1.2.3",
              head: "server",
              can_pause_process: true,
              auth_required: false,
              browse_roots: [],
            }),
        });
      }
      if (path.includes("/suffix/generate")) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(META) });
      }
      if (path.includes("/suffix")) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve("-conv") });
      }
      return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "not mocked" }) });
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.resetModules();

    const { default: FreshSettingsPage } = await import("./SettingsPage");
    render(<FreshSettingsPage />);

    expect(await screen.findByLabelText("Delete original permanently")).toBeInTheDocument();
    expect(screen.getByLabelText("Keep both files")).toBeInTheDocument();
    expect(screen.queryByLabelText("Move original to Trash")).not.toBeInTheDocument();
  });

  it("warns only when keep is selected AND the resolved suffix is empty", async () => {
    const warning = /cannot keep the original/i;

    // An empty resolved suffix comes from resolve_suffix_template returning "".
    const withMode = (cleanup_mode: string, suffix: string) => {
      invokeMock.mockImplementation(((cmd: string) => {
        switch (cmd) {
          case "get_settings":
            return Promise.resolve(makeSettings({ cleanup_mode }));
          case "list_handbrake_presets":
            return Promise.resolve(["Fast 1080p30"]);
          case "get_preset_suffix":
            return Promise.resolve(suffix);
          case "generate_preset_suffix":
            return Promise.resolve(META);
          case "resolve_suffix_template":
            return Promise.resolve(suffix);
          default:
            return Promise.resolve(null);
        }
      }) as typeof invoke);
    };

    withMode("keep", "-conv");
    const a = render(<SettingsPage />);
    expect(await screen.findByLabelText("Keep both files")).toBeInTheDocument();
    // Anchor on the resolved preview so the 250ms debounce has definitely settled before
    // asserting the warning's absence — otherwise this checks the still-empty initial state.
    await waitFor(() => expect(screen.getByText("vacation-conv.mp4")).toBeInTheDocument());
    expect(screen.queryByText(warning)).not.toBeInTheDocument();
    a.unmount();

    withMode("delete", "");
    const b = render(<SettingsPage />);
    expect(await screen.findByLabelText("Keep both files")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("vacation.mp4")).toBeInTheDocument());
    expect(screen.queryByText(warning)).not.toBeInTheDocument();
    b.unmount();

    // Only the combination warns — the setting alone or the empty suffix alone must not.
    withMode("keep", "");
    render(<SettingsPage />);
    expect(await screen.findByText(warning)).toBeInTheDocument();
  });
});
