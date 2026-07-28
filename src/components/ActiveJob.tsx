import { useEffect, useState } from "react";
import type { JobInfo, ConversionProgress } from "../lib/tauri";
import { commands } from "../lib/tauri";
import { fileName, formatEta } from "../lib/format";

interface ActiveJobProps {
  job: JobInfo;
  progress: ConversionProgress | null;
}

export default function ActiveJob({ job, progress }: ActiveJobProps) {
  const [pauseAfter, setPauseAfter] = useState(false);
  const [canPauseProcess, setCanPauseProcess] = useState(true);
  const [actionError, setActionError] = useState<string | null>(null);
  const isPaused = job.status === "paused";
  const percent =
    progress && progress.job_id === job.id ? progress.percent : 0;
  const eta =
    progress && progress.job_id === job.id ? progress.eta_seconds : null;
  const fps =
    progress && progress.job_id === job.id ? progress.fps : null;

  useEffect(() => {
    // getAppInfo() works on both heads (getPlatformCapabilities is desktop-only); can_pause_process
    // is a runtime data field, not a build-time UI-presence gate.
    commands.getAppInfo().then((info) => {
      setCanPauseProcess(info.can_pause_process);
    });
    // The armed-state lives in the backend; seed from it so tab remounts (and the updater
    // flow arming it elsewhere) can't leave this button showing the wrong label.
    commands.getPauseAfterCurrent().then(setPauseAfter).catch(() => {});
  }, []);

  // Controls fire-and-forget invokes; without this a rejected one (e.g. no active process)
  // becomes an invisible unhandled rejection. Surface it inline instead.
  const run = async (fn: () => Promise<void>) => {
    try {
      setActionError(null);
      await fn();
    } catch (e) {
      setActionError(String(e));
    }
  };

  const togglePauseAfter = () =>
    run(async () => {
      if (pauseAfter) {
        await commands.cancelPauseAfterCurrent();
        setPauseAfter(false);
      } else {
        await commands.pauseAfterCurrent();
        setPauseAfter(true);
      }
    });

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
        {canPauseProcess ? (
          // macOS: real process pause/resume via SIGSTOP/SIGCONT
          <>
            {isPaused ? (
              <button
                className="btn btn-small"
                onClick={() => run(() => commands.resumeConversion())}
              >
                Resume
              </button>
            ) : (
              <button
                className="btn btn-small"
                onClick={() => run(() => commands.pauseConversion())}
              >
                Pause
              </button>
            )}
            <button
              className={`btn btn-small${pauseAfter ? " btn-active" : ""}`}
              onClick={togglePauseAfter}
              title={pauseAfter ? "Cancel pause after this job" : "Pause queue after this job finishes"}
            >
              {pauseAfter ? "Will pause" : "Pause after this"}
            </button>
          </>
        ) : (
          // Other platforms: queue-level pause only
          <button
            className={`btn btn-small${pauseAfter ? " btn-active" : ""}`}
            onClick={togglePauseAfter}
            title={pauseAfter ? "Cancel pause after this job" : "Pause queue after this job finishes"}
          >
            {pauseAfter ? "Will pause" : "Pause after this"}
          </button>
        )}
        <button
          className="btn btn-small btn-danger"
          onClick={() => run(() => commands.cancelConversion())}
        >
          Cancel
        </button>
      </div>

      {actionError && <div className="active-job-error">{actionError}</div>}
    </div>
  );
}
