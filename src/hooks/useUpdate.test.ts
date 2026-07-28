import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useUpdate } from "./useUpdate";
import { commands } from "../lib/tauri";

let emit: ((payload: unknown) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: unknown }) => void) => {
    emit = (payload) => cb({ payload });
    return () => { emit = null; };
  }),
}));

const baseState = {
  mode: "automatic" as const,
  status: "idle" as const,
  current_version: "1.0.0",
  available: null,
  just_installed: null,
  last_checked: null,
  last_error: null,
};

describe("useUpdate", () => {
  beforeEach(() => {
    // Every other hook test in this codebase clears mocks between tests (useQueue.test.ts,
    // useSettings.test.ts); without it, vi.spyOn call history leaks across tests in this
    // file and "not.toHaveBeenCalled()" assertions see calls made by an earlier test.
    vi.clearAllMocks();
    emit = null;
    vi.spyOn(commands, "getUpdateState").mockResolvedValue(baseState);
    vi.spyOn(commands, "checkForUpdate").mockResolvedValue(undefined);
    vi.spyOn(commands, "skipUpdateVersion").mockResolvedValue(undefined);
  });

  it("seeds from the backend so a tab remount shows current state", async () => {
    // The panel must not start blank and then pop — the backend is the source of truth.
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state?.current_version).toBe("1.0.0"));
  });

  it("re-renders from the update-state event rather than polling", async () => {
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: "### Fixes\n- a fix" },
      });
    });

    await waitFor(() => {
      expect(result.current.state?.status).toBe("available");
      expect(result.current.state?.available?.version).toBe("1.1.0");
    });
  });

  it("passes the available version to skip so the backend records the right one", async () => {
    // Skip means "not this one" — sending the wrong version would silence a different release.
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: null },
      });
    });
    await waitFor(() => expect(result.current.state?.available).not.toBeNull());

    await act(async () => { await result.current.skip(); });
    expect(commands.skipUpdateVersion).toHaveBeenCalledWith("1.1.0");
  });

  it("does not call skip when nothing is available", async () => {
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.skip(); });
    expect(commands.skipUpdateVersion).not.toHaveBeenCalled();
  });

  it("surfaces a rejected check_for_update as actionError instead of an unhandled rejection", async () => {
    // check_for_update deliberately rejects with a user-facing message (e.g. a manual check
    // refused while an install is pending). The hook must catch it and expose the message,
    // not let the promise reject out from under a button's onClick.
    vi.spyOn(commands, "checkForUpdate").mockRejectedValue(
      "an update is already waiting to install",
    );
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => {
      await expect(result.current.checkNow()).resolves.toBeUndefined();
    });

    expect(result.current.actionError).toBe("an update is already waiting to install");
  });

  it("clears a previous actionError when a new action is attempted", async () => {
    vi.spyOn(commands, "checkForUpdate").mockRejectedValueOnce("boom");
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBe("boom");

    vi.spyOn(commands, "checkForUpdate").mockResolvedValueOnce(undefined);
    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBeNull();
  });

  it("does not let a stale initial getUpdateState response clobber a fresher push event", async () => {
    // If the update-state event arrives before the seeding getUpdateState() call resolves,
    // the late-resolving seed is older data and must not overwrite the newer pushed state.
    let resolveSeed: ((s: unknown) => void) | null = null;
    vi.spyOn(commands, "getUpdateState").mockReturnValue(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      new Promise((resolve) => { resolveSeed = resolve as any; }) as any,
    );

    const { result } = renderHook(() => useUpdate());

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "2.0.0", date: null, notes: null },
      });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("available"));

    await act(async () => {
      resolveSeed?.(baseState); // stale: status "idle", predates the event
    });

    expect(result.current.state?.status).toBe("available");
    expect(result.current.state?.available?.version).toBe("2.0.0");
  });
});
