import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";

export default function QueuePage() {
  const { activeJob, pendingJobs, progress, refresh } =
    useQueue();

  return (
    <div className="queue-page">
      <DropZone onFilesAdded={refresh} />

      {activeJob && <ActiveJob job={activeJob} progress={progress} />}

      {pendingJobs.length > 0 && (
        <div className="section">
          <div className="section-header">Pending ({pendingJobs.length})</div>
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
