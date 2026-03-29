import { useHistory } from "../hooks/useHistory";
import { formatBytes } from "../lib/format";
import HistoryItem from "../components/HistoryItem";

export default function HistoryPage() {
  const { history, summary, hasMore, loading, loadMore } = useHistory();

  return (
    <div className="history-page">
      {summary.total_files > 0 && (
        <div className="history-summary">
          <span className="summary-saved">
            Total saved: {formatBytes(summary.total_saved_bytes)}
          </span>
          <span className="summary-files">
            {summary.total_files} file{summary.total_files !== 1 ? "s" : ""}
          </span>
        </div>
      )}

      <div className="item-list scrollable">
        {history.map((job) => (
          <HistoryItem key={job.id} job={job} />
        ))}
      </div>

      {hasMore && (
        <button className="btn btn-block" onClick={loadMore} disabled={loading}>
          {loading ? "Loading..." : "Load more"}
        </button>
      )}

      {history.length === 0 && !loading && (
        <div className="empty-state">No conversion history yet</div>
      )}
    </div>
  );
}
