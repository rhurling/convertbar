import type { JobInfo, ConversionProgress } from "../lib/tauri";
import { commands } from "../lib/tauri";
import { fileName, formatEta } from "../lib/format";

interface ActiveJobProps {
  job: JobInfo;
  progress: ConversionProgress | null;
}

export default function ActiveJob({ job, progress }: ActiveJobProps) {
  const isPaused = job.status === "paused";
  const percent =
    progress && progress.job_id === job.id ? progress.percent : 0;
  const eta =
    progress && progress.job_id === job.id ? progress.eta_seconds : null;
  const fps =
    progress && progress.job_id === job.id ? progress.fps : null;

  return (
    <div className="active-job">
      <div className="active-job-header">
        <span className="active-job-name" title={job.source_path}>
          {fileName(job.source_path)}
        </span>
        <span className={`badge ${isPaused ? "badge-amber" : "badge-blue"}`}>
          {isPaused ? "Paused" : "Encoding"}
        </span>
      </div>

      <div className="progress-bar-track">
        <div
          className="progress-bar-fill"
          style={{ width: `${Math.min(percent, 100)}%` }}
        />
      </div>

      <div className="active-job-stats">
        <span>{Math.round(percent)}%</span>
        {eta !== null && eta > 0 && <span>ETA {formatEta(eta)}</span>}
        {fps !== null && fps > 0 && <span>{fps.toFixed(1)} fps</span>}
      </div>

      <div className="active-job-actions">
        {isPaused ? (
          <button
            className="btn btn-small"
            onClick={() => commands.resumeConversion()}
          >
            Resume
          </button>
        ) : (
          <button
            className="btn btn-small"
            onClick={() => commands.pauseConversion()}
          >
            Pause
          </button>
        )}
        <button
          className="btn btn-small btn-danger"
          onClick={() => commands.cancelConversion()}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}
