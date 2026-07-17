import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// Capture the drag-drop callback DropZone registers so tests can fire a "drop".
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
    hide: () => Promise.resolve(),
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import DropZone from "./DropZone";
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

describe("DropZone", () => {
  it("auto-adds loose files plus folders of 5 or fewer files, then starts the queue", async () => {
    classified = {
      files: ["/movies/a.mp4"],
      folders: [{ file_count: 3, folder_name: "Clips", folder_path: "/clips" }],
    };
    const onFilesAdded = vi.fn();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/movies/a.mp4", "/clips"]);

    await waitFor(() => expect(onFilesAdded).toHaveBeenCalledTimes(1));
    expect(invokeMock).toHaveBeenCalledWith("add_files", { paths: ["/movies/a.mp4"] });
    expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/clips" });
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
    // No confirmation prompt for an under-threshold folder.
    expect(screen.queryByText(/files from/)).not.toBeInTheDocument();
  });

  it("prompts for confirmation for folders over 5 files and does not start the queue", async () => {
    classified = {
      files: [],
      folders: [{ file_count: 12, folder_name: "BigFolder", folder_path: "/big" }],
    };
    const onFilesAdded = vi.fn();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);

    await waitFor(() =>
      expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
    expect(invokeMock).not.toHaveBeenCalledWith("start_queue");
    expect(onFilesAdded).not.toHaveBeenCalled();
  });

  it("adds the folder and starts the queue when the user confirms", async () => {
    classified = {
      files: [],
      folders: [{ file_count: 12, folder_name: "BigFolder", folder_path: "/big" }],
    };
    const onFilesAdded = vi.fn();
    const user = userEvent.setup();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Add" })).toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(onFilesAdded).toHaveBeenCalledTimes(1));
    expect(invokeMock).toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
    expect(screen.queryByText(/files from/)).not.toBeInTheDocument();
  });

  it("still prompts for a big folder when loose files auto-add in the same drop", async () => {
    // Regression: the auto-added summary ("Added 1") was written into `status`, and the
    // render gated the confirm UI behind `status` being falsy — so a >5-file folder dropped
    // alongside anything that auto-adds had its Add/Skip prompt hidden and was silently
    // swallowed (never confirmed, never queued).
    classified = {
      files: ["/movies/a.mp4"],
      folders: [{ file_count: 12, folder_name: "BigFolder", folder_path: "/big" }],
    };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "add_files":
          return Promise.resolve({ added: [{ id: "1" }], skipped: [] });
        case "confirm_folder_add":
          return Promise.resolve({ added: [], skipped: [] });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const onFilesAdded = vi.fn();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/movies/a.mp4", "/big"]);

    // The confirm prompt for the big folder must appear despite the "Added 1" summary.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Add" })).toBeInTheDocument(),
    );
    expect(screen.getByText(/Add 12 files from/)).toBeInTheDocument();
    // The folder is not auto-added and the queue does not start until the user confirms.
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
    expect(invokeMock).not.toHaveBeenCalledWith("start_queue");
    expect(onFilesAdded).not.toHaveBeenCalled();
  });

  it("starts the queue without adding when the user skips the only pending folder", async () => {
    classified = {
      files: [],
      folders: [{ file_count: 12, folder_name: "BigFolder", folder_path: "/big" }],
    };
    const onFilesAdded = vi.fn();
    const user = userEvent.setup();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Skip" })).toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "Skip" }));

    await waitFor(() => expect(onFilesAdded).toHaveBeenCalledTimes(1));
    expect(invokeMock).not.toHaveBeenCalledWith("confirm_folder_add", { path: "/big" });
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
  });

  it("surfaces an error when confirming a folder add fails", async () => {
    classified = {
      files: [],
      folders: [{ file_count: 12, folder_name: "BigFolder", folder_path: "/big" }],
    };
    const user = userEvent.setup();
    render(<DropZone onFilesAdded={() => {}} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/big"]);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Add" })).toBeInTheDocument(),
    );

    // The confirm now fails; the click must not vanish silently.
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "confirm_folder_add")
        return Promise.reject(new Error("scan failed"));
      if (cmd === "classify_paths") return Promise.resolve(classified);
      if (cmd === "start_queue") return Promise.resolve(undefined);
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }) as typeof invoke);

    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() =>
      expect(screen.getByText(/Error:.*scan failed/)).toBeInTheDocument(),
    );
  });

  it("does not resurrect a confirmed folder when two Adds resolve out of order", async () => {
    // N5: each confirm handler filtered from its render-time pendingFolders snapshot, so with
    // two pending folders the second resolution restored the first (already-removed) row and
    // the "last one → startQueue" check never fired. Removals must be by folder_path against
    // the latest list, not an index against a stale snapshot.
    classified = {
      files: [],
      folders: [
        { file_count: 12, folder_name: "A", folder_path: "/a" },
        { file_count: 12, folder_name: "B", folder_path: "/b" },
      ],
    };
    const onFilesAdded = vi.fn();
    const confirmResolvers: Array<{ path: string; resolve: (v: AddResult) => void }> = [];
    invokeMock.mockImplementation(((cmd: string, args?: { path?: string }) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "confirm_folder_add":
          return new Promise<AddResult>((resolve) =>
            confirmResolvers.push({ path: args!.path!, resolve }),
          );
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    const user = userEvent.setup();
    render(<DropZone onFilesAdded={onFilesAdded} />);
    await waitFor(() => expect(dragBus.handler).not.toBeNull());

    fireDrop(["/a", "/b"]);
    await waitFor(() =>
      expect(screen.getAllByRole("button", { name: "Add" })).toHaveLength(2),
    );

    const addButtons = screen.getAllByRole("button", { name: "Add" });
    await user.click(addButtons[0]); // A
    await user.click(addButtons[1]); // B
    await waitFor(() => expect(confirmResolvers).toHaveLength(2));

    // Confirm A first, then B late; B's resolution must clear the list, not restore A.
    await act(async () => {
      confirmResolvers.find((r) => r.path === "/a")!.resolve({ added: [], skipped: [] });
    });
    await act(async () => {
      confirmResolvers.find((r) => r.path === "/b")!.resolve({ added: [], skipped: [] });
    });

    await waitFor(() => expect(onFilesAdded).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(/files from/)).not.toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("start_queue");
  });

  it("shows a per-reason skip summary after an add", async () => {
    classified = { files: ["/movies/a.mp4", "/movies/b.txt"], folders: [] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "add_files":
          return Promise.resolve({
            added: [{ id: "1" }],
            skipped: [{ reason: "not_video", count: 1 }],
          });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    render(<DropZone onFilesAdded={() => {}} />);
    fireDrop(["/movies/a.mp4", "/movies/b.txt"]);

    await waitFor(() =>
      expect(screen.getByText("Added 1 · 1 skipped (not a video)")).toBeInTheDocument(),
    );
  });
});
