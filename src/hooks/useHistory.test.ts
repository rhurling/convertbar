import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useHistory } from "./useHistory";
import type { HistoryPage, HistorySummary, JobInfo } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((cb) => cb({ payload }));
}

function job(id: string): JobInfo {
  return {
    id,
    source_path: `/in/${id}.mp4`,
    output_path: `/out/${id}.mkv`,
    preset: "p",
    status: "done",
    original_size: null,
    converted_size: null,
    kept_file: null,
    space_saved: null,
    error_message: null,
    failure_class: null,
    queue_order: 0,
    created_at: "",
    started_at: null,
    completed_at: "2026-06-17",
  };
}

// Pages keyed by the offset the hook requests, so loadMore returns the next slice.
let pages: Record<number, HistoryPage> = {};
let summary: HistorySummary = { total_saved_bytes: 0, total_files: 0 };

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  pages = {};
  summary = { total_saved_bytes: 0, total_files: 0 };
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
  invokeMock.mockImplementation(((cmd: string, args?: { offset?: number }) => {
    if (cmd === "get_history") {
      const offset = args?.offset ?? 0;
      return Promise.resolve(pages[offset] ?? { jobs: [], total: 0 });
    }
    if (cmd === "get_history_summary") return Promise.resolve(summary);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  }) as typeof invoke);
});

describe("useHistory", () => {
  it("derives history, summary, and hasMore from the first page", async () => {
    pages = { 0: { jobs: [job("a"), job("b")], total: 5 } };
    summary = { total_saved_bytes: 2048, total_files: 2 };

    const { result } = renderHook(() => useHistory());

    await waitFor(() => expect(result.current.history).toHaveLength(2));
    expect(result.current.summary).toEqual({ total_saved_bytes: 2048, total_files: 2 });
    expect(result.current.hasMore).toBe(true); // 2 of 5 loaded
  });

  it("clears hasMore once the loaded rows cover the total", async () => {
    pages = { 0: { jobs: [job("a"), job("b")], total: 2 } };

    const { result } = renderHook(() => useHistory());

    await waitFor(() => expect(result.current.history).toHaveLength(2));
    expect(result.current.hasMore).toBe(false);
  });

  it("loadMore requests the next page at the current offset and appends", async () => {
    pages = {
      0: { jobs: [job("a"), job("b")], total: 4 },
      2: { jobs: [job("c"), job("d")], total: 4 },
    };

    const { result } = renderHook(() => useHistory());
    await waitFor(() => expect(result.current.history).toHaveLength(2));

    await act(async () => {
      await result.current.loadMore();
    });

    expect(result.current.history.map((j) => j.id)).toEqual(["a", "b", "c", "d"]);
    expect(result.current.hasMore).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith(
      "get_history",
      expect.objectContaining({ offset: 2, limit: 50 }),
    );
  });

  it.each(["job-completed", "job-error"])(
    "re-fetches history when a %s event fires",
    async (event) => {
      pages = { 0: { jobs: [job("a")], total: 1 } };
      const { result } = renderHook(() => useHistory());
      await waitFor(() => expect(result.current.history).toHaveLength(1));

      pages = { 0: { jobs: [job("a"), job("b")], total: 2 } };
      act(() => emit(event));

      await waitFor(() => expect(result.current.history).toHaveLength(2));
    },
  );
});
