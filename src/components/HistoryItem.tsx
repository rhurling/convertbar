import type { JobInfo } from "../lib/tauri";
import {
  durationSeconds,
  fileName,
  formatBytes,
  formatDuration,
  formatPercent,
} from "../lib/format";

interface HistoryItemProps {
  job: JobInfo;
  showDuration?: boolean;
  onContextMenu?: (e: React.MouseEvent, job: JobInfo) => void;
}

export default function HistoryItem({
  job,
  showDuration = false,
  onContextMenu,
}: HistoryItemProps) {
  const isError = job.status === "error";
  const keptOriginal = job.kept_file === "original";

  let badgeClass = "badge-green";
  let badgeLabel = "Saved";
  if (isError) {
    badgeClass = "badge-red";
    badgeLabel = "Error";
  } else if (keptOriginal) {
    badgeClass = "badge-amber";
    badgeLabel = "Kept original";
  } else if (job.status === "skipped") {
    badgeClass = "badge-dim";
    badgeLabel = "Skipped";
  }

  const secs = showDuration
    ? durationSeconds(job.started_at, job.completed_at)
    : null;
  const duration =
    secs !== null ? (
      <span className="history-item-duration" title="Encode time">
        {formatDuration(secs)}
      </span>
    ) : null;

  return (
    <div
      className={`history-item ${isError ? "history-item-error" : ""}`}
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu?.(e, job);
      }}
    >
      <div className="history-item-top">
        <span className="history-item-name" title={job.source_path}>
          {fileName(job.source_path)}
        </span>
        <span className={`badge ${badgeClass}`}>{badgeLabel}</span>
      </div>
      {!isError && (job.original_size !== null || duration) && (
        <div className="history-item-sizes">
          {job.original_size !== null && (
            <>
              <span>{formatBytes(job.original_size)}</span>
              <span className="arrow">&rarr;</span>
              <span>
                {job.converted_size !== null
                  ? formatBytes(job.converted_size)
                  : "—"}
              </span>
              {job.space_saved !== null && job.space_saved > 0 && (
                <span className="saved-pct">
                  -{formatPercent(job.space_saved, job.original_size)}
                </span>
              )}
            </>
          )}
          {duration}
        </div>
      )}
      {isError && (job.error_message || duration) && (
        <div className="history-item-error-row">
          {job.error_message && (
            <span className="history-item-error-msg" title={job.error_message}>
              {job.error_message}
            </span>
          )}
          {duration}
        </div>
      )}
    </div>
  );
}
