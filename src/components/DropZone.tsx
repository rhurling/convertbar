import { useState, useEffect, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands, FolderScanResult } from "../lib/tauri";

interface DropZoneProps {
  onFilesAdded: () => void;
}

export default function DropZone({ onFilesAdded }: DropZoneProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [pendingFolders, setPendingFolders] = useState<FolderScanResult[]>([]);

  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatus("Adding files...");
      try {
        const classified = await commands.classifyPaths(paths);

        if (classified.files.length > 0) {
          await commands.addFiles(classified.files);
        }

        const toConfirm: FolderScanResult[] = [];
        for (const folder of classified.folders) {
          if (folder.file_count === 0) continue;
          if (folder.file_count <= 5) {
            await commands.confirmFolderAdd(folder.folder_path);
          } else {
            toConfirm.push(folder);
          }
        }

        if (toConfirm.length > 0) {
          setPendingFolders(toConfirm);
          setStatus(null);
        } else {
          await commands.startQueue();
          onFilesAdded();
          setStatus(null);
        }
      } catch (e) {
        setStatus(`Error: ${e}`);
        setTimeout(() => setStatus(null), 3000);
      }
    },
    [onFilesAdded],
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
      {status ? (
        <span className="drop-zone-status">{status}</span>
      ) : pendingFolders.length > 0 ? (
        <div className="folder-confirm">
          {pendingFolders.map((folder, i) => (
            <div key={folder.folder_path} className="folder-confirm-item">
              <span>Add {folder.file_count} files from &quot;{folder.folder_name}&quot;?</span>
              <div className="folder-confirm-actions">
                <button className="btn btn-small" onClick={async () => {
                  await commands.confirmFolderAdd(folder.folder_path);
                  const remaining = pendingFolders.filter((_, j) => j !== i);
                  setPendingFolders(remaining);
                  if (remaining.length === 0) {
                    await commands.startQueue();
                    onFilesAdded();
                  }
                }}>Add</button>
                <button className="btn btn-small btn-dim" onClick={() => {
                  const remaining = pendingFolders.filter((_, j) => j !== i);
                  setPendingFolders(remaining);
                  if (remaining.length === 0) {
                    commands.startQueue();
                    onFilesAdded();
                  }
                }}>Skip</button>
              </div>
            </div>
          ))}
        </div>
      ) : (
        <span className="drop-zone-label">
          Drop video files or folders here
        </span>
      )}
    </div>
  );
}
