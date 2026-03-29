import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";
import { commands } from "../lib/tauri";

export default function QueuePage() {
  const { activeJob, pendingJobs, progress, refresh } =
    useQueue();

  return (
    <div className="queue-page">
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
              <QueueItem key={job.id} job={job} onRemoved={refresh} />
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
