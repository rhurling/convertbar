import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, fireEvent, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("1.2.3") }));
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
    history_show_duration: true,
    encode_priority: "normal",
    ...overrides,
  };
}

// Drives get_platform_capabilities per test. Reset in beforeEach so a test that flips it
// cannot leak the note into an unrelated assertion.
let groupScopedFlag = false;

const META: PresetMetadata = {
  codec: "h265",
  resolution: "1080p",
  quality: "hq",
  preset: "Fast 1080p30",
  device: "apple",
};

/**
 * The inputs mount as soon as `get_settings` lands, but `useSettings` keeps loading after that
 * (`list_handbrake_presets`, then `loadPresetData`), and each arrival re-syncs a draft from the
 * freshly loaded value (`SettingsPage.tsx:60-71`). A test that types before the last of those
 * effects has flushed gets its edit silently overwritten, then fails somewhere unrelated —
 * `presetSuffix` is the last value to land (`useSettings.ts:75`), so waiting for it anchors the
 * whole load. Finding an input with `findBy*` is not enough: that resolves on DOM presence.
 */
async function waitForSettingsToSettle() {
  await waitFor(() =>
    expect(screen.getByPlaceholderText(".{resolution}-{codec}")).toHaveValue(
      ".{resolution}-{codec}",
    ),
  );
}

afterEach(() => {
  // Only ever armed by the server-head version test below (stubEnv/stubGlobal/resetModules) —
  // a no-op otherwise, so it is safe to run unconditionally after every test in this file.
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

beforeEach(() => {
  vi.clearAllMocks();
  groupScopedFlag = false;
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
      case "get_platform_capabilities":
        return Promise.resolve({
          can_pause_process: true,
          priority_is_group_scoped: groupScopedFlag,
        });
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

/** Desktop head with a given cleanup mode and a literal (already-resolved) suffix. */
function withMode(cleanup_mode: string, suffix: string) {
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
}

describe("SettingsPage", () => {
  // Desktop's own "Updates" version label was replaced by `<UpdatePanel />` (mocked to null
  // above), which sources the version from useUpdate()/getUpdateState() instead — that path is
  // pinned by UpdatePanel.test.tsx, not here. getAppInfo() still drives SettingsPage's OWN
  // version display, but only on the server head (no UpdatePanel there), so that's what this
  // pins now. Same resetModules/stubEnv approach as events.test.ts's server-head suite: isServerHead
  // is a module-level const, so the env must be stubbed and the module graph reloaded fresh.
  it("renders the app version from getAppInfo() on the server head, next to a releases link (its only version display, since there's no UpdatePanel there)", async () => {
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

    // The server head never checks for updates, so this link is the deployment's only route to
    // "is there anything newer than what I redeployed?" — unlike the desktop head, it cannot be
    // gated on an available update, and must not silently disappear.
    const link = screen.getByRole("link", { name: /release notes/i });
    expect(link).toHaveAttribute("href", "https://github.com/rhurling/convertbar/releases");
    expect(link).toHaveAttribute("target", "_blank");
  });

  it("does not write the HandBrakeCLI path per edit; commits on blur", async () => {
    render(<SettingsPage onHbPathChanged={() => {}} />);
    const input = await screen.findByPlaceholderText(
      "/usr/local/bin/HandBrakeCLI",
    );
    await waitForSettingsToSettle();

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
    await waitForSettingsToSettle();

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
    await waitForSettingsToSettle();
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
    await waitForSettingsToSettle();

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

  it("commits every unblurred draft when the page unmounts", async () => {
    // Commit-on-blur alone loses data: crossing the 1300px layout breakpoint remounts this page
    // (browser zoom crosses it without touching the window), and an unmount fires no `blur`, so
    // a typed-but-unblurred value was silently discarded with no warning.
    const { unmount } = render(<SettingsPage onHbPathChanged={() => {}} />);
    const hbInput = await screen.findByPlaceholderText("/usr/local/bin/HandBrakeCLI");

    // Measured: without this, CPU contention reset hbDraft to "" right after the first change
    // below, so commitHbPath's equality guard saw no diff and wrote nothing — while the three
    // later edits survived, making the failure look like it was about handbrake_path alone.
    await waitForSettingsToSettle();

    fireEvent.change(hbInput, { target: { value: "/new/hb" } });
    fireEvent.change(screen.getByPlaceholderText(".downloading"), {
      target: { value: ".part" },
    });
    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "2.5" } });
    fireEvent.change(screen.getByPlaceholderText(".{resolution}-{codec}"), {
      target: { value: ".hevc" },
    });

    unmount();

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "handbrake_path",
        value: "/new/hb",
      });
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "watch_skip_marker",
        value: ".part",
      });
      expect(invokeMock).toHaveBeenCalledWith("update_setting", {
        key: "low_disk_min_gb",
        value: "2.5",
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_suffix", {
        preset: "Fast 1080p30",
        suffix: ".hevc",
      });
    });
  });

  it("writes nothing when it unmounts before settings have loaded", async () => {
    // Until get_settings lands, every draft holds a placeholder ("" for the paths, "0" for the
    // threshold). A commit-on-unmount that fired anyway would persist those over the user's
    // stored values — an unmount during the initial load must write nothing at all.
    // get_platform_capabilities is the read-only getAppInfo() effect (runs independently of
    // settings loading), not a write, so it's allowed here alongside get_settings.
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "get_settings") return new Promise(() => {}); // never resolves
      if (cmd === "get_platform_capabilities") {
        return Promise.resolve({ can_pause_process: true, priority_is_group_scoped: false });
      }
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }) as typeof invoke);

    const { unmount } = render(<SettingsPage onHbPathChanged={() => {}} />);
    expect(await screen.findByText("Loading settings...")).toBeInTheDocument();

    unmount();

    expect(invokeMock.mock.calls.map((c) => c[0]).sort()).toEqual(
      ["get_platform_capabilities", "get_settings"].sort(),
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

  it("offers the history duration toggle on the server head, where it matters most", async () => {
    // Unlike the menu bar and notification groups, this one is deliberately NOT wrapped in
    // !isServerHead: the Docker web UI is the feature's primary audience. Asserting on the
    // desktop render would pass even if it were gated to desktop only.
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

    expect(await screen.findByLabelText("Show processing time")).toBeInTheDocument();
  });

  it("writes history_show_duration=false when the toggle is unchecked", async () => {
    // Presence is not wiring. Without this, a checkbox bound to the wrong key — or one
    // sending String(!e.target.checked) — ships silently: the Rust side's writable test
    // proves the backend accepts the key, not that the UI sends it.
    render(<SettingsPage />);

    fireEvent.click(await screen.findByLabelText("Show processing time"));

    await waitFor(() =>
      expect(updateCallsFor("history_show_duration")).toHaveLength(1),
    );
    expect(
      (updateCallsFor("history_show_duration")[0][1] as { value: string }).value,
    ).toBe("false");
  });

  it("warns only when keep is selected AND the resolved suffix is empty", async () => {
    const warning = /cannot keep the original/i;

    // An empty resolved suffix comes from resolve_suffix_template returning "".
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

  it("does not call a whitespace-only suffix in-place — the engine only treats the exactly-empty one that way", async () => {
    // A " " suffix produces `vacation .mp4`, a path distinct from the source, so nothing is
    // re-encoded in place and nothing is blocked under keep: those jobs queue and run fine.
    // Warning on `.trim() === ""` told the user their files would be skipped when they would
    // not be, and claimed the skip-by-suffix shortcut was off when a non-empty suffix keeps it
    // on (`stem.ends_with(suffix)` in queue_ops).
    withMode("keep", " ");
    render(<SettingsPage />);

    expect(await screen.findByLabelText("Keep both files")).toBeInTheDocument();
    // Anchor on the resolved preview so the 250ms debounce has settled before asserting absence.
    await waitFor(() => expect(screen.getByText("vacation .mp4")).toBeInTheDocument());
    expect(screen.queryByText(/re-encoded in place/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/cannot keep the original/i)).not.toBeInTheDocument();
  });

  it("offers Trash as a remedy for a keep-blocked in-place job on desktop", async () => {
    // Only `keep` blocks an in-place job (queue_ops.rs) — Trash permits it, routing the source
    // to the OS Trash where it stays recoverable. Naming Delete as the sole way out pushed
    // desktop users into permanent deletion for a job the recoverable mode would have run.
    withMode("keep", "");
    render(<SettingsPage />);

    const warning = await screen.findByText(/cannot keep the original/i);
    expect(warning.textContent).toMatch(/Trash/);
  });

  it("does not offer Trash as that remedy on the server head, which has no Trash mode", async () => {
    // The mirror of the desktop case: the server head deliberately hides the Trash radio
    // (DeleteDisposer, .Trash-<uid> on NAS mounts), so naming it here would send the user
    // looking for a setting that is not on their screen.
    vi.stubEnv("VITE_HEAD", "server");
    const fetchMock = vi.fn((path: string) => {
      if (path === "/api/settings") {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve(makeSettings({ cleanup_mode: "keep" })),
        });
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
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve("") });
      }
      return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "not mocked" }) });
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.resetModules();

    const { default: FreshSettingsPage } = await import("./SettingsPage");
    render(<FreshSettingsPage />);

    const warning = await screen.findByText(/cannot keep the original/i);
    expect(warning.textContent).not.toMatch(/Trash/);
    expect(warning.textContent).toMatch(/Delete/);
  });

  it("writes the chosen encode priority", async () => {
    render(<SettingsPage onHbPathChanged={() => {}} />);

    const idle = await screen.findByLabelText(/only when the machine is idle/i);
    fireEvent.click(idle);

    await waitFor(() => expect(updateCallsFor("encode_priority")).toHaveLength(1));
    expect(
      (updateCallsFor("encode_priority")[0][1] as { value: string }).value,
    ).toBe("idle");
  });

  it("shows the Linux caveat only when priority is group-scoped", async () => {
    // The setting is offered on Linux rather than hidden — autogrouping can be disabled, and a
    // process with no cpu controller on its path does get real host-wide nice — so the note is
    // what keeps it honest for the users where it does nothing.
    groupScopedFlag = true;
    render(<SettingsPage onHbPathChanged={() => {}} />);
    expect(await screen.findByText(/--cpu-shares/)).toBeInTheDocument();
  });

  it("shows no caveat where priority works normally", async () => {
    groupScopedFlag = false;
    render(<SettingsPage onHbPathChanged={() => {}} />);
    // Await the control itself so the assertion cannot pass merely because nothing has
    // rendered yet — the failure mode a bare queryBy would hide.
    await screen.findByLabelText(/only when the machine is idle/i);
    expect(screen.queryByText(/--cpu-shares/)).not.toBeInTheDocument();
  });
});
