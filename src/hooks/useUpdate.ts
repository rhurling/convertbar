import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type UpdateState } from "../lib/tauri";

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
