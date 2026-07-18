import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { AddStarted, AddProgress, AddFinished, AddActivity } from "../lib/tauri";

/**
 * Turns the backend `add-*` events into UI state: `isAdding` (any operation in flight,
 * drives the title-bar spinner) and `activity` (the most-recent operation's detail,
 * drives the Queue-page bar). Owned by App.tsx so tab switches never lose it.
 *
 * Uses a Set of open op ids, not a counter: a watcher scan can emit `add-started` before
 * these listeners attach, so a later `add-finished` for an unseen op must be a harmless
 * no-op rather than driving a counter negative.
 */
export function useAddProgress() {
  const [openOps, setOpenOps] = useState<Set<string>>(new Set());
  const [activity, setActivity] = useState<AddActivity | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    const add = (id: string) =>
      setOpenOps((prev) => {
        if (prev.has(id)) return prev;
        const next = new Set(prev);
        next.add(id);
        return next;
      });

    const unlisteners = [
      listen<AddStarted>("add-started", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id);
        setActivity({ opId: payload.op_id, done: null, total: null });
      }),
      listen<AddProgress>("add-progress", ({ payload }) => {
        if (!mounted.current) return;
        add(payload.op_id); // covers a start we never saw
        setActivity({ opId: payload.op_id, done: payload.done, total: payload.total });
      }),
      listen<AddFinished>("add-finished", ({ payload }) => {
        if (!mounted.current) return;
        setOpenOps((prev) => {
          if (!prev.has(payload.op_id)) return prev;
          const next = new Set(prev);
          next.delete(payload.op_id);
          return next;
        });
        setActivity((cur) => (cur?.opId === payload.op_id ? null : cur));
      }),
    ];

    return () => {
      mounted.current = false;
      unlisteners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);

  return { isAdding: openOps.size > 0, activity };
}
