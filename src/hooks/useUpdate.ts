import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "../lib/events";
import { commands, type UpdateMode, type UpdateState, type UpdateStatus } from "../lib/tauri";
import { isServerHead } from "../lib/head";
import { errorText } from "../lib/errors";

// Statuses where the backend's `manual_check_block` (updater.rs) would refuse a fresh manual
// check: an install is downloading, waiting for the queue to drain, or installed and awaiting
// restart. Exported so the panel's "Check now" button reads the identical set, rather than a
// second hand-copied list that could quietly drift out of sync.
export const MANUAL_CHECK_BLOCKED_STATUSES: ReadonlySet<UpdateStatus> = new Set([
  "checking",
  "downloading",
  "waitingForIdle",
  "readyToRestart",
]);

export function useUpdate() {
  const [state, setState] = useState<UpdateState | null>(null);
  // `checkForUpdate`/`installUpdate`/`skip` deliberately reject with a user-facing message
  // (e.g. a manual check refused while an install is pending) that never reaches `state` —
  // most of those rejections never touch the backend's `last_error`, which only records the
  // scheduler's own background check/install failures. Kept separate so a stale action
  // rejection doesn't get relabelled as "the last background check failed".
  //
  // This is deliberately NOT cleared by an incoming `update-state` push, on purpose, after three
  // rounds of trying to infer from unrelated push data whether a message about a past click was
  // still "current" — every rule leaked somewhere (a stale message could still contradict newer
  // state, or a fresh one could be wiped by an unrelated push before it was read). That inference
  // isn't decidable from push data. It clears on exactly two events instead: the user starts
  // another action (below), or the user dismisses it (`dismissError`) — both are user-driven, not
  // heuristic. The panel is responsible for framing the message as a past attempt (e.g. "Couldn't
  // start the install: …") precisely because it is never revalidated against later state.
  const [actionError, setActionError] = useState<string | null>(null);
  // The panel's actions read the freshest state without re-creating callbacks on every event.
  const latest = useRef<UpdateState | null>(null);
  latest.current = state;

  useEffect(() => {
    // Desktop-only: the updater has no server-head equivalent (commands.getUpdateState() is a
    // `notAvailable` stub there, which throws synchronously rather than rejecting — calling it
    // would crash the effect). `state` stays null, so `useUpdate`'s callers see no update ever
    // available, same as the other isServerHead-gated hooks (useFileIntake's drag-drop listener).
    if (isServerHead) return;

    let alive = true;
    // If the update-state event fires before this seed resolves, the event is newer — the
    // seed must not clobber it (same stale-response guard as useQueue's getQueue race).
    let sawEvent = false;

    commands
      .getUpdateState()
      .then((s) => { if (alive && !sawEvent) setState(s); })
      .catch(() => { /* backend not ready yet; the event will seed us */ });

    const unlisten = listen<UpdateState>("update-state", (e) => {
      if (!alive) return;
      sawEvent = true;
      setState(e.payload);
    });

    return () => {
      alive = false;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const runAction = useCallback(async (action: () => Promise<void>) => {
    setActionError(null);
    try {
      await action();
    } catch (e) {
      setActionError(errorText(e));
    }
  }, []);

  const checkNow = useCallback(
    () => runAction(() => commands.checkForUpdate()),
    [runAction],
  );
  const install = useCallback(
    () => runAction(() => commands.installUpdate()),
    [runAction],
  );
  const restart = useCallback(() => commands.restartApp(), []);
  // Routed through runAction like the other three: the write can reject, and an unhandled
  // rejection is the one outcome the panel cannot show. The new mode is not mirrored locally —
  // the backend's `on_mode_changed` pushes `update-state` back, which is what re-renders the
  // radios.
  const setMode = useCallback(
    (mode: UpdateMode) => runAction(() => commands.updateSetting("update_mode", mode)),
    [runAction],
  );

  const skip = useCallback(
    () =>
      runAction(async () => {
        const version = latest.current?.available?.version;
        if (!version) return;
        await commands.skipUpdateVersion(version);
      }),
    [runAction],
  );

  const dismissError = useCallback(() => setActionError(null), []);

  return { state, actionError, checkNow, install, skip, setMode, restart, dismissError };
}
