import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type WatchedDirectory } from "../lib/tauri";

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
      if (mounted.current) setError(String(e));
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

  // Opens the folder picker, then registers the chosen folder with default settings. Returns
  // silently if the user cancels the picker.
  const addDirectory = useCallback(async () => {
    setError(null);
    try {
      const path = await commands.pickFolder();
      if (!path) return;
      await commands.addWatchedDirectory(
        path,
        DEFAULT_RECURSIVE,
        DEFAULT_DELAY_SECS,
      );
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }, [refresh]);

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
        setError(String(e));
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
        setError(String(e));
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
        setError(String(e));
      }
    },
    [refresh],
  );

  return {
    directories,
    loading,
    error,
    addDirectory,
    updateDirectory,
    setEnabled,
    removeDirectory,
  };
}
