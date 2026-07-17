import { useState, useEffect, useCallback, useRef } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands, FolderScanResult, AddResult } from "../lib/tauri";
import { summarizeAdds } from "../lib/addSummary";

interface DropZoneProps {
  onFilesAdded: () => void;
}

export default function DropZone({ onFilesAdded }: DropZoneProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [pendingFolders, setPendingFolders] = useState<FolderScanResult[]>([]);

  // Authoritative copy updated synchronously so concurrent confirm/skip handlers each filter
  // from the latest list, not their render-time snapshot — otherwise a slow-resolving handler
  // restores an already-removed folder and the "last one → startQueue" check never fires.
  const pendingRef = useRef<FolderScanResult[]>([]);
  const setPending = useCallback((next: FolderScanResult[]) => {
    pendingRef.current = next;
    setPendingFolders(next);
  }, []);

  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatus("Adding files...");
      try {
        const classified = await commands.classifyPaths(paths);
        const results: AddResult[] = [];

        if (classified.files.length > 0) {
          results.push(await commands.addFiles(classified.files));
        }

        const toConfirm: FolderScanResult[] = [];
        for (const folder of classified.folders) {
          if (folder.file_count === 0) continue;
          if (folder.file_count <= 5) {
            results.push(await commands.confirmFolderAdd(folder.folder_path));
          } else {
            toConfirm.push(folder);
          }
        }

        if (toConfirm.length > 0) {
          setPending(toConfirm);
          setStatus(summarizeAdds(results));
        } else {
          await commands.startQueue();
          onFilesAdded();
          const summary = summarizeAdds(results);
          setStatus(summary);
          if (summary) setTimeout(() => setStatus(null), 4000);
        }
      } catch (e) {
        setStatus(`Error: ${e}`);
        setTimeout(() => setStatus(null), 3000);
      }
    },
    [onFilesAdded, setPending],
  );

  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const unlisten = appWindow.onDragDropEvent((event) => {
      if (event.payload.type === "over" || event.payload.type === "enter") {
        setIsDragOver(true);
      } else if (event.payload.type === "drop") {
        setIsDragOver(false);
        handlePaths(event.payload.paths);
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [handlePaths]);

  return (
    <div className={`drop-zone ${isDragOver ? "drag-over" : ""}`}>
      {pendingFolders.length > 0 ? (
        <div className="folder-confirm">
          {status && <span className="drop-zone-status">{status}</span>}
          {pendingFolders.map((folder) => (
            <div key={folder.folder_path} className="folder-confirm-item">
              <span>Add {folder.file_count} files from &quot;{folder.folder_name}&quot;?</span>
              <div className="folder-confirm-actions">
                <button className="btn btn-small" onClick={async () => {
                  try {
                    const res = await commands.confirmFolderAdd(folder.folder_path);
                    const remaining = pendingRef.current.filter(
                      (f) => f.folder_path !== folder.folder_path,
                    );
                    setPending(remaining);
                    if (remaining.length === 0) {
                      await commands.startQueue();
                      onFilesAdded();
                      const summary = summarizeAdds([res]);
                      setStatus(summary);
                      if (summary) setTimeout(() => setStatus(null), 4000);
                    }
                  } catch (e) {
                    setStatus(`Error: ${e}`);
                    setTimeout(() => setStatus(null), 3000);
                  }
                }}>Add</button>
                <button className="btn btn-small btn-dim" onClick={() => {
                  const remaining = pendingRef.current.filter(
                    (f) => f.folder_path !== folder.folder_path,
                  );
                  setPending(remaining);
                  if (remaining.length === 0) {
                    commands.startQueue();
                    onFilesAdded();
                  }
                }}>Skip</button>
              </div>
            </div>
          ))}
        </div>
      ) : status ? (
        <span className="drop-zone-status">{status}</span>
      ) : (
        <span className="drop-zone-label">
          Drop video files or folders here
        </span>
      )}
    </div>
  );
}
