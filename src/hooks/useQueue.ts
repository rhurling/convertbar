import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type JobInfo, type ConversionProgress } from "../lib/tauri";

export function useQueue() {
  const [queue, setQueue] = useState<JobInfo[]>([]);
  const [progress, setProgress] = useState<ConversionProgress | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const q = await commands.getQueue();
      if (mounted.current) setQueue(q);
    } catch (e) {
      console.error("Failed to refresh queue:", e);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();

    const unlisteners = [
      listen<ConversionProgress>("conversion-progress", (event) => {
        if (mounted.current) setProgress(event.payload);
      }),
      listen("job-status-changed", () => {
        refresh();
      }),
      listen("job-completed", () => {
        refresh();
      }),
      listen("job-error", () => {
        refresh();
      }),
    ];

    return () => {
      mounted.current = false;
      unlisteners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, [refresh]);

  const activeJob = queue.find(
    (j) => j.status === "encoding" || j.status === "paused",
  );
  const pendingJobs = queue.filter((j) => j.status === "queued");

  return { queue, activeJob, pendingJobs, progress, refresh };
}
