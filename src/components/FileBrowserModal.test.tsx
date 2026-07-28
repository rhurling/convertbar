import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";

const fsListMock = vi.fn();
vi.mock("../lib/transport/http", () => ({
  httpCommands: { fsList: (path: string) => fsListMock(path) },
}));

import FileBrowserModal from "./FileBrowserModal";
import type { FsEntry } from "../lib/transport/types";

function entry(overrides: Partial<FsEntry>): FsEntry {
  return { name: "x", path: "/x", is_dir: false, size: null, ...overrides };
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("FileBrowserModal", () => {
  it("renders entries returned by fsList for the root path on mount", async () => {
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "Movies", path: "/Movies", is_dir: true }),
        entry({ name: "clip.mp4", path: "/clip.mp4", size: 1000 }),
      ],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/"));
    expect(await screen.findByText("Movies")).toBeInTheDocument();
    expect(screen.getByText("clip.mp4")).toBeInTheDocument();
  });

  it("navigates into a directory on click", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
        });
      }
      if (path === "/Movies") {
        return Promise.resolve({
          entries: [entry({ name: "clip.mp4", path: "/Movies/clip.mp4" })],
        });
      }
      return Promise.reject(new Error(`unexpected path: ${path}`));
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("Movies"));

    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
    expect(await screen.findByText("clip.mp4")).toBeInTheDocument();
  });

  it("multi-selects files and calls onSelect with their paths", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "a.mp4", path: "/a.mp4" }), entry({ name: "b.mp4", path: "/b.mp4" })],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("a.mp4"));
    fireEvent.click(await screen.findByText("b.mp4"));
    fireEvent.click(screen.getByRole("button", { name: /add 2 files/i }));

    expect(onSelect).toHaveBeenCalledWith(["/a.mp4", "/b.mp4"]);
  });

  it("directory mode confirms the current directory, not any selected entry", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
        });
      }
      return Promise.resolve({ entries: [] });
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="directory" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("Movies"));
    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));

    fireEvent.click(screen.getByRole("button", { name: /choose this folder/i }));

    expect(onSelect).toHaveBeenCalledWith(["/Movies"]);
  });

  it("navigates back up via the breadcrumb", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
        });
      }
      if (path === "/Movies") {
        return Promise.resolve({
          entries: [entry({ name: "clip.mp4", path: "/Movies/clip.mp4" })],
        });
      }
      return Promise.reject(new Error(`unexpected path: ${path}`));
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("Movies"));
    await screen.findByText("clip.mp4");

    fireEvent.click(screen.getByRole("button", { name: "/" }));

    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/"));
    expect(await screen.findByText("Movies")).toBeInTheDocument();
  });

  it("calls onClose when the close button is clicked", async () => {
    fsListMock.mockResolvedValue({ entries: [] });
    const onClose = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={onClose} />);
    await waitFor(() => expect(fsListMock).toHaveBeenCalled());

    fireEvent.click(screen.getByTitle("Close"));

    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("disables the Add-files confirm button until at least one file is selected", async () => {
    fsListMock.mockResolvedValue({ entries: [entry({ name: "a.mp4", path: "/a.mp4" })] });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    expect(await screen.findByRole("button", { name: /add 0 files/i })).toBeDisabled();
  });
});
