import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type WatchedDirectory } from "../lib/tauri";
import { errorText } from "../lib/errors";

const DEFAULT_RECURSIVE = false;
const DEFAULT_DELAY_SECS = 5;

export function useWatchedDirectories() {
  const [directories, setDirectories] = useState<WatchedDirectory[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const dirs = await commands.getWatchedDirectories();
      if (mounted.current) setDirectories(dirs);
    } catch (e) {
      if (mounted.current) setError(errorText(e));
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    return () => {
      mounted.current = false;
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
