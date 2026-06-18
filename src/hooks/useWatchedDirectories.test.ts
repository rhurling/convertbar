import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { useWatchedDirectories } from "./useWatchedDirectories";
import type { WatchedDirectory } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);

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
  directories = [];
  pickedFolder = "/picked";
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

  it("surfaces a backend error from add as a string", async () => {
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "get_watched_directories") return Promise.resolve([]);
      if (cmd === "pick_folder") return Promise.resolve("/dupe");
      if (cmd === "add_watched_directory")
        return Promise.reject("This folder is already being watched");
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
