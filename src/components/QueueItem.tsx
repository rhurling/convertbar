import { useState } from "react";
import type { JobInfo } from "../lib/tauri";
import { commands } from "../lib/tauri";
import { fileName } from "../lib/format";

interface QueueItemProps {
  job: JobInfo;
  onRemoved: () => void;
  onDragStart?: (id: string) => void;
  onDragOver?: (id: string) => void;
  onDrop?: (draggedId: string, targetId: string) => void;
  isDragOver?: boolean;
}

export default function QueueItem({ job, onRemoved, onDragStart, onDragOver, onDrop, isDragOver }: QueueItemProps) {
  const isInPlace = job.source_path === job.output_path;
  const [removing, setRemoving] = useState(false);

  const handleRemove = async () => {
    if (removing) return; // in-flight guard: a double-click must not fire remove_job twice
    setRemoving(true);
    try {
      await commands.removeJob(job.id);
      onRemoved();
    } catch (e) {
      console.error("Failed to remove job:", e);
      setRemoving(false); // let the user retry a failed removal
    }
  };

  return (
    <div
      className={`queue-item ${isDragOver ? "drag-over" : ""}`}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("text/plain", job.id);
        e.dataTransfer.effectAllowed = "move";
        onDragStart?.(job.id);
      }}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        onDragOver?.(job.id);
      }}
      onDrop={(e) => {
        e.preventDefault();
        const draggedId = e.dataTransfer.getData("text/plain");
        if (draggedId && draggedId !== job.id) {
          onDrop?.(draggedId, job.id);
        }
      }}
      onDragLeave={() => onDragOver?.("")}
    >
      <span className="drag-handle">&equiv;</span>
      <span className="queue-item-name" title={job.source_path}>
        {fileName(job.source_path)}
      </span>
      {isInPlace && (
        <span className="badge badge-dim" title="Re-encoded in place, replacing the original">
          In place
        </span>
      )}
      <span className="badge badge-dim">Queued</span>
      <button className="btn-icon" onClick={handleRemove} disabled={removing} title="Remove">
        &times;
      </button>
    </div>
  );
}
