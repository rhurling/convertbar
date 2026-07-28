import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import UpdatePanel from "./UpdatePanel";

const mockUpdate = {
  state: null as unknown,
  actionError: null as string | null,
  checkNow: vi.fn(),
  install: vi.fn(),
  skip: vi.fn(),
  restart: vi.fn(),
};

vi.mock("../hooks/useUpdate", () => ({
  useUpdate: () => mockUpdate,
  // Real set from the hook (checking/downloading/waitingForIdle/readyToRestart) — kept in sync
  // by hand since this mock replaces the whole module; UpdatePanel imports it to disable
  // "Check now" in exactly the statuses the backend's manual_check_block would refuse.
  MANUAL_CHECK_BLOCKED_STATUSES: new Set(["checking", "downloading", "waitingForIdle", "readyToRestart"]),
}));
vi.mock("../lib/tauri", () => ({
  commands: { updateSetting: vi.fn().mockResolvedValue(undefined) },
}));

import { commands } from "../lib/tauri";

const base = {
  mode: "automatic",
  status: "idle",
  current_version: "1.0.0",
  available: null,
  just_installed: null,
  last_checked: null,
  last_error: null,
};

describe("UpdatePanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUpdate.state = base;
    mockUpdate.actionError = null;
  });

  it("renders release notes as plain text, not markup", async () => {
    // Notes are arbitrary release-body markdown from a remote endpoint. Rendering them as
    // HTML would be an injection surface for content the app does not control.
    mockUpdate.state = {
      ...base,
      status: "available",
      available: { version: "1.1.0", date: null, notes: "<img src=x onerror=alert(1)>" },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText("<img src=x onerror=alert(1)>")).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
  });

  it("shows a deferred install instead of appearing to do nothing", async () => {
    // Without this the user presses Install during an encode and sees no change at all.
    mockUpdate.state = {
      ...base,
      status: "waitingForIdle",
      available: { version: "1.1.0", date: null, notes: null },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText(/will install when the queue finishes/i)).toBeInTheDocument();
  });

  it("offers Install and Skip only when an update is available", async () => {
    render(<UpdatePanel />);
    expect(screen.queryByRole("button", { name: /install/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /skip/i })).toBeNull();

    mockUpdate.state = {
      ...base,
      status: "available",
      available: { version: "1.1.0", date: null, notes: "notes" },
    };
    render(<UpdatePanel />);
    await userEvent.click(await screen.findByRole("button", { name: /install/i }));
    expect(mockUpdate.install).toHaveBeenCalled();
  });

  it("shows what's new after an automatic install so the changelog is not lost", async () => {
    mockUpdate.state = {
      ...base,
      status: "readyToRestart",
      just_installed: { version: "1.1.0", notes: "### Fixes\n- a fix" },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText(/what's new in 1\.1\.0/i)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /restart/i }));
    expect(mockUpdate.restart).toHaveBeenCalled();
  });

  it("keeps an error visible instead of clearing it on a timer", () => {
    // The old "Check for updates" button cleared its status via setTimeout after 3-5s.
    // Advancing well past that window and re-asserting is what actually proves nothing
    // clears this error anymore — without the advance, this would pass even if a timer
    // had been added back in.
    vi.useFakeTimers();
    try {
      mockUpdate.state = { ...base, status: "error", last_error: "network unreachable" };
      render(<UpdatePanel />);
      expect(screen.getByText(/network unreachable/i)).toBeInTheDocument();

      vi.advanceTimersByTime(10 * 60 * 1000);
      expect(screen.getByText(/network unreachable/i)).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("renders a rejected manual action even though it never touched last_error", async () => {
    // e.g. "Check now" refused because an install is already pending — the backend rejects
    // the call directly and never writes state.last_error for it. If the panel only rendered
    // last_error, this failure would be invisible.
    mockUpdate.state = { ...base, status: "readyToRestart" };
    mockUpdate.actionError = "an update is already waiting to install once the queue is idle";
    render(<UpdatePanel />);
    expect(
      await screen.findByText(/an update is already waiting to install/i),
    ).toBeInTheDocument();
  });

  it("does not render the same offline-check message twice", async () => {
    // A manual check made while offline dual-writes the identical string to both
    // state.last_error (the scheduler's persistent channel) and actionError (the rejected
    // promise from this specific click) — the panel must collapse that to one line.
    mockUpdate.state = { ...base, status: "error", last_error: "network unreachable" };
    mockUpdate.actionError = "network unreachable";
    render(<UpdatePanel />);
    expect(await screen.findAllByText(/network unreachable/i)).toHaveLength(1);
  });

  it("renders mode radios that reflect and update the current mode", async () => {
    mockUpdate.state = { ...base, mode: "automatic" };
    render(<UpdatePanel />);

    expect(screen.getByLabelText("Automatic")).toBeChecked();
    expect(screen.getByLabelText("Notify me")).not.toBeChecked();

    await userEvent.click(screen.getByLabelText("Notify me"));
    expect(commands.updateSetting).toHaveBeenCalledWith("update_mode", "notify");
  });

  it("disables Check now while an action the backend would refuse is in flight", async () => {
    mockUpdate.state = { ...base, status: "downloading", available: { version: "1.1.0", date: null, notes: null } };
    render(<UpdatePanel />);
    expect(screen.getByRole("button", { name: /check now/i })).toBeDisabled();
  });
});
