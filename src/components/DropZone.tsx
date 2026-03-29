import { useState, useEffect, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands } from "../lib/tauri";

interface DropZoneProps {
  onFilesAdded: () => void;
}

export default function DropZone({ onFilesAdded }: DropZoneProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatus("Adding files...");
      try {
        const files: string[] = [];
        const folders: string[] = [];

        for (const p of paths) {
          // Simple heuristic: paths with an extension are files
          const lastSegment = p.split("/").pop() || "";
          if (lastSegment.includes(".")) {
            files.push(p);
          } else {
            folders.push(p);
          }
        }

        if (files.length > 0) {
          await commands.addFiles(files);
        }

        for (const folder of folders) {
          const scan = await commands.scanFolder(folder);
          const ok = window.confirm(
            `Add ${scan.file_count} video file(s) from "${scan.folder_name}"?`,
          );
          if (ok) {
            await commands.confirmFolderAdd(folder);
          }
        }

        await commands.startQueue();
        onFilesAdded();
        setStatus(null);
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
      ) : (
        <span className="drop-zone-label">
          Drop video files or folders here
        </span>
      )}
    </div>
  );
}
