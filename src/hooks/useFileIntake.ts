import { useState, useEffect, useRef, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands, type FolderScanResult, type AddResult } from "../lib/tauri";
import { isServerHead } from "../lib/head";
import { summarizeAdds } from "../lib/addSummary";
import { errorText } from "../lib/errors";

/** Folders with this many files or fewer are added without a confirm prompt. */
const AUTO_ADD_MAX = 5;

type AddTask = ({ kind: "files"; paths: string[] } | { kind: "folder"; folder: FolderScanResult }) & {
  /** What classify dropped from the same selection (empty folders, vanished paths), appended to
   * this task's own summary. It cannot be shown when it is discovered: every task overwrites the
   * status with its summary as it lands, so a message set at classify time is wiped within
   * milliseconds — which is how a partial outcome came to report only its good half. */
  notice?: string;
};

/** A folder awaiting the user's confirmation, plus any notice its selection has to report once
 * the user answers — the notice outlives the prompt because the prompt has no deadline. */
type PendingConfirm = { folder: FolderScanResult; notice?: string };

export interface FileIntake {
  pendingConfirm: FolderScanResult | null;
  onAdd: () => void;
  onSkip: () => void;
  status: string | null;
  isDragOver: boolean;
  /** Feeds the same classify → enqueue pipeline as a drag-drop, for the server head's file-browser modal. */
  addPaths: (paths: string[]) => Promise<void>;
}

/**
 * Owns the whole drag-drop intake pipeline. Mounted in always-mounted `App` so drops work on
 * any tab (a drop calls `onDrop` to switch to Queue) and confirm/scan state survives tab
 * switches. The heavy scan/probe work runs through a single serialized pipeline: each
 * `add_files`/`confirm_folder_add` is awaited before the next, so a new drop appends and never
 * interrupts the folder currently being scanned. `confirmQueueRef` is the source of truth for
 * what awaits confirmation; the shown prompt is always its head.
 */
export function useFileIntake(opts: { onDrop: () => void }): FileIntake {
  const [isDragOver, setIsDragOver] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [, forceRender] = useState(0);
  const bump = useCallback(() => forceRender((n) => n + 1), []);

  const confirmQueueRef = useRef<PendingConfirm[]>([]);
  const taskQueueRef = useRef<AddTask[]>([]);
  const runningRef = useRef(false);
  const statusTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Keep the switch callback in a ref so the drag-drop effect can register exactly once.
  const onDropRef = useRef(opts.onDrop);
  onDropRef.current = opts.onDrop;

  // Always clear the prior auto-clear timer before setting a new status, so a later task's
  // summary can never be wiped by an earlier task's stale 4s timer.
  const setStatusMsg = useCallback((text: string | null, autoClear = false) => {
    if (statusTimerRef.current) {
      clearTimeout(statusTimerRef.current);
      statusTimerRef.current = null;
    }
    setStatus(text);
    if (autoClear && text) {
      statusTimerRef.current = setTimeout(() => setStatus(null), 4000);
    }
  }, []);

  // Drains the task queue one at a time. The while loop re-reads the live ref each iteration,
  // and runningRef is cleared synchronously (no await between the last check and the clear),
  // so a task pushed by enqueue() is always either picked up by the running drain or starts a
  // fresh one — never stranded.
  const runNext = useCallback(async () => {
    if (runningRef.current) return;
    runningRef.current = true;
    try {
      while (taskQueueRef.current.length > 0) {
        const task = taskQueueRef.current.shift()!;
        try {
          const res: AddResult =
            task.kind === "files"
              ? await commands.addFiles(task.paths)
              : await commands.confirmFolderAdd(task.folder.folder_path);
          await commands.startQueue();
          // summarizeAdds returns string | null; a null status renders nothing.
          const parts = [summarizeAdds([res]), task.notice].filter((p): p is string => !!p);
          setStatusMsg(parts.length > 0 ? parts.join(" · ") : null, true);
        } catch (e) {
          setStatusMsg(`Error: ${errorText(e)}`, true);
        }
      }
    } finally {
      runningRef.current = false;
    }
  }, [setStatusMsg]);

  const enqueue = useCallback(
    (task: AddTask) => {
      taskQueueRef.current.push(task);
      void runNext();
    },
    [runNext],
  );

  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatusMsg("Adding…"); // immediate feedback so a slow classify walk isn't dead air
      let classified;
      try {
        classified = await commands.classifyPaths(paths);
      } catch (e) {
        setStatusMsg(`Error: ${errorText(e)}`, true);
        return;
      }

      // Build the whole batch before running any of it: the shortfall below is only knowable
      // once every path has been sorted, and it has to be attached to the task whose summary
      // lands last, or the summaries would bury it.
      const tasks: AddTask[] = [];
      let emptyFolders = 0;
      if (classified.files.length > 0) {
        tasks.push({ kind: "files", paths: classified.files });
      }
      const confirms: FolderScanResult[] = [];
      for (const folder of classified.folders) {
        if (folder.file_count === 0) {
          emptyFolders++;
        } else if (folder.file_count <= AUTO_ADD_MAX) {
          tasks.push({ kind: "folder", folder });
        } else {
          confirms.push(folder);
        }
      }

      // classify_paths silently drops any path that is neither a file nor a directory by the
      // time it runs (deleted/renamed since being picked), so a selection can shrink without
      // ever raising an error. Both shortfalls are invisible in the result unless said aloud:
      // a two-item pick reporting "Added 1" gives the user no way to tell which item is gone.
      const missing = paths.length - classified.files.length - classified.folders.length;
      const skipped: string[] = [];
      if (emptyFolders > 0) {
        skipped.push(`no videos in ${emptyFolders} folder${emptyFolders === 1 ? "" : "s"}`);
      }
      if (missing > 0) {
        skipped.push(
          `${missing} path${missing === 1 ? "" : "s"} no longer exist${missing === 1 ? "s" : ""}`,
        );
      }
      const notice = skipped.join(" · ");

      // push, never replace — the anti-clobber invariant.
      const pending: PendingConfirm[] = confirms.map((folder) => ({ folder }));
      confirmQueueRef.current.push(...pending);

      if (tasks.length > 0) {
        // Rides on the last task: it is the one whose summary stays on screen.
        if (notice) tasks[tasks.length - 1].notice = notice;
        for (const task of tasks) enqueue(task);
        if (pending.length > 0) bump();
        setStatusMsg(null); // clear the placeholder; tasks report via the scanner + summaries
        return;
      }

      if (pending.length > 0) {
        // Nothing runs until the user answers the prompt, so the notice goes up now — the status
        // line is free, and a prompt that is never answered would otherwise report nothing at
        // all — and also rides the confirmed add, so its summary still carries it however long
        // the user takes to decide.
        if (notice) pending[pending.length - 1].notice = notice;
        bump();
        setStatusMsg(notice ? `Skipped · ${notice}` : null, Boolean(notice));
        return;
      }

      // Nothing was enqueued and nothing is pending confirmation. Without this, "Adding…" was
      // the last thing the user ever saw — a folder with no videos (or a vanished path) closed
      // the modal, flashed "Adding…", and then did nothing at all.
      setStatusMsg(notice ? `Nothing added · ${notice}` : "Nothing to add", true);
    },
    [enqueue, setStatusMsg, bump],
  );

  const handlePathsRef = useRef(handlePaths);
  handlePathsRef.current = handlePaths;

  // Register the single window-level listener once (empty deps + refs), StrictMode-safe.
  useEffect(() => {
    // Desktop-only: there's no native OS drag-drop event in a browser tab. The server head's
    // intake goes through the file-browser modal (addPaths, below) instead, so the listener is
    // never registered at all on that build rather than gating the handler's body.
    if (isServerHead) return;
    const appWindow = getCurrentWebviewWindow();
    const unlisten = appWindow.onDragDropEvent((event) => {
      if (event.payload.type === "over" || event.payload.type === "enter") {
        setIsDragOver(true);
      } else if (event.payload.type === "drop") {
        setIsDragOver(false);
        onDropRef.current();
        void handlePathsRef.current(event.payload.paths);
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const onAdd = useCallback(() => {
    const next = confirmQueueRef.current[0];
    if (!next) return;
    confirmQueueRef.current.shift();
    bump();
    enqueue({ kind: "folder", folder: next.folder, notice: next.notice });
  }, [enqueue, bump]);

  const onSkip = useCallback(() => {
    if (confirmQueueRef.current.length === 0) return;
    confirmQueueRef.current.shift();
    bump();
  }, [bump]);

  const pendingConfirm = confirmQueueRef.current[0]?.folder ?? null;

  return { pendingConfirm, onAdd, onSkip, status, isDragOver, addPaths: handlePaths };
}
