import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type UpdateState, type UpdateStatus } from "../lib/tauri";

// Statuses where the backend's `manual_check_block` (updater.rs) would refuse a fresh manual
// check: an install is downloading, waiting for the queue to drain, or installed and awaiting
// restart. Exported so the panel's "Check now" button reads the identical set, rather than a
// second hand-copied list that could quietly drift out of sync with the rule below.
//
// Also governs when a stale `actionError` is safe to clear on an incoming `update-state` push:
// while status stays in this set, the situation that produced the error (most often the very
// refusal above) may still be actively unfolding — e.g. the pending install the refusal named
// keeps emitting its own progress events — and one of those must not race the error off screen
// before the user reads it. Only once status leaves this set (back to idle, available, or error)
// has the situation genuinely settled, and any earlier actionError is guaranteed stale rather
// than merely old — see the "network unreachable" -> mode change -> "available" case this fixes.
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
  const [actionError, setActionError] = useState<string | null>(null);
  // The panel's actions read the freshest state without re-creating callbacks on every event.
  const latest = useRef<UpdateState | null>(null);
  latest.current = state;

  useEffect(() => {
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
      // A push landing while status is still "blocked" doesn't mean the situation has settled —
      // see the comment on MANUAL_CHECK_BLOCKED_STATUSES. Clearing unconditionally here would
      // wipe an actionError that's still the live explanation for what's happening.
      if (!MANUAL_CHECK_BLOCKED_STATUSES.has(e.payload.status)) {
        setActionError(null);
      }
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
      setActionError(String(e));
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

  const skip = useCallback(
    () =>
      runAction(async () => {
        const version = latest.current?.available?.version;
        if (!version) return;
        await commands.skipUpdateVersion(version);
      }),
    [runAction],
  );

  return { state, actionError, checkNow, install, skip, restart };
}
