import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import HistoryPage from "./HistoryPage";
import type {
  AppSettings,
  HistoryPage as HistoryPageData,
  HistorySummary,
  JobInfo,
  PathsExist,
  PurgeResult,
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
    failure_class: null,
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

// A row from the bad-source review list — same shape as a history error row, but with a
// failure_class the review list is scoped by.
function badSourceJob(id: string): JobInfo {
  return {
    ...erroredJob(id),
    error_message: "Source appears truncated\nmore detail",
    failure_class: "bad_source_truncated",
  };
}

function makeSettings(badSourceAction: "trash" | "delete"): AppSettings {
  return {
    preset: "Fast 1080p30",
    cleanup_mode: "trash",
    launch_at_login: false,
    handbrake_path: "",
    menubar_show_percent: true,
    menubar_show_eta: true,
    menubar_show_queue: true,
    menubar_show_filename: true,
    menubar_show_fps: false,
    notifications_per_file: false,
    notifications_errors_only: false,
    notifications_queue_done: false,
    skip_already_converted: false,
    skip_by_source_media: true,
    watch_skip_marker: ".downloading",
    low_disk_min_gb: 0,
    bad_source_action: badSourceAction,
  };
}

let page: HistoryPageData = { jobs: [], total: 0 };
let summary: HistorySummary = { total_saved_bytes: 0, total_files: 0 };
let pathsExist: PathsExist = { source_exists: true, output_exists: true };
let badSources: JobInfo[] = [];
let settings: AppSettings = makeSettings("trash");
let purgeResults: PurgeResult[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  page = { jobs: [], total: 0 };
  summary = { total_saved_bytes: 0, total_files: 0 };
  pathsExist = { source_exists: true, output_exists: true };
  badSources = [];
  settings = makeSettings("trash");
  purgeResults = [];
  listenMock.mockImplementation((() => Promise.resolve(() => {})) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === "get_history") return Promise.resolve(page);
    if (cmd === "get_history_summary") return Promise.resolve(summary);
    if (cmd === "clear_completed") return Promise.resolve(undefined);
    if (cmd === "check_paths_exist") return Promise.resolve(pathsExist);
    if (cmd === "open_path") return Promise.resolve(undefined);
    if (cmd === "reveal_in_dir") return Promise.resolve(undefined);
    if (cmd === "remove_history_entry") return Promise.resolve(undefined);
    if (cmd === "get_bad_sources") return Promise.resolve(badSources);
    if (cmd === "purge_bad_sources") return Promise.resolve(purgeResults);
    if (cmd === "get_settings") return Promise.resolve(settings);
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

  describe("bad-source review list", () => {
    it("shows the bad-source banner and requires a confirm before destroying", async () => {
      badSources = [badSourceJob("a")];
      render(<HistoryPage />);

      const banner = await screen.findByText(/bad sources \(1\)/i);
      expect(banner).toBeTruthy();

      // First press only arms the confirm — nothing is destroyed yet.
      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      expect(invokeMock).not.toHaveBeenCalledWith(
        "purge_bad_sources",
        expect.anything(),
      );

      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("purge_bad_sources", { ids: ["a"] }),
      );
    });

    it("hides the banner entirely when there are no bad sources", async () => {
      badSources = [];
      render(<HistoryPage />);
      await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("get_bad_sources"));
      expect(screen.queryByText(/bad sources/i)).toBeNull();
    });

    it("reads a still-loading (null) settings object as the non-destructive Trash default, then switches to Delete once settings resolve", async () => {
      badSources = [badSourceJob("a")];
      // Hold get_settings pending so the component genuinely renders with
      // settings === null before we resolve it — asserting the terminal "delete" state
      // alone would never exercise the null-default branch this test is named for.
      let resolveSettings: (s: AppSettings) => void = () => {};
      const settingsPromise = new Promise<AppSettings>((resolve) => {
        resolveSettings = resolve;
      });
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "get_settings") return settingsPromise;
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);

      // Settings is still unresolved (null) here — must read as the non-destructive default.
      await screen.findByRole("button", { name: /move 1 to trash/i });
      expect(
        screen.queryByRole("button", { name: /delete 1 permanently/i }),
      ).toBeNull();

      resolveSettings(makeSettings("delete"));

      await screen.findByRole("button", { name: /delete 1 permanently/i });
      expect(
        screen.queryByRole("button", { name: /move 1 to trash/i }),
      ).toBeNull();
    });

    // Safety-critical: the confirm button must pass the review list's own ids, never
    // History's. A History page can (and typically does) show far more rows than the
    // review list — including this exact bad-source row, since get_history includes
    // errored jobs too. Wiring the wrong collection here would ask the backend to destroy
    // whatever History happens to be showing, not just the reviewed bad sources.
    it("passes only bad-source ids to purge, never the full history list", async () => {
      page = { jobs: [doneJob("hist-1"), erroredJob("a")], total: 2 };
      badSources = [badSourceJob("a")];
      purgeResults = [{ id: "a", outcome: "purged" }];

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("purge_bad_sources", { ids: ["a"] }),
      );
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "purge_bad_sources");
      expect(call?.[1]).toEqual({ ids: ["a"] });
      expect(call?.[1]).not.toEqual({ ids: ["hist-1", "a"] });
    });

    it("reports outcomes honestly when some files are left alone, not silently implying everything was destroyed", async () => {
      badSources = [badSourceJob("a"), badSourceJob("b")];
      purgeResults = [
        { id: "a", outcome: "purged" },
        { id: "b", outcome: "in_use" },
      ];

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(2\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 2 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() =>
        expect(screen.getByText(/1 file\(s\) were left alone: in use/i)).toBeInTheDocument(),
      );
    });

    // Regression: an already_gone row now gets stamped purged by the backend fix (see
    // queue.rs mark_purged), so it drops out of the review list on refresh — the same as a
    // genuinely destroyed row. If the outcome note were still scoped inside the
    // badSources.length > 0 block, the whole panel — note included — would vanish the
    // instant the list emptied, and the user would have zero indication the file was
    // spared rather than destroyed.
    it("keeps the outcome note visible even after the reviewed row drops out of the list entirely", async () => {
      badSources = [badSourceJob("a")];
      purgeResults = [{ id: "a", outcome: "already_gone" }];
      let purged = false;
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(purged ? [] : badSources);
        if (cmd === "purge_bad_sources") {
          purged = true;
          return Promise.resolve(purgeResults);
        }
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      // The list itself empties out...
      await waitFor(() => expect(screen.queryByText(/bad sources \(1\)/i)).toBeNull());
      // ...but the note explaining the file was left alone must still be visible.
      expect(
        screen.getByText(/1 file\(s\) were left alone: already gone/i),
      ).toBeInTheDocument();
    });
  });
});
