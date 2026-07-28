import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type UpdateState, type UpdateStatus } from "../lib/tauri";

// Statuses where the backend's `manual_check_block` (updater.rs) would refuse a fresh manual
// check: an install is downloading, waiting for the queue to drain, or installed and awaiting
// restart. Exported so the panel's "Check now" button reads the identical set, rather than a
// second hand-copied list that could quietly drift out of sync with the rule below.
//
// Also governs when a *checkNow* actionError is safe to clear on an incoming `update-state`
// push — see ActionKind below for why this only applies to checkNow, not install/skip. While
// status stays in this set, the situation that produced a checkNow error (most often a
// manual_check_block refusal) may still be actively unfolding — e.g. the pending install the
// refusal named keeps emitting its own progress events — and one of those must not race the
// error off screen before the user reads it. Only once status leaves this set (back to idle,
// available, or error) has the situation genuinely settled, and any earlier actionError is
// guaranteed stale rather than merely old — see the "network unreachable" -> mode change ->
// "available" case this rule fixes.
export const MANUAL_CHECK_BLOCKED_STATUSES: ReadonlySet<UpdateStatus> = new Set([
  "checking",
  "downloading",
  "waitingForIdle",
  "readyToRestart",
]);

// Which action produced the current actionError, so an incoming push can decide whether it's
// entitled to clear it. The three actions do not share one lifecycle:
//   - checkNow either rejects with a manual_check_block refusal (true for as long as status
//     stays blocked) or a network failure that dual-writes the identical text into
//     `state.last_error` (superseded the moment a later check succeeds) — both resolve exactly
//     when status leaves MANUAL_CHECK_BLOCKED_STATUSES.
//   - install/skip (`install_pending`/`skip_update_version` in the Rust source) reject with
//     precondition/serialization refusals — "an update operation is already running", "no
//     update available", "app state unavailable", a poisoned-lock message — and every one of
//     those Err paths returns before touching `status` at all. There is no push that
//     "resolves" one in general; a push merely reporting unrelated progress (e.g. a concurrent
//     cycle finishing and going idle) must not be mistaken for that resolution. So these clear
//     only when the user acts again, exactly like the pre-push-clearing behavior.
type ActionKind = "check" | "install" | "skip";

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
  // Which action `actionError` currently belongs to (see ActionKind). A ref, not state: only
  // the push listener and runAction ever read/write it, and neither needs a re-render from it.
  const actionKind = useRef<ActionKind | null>(null);

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
      // Only a checkNow error is ever cleared by a push, and only once status has left the
      // blocked set — see ActionKind and the comment on MANUAL_CHECK_BLOCKED_STATUSES. An
      // install/skip error is never touched here: no push can be trusted to mean "your refused
      // click no longer matters", since none of their Err paths move status in the first place.
      if (actionKind.current === "check" && !MANUAL_CHECK_BLOCKED_STATUSES.has(e.payload.status)) {
        setActionError(null);
        actionKind.current = null;
      }
    });

    return () => {
      alive = false;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const runAction = useCallback(async (kind: ActionKind, action: () => Promise<void>) => {
    actionKind.current = kind;
    setActionError(null);
    try {
      await action();
    } catch (e) {
      setActionError(String(e));
    }
  }, []);

  const checkNow = useCallback(
    () => runAction("check", () => commands.checkForUpdate()),
    [runAction],
  );
  const install = useCallback(
    () => runAction("install", () => commands.installUpdate()),
    [runAction],
  );
  const restart = useCallback(() => commands.restartApp(), []);

  const skip = useCallback(
    () =>
      runAction("skip", async () => {
        const version = latest.current?.available?.version;
        if (!version) return;
        await commands.skipUpdateVersion(version);
      }),
    [runAction],
  );

  return { state, actionError, checkNow, install, skip, restart };
}
