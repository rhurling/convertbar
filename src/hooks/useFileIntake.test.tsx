import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// Capture the drag-drop callback the hook registers so tests can fire a "drop".
const dragBus = vi.hoisted(() => ({
  handler: null as null | ((e: { payload: { type: string; paths?: string[] } }) => void),
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({
    onDragDropEvent: (cb: (e: { payload: { type: string; paths?: string[] } }) => void) => {
      dragBus.handler = cb;
      return Promise.resolve(() => {
        dragBus.handler = null;
      });
    },
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import { useFileIntake } from "./useFileIntake";
import type { ClassifiedPaths, AddResult } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);
let classified: ClassifiedPaths = { files: [], folders: [] };

function fireDrop(paths: string[]) {
  act(() => {
    dragBus.handler?.({ payload: { type: "drop", paths } });
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  dragBus.handler = null;
  classified = { files: [], folders: [] };
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "classify_paths":
        return Promise.resolve(classified);
      case "add_files":
        return Promise.resolve({ added: [], skipped: [] });
      case "confirm_folder_add":
        return Promise.resolve({ added: [], skipped: [] });
      case "start_queue":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("useFileIntake", () => {
  it("auto-adds loose files and ≤5-file folders, starts the queue, and switches to Queue on drop", async () => {
    classified = {
      files: ["/m/a.mp4"],
      folders: [{ file_count: 3, folder_name: "Clips", folder_path: "/clips" }],
    };
    const onDrop = vi.fn();
    renderHook(() => useFileIntake({ onDrop }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/clips"]);

    expect(onDrop).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/m/a.mp4"] }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/clips" }));
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
  });

  it("prompts for >5-file folders one at a time", async () => {
    classified = {
      files: [],
      folders: [
        { file_count: 12, folder_name: "A", folder_path: "/a" },
        { file_count: 20, folder_name: "B", folder_path: "/b" },
      ],
    };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a", "/b"]);

    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/a" });

    act(() => result.current.onAdd());
    // A advances to B synchronously; A's task is enqueued.
    expect(result.current.pendingConfirm?.folder_path).toBe("/b");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/a" }));

    act(() => result.current.onSkip());
    expect(result.current.pendingConfirm).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/b" });
  });

  it("runs the heavy adds sequentially — the second waits for the first to resolve", async () => {
    classified = {
      files: [],
      folders: [
        { file_count: 12, folder_name: "A", folder_path: "/a" },
        { file_count: 12, folder_name: "B", folder_path: "/b" },
      ],
    };
    const resolvers: Array<{ path: string; resolve: (v: AddResult) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: { path?: string }) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return new Promise<AddResult>((resolve) => resolvers.push({ path: args!.path!, resolve }));
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a", "/b"]);
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    act(() => result.current.onAdd()); // enqueue A, show B
    act(() => result.current.onAdd()); // enqueue B

    await waitFor(() => expect(resolvers).toHaveLength(1)); // only A started
    expect(resolvers[0].path).toBe("/a");

    await act(async () => {}); // flush microtasks — B must still not have started
    expect(resolvers).toHaveLength(1);

    await act(async () => {
      resolvers[0].resolve({ added: [], skipped: [] });
    });
    await waitFor(() => expect(resolvers).toHaveLength(2)); // B starts only after A resolves
    expect(resolvers[1].path).toBe("/b");
  });

  it("a new drop never interrupts or loses the in-flight scan", async () => {
    // User requirement #5: a folder scan already in flight must run to completion; a new drop
    // during that scan is appended behind it, not dropped and not raced ahead of.
    classified = {
      files: [],
      folders: [{ file_count: 12, folder_name: "A", folder_path: "/a" }],
    };
    const resolvers: Array<{ path: string; resolve: (v: AddResult) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: { path?: string; paths?: string[] }) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return new Promise<AddResult>((resolve) => resolvers.push({ path: args!.path!, resolve }));
        case "add_files":
          return Promise.resolve({ added: [], skipped: [] });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a"]);
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    act(() => result.current.onAdd()); // enqueues A's confirm_folder_add — now in flight, unresolved
    await waitFor(() => expect(resolvers).toHaveLength(1));

    classified = { files: ["/x.mp4"], folders: [] };
    fireDrop(["/x.mp4"]);

    await act(async () => {}); // flush the second drop's classify + enqueue
    // The loose-file task is queued behind A's still-running scan — not run, not lost.
    expect(invokeMock).not.toHaveBeenCalledWith("add_files", { paths: ["/x.mp4"] });

    await act(async () => {
      resolvers[0].resolve({ added: [], skipped: [] });
    });
    // Only after A completes does the queued task run.
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/x.mp4"] }));
  });

  it("auto-adds exactly ≤5-file folders and prompts for >5 (AUTO_ADD_MAX boundary)", async () => {
    classified = {
      files: [],
      folders: [
        { file_count: 5, folder_name: "Five", folder_path: "/five" },
        { file_count: 6, folder_name: "Six", folder_path: "/six" },
      ],
    };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/five", "/six"]);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/five" }));
    expect(result.current.pendingConfirm?.folder_path).toBe("/six");
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/six" });
  });

  it("surfaces a classify_paths failure in the status line", async () => {
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.reject(new Error("scan failed"));
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/whatever"]);

    await waitFor(() => expect(result.current.status).toMatch(/Error:.*scan failed/));
  });

  it("does not drop a folder when two separate drops' classify resolve back-to-back", async () => {
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    classified = { files: [], folders: [{ file_count: 12, folder_name: "A", folder_path: "/a" }] };
    fireDrop(["/a"]);
    classified = { files: [], folders: [{ file_count: 12, folder_name: "B", folder_path: "/b" }] };
    fireDrop(["/b"]);

    // Both folders are queued for confirmation; head is A, B waits behind it.
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/a"));
    act(() => result.current.onSkip());
    expect(result.current.pendingConfirm?.folder_path).toBe("/b");
  });

  it("shows a per-reason skip summary after an add", async () => {
    classified = { files: ["/m/a.mp4", "/m/b.txt"], folders: [] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "add_files":
          return Promise.resolve({ added: [{ id: "1" }], skipped: [{ reason: "not_video", count: 1 }] });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/m/b.txt"]);

    await waitFor(() => expect(result.current.status).toBe("Added 1 · 1 skipped (not a video)"));
  });

  it("auto-adds loose files while a >5-file folder in the same drop still prompts", async () => {
    // Regression guard for the mixed-drop bug (old DropZone.test.tsx:117): the loose file must
    // auto-add and the big folder must still show a confirm — neither swallows the other.
    classified = {
      files: ["/m/a.mp4"],
      folders: [{ file_count: 12, folder_name: "Big", folder_path: "/big" }],
    };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/m/a.mp4", "/big"]);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/m/a.mp4"] }));
    expect(result.current.pendingConfirm?.folder_path).toBe("/big");
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
  });

  it("addPaths (the server head's file-browser-modal entry point) feeds the same classify → enqueue pipeline as a drop", async () => {
    classified = { files: ["/m/a.mp4"], folders: [] };
    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    await act(async () => {
      await result.current.addPaths(["/m/a.mp4"]);
    });

    expect(invokeMock).toHaveBeenCalledWith("classify_paths", { paths: ["/m/a.mp4"] });
    expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/m/a.mp4"] });
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
  });

  it("surfaces an error in the status line when a folder add fails", async () => {
    // Regression guard for old DropZone.test.tsx:178 — a failing confirm must not vanish silently.
    classified = { files: [], folders: [{ file_count: 12, folder_name: "Big", folder_path: "/big" }] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return Promise.reject(new Error("scan failed"));
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const { result } = renderHook(() => useFileIntake({ onDrop: vi.fn() }));
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);
    await waitFor(() => expect(result.current.pendingConfirm?.folder_path).toBe("/big"));
    act(() => result.current.onAdd());

    await waitFor(() => expect(result.current.status).toMatch(/Error:.*scan failed/));
  });
});
