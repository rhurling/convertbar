import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "../lib/events";
import { commands, type JobInfo, type ConversionProgress } from "../lib/tauri";

export function useQueue() {
  const [queue, setQueue] = useState<JobInfo[]>([]);
  const [progress, setProgress] = useState<ConversionProgress | null>(null);
  const mounted = useRef(true);
  // Monotonic request id: a single job transition fans out several events, each firing
  // its own getQueue(); responses can resolve out of order, so drop any that isn't the latest.
  const latestRequest = useRef(0);

  const refresh = useCallback(async () => {
    const requestId = ++latestRequest.current;
    try {
      const q = await commands.getQueue();
      if (mounted.current && requestId === latestRequest.current) setQueue(q);
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
      listen("queue-updated", () => {
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
