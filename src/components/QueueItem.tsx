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
  const handleRemove = async () => {
    await commands.removeJob(job.id);
    onRemoved();
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
      <span className="badge badge-dim">Queued</span>
      <button className="btn-icon" onClick={handleRemove} title="Remove">
        &times;
      </button>
    </div>
  );
}
