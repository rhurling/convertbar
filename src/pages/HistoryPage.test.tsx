import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import HistoryPage from "./HistoryPage";
import type {
  HistoryPage as HistoryPageData,
  HistorySummary,
  JobInfo,
  PathsExist,
} from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

function erroredJob(id: string): JobInfo {
  return {
    id,
    source_path: `/in/${id}.mp4`,
    output_path: `/out/${id}.mkv`,
    preset: "p",
    status: "error",
    original_size: null,
    converted_size: null,
    kept_file: null,
    space_saved: null,
    error_message: "boom",
    queue_order: 0,
    created_at: "",
    completed_at: "2026-06-17",
  };
}

function doneJob(id: string): JobInfo {
  return {
    ...erroredJob(id),
    status: "done",
    kept_file: "converted",
    original_size: 1000,
    converted_size: 500,
    space_saved: 500,
    error_message: null,
  };
}

let page: HistoryPageData = { jobs: [], total: 0 };
let summary: HistorySummary = { total_saved_bytes: 0, total_files: 0 };
let pathsExist: PathsExist = { source_exists: true, output_exists: true };

beforeEach(() => {
  vi.clearAllMocks();
  page = { jobs: [], total: 0 };
  summary = { total_saved_bytes: 0, total_files: 0 };
  pathsExist = { source_exists: true, output_exists: true };
  listenMock.mockImplementation((() => Promise.resolve(() => {})) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === "get_history") return Promise.resolve(page);
    if (cmd === "get_history_summary") return Promise.resolve(summary);
    if (cmd === "clear_completed") return Promise.resolve(undefined);
    if (cmd === "check_paths_exist") return Promise.resolve(pathsExist);
    if (cmd === "open_path") return Promise.resolve(undefined);
    if (cmd === "reveal_in_dir") return Promise.resolve(undefined);
    if (cmd === "remove_history_entry") return Promise.resolve(undefined);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  }) as typeof invoke);
});

describe("HistoryPage", () => {
  it("offers the Clear menu when History holds only errored jobs", async () => {
    // Only errored jobs: no successful conversions, so the savings summary counts zero.
    page = { jobs: [erroredJob("a")], total: 1 };
    summary = { total_saved_bytes: 0, total_files: 0 };

    render(<HistoryPage />);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Clear/ })).toBeInTheDocument(),
    );
  });

  it("hides the savings summary when no conversion succeeded", async () => {
    // The space-saved figure is meaningless with zero successful conversions, so
    // it stays hidden even though the bar (and its Clear menu) is shown for errors.
    page = { jobs: [erroredJob("a")], total: 1 };
    summary = { total_saved_bytes: 0, total_files: 0 };

    render(<HistoryPage />);

    await waitFor(() => expect(screen.getByText("boom")).toBeInTheDocument());
    expect(screen.queryByText(/Total saved/)).not.toBeInTheDocument();
  });

  it("shows the savings summary alongside the Clear menu when conversions succeeded", async () => {
    page = { jobs: [erroredJob("a")], total: 1 };
    summary = { total_saved_bytes: 1024, total_files: 3 };

    render(<HistoryPage />);

    await waitFor(() => expect(screen.getByText(/Total saved/)).toBeInTheDocument());
    expect(screen.getByText("3 files")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Clear/ })).toBeInTheDocument();
  });

  describe("context menu", () => {
    async function openMenuOn(fileName: string) {
      await waitFor(() =>
        expect(screen.getByText(new RegExp(fileName))).toBeInTheDocument(),
      );
      fireEvent.contextMenu(screen.getByText(new RegExp(fileName)));
      return screen.getByRole("menu");
    }

    it("opens the surviving file via open_path and closes", async () => {
      page = { jobs: [doneJob("a")], total: 1 };
      render(<HistoryPage />);
      await openMenuOn("a.mp4");

      const openButton = screen.getByRole("menuitem", { name: "Open" });
      // Enabled once the existence check resolves (kept converted → output path).
      await waitFor(() => expect(openButton).toBeEnabled());
      fireEvent.click(openButton);

      expect(invokeMock).toHaveBeenCalledWith("open_path", { path: "/out/a.mkv" });
      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    });

    it("reveals the surviving file via reveal_in_dir", async () => {
      page = { jobs: [doneJob("a")], total: 1 };
      render(<HistoryPage />);
      await openMenuOn("a.mp4");

      const revealButton = screen.getByRole("menuitem", {
        name: "Open containing folder",
      });
      await waitFor(() => expect(revealButton).toBeEnabled());
      fireEvent.click(revealButton);

      expect(invokeMock).toHaveBeenCalledWith("reveal_in_dir", { path: "/out/a.mkv" });
    });

    it("disables open actions and hints when no surviving file exists", async () => {
      page = { jobs: [doneJob("a")], total: 1 };
      pathsExist = { source_exists: false, output_exists: false };
      render(<HistoryPage />);
      await openMenuOn("a.mp4");

      await waitFor(() =>
        expect(screen.getByText("File missing")).toBeInTheDocument(),
      );
      expect(screen.getByRole("menuitem", { name: "Open" })).toBeDisabled();
      expect(
        screen.getByRole("menuitem", { name: "Open containing folder" }),
      ).toBeDisabled();
      // Removing the entry must stay possible for a vanished file.
      expect(
        screen.getByRole("menuitem", { name: "Remove from history" }),
      ).toBeEnabled();
    });

    it("removes the entry and refreshes the list", async () => {
      page = { jobs: [doneJob("a")], total: 1 };
      render(<HistoryPage />);
      await openMenuOn("a.mp4");

      invokeMock.mockClear();
      fireEvent.click(screen.getByRole("menuitem", { name: "Remove from history" }));

      expect(invokeMock).toHaveBeenCalledWith("remove_history_entry", { id: "a" });
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_history", expect.anything()),
      );
    });

    it("closes on Escape without invoking anything", async () => {
      page = { jobs: [doneJob("a")], total: 1 };
      render(<HistoryPage />);
      await openMenuOn("a.mp4");

      fireEvent.keyDown(document, { key: "Escape" });

      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
      expect(invokeMock).not.toHaveBeenCalledWith("open_path", expect.anything());
    });
  });
});
