import { useState } from "react";
import { MANUAL_CHECK_BLOCKED_STATUSES, useUpdate } from "../hooks/useUpdate";
import { commands, type AvailableUpdate, type UpdateMode } from "../lib/tauri";

const MODES: { value: UpdateMode; label: string }[] = [
  { value: "automatic", label: "Automatic" },
  { value: "notify", label: "Notify me" },
  { value: "off", label: "Manual only" },
];

// Which of the three actions produced the current actionError, tracked here (not in the hook)
// purely to pick the right past-tense framing below. Set synchronously by the button's own
// onClick, before the async action runs — nothing else ever touches it, so unlike every prior
// attempt at tracking "which action" inside the hook, there is no event listener racing it.
type ActionKind = "check" | "install" | "skip";

const ACTION_PREFIX: Record<ActionKind, string> = {
  check: "Couldn't check for updates",
  install: "Couldn't start the install",
  skip: "Couldn't skip that version",
};

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
  const { state, actionError, checkNow, install, skip, restart, dismissError } = useUpdate();
  const [lastAction, setLastAction] = useState<ActionKind | null>(null);
  const [pending, setPending] = useState(false);

  // `lastAction` only names the right action if at most one action is outstanding: it is set on
  // click, while `actionError` is set whenever an action settles, so two overlapping actions
  // could pair the later click's name with the earlier action's failure ("Couldn't skip that
  // version: …" for a skip that worked). Rather than reconciling that afterwards, the action
  // buttons are disabled for the duration, which also leaves `actionError` a single writer at a
  // time. The re-entry guard mirrors QueueItem/HistoryPage: the disabled attribute is the user
  // affordance, this is the invariant.
  const startAction = async (kind: ActionKind, action: () => Promise<void>) => {
    if (pending) return;
    setLastAction(kind);
    setPending(true);
    try {
      await action();
    } finally {
      // Always re-enables — a rejected action leaves the panel usable, not inert.
      setPending(false);
    }
  };

  if (!state) return null;

  // `actionError` (a rejected checkNow/install/skip) and `state.last_error` (the scheduler's
  // own background check/install failure) are different channels, but a manual check made
  // while offline dual-writes the identical raw message to both. Comparing the raw strings
  // (before the past-tense framing below is applied) and preferring `last_error` avoids showing
  // the same fact twice while still surfacing an actionError with distinct text (e.g. "an update
  // is already waiting to install", which never touches last_error).
  const showActionError = actionError && actionError !== state.last_error;
  // actionError never expires on its own (see useUpdate) — it is a past-tense record of one
  // click's outcome, not a claim about current state, so framing it that way is what keeps it
  // from ever contradicting whatever renders below (e.g. an Install failure sitting above a
  // completed "What's new" restart prompt is fine once it reads as something that *was* refused,
  // not something that *is* happening).
  const framedActionError = lastAction ? `${ACTION_PREFIX[lastAction]}: ${actionError}` : actionError;

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
          onClick={() => startAction("check", checkNow)}
          disabled={pending || MANUAL_CHECK_BLOCKED_STATUSES.has(state.status)}
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
      {showActionError && (
        <div className="update-error">
          <span>{framedActionError}</span>
          <button className="btn-icon" onClick={dismissError} title="Dismiss">
            &times;
          </button>
        </div>
      )}

      {state.status === "available" && state.available && (
        <div className="update-available">
          <AvailableDetails update={state.available} />
          <div className="setting-row">
            <button
              className="btn btn-small"
              onClick={() => startAction("install", install)}
              disabled={pending}
            >
              Install and restart
            </button>
            <button
              className="btn btn-small"
              onClick={() => startAction("skip", skip)}
              disabled={pending}
            >
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
