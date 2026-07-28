import { useUpdate } from "../hooks/useUpdate";
import { commands, type AvailableUpdate, type UpdateMode } from "../lib/tauri";

const MODES: { value: UpdateMode; label: string }[] = [
  { value: "automatic", label: "Automatic" },
  { value: "notify", label: "Notify me" },
  { value: "off", label: "Manual only" },
];

// Statuses where the backend would refuse a manual check anyway (`manual_check_block` in
// updater.rs: an install is downloading, waiting for the queue, or already installed and
// awaiting restart) — disabled here so the button doesn't produce a guaranteed actionError.
const CHECK_BLOCKED: ReadonlySet<string> = new Set([
  "checking",
  "downloading",
  "waitingForIdle",
  "readyToRestart",
]);

function relativeTime(unixSeconds: number): string {
  const mins = Math.max(0, Math.round((Date.now() / 1000 - unixSeconds) / 60));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} minute${mins === 1 ? "" : "s"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

function AvailableDetails({ update }: { update: AvailableUpdate }) {
  return (
    <>
      <div className="update-version">
        Version {update.version}
        {update.date && <span className="update-date"> · {update.date}</span>}
      </div>
      {update.notes && <pre className="update-notes">{update.notes}</pre>}
    </>
  );
}

export default function UpdatePanel() {
  const { state, actionError, checkNow, install, skip, restart } = useUpdate();
  if (!state) return null;

  // `actionError` (a rejected checkNow/install/skip) and `state.last_error` (the scheduler's
  // own background check/install failure) are different channels, but a manual check made
  // while offline dual-writes the identical message to both. Preferring `last_error` — the
  // persistent, backend-driven signal — and suppressing an `actionError` that merely repeats
  // it avoids showing the same line twice while still surfacing an actionError with distinct
  // text (e.g. "an update is already waiting to install", which never touches last_error).
  const showActionError = actionError && actionError !== state.last_error;

  return (
    <div className="setting-group">
      <label className="setting-label">
        Updates <span className="version-label">v{state.current_version}</span>
      </label>

      <div className="setting-radios">
        {MODES.map((m) => (
          <label key={m.value} className="radio-label">
            <input
              type="radio"
              name="update_mode"
              checked={state.mode === m.value}
              onChange={() => commands.updateSetting("update_mode", m.value)}
            />
            {m.label}
          </label>
        ))}
      </div>

      <div className="setting-row">
        <button
          className="btn btn-small"
          onClick={() => checkNow()}
          disabled={CHECK_BLOCKED.has(state.status)}
        >
          Check now
        </button>
        <span className="update-status">
          {state.status === "checking" && "Checking…"}
          {state.status === "downloading" && "Downloading…"}
          {state.status !== "checking" &&
            state.status !== "downloading" &&
            state.last_checked !== null &&
            `Last checked ${relativeTime(state.last_checked)}`}
        </span>
      </div>

      {state.last_error && <div className="update-error">{state.last_error}</div>}
      {showActionError && <div className="update-error">{actionError}</div>}

      {state.status === "available" && state.available && (
        <div className="update-available">
          <AvailableDetails update={state.available} />
          <div className="setting-row">
            <button className="btn btn-small" onClick={() => install()}>
              Install and restart
            </button>
            <button className="btn btn-small" onClick={() => skip()}>
              Skip this version
            </button>
          </div>
        </div>
      )}

      {state.status === "waitingForIdle" && state.available && (
        <div className="update-available">
          <AvailableDetails update={state.available} />
          <div className="update-deferred">
            Downloaded — will install when the queue finishes
          </div>
        </div>
      )}

      {/* `just_installed` is read from the database independent of the in-memory `status`, so
          it survives the restart it names — after relaunch, status resets to idle on the new
          binary but this stays populated until the next update overwrites it. Suppressed only
          while a *newer* update is on offer (status "available"), which takes visual priority. */}
      {state.status !== "available" && state.just_installed && (
        <div className="update-available">
          <div className="update-version">What&apos;s new in {state.just_installed.version}</div>
          {state.just_installed.notes && (
            <pre className="update-notes">{state.just_installed.notes}</pre>
          )}
          {state.status === "readyToRestart" && (
            <button className="btn btn-small" onClick={() => restart()}>
              Restart now
            </button>
          )}
        </div>
      )}
    </div>
  );
}
