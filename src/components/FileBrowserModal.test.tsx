import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent, act } from "@testing-library/react";

const fsListMock = vi.fn();
let browseRoots: string[] = ["/"];
vi.mock("../lib/transport/http", () => ({
  httpCommands: {
    fsList: (path: string) => fsListMock(path),
    getAppInfo: () =>
      Promise.resolve({
        version: "1.0.0",
        head: "server",
        can_pause_process: false,
        auth_required: false,
        browse_roots: browseRoots,
      }),
  },
}));

import FileBrowserModal from "./FileBrowserModal";
import type { FsEntry } from "../lib/transport/types";

function entry(overrides: Partial<FsEntry>): FsEntry {
  return { name: "x", path: "/x", is_dir: false, size: null, ...overrides };
}

beforeEach(() => {
  vi.clearAllMocks();
  browseRoots = ["/"];
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

  it("navigates into a directory via its open button, not by clicking the row", async () => {
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

    // Clicking the row selects the folder rather than entering it — that uniformity is
    // what lets a shift-range span folders and files.
    fireEvent.click(await screen.findByText("Movies"));
    expect(fsListMock).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
    expect(await screen.findByText("clip.mp4")).toBeInTheDocument();
  });

  it("selects every row in the listing from the select-all control", async () => {
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "2024", path: "/2024", is_dir: true }),
        entry({ name: "a.mp4", path: "/a.mp4" }),
      ],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByLabelText("Select all"));
    fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));

    expect(onSelect).toHaveBeenCalledWith(["/2024", "/a.mp4"]);
  });

  it("shows the select-all box as indeterminate when only some rows are selected", async () => {
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "a.mp4", path: "/a.mp4" }),
        entry({ name: "b.mp4", path: "/b.mp4" }),
      ],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    const selectAll = (await screen.findByLabelText("Select all")) as HTMLInputElement;
    expect(selectAll.checked).toBe(false);
    expect(selectAll.indeterminate).toBe(false);

    fireEvent.click(screen.getByText("a.mp4"));
    // Partial selection reads as indeterminate, not unchecked — otherwise the header
    // claims nothing is selected while a row is ticked right below it.
    expect(selectAll.indeterminate).toBe(true);

    fireEvent.click(screen.getByText("b.mp4"));
    expect(selectAll.checked).toBe(true);
    expect(selectAll.indeterminate).toBe(false);
  });

  it("keeps directory mode free of checkboxes", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
    });

    render(<FileBrowserModal mode="directory" onSelect={vi.fn()} onClose={vi.fn()} />);

    expect(await screen.findByText("Movies")).toBeInTheDocument();
    expect(screen.queryByLabelText("Select all")).not.toBeInTheDocument();
    // Directory mode still navigates on a row click, as it always has.
    fireEvent.click(screen.getByText("Movies"));
    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
  });

  it("toggles selection when clicking directly on the row's checkbox", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "a.mp4", path: "/a.mp4" })],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    // Query by the checkbox's own accessible name (the entry name), not the row text — this is
    // the checkbox input itself, the exact click target that was previously a dead no-op.
    const checkbox = (await screen.findByLabelText("a.mp4")) as HTMLInputElement;
    expect(checkbox.checked).toBe(false);

    fireEvent.click(checkbox);

    expect(checkbox.checked).toBe(true);
  });

  it("selects a row from the keyboard in files mode", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "a.mp4", path: "/a.mp4" })],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    const nameEl = await screen.findByText("a.mp4");
    const row = nameEl.closest(".file-browser-entry") as HTMLElement;
    row.focus();
    fireEvent.keyDown(row, { key: "Enter" });

    expect(screen.getByRole("button", { name: /^Add 1 item/ })).not.toBeDisabled();
  });

  it("enters a folder from the keyboard in directory mode", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
        });
      }
      return Promise.resolve({ entries: [] });
    });

    render(<FileBrowserModal mode="directory" onSelect={vi.fn()} onClose={vi.fn()} />);

    const nameEl = await screen.findByText("Movies");
    const row = nameEl.closest(".file-browser-entry") as HTMLElement;
    row.focus();
    fireEvent.keyDown(row, { key: " " });

    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
  });

  it("ignores a keydown that bubbles up from the Open button rather than the row", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    const openButton = await screen.findByRole("button", { name: "Open Movies" });
    fireEvent.keyDown(openButton, { key: "Enter" });

    // jsdom doesn't synthesize the browser's native button-activation click from this keydown,
    // so we can only assert the half we can observe: the row's handler must not have run, i.e.
    // the folder was not also toggled into the selection.
    expect(screen.getByRole("button", { name: /^Add 0 items/ })).toBeDisabled();
  });

  it("multi-selects files and calls onSelect with their paths", async () => {
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "a.mp4", path: "/a.mp4" }), entry({ name: "b.mp4", path: "/b.mp4" })],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("a.mp4"));
    fireEvent.click(await screen.findByText("b.mp4"));
    fireEvent.click(screen.getByRole("button", { name: /add 2 items/i }));

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

    await screen.findByText("Movies");
    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
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

    expect(await screen.findByRole("button", { name: /add 0 items/i })).toBeDisabled();
  });

  it('starts at the configured browse root instead of always guessing "/"', async () => {
    browseRoots = ["/media"];
    fsListMock.mockResolvedValue({
      entries: [entry({ name: "movie.mp4", path: "/media/movie.mp4" })],
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/media"));
    expect(fsListMock).not.toHaveBeenCalledWith("/");
    expect(await screen.findByText("movie.mp4")).toBeInTheDocument();
    // The breadcrumb's root crumb is the configured root itself...
    expect(screen.getByRole("button", { name: "/media" })).toBeInTheDocument();
    // ...never a "/" crumb above it (the server would 403 on anything outside browse_roots).
    expect(screen.queryByRole("button", { name: "/" })).toBeNull();
  });

  it("stops breadcrumb up-navigation at the containing configured root when browsing deeper", async () => {
    browseRoots = ["/media"];
    fsListMock.mockImplementation((path: string) => {
      if (path === "/media") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/media/Movies", is_dir: true })],
        });
      }
      if (path === "/media/Movies") {
        return Promise.resolve({
          entries: [entry({ name: "clip.mp4", path: "/media/Movies/clip.mp4" })],
        });
      }
      return Promise.reject(new Error(`unexpected path: ${path}`));
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    await screen.findByText("Movies");
    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
    await screen.findByText("clip.mp4");

    // Breadcrumb reads [/media] > Movies — no crumb above /media is offered.
    expect(screen.getByRole("button", { name: "/media" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Movies" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "/" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "/media" }));
    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/media"));
    expect(await screen.findByText("Movies")).toBeInTheDocument();
  });

  it("keeps the selection when navigating between directories", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [
            entry({ name: "Movies", path: "/Movies", is_dir: true }),
            entry({ name: "root.mp4", path: "/root.mp4" }),
          ],
        });
      }
      return Promise.resolve({
        entries: [entry({ name: "inner.mp4", path: "/Movies/inner.mp4" })],
      });
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("root.mp4"));
    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
    fireEvent.click(await screen.findByText("inner.mp4"));

    // The whole point of persistence: gather from more than one folder in one pass.
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));
    expect(onSelect).toHaveBeenCalledWith(["/root.mp4", "/Movies/inner.mp4"]);
  });

  it("shift-clicking selects the range and never deselects", async () => {
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "2024", path: "/2024", is_dir: true }),
        entry({ name: "a.mp4", path: "/a.mp4" }),
        entry({ name: "b.mp4", path: "/b.mp4" }),
        entry({ name: "c.mp4", path: "/c.mp4" }),
      ],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("c.mp4"));      // an unrelated earlier pick
    fireEvent.click(screen.getByText("2024"));               // anchor, and a folder
    fireEvent.click(screen.getByText("b.mp4"), { shiftKey: true });

    // Additive: c.mp4 survives even though it is outside the range. A mis-aimed
    // shift-click must never silently drop earlier work.
    fireEvent.click(screen.getByRole("button", { name: /^Add 4 items/ }));
    expect(onSelect).toHaveBeenCalledWith(["/c.mp4", "/2024", "/a.mp4", "/b.mp4"]);
  });

  it("clears the selection from the footer", async () => {
    fsListMock.mockResolvedValue({ entries: [entry({ name: "a.mp4", path: "/a.mp4" })] });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("a.mp4"));
    expect(screen.getByText("1 selected")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.queryByText("1 selected")).not.toBeInTheDocument();
  });

  it("passes a folder and a file inside it through untouched", async () => {
    // Deliberately NOT deduped here. The backend already does it: useFileIntake enqueues
    // files before folders, and add_files_to_db skips anything already queued. Deduping
    // in the picker would silently lose the ticked file if the user skips the folder's
    // >5-file confirm prompt.
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "Movies", path: "/Movies", is_dir: true }),
        entry({ name: "clip.mp4", path: "/Movies/clip.mp4" }),
      ],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("Movies"));
    fireEvent.click(screen.getByText("clip.mp4"));
    fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));

    expect(onSelect).toHaveBeenCalledWith(["/Movies", "/Movies/clip.mp4"]);
  });

  it("resets the shift anchor on navigation, even back to the same directory", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [
            entry({ name: "Movies", path: "/Movies", is_dir: true }),
            entry({ name: "a.mp4", path: "/a.mp4" }),
            entry({ name: "b.mp4", path: "/b.mp4" }),
            entry({ name: "c.mp4", path: "/c.mp4" }),
          ],
        });
      }
      return Promise.resolve({ entries: [] });
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("a.mp4")); // sets the anchor to /a.mp4
    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
    await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
    fireEvent.click(screen.getByRole("button", { name: "/" })); // back to the same directory
    await screen.findByText("a.mp4");

    fireEvent.click(screen.getByText("c.mp4"), { shiftKey: true });

    // Without the anchor reset, a.mp4 would still resolve inside this listing and this
    // shift-click would sweep in b.mp4 as a stale range instead of toggling only c.mp4.
    fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));
    expect(onSelect).toHaveBeenCalledWith(["/a.mp4", "/c.mp4"]);
  });

  it("clearing the selection also resets the shift anchor", async () => {
    fsListMock.mockResolvedValue({
      entries: [
        entry({ name: "a.mp4", path: "/a.mp4" }),
        entry({ name: "b.mp4", path: "/b.mp4" }),
        entry({ name: "c.mp4", path: "/c.mp4" }),
      ],
    });
    const onSelect = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("a.mp4")); // sets the anchor to /a.mp4
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));

    fireEvent.click(screen.getByText("c.mp4"), { shiftKey: true });

    // Without the anchor reset, this would resolve as a stale range from the cleared
    // a.mp4 anchor through to c.mp4, resurrecting a.mp4 and sweeping in b.mp4 too.
    fireEvent.click(screen.getByRole("button", { name: /^Add 1 item/ }));
    expect(onSelect).toHaveBeenCalledWith(["/c.mp4"]);
  });

  it("navigates to a typed path", async () => {
    fsListMock.mockImplementation((path: string) =>
      path === "/media/movies"
        ? Promise.resolve({ entries: [entry({ name: "deep.mp4", path: "/media/movies/deep.mp4" })] })
        : Promise.resolve({ entries: [] }),
    );

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    const input = await screen.findByLabelText("Go to path");
    fireEvent.change(input, { target: { value: "/media/movies" } });
    fireEvent.submit(input.closest("form")!);

    expect(await screen.findByText("deep.mp4")).toBeInTheDocument();
  });

  it("shows the server's error for a forbidden path and keeps the current listing", async () => {
    fsListMock.mockImplementation((path: string) =>
      path === "/"
        ? Promise.resolve({ entries: [entry({ name: "visible.mp4", path: "/visible.mp4" })] })
        : Promise.reject(new Error("path outside allowed roots")),
    );

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);
    expect(await screen.findByText("visible.mp4")).toBeInTheDocument();

    const input = screen.getByLabelText("Go to path");
    fireEvent.change(input, { target: { value: "/etc" } });
    fireEvent.submit(input.closest("form")!);

    expect(await screen.findByText("path outside allowed roots")).toBeInTheDocument();
    // A rejected jump must not blank the listing the user was looking at.
    expect(screen.getByText("visible.mp4")).toBeInTheDocument();
  });

  it("does not close when the backdrop is clicked", async () => {
    fsListMock.mockResolvedValue({ entries: [] });
    const onClose = vi.fn();

    const { container } = render(
      <FileBrowserModal mode="files" onSelect={vi.fn()} onClose={onClose} />,
    );
    await waitFor(() => expect(fsListMock).toHaveBeenCalled());

    fireEvent.click(container.querySelector(".modal-overlay")!);

    // A stray backdrop click used to discard the whole selection. The accident class is
    // removed outright rather than guarded with a confirm dialog.
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("discards a listing that lands after the user has navigated somewhere newer", async () => {
    let landSlow!: (result: { entries: FsEntry[] }) => void;
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [
            entry({ name: "Slow", path: "/Slow", is_dir: true }),
            entry({ name: "root.mp4", path: "/root.mp4" }),
          ],
        });
      }
      if (path === "/Slow") {
        return new Promise<{ entries: FsEntry[] }>((resolve) => {
          landSlow = resolve;
        });
      }
      return Promise.reject(new Error(`unexpected path: ${path}`));
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Open Slow" }));
    // Impatient: a cold-NAS folder is still listing, so go back to the root.
    fireEvent.click(screen.getByRole("button", { name: "/" }));
    await screen.findByText("root.mp4");

    await act(async () => {
      landSlow({ entries: [entry({ name: "slow.mp4", path: "/Slow/slow.mp4" })] });
    });

    // Arrival order must not beat user intent: the abandoned folder cannot yank the
    // listing back, which would also mean a click lands on whatever row moved into place.
    expect(screen.getByText("root.mp4")).toBeInTheDocument();
    expect(screen.queryByText("slow.mp4")).toBeNull();
  });

  it("discards an error from a request the user already navigated away from", async () => {
    let failSlow!: (reason: Error) => void;
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [
            entry({ name: "Slow", path: "/Slow", is_dir: true }),
            entry({ name: "root.mp4", path: "/root.mp4" }),
          ],
        });
      }
      if (path === "/Slow") {
        return new Promise<{ entries: FsEntry[] }>((_resolve, reject) => {
          failSlow = reject;
        });
      }
      return Promise.reject(new Error(`unexpected path: ${path}`));
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Open Slow" }));
    fireEvent.click(screen.getByRole("button", { name: "/" }));
    await screen.findByText("root.mp4");

    await act(async () => {
      failSlow(new Error("mount timed out"));
    });

    // A late failure must not put an error banner over a listing that loaded fine —
    // nothing clears it, since setError(null) only runs when a load starts.
    expect(screen.queryByText("mount timed out")).toBeNull();
    expect(screen.getByText("root.mp4")).toBeInTheDocument();
  });

  it("keeps showing the spinner when an abandoned request lands mid-navigation", async () => {
    let landSlow!: (result: { entries: FsEntry[] }) => void;
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [
            entry({ name: "Slow", path: "/Slow", is_dir: true }),
            entry({ name: "root.mp4", path: "/root.mp4" }),
          ],
        });
      }
      if (path === "/Slow") {
        return new Promise<{ entries: FsEntry[] }>((resolve) => {
          landSlow = resolve;
        });
      }
      // "/Slower" never settles.
      return new Promise<{ entries: FsEntry[] }>(() => {});
    });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Open Slow" }));
    const input = screen.getByLabelText("Go to path");
    fireEvent.change(input, { target: { value: "/Slower" } });
    fireEvent.submit(input.closest("form")!);

    await act(async () => {
      landSlow({ entries: [entry({ name: "slow.mp4", path: "/Slow/slow.mp4" })] });
    });

    // The stale arrival must not clear `loading`, which would drop the spinner and expose
    // the old listing as though the pending navigation had finished.
    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(screen.queryByText("root.mp4")).toBeNull();
  });

  it("does not claim an empty folder when the first listing never landed", async () => {
    fsListMock.mockRejectedValue(new Error("connection refused"));

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    expect(await screen.findByText("connection refused")).toBeInTheDocument();
    // "Empty folder" is a claim about a folder we listed. We listed nothing.
    expect(screen.queryByText("Empty folder")).toBeNull();
  });

  it("exposes the picker as a modal dialog named by its own header", async () => {
    fsListMock.mockResolvedValue({ entries: [] });

    render(<FileBrowserModal mode="directory" onSelect={vi.fn()} onClose={vi.fn()} />);

    // Without aria-modal a screen reader keeps offering the page behind the overlay, which
    // the backdrop's inertness means the user cannot even reach with a click.
    const dialog = screen.getByRole("dialog", { name: "Choose a folder" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    await waitFor(() => expect(fsListMock).toHaveBeenCalled());
  });

  it("opens with focus inside the dialog", async () => {
    fsListMock.mockResolvedValue({ entries: [] });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    // Focus has to start in the dialog, or a keyboard user Tabs the whole page behind the
    // overlay before reaching the picker they just opened.
    expect(document.activeElement).toBe(screen.getByRole("dialog", { name: "Add files" }));
    await waitFor(() => expect(fsListMock).toHaveBeenCalled());
  });

  it("closes on Escape", async () => {
    fsListMock.mockResolvedValue({ entries: [] });
    const onClose = vi.fn();

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={onClose} />);
    await waitFor(() => expect(fsListMock).toHaveBeenCalled());

    fireEvent.keyDown(document, { key: "Escape" });

    // The backdrop is deliberately inert, so Escape is the only dismissal that isn't
    // Tab-hunting for Cancel.
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("cycles Tab inside the dialog instead of leaking into the page behind it", async () => {
    fsListMock.mockResolvedValue({ entries: [entry({ name: "a.mp4", path: "/a.mp4" })] });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    // Select one file so the confirm button is enabled and is therefore the last stop.
    fireEvent.click(await screen.findByText("a.mp4"));
    const close = screen.getByTitle("Close");
    const confirm = screen.getByRole("button", { name: /^Add 1 item/ });

    confirm.focus();
    fireEvent.keyDown(confirm, { key: "Tab" });
    expect(document.activeElement).toBe(close);

    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(confirm);
  });

  it("pulls focus back into the dialog when Tab is pressed with focus adrift", async () => {
    fsListMock.mockResolvedValue({ entries: [entry({ name: "a.mp4", path: "/a.mp4" })] });

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);
    await screen.findByText("a.mp4");

    // A click on the inert backdrop leaves activeElement on <body>.
    (document.activeElement as HTMLElement).blur();
    expect(document.activeElement).toBe(document.body);

    fireEvent.keyDown(document.body, { key: "Tab" });

    expect(document.activeElement).toBe(screen.getByTitle("Close"));
  });

  it("leaves focus on the jump-to-path input when its own navigation lands", async () => {
    fsListMock.mockImplementation((path: string) =>
      path === "/media/movies"
        ? Promise.resolve({ entries: [entry({ name: "deep.mp4", path: "/media/movies/deep.mp4" })] })
        : Promise.resolve({ entries: [] }),
    );

    render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

    const input = (await screen.findByLabelText("Go to path")) as HTMLInputElement;
    input.focus();
    fireEvent.change(input, { target: { value: "/media/movies" } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByText("deep.mp4")).toBeInTheDocument();

    // The restore is for focus that was actually lost with an unmounted row, not a grab on
    // every listing: this input is still mounted, and typing position belongs to the user.
    expect(document.activeElement).toBe(input);
  });

  it("moves focus to the listing when a navigation unmounts the focused row", async () => {
    fsListMock.mockImplementation((path: string) => {
      if (path === "/") {
        return Promise.resolve({
          entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
        });
      }
      return Promise.resolve({ entries: [entry({ name: "clip.mp4", path: "/Movies/clip.mp4" })] });
    });

    const { container } = render(
      <FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />,
    );

    const row = (await screen.findByText("Movies")).closest(".file-browser-entry") as HTMLElement;
    row.focus();
    expect(document.activeElement).toBe(row);

    fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
    await screen.findByText("clip.mp4");

    // The focused row is gone with the old listing. Focus must land on the new listing
    // rather than <body>, or the keyboard user restarts from the top of the document
    // after every single folder they enter.
    expect(document.activeElement).toBe(container.querySelector(".file-browser-list"));
  });
});
