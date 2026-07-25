import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useBadSources } from "./useBadSources";
import { commands } from "../lib/tauri";
import { listen } from "@tauri-apps/api/event";

vi.mock("../lib/tauri", () => ({
  commands: {
    getBadSources: vi.fn(),
    purgeBadSources: vi.fn(),
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const listenMock = vi.mocked(listen);

const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((cb) => cb({ payload }));
}

const row = (id: string, failure_class: string) =>
  ({ id, source_path: `/m/${id}.mkv`, failure_class }) as never;

beforeEach(() => {
  vi.resetAllMocks();
  listeners.clear();
  listenMock.mockImplementation(((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => {
      listeners.get(event)!.delete(cb);
    });
  }) as typeof listen);
});

describe("useBadSources", () => {
  it("loads the list on mount", async () => {
    vi.mocked(commands.getBadSources).mockResolvedValue([
      row("a", "bad_source"),
      row("b", "bad_source_truncated"),
    ]);
    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(2));
  });

  it("refetches after a purge so handled rows disappear", async () => {
    vi.mocked(commands.getBadSources)
      .mockResolvedValueOnce([row("a", "bad_source")])
      .mockResolvedValueOnce([]);
    vi.mocked(commands.purgeBadSources).mockResolvedValue([
      { id: "a", outcome: "purged" },
    ]);

    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(1));

    await act(async () => {
      await result.current.purge(["a"]);
    });

    expect(commands.purgeBadSources).toHaveBeenCalledWith(["a"]);
    await waitFor(() => expect(result.current.badSources).toHaveLength(0));
  });

  it("surfaces outcomes so skipped files can be reported, not silently ignored", async () => {
    vi.mocked(commands.getBadSources).mockResolvedValue([row("a", "bad_source")]);
    vi.mocked(commands.purgeBadSources).mockResolvedValue([
      { id: "a", outcome: "recovered" },
    ]);
    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(1));

    let outcomes;
    await act(async () => {
      outcomes = await result.current.purge(["a"]);
    });
    expect(outcomes).toEqual([{ id: "a", outcome: "recovered" }]);
  });

  // I3: with the queue running on the History tab, a newly-truncated job must make the
  // "Bad sources" panel refresh on its own — the same job-error event useHistory already
  // listens on — not just once on mount.
  it("re-fetches the review list when a job-error event fires", async () => {
    vi.mocked(commands.getBadSources)
      .mockResolvedValueOnce([row("a", "bad_source")])
      .mockResolvedValueOnce([row("a", "bad_source"), row("b", "bad_source_truncated")]);

    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(1));

    act(() => emit("job-error"));

    await waitFor(() => expect(result.current.badSources).toHaveLength(2));
  });
});
