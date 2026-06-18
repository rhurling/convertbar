import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import WatchedFoldersPage from "./WatchedFoldersPage";
import type { WatchedDirectory } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);

function dir(overrides: Partial<WatchedDirectory>): WatchedDirectory {
  return {
    id: "1",
    path: "/Users/me/Downloads",
    recursive: false,
    stability_delay_secs: 5,
    enabled: true,
    created_at: "",
    ...overrides,
  };
}

let directories: WatchedDirectory[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  directories = [];
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "get_watched_directories":
        return Promise.resolve(directories);
      case "pick_folder":
        return Promise.resolve("/Users/me/Movies");
      default:
        return Promise.resolve(undefined);
    }
  }) as typeof invoke);
});

describe("WatchedFoldersPage", () => {
  it("shows the empty state when no folders are watched", async () => {
    render(<WatchedFoldersPage />);
    expect(
      await screen.findByText(/no watched folders yet/i),
    ).toBeInTheDocument();
  });

  it("renders a row per watched folder using the folder's basename", async () => {
    directories = [
      dir({ id: "a", path: "/Users/me/Downloads" }),
      dir({ id: "b", path: "/Users/me/Media/incoming" }),
    ];
    render(<WatchedFoldersPage />);

    expect(await screen.findByText("Downloads")).toBeInTheDocument();
    expect(screen.getByText("incoming")).toBeInTheDocument();
  });

  it("registers the picked folder when Add folder is clicked", async () => {
    const user = userEvent.setup();
    render(<WatchedFoldersPage />);
    await screen.findByText(/no watched folders yet/i);

    await user.click(screen.getByRole("button", { name: /add folder/i }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("pick_folder"),
    );
    expect(invokeMock).toHaveBeenCalledWith("add_watched_directory", {
      path: "/Users/me/Movies",
      recursive: false,
      stabilityDelaySecs: 5,
    });
  });

  it("pauses a folder when its enable checkbox is unticked", async () => {
    directories = [dir({ id: "x", enabled: true })];
    const user = userEvent.setup();
    render(<WatchedFoldersPage />);

    const toggle = await screen.findByTitle(/click to pause/i);
    await user.click(toggle);

    expect(invokeMock).toHaveBeenCalledWith("set_watched_directory_enabled", {
      id: "x",
      enabled: false,
    });
  });

  it("removes a folder when its remove button is clicked", async () => {
    directories = [dir({ id: "x" })];
    const user = userEvent.setup();
    render(<WatchedFoldersPage />);

    await screen.findByText("Downloads");
    await user.click(screen.getByTitle(/stop watching this folder/i));

    expect(invokeMock).toHaveBeenCalledWith("remove_watched_directory", {
      id: "x",
    });
  });
});
