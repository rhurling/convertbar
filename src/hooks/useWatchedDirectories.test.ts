import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useWatchedDirectories } from "./useWatchedDirectories";
import type { WatchedDirectory } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

// Mocked event bus: record each listener so a test can fire the event by name.
const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((cb) => cb({ payload }));
}

function fetchCount() {
  return invokeMock.mock.calls.filter((c) => c[0] === "get_watched_directories")
    .length;
}

function dir(overrides: Partial<WatchedDirectory>): WatchedDirectory {
  return {
    id: "1",
    path: "/movies",
    recursive: false,
    stability_delay_secs: 5,
    enabled: true,
    created_at: "",
    ...overrides,
  };
}

let directories: WatchedDirectory[] = [];
let pickedFolder: string | null = "/picked";

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  directories = [];
  pickedFolder = "/picked";
  listenMock.mockImplementation(((
    event: string,
    cb: (e: { payload: unknown }) => void,
  ) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "get_watched_directories":
        return Promise.resolve(directories);
      case "pick_folder":
        return Promise.resolve(pickedFolder);
      case "add_watched_directory":
      case "update_watched_directory":
      case "set_watched_directory_enabled":
      case "remove_watched_directory":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("useWatchedDirectories", () => {
  it("loads the watched directories on mount", async () => {
    directories = [dir({ id: "a" }), dir({ id: "b" })];
    const { result } = renderHook(() => useWatchedDirectories());

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.directories.map((d) => d.id)).toEqual(["a", "b"]);
  });

  it("adds the picked folder with default settings then refreshes", async () => {
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.addDirectory();
    });

    expect(invokeMock).toHaveBeenCalledWith("pick_folder");
    expect(invokeMock).toHaveBeenCalledWith("add_watched_directory", {
      path: "/picked",
      recursive: false,
      stabilityDelaySecs: 5,
    });
    // A refresh (second get_watched_directories) follows the add.
    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "get_watched_directories"),
    ).toHaveLength(2);
  });

  it("does not register anything when the folder picker is cancelled", async () => {
    pickedFolder = null;
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.addDirectory();
    });

    expect(invokeMock).not.toHaveBeenCalledWith(
      "add_watched_directory",
      expect.anything(),
    );
  });

  it("toggles, updates, and removes a directory through the backend", async () => {
    directories = [dir({ id: "x" })];
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.setEnabled("x", false);
    });
    expect(invokeMock).toHaveBeenCalledWith("set_watched_directory_enabled", {
      id: "x",
      enabled: false,
    });

    await act(async () => {
      await result.current.updateDirectory("x", true, 12);
    });
    expect(invokeMock).toHaveBeenCalledWith("update_watched_directory", {
      id: "x",
      recursive: true,
      stabilityDelaySecs: 12,
    });

    await act(async () => {
      await result.current.removeDirectory("x");
    });
    expect(invokeMock).toHaveBeenCalledWith("remove_watched_directory", {
      id: "x",
    });
  });

  // The Watch Folders panel is permanently mounted at two-col/three-col, so it never remounts
  // to refetch. Without these two triggers it shows whatever the list was at page load — for
  // days — while a second browser tab on the server head edits it out from under the user.
  it("refetches when another client changes the watch list", async () => {
    directories = [dir({ id: "a" })];
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    directories = [dir({ id: "a" }), dir({ id: "b", path: "/added-elsewhere" })];
    await act(async () => {
      emit("watched-directories-updated");
    });

    await waitFor(() =>
      expect(result.current.directories.map((d) => d.id)).toEqual(["a", "b"]),
    );
  });

  it("refetches after an SSE reconnect to heal a missed change", async () => {
    directories = [dir({ id: "a" })];
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    // The event announcing this removal was dropped while the connection was down.
    directories = [];
    await act(async () => {
      window.dispatchEvent(new Event("convertbar:events-reconnected"));
    });

    await waitFor(() => expect(result.current.directories).toHaveLength(0));
  });

  it("ignores a stale fetch that resolves after a newer one", async () => {
    // A mutation refreshes explicitly *and* provokes the backend event, so two fetches are now
    // in flight at once. If the older response is allowed to land last it overwrites the newer
    // list with a pre-mutation snapshot, and nothing refetches afterwards to correct it.
    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    const resolvers: Array<(v: WatchedDirectory[]) => void> = [];
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "get_watched_directories")
        return new Promise<WatchedDirectory[]>((r) => resolvers.push(r));
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }) as typeof invoke);

    act(() => emit("watched-directories-updated"));
    act(() => emit("watched-directories-updated"));
    await waitFor(() => expect(resolvers).toHaveLength(2));

    // Newest resolves first, then the stale one.
    await act(async () => {
      resolvers[1]([dir({ id: "fresh" })]);
      resolvers[0]([dir({ id: "stale" })]);
    });

    expect(result.current.directories.map((d) => d.id)).toEqual(["fresh"]);
  });

  it("stops listening once unmounted", async () => {
    const { result, unmount } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    unmount();
    const before = fetchCount();
    await act(async () => {
      emit("watched-directories-updated");
      window.dispatchEvent(new Event("convertbar:events-reconnected"));
    });

    expect(fetchCount()).toBe(before);
  });

  it("surfaces a backend error from add", async () => {
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "get_watched_directories") return Promise.resolve([]);
      if (cmd === "pick_folder") return Promise.resolve("/dupe");
      if (cmd === "add_watched_directory")
        return Promise.reject({ error: "This folder is already being watched" });
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }) as typeof invoke);

    const { result } = renderHook(() => useWatchedDirectories());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.addDirectory();
    });

    await waitFor(() =>
      expect(result.current.error).toContain("already being watched"),
    );
  });
});
