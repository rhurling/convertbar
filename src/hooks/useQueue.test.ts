import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useQueue } from "./useQueue";
import type { JobInfo } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

// Mocked event bus: record each listener so a test can fire the event by name.
const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((cb) => cb({ payload }));
}

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "1",
    source_path: "/in/a.mp4",
    output_path: "/out/a.mkv",
    preset: "p",
    status: "queued",
    original_size: null,
    converted_size: null,
    kept_file: null,
    space_saved: null,
    error_message: null,
    queue_order: 0,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

let queueData: JobInfo[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  queueData = [];
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === "get_queue") return Promise.resolve(queueData);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  }) as typeof invoke);
});

describe("useQueue", () => {
  it("derives activeJob and pendingJobs from the fetched queue", async () => {
    queueData = [
      job({ id: "1", status: "done" }),
      job({ id: "2", status: "queued" }),
      job({ id: "3", status: "encoding" }),
      job({ id: "4", status: "queued" }),
    ];

    const { result } = renderHook(() => useQueue());

    await waitFor(() => expect(result.current.queue).toHaveLength(4));
    expect(result.current.activeJob?.id).toBe("3");
    expect(result.current.pendingJobs.map((j) => j.id)).toEqual(["2", "4"]);
  });

  it("treats a paused job as the active job", async () => {
    queueData = [job({ id: "9", status: "paused" })];

    const { result } = renderHook(() => useQueue());

    await waitFor(() => expect(result.current.activeJob?.id).toBe("9"));
    expect(result.current.pendingJobs).toHaveLength(0);
  });

  it("stores the payload from conversion-progress events", async () => {
    const { result } = renderHook(() => useQueue());
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("get_queue"));

    const payload = { job_id: "1", percent: 50, eta_seconds: 30, fps: 24, avg_fps: 22 };
    act(() => emit("conversion-progress", payload));

    expect(result.current.progress).toEqual(payload);
  });

  it.each(["job-status-changed", "job-completed", "job-error", "queue-updated"])(
    "re-fetches the queue when a %s event fires",
    async (event) => {
      queueData = [job({ id: "1", status: "queued" })];
      const { result } = renderHook(() => useQueue());
      await waitFor(() => expect(result.current.queue).toHaveLength(1));
      expect(invokeMock).toHaveBeenCalledTimes(1);

      // Backend state changed; the event should drive a re-fetch.
      queueData = [
        job({ id: "1", status: "encoding" }),
        job({ id: "2", status: "queued" }),
      ];
      emit(event);

      await waitFor(() => expect(result.current.activeJob?.id).toBe("1"));
      expect(invokeMock).toHaveBeenCalledTimes(2);
    },
  );
});
