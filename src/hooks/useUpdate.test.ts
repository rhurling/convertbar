import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
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

  it("does not let any push clear a checkNow error, at any status — only another action or a dismissal can", async () => {
    // Three rounds of trying to infer "is this push related to the error" each leaked somewhere:
    // a push that resolved a real failure could arrive before the rejection that set it (silently
    // untagging it), and a push merely reporting unrelated progress could still race a refusal off
    // screen. actionError is a past-tense record of one click's outcome now, not a claim about
    // current state, so no push is entitled to remove it — checked here across a progression of
    // statuses, ending at "available", the exact one from the original bug this guarded.
    const message = "an update is already waiting to install once the queue is idle";
    vi.spyOn(commands, "checkForUpdate").mockRejectedValue(message);
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBe(message);

    act(() => { emit?.({ ...baseState, status: "downloading" }); });
    await waitFor(() => expect(result.current.state?.status).toBe("downloading"));
    expect(result.current.actionError).toBe(message);

    act(() => { emit?.({ ...baseState, status: "waitingForIdle" }); });
    await waitFor(() => expect(result.current.state?.status).toBe("waitingForIdle"));
    expect(result.current.actionError).toBe(message);

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: null },
      });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("available"));
    expect(result.current.actionError).toBe(message);
  });

  it("does not let a push clear an Install rejection, even one that reaches readyToRestart", async () => {
    // install_pending's Err paths (updater.rs: "an update operation is already running", "no
    // update available", a network-error string) never touch `status` — Install only renders
    // while status is already "available", and none of those refusals move it. The concrete case
    // that matters: a concurrent cycle holding the latch is what caused this rejection, and that
    // very cycle can go on to finish the install (status -> readyToRestart). The hook no longer
    // tries to decide whether that resolves the error — it's the panel's job to word the message
    // so it stays true (a past attempt) rather than contradicting the present (a finished install).
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
      emit?.({ ...baseState, status: "readyToRestart", just_installed: { version: "1.1.0", notes: null } });
    });
    await waitFor(() => expect(result.current.state?.status).toBe("readyToRestart"));
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

  it("clears actionError when the user dismisses it", async () => {
    // The only two ways actionError is meant to go away: the user tries again (already covered
    // above), or the user explicitly dismisses it. No push-based path exists any more.
    vi.spyOn(commands, "checkForUpdate").mockRejectedValue("network unreachable");
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.checkNow(); });
    expect(result.current.actionError).toBe("network unreachable");

    act(() => { result.current.dismissError(); });
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

describe("useUpdate (server head)", () => {
  // isServerHead is a module-level const (../lib/head), so the env must be stubbed and the
  // whole module graph reloaded fresh — same resetModules/stubEnv pattern as
  // src/lib/events.test.ts's "server head" suite and SettingsPage.test.tsx's version test.
  beforeEach(() => {
    vi.resetModules();
    // ../lib/events (imported by useUpdate) opens `new EventSource("/api/events")` at module
    // load time on the server head; jsdom has no EventSource, so it must be stubbed even though
    // this test never expects it to be used.
    vi.stubGlobal(
      "EventSource",
      class {
        addEventListener() {}
        removeEventListener() {}
      },
    );
    vi.stubEnv("VITE_HEAD", "server");
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it("does not throw on mount — getUpdateState() is a synchronous-throw notAvailable stub there (transport/http.ts), not a rejected promise, so an unguarded call crashes the effect", async () => {
    const { useUpdate: freshUseUpdate } = await import("./useUpdate");
    expect(() => renderHook(() => freshUseUpdate())).not.toThrow();
  });
});
