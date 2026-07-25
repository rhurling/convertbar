import { useState, useEffect, useCallback } from "react";
import { commands, type JobInfo, type PurgeResult } from "../lib/tauri";

export function useBadSources() {
  const [badSources, setBadSources] = useState<JobInfo[]>([]);

  const refresh = useCallback(async () => {
    try {
      setBadSources(await commands.getBadSources());
    } catch (e) {
      console.error("Failed to load bad sources:", e);
    }
  }, []);

  const purge = useCallback(
    async (ids: string[]): Promise<PurgeResult[]> => {
      const outcomes = await commands.purgeBadSources(ids);
      await refresh();
      return outcomes;
    },
    [refresh],
  );

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { badSources, refresh, purge };
}
