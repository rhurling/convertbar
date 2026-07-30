import { FolderScanResult } from "../lib/tauri";

interface DropZoneProps {
  pendingConfirm: FolderScanResult | null;
  onAdd: () => void;
  onSkip: () => void;
  status: string | null;
  isDragOver: boolean;
  /** Server head: there is no OS drag-drop in a browser tab, so the surface opens the
   *  file browser instead of advertising a drop that cannot happen. */
  onPick?: () => void;
}

/**
 * Presentational drop surface. All intake orchestration lives in `useFileIntake` (App-owned);
 * this component just renders the confirm prompt / transient status / label three-way switch
 * and calls the passed handlers. The window-level drag-drop listener lives in the hook, so this
 * component is never the reason a drop is or isn't captured.
 */
export default function DropZone({ pendingConfirm, onAdd, onSkip, status, isDragOver, onPick }: DropZoneProps) {
  return (
    <div className={`drop-zone ${isDragOver ? "drag-over" : ""}`}>
      {pendingConfirm ? (
        <div className="folder-confirm">
          {status && <span className="drop-zone-status">{status}</span>}
          <div className="folder-confirm-item">
            <span>
              Add {pendingConfirm.file_count} files from &quot;{pendingConfirm.folder_name}&quot;?
            </span>
            <div className="folder-confirm-actions">
              <button className="btn btn-small" onClick={onAdd}>
                Add
              </button>
              <button className="btn btn-small btn-dim" onClick={onSkip}>
                Skip
              </button>
            </div>
          </div>
        </div>
      ) : status ? (
        <span className="drop-zone-status">{status}</span>
      ) : onPick ? (
        <button type="button" className="drop-zone-pick" onClick={onPick}>
          Add files or folders…
        </button>
      ) : (
        <span className="drop-zone-label">Drop video files or folders here</span>
      )}
    </div>
  );
}
