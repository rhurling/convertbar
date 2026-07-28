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

  it("clears actionError once a pushed event shows the situation has settled", async () => {
    // Reproduces the exact bug this guards: an offline manual check sets actionError to
    // "network unreachable" (and, via the backend, the identical state.last_error). The user
    // then flips the update mode, which triggers an unrelated background check that succeeds —
    // status becomes "available" and last_error clears. Nothing about that push touches
    // actionError directly; if it isn't cleared, the "network unreachable" banner keeps showing
    // directly above the now-available update, contradicting the state right beside it.
    vi.spyOn(commands, "checkForUpdate").mockRejectedValue("network unreachable");
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBe("network unreachable");

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: null },
        last_error: null,
      });
    });

    await waitFor(() => expect(result.current.state?.status).toBe("available"));
    expect(result.current.actionError).toBeNull();
  });

  it("does not let a push clear actionError while the blocking situation is still unfolding", async () => {
    // The refusal "an update is already waiting to install" is rejected synchronously by the
    // backend before any state mutation, so no event accompanies it — but the pending install it
    // names keeps emitting its own progress (downloading -> waitingForIdle) around the same time.
    // None of those pushes may race the refusal off screen: the reason the check was refused is
    // still true for as long as status stays in a "blocked" state.
    vi.spyOn(commands, "checkForUpdate").mockRejectedValue(
      "an update is already waiting to install once the queue is idle",
    );
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBe(
      "an update is already waiting to install once the queue is idle",
    );

    act(() => {
      emit?.({ ...baseState, status: "downloading" });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("downloading"));
    expect(result.current.actionError).toBe(
      "an update is already waiting to install once the queue is idle",
    );

    act(() => {
      emit?.({ ...baseState, status: "waitingForIdle" });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("waitingForIdle"));
    expect(result.current.actionError).toBe(
      "an update is already waiting to install once the queue is idle",
    );

    // Only once status leaves the blocked set (the install finished and the app is idle again)
    // has the situation genuinely settled, and the stale refusal is safe to drop.
    act(() => {
      emit?.({ ...baseState, status: "idle" });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("idle"));
    expect(result.current.actionError).toBeNull();
  });

  it("does not let a push clear an Install rejection while status stays available", async () => {
    // install_pending's Err paths (updater.rs: "an update operation is already running", "no
    // update available", a network-error string) never touch `status` — Install only renders
    // while status is already "available", and none of those refusals move it. A later, wholly
    // unrelated push (e.g. a concurrent cycle finishing and going idle) must not wipe this.
    vi.spyOn(commands, "getUpdateState").mockResolvedValue({
      ...baseState,
      status: "available",
      available: { version: "1.1.0", date: null, notes: null },
    });
    vi.spyOn(commands, "installUpdate").mockRejectedValue(
      "an update operation is already running",
    );
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state?.status).toBe("available"));

    await act(async () => { await result.current.install(); });
    expect(result.current.actionError).toBe("an update operation is already running");

    act(() => {
      emit?.({ ...baseState, status: "idle" });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("idle"));
    expect(result.current.actionError).toBe("an update operation is already running");
  });

  it("does not let a push clear a Skip rejection while status stays available", async () => {
    // skip_update_version's Err paths ("app state unavailable", a poisoned-lock message) both
    // return before its only status-touching call (`clear_status`, reached only on success) —
    // so a rejected Skip never moves status either, and the same protection applies.
    vi.spyOn(commands, "getUpdateState").mockResolvedValue({
      ...baseState,
      status: "available",
      available: { version: "1.1.0", date: null, notes: null },
    });
    vi.spyOn(commands, "skipUpdateVersion").mockRejectedValue("app state unavailable");
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state?.status).toBe("available"));

    await act(async () => { await result.current.skip(); });
    expect(result.current.actionError).toBe("app state unavailable");

    act(() => {
      emit?.({ ...baseState, status: "idle" });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("idle"));
    expect(result.current.actionError).toBe("app state unavailable");
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
