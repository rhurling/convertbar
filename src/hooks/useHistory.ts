import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  type JobInfo,
  type HistorySummary,
} from "../lib/tauri";

const PAGE_SIZE = 50;

export function useHistory() {
  const [history, setHistory] = useState<JobInfo[]>([]);
  const [summary, setSummary] = useState<HistorySummary>({
    total_saved_bytes: 0,
    total_files: 0,
  });
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [sortBy, setSortBy] = useState<string>("completed_at");

  const searchTimeoutRef = useRef<number>(undefined);
  const setSearchDebounced = useCallback((value: string) => {
    if (searchTimeoutRef.current) clearTimeout(searchTimeoutRef.current);
    searchTimeoutRef.current = window.setTimeout(() => setSearch(value), 300);
  }, []);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      const [page, sum] = await Promise.all([
        commands.getHistory(PAGE_SIZE, 0, search || undefined, sortBy),
        commands.getHistorySummary(search || undefined),
      ]);
      setHistory(page.jobs);
      setTotal(page.total);
      setSummary(sum);
    } catch (e) {
      console.error("Failed to load history:", e);
    } finally {
      setLoading(false);
    }
  }, [search, sortBy]);

  const loadMore = useCallback(async () => {
    try {
      setLoading(true);
      const page = await commands.getHistory(PAGE_SIZE, history.length, search || undefined, sortBy);
      setHistory((prev) => [...prev, ...page.jobs]);
      setTotal(page.total);
    } catch (e) {
      console.error("Failed to load more history:", e);
    } finally {
      setLoading(false);
    }
  }, [history.length, search, sortBy]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    const unlistenCompleted = listen("job-completed", () => {
      refresh();
    });
    const unlistenError = listen("job-error", () => {
      refresh();
    });
    return () => {
      unlistenCompleted.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, [refresh]);

  const hasMore = history.length < total;

  return { history, summary, hasMore, loading, loadMore, refresh, search, setSearchDebounced, sortBy, setSortBy };
}
