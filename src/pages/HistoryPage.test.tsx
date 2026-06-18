import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import HistoryPage from "./HistoryPage";
import type { HistoryPage as HistoryPageData, HistorySummary, JobInfo } from "../lib/tauri";

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

let page: HistoryPageData = { jobs: [], total: 0 };
let summary: HistorySummary = { total_saved_bytes: 0, total_files: 0 };

beforeEach(() => {
  vi.clearAllMocks();
  page = { jobs: [], total: 0 };
  summary = { total_saved_bytes: 0, total_files: 0 };
  listenMock.mockImplementation((() => Promise.resolve(() => {})) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === "get_history") return Promise.resolve(page);
    if (cmd === "get_history_summary") return Promise.resolve(summary);
    if (cmd === "clear_completed") return Promise.resolve(undefined);
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
});
