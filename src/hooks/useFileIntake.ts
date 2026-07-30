import { useState, useEffect, useRef, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands, type FolderScanResult, type AddResult } from "../lib/tauri";
import { isServerHead } from "../lib/head";
import { summarizeAdds } from "../lib/addSummary";
import { errorText } from "../lib/errors";

/** Folders with this many files or fewer are added without a confirm prompt. */
const AUTO_ADD_MAX = 5;

type AddTask =
  | { kind: "files"; paths: string[] }
  | { kind: "folder"; folder: FolderScanResult };

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

  const confirmQueueRef = useRef<FolderScanResult[]>([]);
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
          setStatusMsg(summarizeAdds([res]), true);
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

      // Whether anything was either enqueued now or pushed to confirmQueueRef for the user to
      // confirm later — both cases already give the user feedback (a task summary or the
      // confirm prompt itself), so only a fully empty outcome needs a message of its own.
      let enqueuedOrPending = false;
      let emptyFolders = 0;
      if (classified.files.length > 0) {
        enqueuedOrPending = true;
        enqueue({ kind: "files", paths: classified.files });
      }
      for (const folder of classified.folders) {
        if (folder.file_count === 0) {
          emptyFolders++;
          continue;
        }
        enqueuedOrPending = true;
        if (folder.file_count <= AUTO_ADD_MAX) {
          enqueue({ kind: "folder", folder });
        } else {
          confirmQueueRef.current.push(folder); // push, never replace — the anti-clobber invariant
          bump();
        }
      }

      if (enqueuedOrPending) {
        setStatusMsg(null); // clear the placeholder; tasks report via the scanner + summaries
        return;
      }

      // Nothing was enqueued and nothing is pending confirmation. classify_paths silently drops
      // any path that is neither a file nor a directory by the time it runs (deleted/renamed
      // since being picked), so a selection can shrink to zero without ever raising an error.
      // Without this, "Adding…" was the last thing the user ever saw — a folder with no videos
      // (or a vanished path) closed the modal, flashed "Adding…", and then did nothing at all.
      const missing = paths.length - classified.files.length - classified.folders.length;
      const parts: string[] = [];
      if (emptyFolders > 0) {
        parts.push(`no videos in ${emptyFolders} folder${emptyFolders === 1 ? "" : "s"}`);
      }
      if (missing > 0) {
        parts.push(
          `${missing} path${missing === 1 ? "" : "s"} no longer exist${missing === 1 ? "s" : ""}`,
        );
      }
      setStatusMsg(parts.length > 0 ? `Nothing added · ${parts.join(" · ")}` : "Nothing to add", true);
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
    const folder = confirmQueueRef.current[0];
    if (!folder) return;
    confirmQueueRef.current.shift();
    bump();
    enqueue({ kind: "folder", folder });
  }, [enqueue, bump]);

  const onSkip = useCallback(() => {
    if (confirmQueueRef.current.length === 0) return;
    confirmQueueRef.current.shift();
    bump();
  }, [bump]);

  const pendingConfirm = confirmQueueRef.current[0] ?? null;

  return { pendingConfirm, onAdd, onSkip, status, isDragOver, addPaths: handlePaths };
}
