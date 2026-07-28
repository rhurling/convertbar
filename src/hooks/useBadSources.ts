import { useState, useEffect, useCallback } from "react";
import { listen } from "../lib/events";
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

  // A newly-classified bad source arrives as a job-error, the same event HistoryPage's own
  // useHistory already listens on — mirror that effect so the review list stays live while
  // the queue is running, instead of only ever reflecting whatever was true on mount.
  useEffect(() => {
    const unlisten = listen("job-error", () => {
      refresh();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);

  return { badSources, refresh, purge };
}
