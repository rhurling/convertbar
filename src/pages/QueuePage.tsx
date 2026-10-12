import { useState, useEffect } from "react";
import { listen } from "../lib/events";
import { useQueue } from "../hooks/useQueue";
import DropZone from "../components/DropZone";
import ActiveJob from "../components/ActiveJob";
import QueueItem from "../components/QueueItem";
import { commands } from "../lib/tauri";
import type { HandbrakeStatus, LowDiskPause, UnreachableStop } from "../lib/tauri";
import { formatBytes } from "../lib/format";
import AddingIndicator from "../components/AddingIndicator";
import type { AddActivity } from "../lib/tauri";
import type { FileIntake } from "../hooks/useFileIntake";
import { isServerHead } from "../lib/head";
import FileBrowserModal from "../components/FileBrowserModal";

interface QueuePageProps {
  hbStatus: HandbrakeStatus | null;
  adding: AddActivity | null;
  isAdding: boolean;
  intake: FileIntake;
}

function lowDiskMessage(p: LowDiskPause): string {
  return (
    `Only ${formatBytes(p.available_bytes)} free on the destination — ` +
    `need ${formatBytes(p.required_bytes)} to start the next file. Free up space, then Resume.`
  );
}

export default function QueuePage({ hbStatus, adding, isAdding, intake }: QueuePageProps) {
  const { activeJob, pendingJobs, progress, phase, refresh } = useQueue();
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [lowDiskMsg, setLowDiskMsg] = useState<string | null>(null);
  const [showBrowser, setShowBrowser] = useState(false);
  const [unreachable, setUnreachable] = useState<UnreachableStop | null>(null);

  // The queue stops itself before starting a file when the destination disk is low; surface why.
  useEffect(() => {
    const un = listen<LowDiskPause>("queue-paused-low-disk", (e) => {
      setLowDiskMsg(lowDiskMessage(e.payload));
    });
    return () => {
      un.then((u) => u());
    };
  }, []);

  // Seed the banner from backend state so a pause that fired while this tab was unmounted
  // (or at launch) is still explained — the live event only fires while mounted.
  useEffect(() => {
    commands
      .getLowDiskPause()
      .then((p) => {
        if (p) setLowDiskMsg(lowDiskMessage(p));
      })
      .catch(() => {});
  }, []);

  // Clear the low-disk notice once the queue restarts (an active job appears) or the pending
  // list empties (e.g. the user hit Clear) — otherwise the banner lingers over an empty queue.
  useEffect(() => {
    if (activeJob || pendingJobs.length === 0) setLowDiskMsg(null);
  }, [activeJob, pendingJobs.length]);

  // A remote host that could not be reached stops the queue (ADR 0003); say which, live and on
  // mount. The stop outlives an empty queue — Start takes the failed job back — so it is not
  // cleared with the pending list like the low-disk notice: it is re-read instead, which also
  // drops it after Clear, which forgets it server-side.
  useEffect(() => {
    const un = listen<UnreachableStop>("queue-paused-unreachable", (e) => {
      setUnreachable(e.payload);
    });
    return () => {
      un.then((u) => u());
    };
  }, []);

  useEffect(() => {
    if (activeJob) {
      setUnreachable(null);
      return;
    }
    let live = true;
    commands
      .getUnreachableStop()
      .then((s) => {
        if (live) setUnreachable(s);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [activeJob, pendingJobs.length]);

  const handleDrop = async (draggedId: string, targetId: string) => {
    setDragOverId(null);
    const ids = pendingJobs.map((j) => j.id);
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
      <DropZone
        pendingConfirm={intake.pendingConfirm}
        onAdd={intake.onAdd}
        onSkip={intake.onSkip}
        status={intake.status}
        isDragOver={intake.isDragOver}
        onPick={isServerHead ? () => setShowBrowser(true) : undefined}
      />

      {showBrowser && (
        <FileBrowserModal
          mode="files"
          onSelect={(paths, remote) => {
            setShowBrowser(false);
            intake.addPaths(paths, remote);
          }}
          onClose={() => setShowBrowser(false)}
        />
      )}

      <AddingIndicator activity={adding} />

      {lowDiskMsg && (
        <div className="hb-warning" role="status" aria-live="polite">
          <span className="hb-warning-icon">&#9888;&#65039;</span>
          <div>
            <strong>Queue paused — low disk space</strong>
            <p>{lowDiskMsg}</p>
          </div>
        </div>
      )}

      {unreachable && (
        <div className="hb-warning" role="status" aria-live="polite">
          <span className="hb-warning-icon">&#9888;&#65039;</span>
          <div>
            <strong>Queue stopped — {unreachable.host} is unreachable</strong>
            <p>
              The last job failed at the {unreachable.step} step. Check the connection to{" "}
              {unreachable.host}, then retry it.
            </p>
          </div>
          {/* Its own control: the failed job is an error row, so with nothing else queued the
              Pending section and its Start button are not shown at all. */}
          <button
            className="btn btn-small"
            onClick={async () => {
              await commands.startQueue();
              // The run that starts clears the stop; if the host is still down, the event puts
              // the banner straight back.
              setUnreachable(null);
              refresh();
            }}
          >
            Retry
          </button>
        </div>
      )}

      {activeJob && <ActiveJob job={activeJob} progress={progress} phase={phase} />}

      {pendingJobs.length > 0 && (
        <div className="section">
          <div className="section-header">
            <span>Pending ({pendingJobs.length})</span>
            <div className="section-header-actions">
              {!activeJob && (
                <button
                  className="btn btn-small"
                  onClick={async () => {
                    await commands.startQueue();
                    refresh();
                  }}
                >
                  Resume
                </button>
              )}
              <button
                className="btn btn-small btn-dim"
                onClick={async () => {
                  await commands.clearQueue();
                  refresh();
                }}
              >
                Clear
              </button>
            </div>
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

      {!isAdding && !activeJob && pendingJobs.length === 0 && (
        <div className="empty-state">
          <span className="empty-state-icon">&#128194;</span>
          <span>
            {isServerHead
              ? "Add files or folders to get started"
              : "Drag video files or folders here to get started"}
          </span>
        </div>
      )}
    </div>
  );
}
