import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";
import HistoryItem from "../components/HistoryItem";

export default function QueuePage() {
  const { activeJob, pendingJobs, recentCompleted, progress, refresh } =
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

      {recentCompleted.length > 0 && (
        <div className="section">
          <div className="section-header">Recent</div>
          <div className="item-list">
            {recentCompleted.map((job) => (
              <HistoryItem key={job.id} job={job} />
            ))}
          </div>
        </div>
      )}

      {!activeJob && pendingJobs.length === 0 && recentCompleted.length === 0 && (
        <div className="empty-state">No items in queue</div>
      )}
    </div>
  );
}
