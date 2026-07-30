import { describe, it, expect, vi, beforeEach } from "vitest";
import { StrictMode } from "react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

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
    update_mode: "automatic",
  };
}

let page: HistoryPageData = { jobs: [], total: 0 };
let summary: HistorySummary = { total_saved_bytes: 0, total_files: 0 };
let pathsExist: PathsExist = { source_exists: true, output_exists: true };
let badSources: JobInfo[] = [];
let settings: AppSettings = makeSettings("trash");
let purgeResults: PurgeResult[] = [];
// When set, purge rejects with it — shaped as the desktop backend rejects.
let purgeFailure: unknown = null;

const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((cb) => cb({ payload }));
}

beforeEach(() => {
  vi.clearAllMocks();
  page = { jobs: [], total: 0 };
  summary = { total_saved_bytes: 0, total_files: 0 };
  pathsExist = { source_exists: true, output_exists: true };
  badSources = [];
  settings = makeSettings("trash");
  purgeResults = [];
  purgeFailure = null;
  listeners.clear();
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === "get_history") return Promise.resolve(page);
    if (cmd === "get_history_summary") return Promise.resolve(summary);
    if (cmd === "clear_completed") return Promise.resolve(undefined);
    if (cmd === "check_paths_exist") return Promise.resolve(pathsExist);
    if (cmd === "open_path") return Promise.resolve(undefined);
    if (cmd === "reveal_in_dir") return Promise.resolve(undefined);
    if (cmd === "remove_history_entry") return Promise.resolve(undefined);
    if (cmd === "get_bad_sources") return Promise.resolve(badSources);
    if (cmd === "purge_bad_sources")
      return purgeFailure ? Promise.reject(purgeFailure) : Promise.resolve(purgeResults);
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

  it("labels the summary as potential savings under Keep mode, since nothing is freed yet", async () => {
    // Finding 3 of the final-review pass: under cleanup_mode=keep, both files stay on disk,
    // so "Total saved" overstates what actually happened — the figure is only what deleting
    // the originals by hand would save.
    settings = { ...makeSettings("trash"), cleanup_mode: "keep" };
    page = { jobs: [erroredJob("a")], total: 1 };
    summary = { total_saved_bytes: 1024, total_files: 3 };

    render(<HistoryPage />);

    await waitFor(() => expect(screen.getByText(/Potential savings/)).toBeInTheDocument());
    expect(screen.queryByText(/^Total saved/)).not.toBeInTheDocument();
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

    // Two REAL rows as stored by HandBrake 1.11.2: the promoted headline is ffmpeg's own
    // diagnostic, so the messages differ only by a heap pointer. If the panel labels rows by
    // that message the user is asked to approve destroying two rows they cannot tell apart —
    // which is exactly what shipped. Each row must be identified by its own filename.
    const REAL_PATH_A =
      "/Users/x/Downloads/Archive/Loose Files (Bunkr)/4 GIRLS FOR YOU - Mia Nouvelle, Emma Spice und Elena Rebell.mp4";
    const REAL_PATH_B =
      "/Users/x/Downloads/Archive/emmaxspice (Bunkr)/0hhmaofaq461uv8t393t7_source.mp4";

    function corruptJob(id: string, path: string, pointer: string): JobInfo {
      return {
        ...erroredJob(id),
        source_path: path,
        error_message: `Conversion failed: [mov,mp4,m4a,3gp,3g2,mj2 @ ${pointer}] moov atom not found\n[00:30:36] Compile-time hardening features are enabled`,
        failure_class: "bad_source",
      };
    }

    it("identifies every reviewed row by its own filename, with the full path on hover", async () => {
      badSources = [
        corruptJob("a", REAL_PATH_A, "0x8e0a44000"),
        corruptJob("b", REAL_PATH_B, "0xbe8a3c000"),
      ];
      render(<HistoryPage />);

      const nameA = await screen.findByText(
        "4 GIRLS FOR YOU - Mia Nouvelle, Emma Spice und Elena Rebell.mp4",
      );
      const nameB = await screen.findByText("0hhmaofaq461uv8t393t7_source.mp4");
      expect(nameA.getAttribute("title")).toBe(REAL_PATH_A);
      expect(nameB.getAttribute("title")).toBe(REAL_PATH_B);
    });

    it("states the reason in plain English from failure_class, never HandBrake's raw stderr", async () => {
      badSources = [
        corruptJob("a", REAL_PATH_A, "0x8e0a44000"),
        { ...corruptJob("b", REAL_PATH_B, "0xbe8a3c000"), failure_class: "bad_source_truncated" },
      ];
      render(<HistoryPage />);

      expect(await screen.findByText(/unreadable/i)).toBeTruthy();
      expect(screen.getByText(/incomplete/i)).toBeTruthy();
      // The ffmpeg component tag and heap pointer are noise that pushed the real reason
      // ("moov atom not found") off the right edge of the panel.
      expect(screen.queryByText(/0x8e0a44000/)).toBeNull();
      expect(screen.queryByText(/mov,mp4,m4a/)).toBeNull();
    });

    // A failure_class this build doesn't have wording for must still say SOMETHING about the
    // row rather than render blank — same defensive posture as OUTCOME_LABELS.
    it("falls back to the stored message for a failure_class it doesn't recognize yet", async () => {
      badSources = [
        {
          ...corruptJob("a", REAL_PATH_A, "0x8e0a44000"),
          error_message: "Conversion failed: some future diagnostic\nnoise below",
          failure_class: "bad_source_from_a_future_version",
        },
      ];
      render(<HistoryPage />);

      expect(await screen.findByText("Conversion failed: some future diagnostic")).toBeTruthy();
    });

    it("hides the banner entirely when there are no bad sources", async () => {
      badSources = [];
      render(<HistoryPage />);
      await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("get_bad_sources"));
      expect(screen.queryByText(/bad sources/i)).toBeNull();
    });

    // C4: while settings is still loading we don't yet know whether bad_source_action is
    // trash or delete, but the backend always reads the real setting regardless of what the
    // button says. Rendering *any* wording (even the "safe-looking" Trash default) risks
    // fronting a permanent delete for a delete-configured user who clicks during the load
    // window — so the button must not exist at all until settings resolve.
    it("does not render the purge button until settings resolve, so Trash wording can never front a permanent delete", async () => {
      badSources = [badSourceJob("a")];
      // Hold get_settings pending so the component genuinely renders with
      // settings === null before we resolve it.
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

      // The review list itself is informational and safe to show immediately...
      await screen.findByText(/bad sources \(1\)/i);
      // ...but neither wording of the purge button exists while settings is unresolved.
      expect(screen.queryByRole("button", { name: /move 1 to trash/i })).toBeNull();
      expect(screen.queryByRole("button", { name: /delete 1 permanently/i })).toBeNull();

      resolveSettings(makeSettings("delete"));

      await screen.findByRole("button", { name: /delete 1 permanently/i });
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

    it("does not tell the user to retry a purge that panicked", async () => {
      // "Please try again" is advice for a transient failure. A panic is a bug and retrying
      // will reproduce it, so this catch — which showed fixed copy and therefore survived the
      // String(e) sweep untouched — has to branch on the discriminator.
      badSources = [badSourceJob("a")];
      purgeFailure = { error: "task panicked: boom", kind: "panic" };

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      expect(
        await screen.findByText("Internal error (this is a bug): task panicked: boom"),
      ).toBeInTheDocument();
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
        expect(
          screen.getByText(/1 file\(s\) removed\. 1 still queued or being converted\./i),
        ).toBeInTheDocument(),
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
        screen.getByText(/0 file\(s\) removed\. 1 already deleted\./i),
      ).toBeInTheDocument();
    });

    // M4: with I3 wired up, a job-error event can bring in a bad source unrelated to whatever
    // purge last ran. A stale note from that earlier purge must not linger beside it — it would
    // read as if it described the newly-arrived row.
    it("clears a stale outcome note once an unrelated new bad source arrives", async () => {
      badSources = [badSourceJob("a"), badSourceJob("b")];
      purgeResults = [
        { id: "a", outcome: "purged" },
        { id: "b", outcome: "in_use" },
      ];
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "purge_bad_sources") return Promise.resolve(purgeResults);
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(2\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 2 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
      await screen.findByText(/1 file\(s\) removed\. 1 still queued or being converted\./i);

      // A new, unrelated job fails independently — the review list grows with an id that was
      // never part of the purge just run.
      badSources = [badSourceJob("b"), badSourceJob("c")];
      act(() => emit("job-error"));

      await waitFor(() => expect(screen.getByText(/bad sources \(2\)/i)).toBeInTheDocument());
      // The note clears via a cascading effect one render after the list itself updates, so
      // this must be polled too rather than asserted synchronously right after the line above.
      await waitFor(() =>
        expect(
          screen.queryByText(/1 file\(s\) removed\. 1 still queued or being converted\./i),
        ).not.toBeInTheDocument(),
      );
    });

    // M4: pressing Confirm again must not leave a PREVIOUS run's note on screen while the new
    // purge (which can take up to ~30s per rescanned row) is still in flight.
    it("clears a stale outcome note the instant a new purge attempt starts", async () => {
      badSources = [badSourceJob("a")];
      purgeResults = [{ id: "a", outcome: "in_use" }];
      let resolvePurge: (r: PurgeResult[]) => void = () => {};
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "purge_bad_sources") {
          return new Promise<PurgeResult[]>((resolve) => {
            resolvePurge = resolve;
          });
        }
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      // First purge: resolves immediately and leaves a note.
      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
      resolvePurge(purgeResults);
      await screen.findByText(/0 file\(s\) removed\. 1 still queued or being converted\./i);

      // Second purge attempt on the same row: the stale note must disappear right away, not
      // only once this new (still-pending) purge resolves.
      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      expect(
        screen.queryByText(/0 file\(s\) removed\. 1 still queued or being converted\./i),
      ).not.toBeInTheDocument();
    });

    // M3: purge() (unlike refresh()) does not catch its own rejection. Without a catch here, a
    // rejected purge_bad_sources escapes the click handler entirely — the confirm UI stays
    // armed and the user gets zero feedback that anything went wrong.
    it("surfaces a message and disarms the confirm UI when purge_bad_sources rejects", async () => {
      badSources = [badSourceJob("a")];
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "purge_bad_sources") return Promise.reject({ error: "IPC failed" });
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() =>
        expect(screen.getByText(/failed to process bad sources/i)).toBeInTheDocument(),
      );
      // The confirm step must not stay stuck armed after a failure.
      expect(screen.queryByRole("button", { name: /confirm/i })).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: /move 1 to trash/i }),
      ).toBeInTheDocument();
    });

    // C1: confirmingPurge used to be a bare boolean and runPurge mapped over the CURRENT
    // badSources at click time. So: arm on 3 reviewed rows, a 4th job fails while the confirm
    // text is on screen, click Confirm — and a file the user never saw gets destroyed too.
    // The fix snapshots the armed ids and disarms the instant the reviewed set changes, so a
    // stale click can never reach the backend with an unreviewed id.
    it("disarms the confirm when the bad-source list changes while armed, so a stale click can't purge an unreviewed arrival", async () => {
      badSources = [badSourceJob("a"), badSourceJob("b"), badSourceJob("c")];
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "purge_bad_sources") return Promise.resolve(purgeResults);
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(3\)/i);

      // Arm with exactly the 3 rows the user reviewed.
      fireEvent.click(screen.getByRole("button", { name: /move 3 to trash/i }));
      expect(
        screen.getByRole("button", { name: /confirm — move 3 to trash/i }),
      ).toBeInTheDocument();

      // A 4th job fails while the confirm strip is up — the user never reviewed it.
      badSources = [
        badSourceJob("a"),
        badSourceJob("b"),
        badSourceJob("c"),
        badSourceJob("d"),
      ];
      act(() => emit("job-error"));
      await waitFor(() => expect(screen.getByText(/bad sources \(4\)/i)).toBeInTheDocument());

      // The disarm runs in a cascading effect one render after the list itself updates (the
      // same pattern the stale-note-clearing effect above has to be polled for), so this must
      // be polled too rather than asserted synchronously right after the line above.
      await waitFor(() =>
        expect(screen.queryByRole("button", { name: /confirm/i })).not.toBeInTheDocument(),
      );
      expect(screen.getByRole("button", { name: /move 4 to trash/i })).toBeInTheDocument();

      // Nothing was ever sent to the backend — the stale arm never reached purge_bad_sources.
      expect(invokeMock).not.toHaveBeenCalledWith("purge_bad_sources", expect.anything());
    });

    // C1: the confirm strip must restate the count at the moment of commitment, not just show
    // a bare "Confirm" that could apply to any number of files.
    it("shows the reviewed count on the Confirm button itself", async () => {
      badSources = [badSourceJob("a"), badSourceJob("b")];
      render(<HistoryPage />);
      await screen.findByText(/bad sources \(2\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 2 to trash/i }));

      expect(
        screen.getByRole("button", { name: /confirm — move 2 to trash/i }),
      ).toBeInTheDocument();
    });

    // C2: after Confirm the buttons used to stay enabled while the purge ran (each rescan can
    // take ~30s per file), so a second click launched a concurrent second purge. Clicking
    // Cancel mid-flight visually disarmed but aborted nothing, falsely implying the user had
    // stopped it.
    it("disables Confirm/Cancel and shows a pending indicator while a purge is in flight, guarding against re-entry", async () => {
      badSources = [badSourceJob("a")];
      let resolvePurge: (r: PurgeResult[]) => void = () => {};
      let purgeCallCount = 0;
      invokeMock.mockImplementation(((cmd: string) => {
        if (cmd === "get_history") return Promise.resolve(page);
        if (cmd === "get_history_summary") return Promise.resolve(summary);
        if (cmd === "get_settings") return Promise.resolve(settings);
        if (cmd === "get_bad_sources") return Promise.resolve(badSources);
        if (cmd === "purge_bad_sources") {
          purgeCallCount++;
          return new Promise<PurgeResult[]>((resolve) => {
            resolvePurge = resolve;
          });
        }
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }) as typeof invoke);

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() => expect(screen.getByText(/removing 1 file/i)).toBeInTheDocument());
      expect(screen.getByRole("button", { name: /confirm/i })).toBeDisabled();
      expect(screen.getByRole("button", { name: /cancel/i })).toBeDisabled();

      // Further clicks on the disabled buttons must not fire a second purge or fake an abort.
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
      fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
      expect(purgeCallCount).toBe(1);

      await act(async () => {
        resolvePurge([{ id: "a", outcome: "purged" }]);
      });
    });

    // C3: the old note was `skipped.map(r => r.outcome.replace(/_/g, " ")).join(", ")` — an
    // underscore-stripped enum dump that concatenates duplicates and uses jargon
    // ("unverifiable") a non-technical user can't act on. The fix aggregates by outcome with
    // counts and plain-English wording for all six non-purged outcomes.
    it("aggregates skipped outcomes into counted, plain-English phrasing instead of an enum dump", async () => {
      badSources = [
        badSourceJob("a"),
        badSourceJob("b"),
        badSourceJob("c"),
        badSourceJob("d"),
        badSourceJob("e"),
      ];
      purgeResults = [
        { id: "a", outcome: "purged" },
        { id: "b", outcome: "in_use" },
        { id: "c", outcome: "in_use" },
        { id: "d", outcome: "changed" },
        { id: "e", outcome: "unverifiable" },
      ];

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(5\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 5 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() =>
        expect(
          screen.getByText(
            /1 file\(s\) removed\. 2 still queued or being converted, 1 changed on disk since it was flagged, 1 couldn't be re-checked \(drive unavailable\?\)\./i,
          ),
        ).toBeInTheDocument(),
      );
    });

    // C3: an outcome string the frontend doesn't recognize (a future backend variant) must
    // still render safely — never "undefined" — and must not be silently dropped from the count.
    it("falls back to generic wording for an outcome string the frontend doesn't recognize yet", async () => {
      badSources = [badSourceJob("a")];
      purgeResults = [
        { id: "a", outcome: "mystery_outcome" as unknown as PurgeResult["outcome"] },
      ];

      render(<HistoryPage />);
      await screen.findByText(/bad sources \(1\)/i);

      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() =>
        expect(screen.getByText(/0 file\(s\) removed\. 1 left alone\./i)).toBeInTheDocument(),
      );
    });

    // R1: `mountedRef` only ever registered a cleanup (`mountedRef.current = false`) and never
    // re-set it to `true` in the effect body. RTL's plain `render()` never exercises this,
    // because it doesn't wrap in StrictMode — but src/main.tsx does, and React 19's dev
    // double-invoke runs an effect's mount -> cleanup -> mount on the SAME instance, so that
    // cleanup latches mountedRef.current false for the rest of the session. Every post-await
    // update in runPurge (setArmedIds(null), setPurging(false), setPurgeOutcomeNote) is then
    // silently skipped forever: the strip is stuck on "Removing 1 file…" with Confirm/Cancel
    // both disabled, with zero indication of what happened to the file.
    it("still reports the outcome and re-enables the strip after a purge, under StrictMode's double-invoked effects", async () => {
      badSources = [badSourceJob("a")];
      // A skipped outcome (rather than a fully-successful purge) so buildOutcomeNote actually
      // renders text — an all-purged batch legitimately renders no note (see buildOutcomeNote),
      // which would make this test pass vacuously regardless of the mountedRef bug.
      purgeResults = [{ id: "a", outcome: "in_use" }];

      render(
        <StrictMode>
          <HistoryPage />
        </StrictMode>,
      );

      await screen.findByText(/bad sources \(1\)/i);
      fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
      fireEvent.click(screen.getByRole("button", { name: /confirm/i }));

      await waitFor(() => expect(screen.getByText(/removing 1 file/i)).toBeInTheDocument());

      // Without the fix, this never resolves — the note never appears and the strip stays
      // stuck on "Removing 1 file…" forever.
      await waitFor(() =>
        expect(
          screen.getByText(/0 file\(s\) removed\. 1 still queued or being converted\./i),
        ).toBeInTheDocument(),
      );
      expect(screen.queryByText(/removing 1 file/i)).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /confirm/i })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /move 1 to trash/i })).toBeEnabled();
    });
  });
});
