import type { JobInfo } from "../lib/tauri";
import { fileName, formatBytes, formatPercent } from "../lib/format";

interface HistoryItemProps {
  job: JobInfo;
}

export default function HistoryItem({ job }: HistoryItemProps) {
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

  return (
    <div className={`history-item ${isError ? "history-item-error" : ""}`}>
      <div className="history-item-top">
        <span className="history-item-name" title={job.source_path}>
          {fileName(job.source_path)}
        </span>
        <span className={`badge ${badgeClass}`}>{badgeLabel}</span>
      </div>
      {!isError && job.original_size !== null && (
        <div className="history-item-sizes">
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
        </div>
      )}
      {isError && job.error_message && (
        <div className="history-item-error-msg">{job.error_message}</div>
      )}
    </div>
  );
}
