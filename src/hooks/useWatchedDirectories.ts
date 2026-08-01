import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "../lib/events";
import { commands, type WatchedDirectory } from "../lib/tauri";
import { errorText } from "../lib/errors";

const DEFAULT_RECURSIVE = false;
const DEFAULT_DELAY_SECS = 5;

export function useWatchedDirectories() {
  const [directories, setDirectories] = useState<WatchedDirectory[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  // Monotonic request id: a mutation refreshes explicitly and also provokes the backend
  // event, so two fetches run at once and can resolve out of order. Drop any that isn't
  // the latest — otherwise the older snapshot lands last and sticks, with nothing left to
  // refetch and correct it.
  const latestRequest = useRef(0);

  const refresh = useCallback(async () => {
    // `unlisten` resolves a tick after unmount, so an event landing in that window would
    // otherwise still spend a round trip on a dead instance.
    if (!mounted.current) return;
    const requestId = ++latestRequest.current;
    try {
      const dirs = await commands.getWatchedDirectories();
      if (mounted.current && requestId === latestRequest.current)
        setDirectories(dirs);
    } catch (e) {
      if (mounted.current) setError(errorText(e));
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();

    // This panel is permanently mounted at two-col/three-col, so it never remounts to
    // refetch. On the server head another browser can edit the watch list at any time;
    // without this listener the panel would show the page-load snapshot indefinitely.
    const unlisten = listen("watched-directories-updated", () => {
      refresh();
    });

    // Server head only: after an SSE reconnect, refetch to heal any events missed while the
    // connection was down. Never fires on desktop, so this listener is inert there.
    window.addEventListener("convertbar:events-reconnected", refresh);

    return () => {
      mounted.current = false;
      unlisten.then((un) => un());
      window.removeEventListener("convertbar:events-reconnected", refresh);
    };
  }, [refresh]);

  // Registers a folder (already chosen, by whatever means) with default settings. Shared by
  // both the desktop picker flow below and the server head's file-browser modal (directory
  // mode), which has no pickFolder equivalent and calls this directly with its selected path.
  const addDirectoryAtPath = useCallback(
    async (path: string) => {
      setError(null);
      try {
        await commands.addWatchedDirectory(
          path,
          DEFAULT_RECURSIVE,
          DEFAULT_DELAY_SECS,
        );
        await refresh();
      } catch (e) {
        setError(errorText(e));
      }
    },
    [refresh],
  );

  // Opens the folder picker, then registers the chosen folder. Returns silently if the user
  // cancels the picker. Desktop-only: pickFolder() has no server implementation.
  const addDirectory = useCallback(async () => {
    setError(null);
    try {
      const path = await commands.pickFolder();
      if (!path) return;
      await addDirectoryAtPath(path);
    } catch (e) {
      setError(errorText(e));
    }
  }, [addDirectoryAtPath]);

  const updateDirectory = useCallback(
    async (id: string, recursive: boolean, stabilityDelaySecs: number) => {
      setError(null);
      try {
        await commands.updateWatchedDirectory(
          id,
          recursive,
          stabilityDelaySecs,
        );
        await refresh();
      } catch (e) {
        setError(errorText(e));
      }
    },
    [refresh],
  );

  const setEnabled = useCallback(
    async (id: string, enabled: boolean) => {
      setError(null);
      try {
        await commands.setWatchedDirectoryEnabled(id, enabled);
        await refresh();
      } catch (e) {
        setError(errorText(e));
      }
    },
    [refresh],
  );

  const removeDirectory = useCallback(
    async (id: string) => {
      setError(null);
      try {
        await commands.removeWatchedDirectory(id);
        await refresh();
      } catch (e) {
        setError(errorText(e));
      }
    },
    [refresh],
  );

  return {
    directories,
    loading,
    error,
    addDirectory,
    addDirectoryAtPath,
    updateDirectory,
    setEnabled,
    removeDirectory,
  };
}
