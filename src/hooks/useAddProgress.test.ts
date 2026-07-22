import { it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { listen } from "@tauri-apps/api/event";
import { useAddProgress } from "./useAddProgress";

const listenMock = vi.mocked(listen);
const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();

function emit(event: string, payload: unknown) {
  act(() => {
    listeners.get(event)?.forEach((cb) => cb({ payload }));
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
});

it("goes adding on start and clears on finish", () => {
  const { result } = renderHook(() => useAddProgress());
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();

  emit("add-started", { op_id: "a", label: "Folder" });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "a", label: "Folder", done: null, total: null });

  emit("add-progress", { op_id: "a", label: "Folder", done: 3, total: 10 });
  expect(result.current.activity).toEqual({ opId: "a", label: "Folder", done: 3, total: 10 });

  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(false);
  expect(result.current.activity).toBeNull();
});

it("stays adding until every overlapping op finishes", () => {
  const { result } = renderHook(() => useAddProgress());
  emit("add-started", { op_id: "a", label: "A" });
  emit("add-started", { op_id: "b", label: "B" });
  emit("add-finished", { op_id: "a" });
  expect(result.current.isAdding).toBe(true); // b still open
  emit("add-finished", { op_id: "b" });
  expect(result.current.isAdding).toBe(false);
});

it("tolerates progress for an op whose start it missed, keeping its label", () => {
  // A watcher scan can emit add-started before the webview attaches listeners.
  const { result } = renderHook(() => useAddProgress());
  emit("add-progress", { op_id: "x", label: "Watched", done: 1, total: 4 });
  expect(result.current.isAdding).toBe(true);
  expect(result.current.activity).toEqual({ opId: "x", label: "Watched", done: 1, total: 4 });
});

it("ignores a stray finish for an unseen op", () => {
  const { result } = renderHook(() => useAddProgress());
  emit("add-finished", { op_id: "ghost" });
  expect(result.current.isAdding).toBe(false);
});
