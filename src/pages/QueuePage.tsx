import { useState } from "react";
import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";
import { commands } from "../lib/tauri";
import type { HandbrakeStatus } from "../lib/tauri";

interface QueuePageProps {
  hbStatus: HandbrakeStatus | null;
}

export default function QueuePage({ hbStatus }: QueuePageProps) {
  const { activeJob, pendingJobs, progress, refresh } =
    useQueue();
  const [dragOverId, setDragOverId] = useState<string | null>(null);

  const handleDrop = async (draggedId: string, targetId: string) => {
    setDragOverId(null);
    const ids = pendingJobs.map(j => j.id);
    const fromIdx = ids.indexOf(draggedId);
    const toIdx = ids.indexOf(targetId);
    if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return;
    ids.splice(fromIdx, 1);
    ids.splice(toIdx, 0, draggedId);
    await commands.reorderQueue(ids);
    refresh();
  };

  return (
    <div className="queue-page">
      {hbStatus && !hbStatus.found && (
        <div className="hb-warning">
          <span className="hb-warning-icon">&#9888;&#65039;</span>
          <div>
            <strong>HandBrakeCLI not found</strong>
            <p>Install via: <code>brew install handbrake</code> or set the path in Settings.</p>
          </div>
        </div>
      )}
      <DropZone onFilesAdded={refresh} />

      {activeJob && <ActiveJob job={activeJob} progress={progress} />}

      {pendingJobs.length > 0 && (
        <div className="section">
          <div className="section-header">
            <span>Pending ({pendingJobs.length})</span>
            <button className="btn btn-small btn-dim" onClick={async () => {
              await commands.clearQueue();
              refresh();
            }}>Clear</button>
          </div>
          <div className="item-list">
            {pendingJobs.map((job) => (
              <QueueItem
                key={job.id}
                job={job}
                onRemoved={refresh}
                onDragStart={() => {}}
                onDragOver={(id) => setDragOverId(id)}
                onDrop={handleDrop}
                isDragOver={dragOverId === job.id}
              />
            ))}
          </div>
        </div>
      )}

      {!activeJob && pendingJobs.length === 0 && (
        <div className="empty-state">No items in queue</div>
      )}
    </div>
  );
}
