import type { JobInfo } from "../lib/tauri";
import { commands } from "../lib/tauri";
import { fileName } from "../lib/format";

interface QueueItemProps {
  job: JobInfo;
  onRemoved: () => void;
}

export default function QueueItem({ job, onRemoved }: QueueItemProps) {
  const handleRemove = async () => {
    await commands.removeJob(job.id);
    onRemoved();
  };

  return (
    <div className="queue-item">
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
