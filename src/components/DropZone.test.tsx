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
import type { ClassifiedPaths } from "../lib/tauri";

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
        return Promise.resolve([]);
      case "confirm_folder_add":
        return Promise.resolve([]);
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
});
